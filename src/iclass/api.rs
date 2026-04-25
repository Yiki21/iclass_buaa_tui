//! iClass client implementation, login flow, course loading, and sign requests.
//! Sorry for this big file because i don't wanna split it into multi shits
//! and that's make the maintain more difficult for me
//! maybe in the future i will carefully split it

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Duration, Local, Utc};
use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, HeaderMap, HeaderValue, REFERER, USER_AGENT};
use scraper::{Html, Selector};
use serde_json::Value;
use std::collections::HashSet;

use crate::bykc::BykcApi;
use crate::constants::{SSO_VPN_LOGIN, VPN_OFFSET_CORRECTION_MS, network_urls};
use crate::model::{CourseDetailItem, CourseItem, LoginInput, Session, SignOutcome, SignQrData};

#[derive(Clone, Debug)]
pub struct IClassApi {
    client: reqwest::Client,
    use_vpn: bool,
}

impl IClassApi {
    /// Creates one iClass HTTP client with the headers shared by all requests.
    pub fn new(use_vpn: bool) -> Result<Self> {
        let mut headers = HeaderMap::new();
        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 \
                 (KHTML, like Gecko) Chrome/134.0.0.0 Safari/537.36",
            ),
        );
        headers.insert(ACCEPT_LANGUAGE, HeaderValue::from_static("zh-CN,zh;q=0.9"));
        headers.insert(
            ACCEPT,
            HeaderValue::from_static("application/json, text/html;q=0.9, */*;q=0.8"),
        );

        let client = reqwest::Client::builder()
            .cookie_store(true)
            .default_headers(headers)
            .build()
            .context("failed to build reqwest client")?;

        Ok(Self { client, use_vpn })
    }

    /// Logs in and captures the server clock offset needed by later sign requests.
    pub async fn login(&self, input: &LoginInput) -> Result<Session> {
        let student_id = input.student_id.trim();
        if student_id.is_empty() && !self.use_vpn {
            bail!("学号不能为空");
        }

        if self.use_vpn {
            self.vpn_login(&input.vpn_username, &input.vpn_password)
                .await?;
        }

        let (user_info, server_time_offset_ms) = self.fetch_user_info(student_id).await?;
        let user_id = user_info
            .get("id")
            .and_then(Value::as_i64)
            .map(|v| v.to_string())
            .or_else(|| {
                user_info
                    .get("id")
                    .and_then(Value::as_str)
                    .map(ToOwned::to_owned)
            })
            .unwrap_or_default();
        let user_name = user_info
            .get("realName")
            .and_then(Value::as_str)
            .or_else(|| user_info.get("name").and_then(Value::as_str))
            .unwrap_or(student_id)
            .to_string();
        let session_id = user_info
            .get("sessionId")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        if user_id.is_empty() || session_id.is_empty() {
            bail!("登录成功但用户信息不完整，请重试");
        }

        Ok(Session {
            api: self.clone(),
            bykc_api: if input.use_vpn {
                Some(BykcApi::new(input.clone())?)
            } else {
                None
            },
            user_id,
            user_name,
            session_id,
            server_time_offset_ms,
            use_vpn: self.use_vpn,
        })
    }

    /// Loads course details from both the legacy list/detail flow and the daily schedule API.
    ///
    /// Why:
    /// The upstream iClass APIs are inconsistent. One endpoint is better for
    /// complete course coverage, while the other is better for near-term
    /// schedule accuracy. Merging both gives the TUI and CLI a more reliable
    /// view of what can actually be signed.
    ///
    /// How:
    /// Query the semester-based course list first, expand it into detail rows,
    /// then load the date-based rows for `future_days` concurrently. Finally,
    /// deduplicate by course/date/time identity and sort for stable display.
    pub async fn get_merged_course_details(
        &self,
        session: &Session,
        future_days: usize,
    ) -> Result<Vec<CourseDetailItem>> {
        let semester_code = self
            .get_current_semester(&session.user_id, &session.session_id)
            .await?
            .ok_or_else(|| anyhow!("未获取到当前学期"))?;
        let courses = self
            .get_courses(&session.user_id, &session.session_id, &semester_code)
            .await?;
        let detail_data = self
            .get_courses_detail(&session.user_id, &session.session_id, &courses)
            .await?;

        let mut merged = Vec::with_capacity(detail_data.len());
        let mut seen = HashSet::new();
        for item in detail_data {
            let key = merged_key(&item);
            if seen.insert(key) {
                merged.push(item);
            }
        }

        let mut tasks = Vec::with_capacity(future_days + 1);
        for offset in 0..=future_days {
            let api = self.clone();
            let user_id = session.user_id.clone();
            let session_id = session.session_id.clone();
            let date_str = (Local::now().date_naive() + Duration::days(offset as i64))
                .format("%Y%m%d")
                .to_string();
            tasks.push(tokio::spawn(async move {
                api.get_course_by_date(&user_id, &session_id, &date_str)
                    .await
            }));
        }

        for task in tasks {
            for item in task.await.context("按日期获取课程任务执行失败")?? {
                let key = merged_key(&item);
                if seen.insert(key) {
                    merged.push(item);
                }
            }
        }

        merged.sort_by(|a, b| {
            (
                a.date.as_str(),
                a.start_time.as_str(),
                a.end_time.as_str(),
                a.name.as_str(),
            )
                .cmp(&(
                    b.date.as_str(),
                    b.start_time.as_str(),
                    b.end_time.as_str(),
                    b.name.as_str(),
                ))
        });

        Ok(merged)
    }

    /// Submits one immediate iClass sign request using the server-aligned timestamp.
    ///
    /// Why:
    /// iClass sign requests are sensitive to server time drift. Reusing the
    /// offset captured at login keeps the CLI and TUI aligned with the server's
    /// notion of "now" without adding extra round trips before every sign.
    ///
    /// How:
    /// Reuse the same JSON parsing and business-status check as the other iClass
    /// endpoints so sign requests fail consistently on malformed responses or
    /// non-zero `STATUS` codes instead of silently inventing a fallback payload.
    pub async fn sign_now(&self, session: &Session, course_sched_id: &str) -> Result<SignOutcome> {
        let course_sched_id = course_sched_id.trim();
        if course_sched_id.is_empty() {
            bail!("courseSchedId 不能为空");
        }

        let urls = network_urls(self.use_vpn);
        let timestamp = session.server_now_millis().to_string();

        let response = self
            .client
            .post(urls.scan_sign)
            .query(&[
                ("id", session.user_id.as_str()),
                ("courseSchedId", course_sched_id),
                ("timestamp", timestamp.as_str()),
            ])
            .header("sessionId", &session.session_id)
            .send()
            .await
            .context("签到请求失败")?;

        let http_status = response.status().as_u16();
        let raw_response = parse_json(response).await.context("签到响应解析失败")?;
        ensure_status_ok(&raw_response).context("签到失败")?;

        let server_status = raw_response
            .get("STATUS")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let message = raw_response
            .get("ERRMSG")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .or_else(|| raw_response.get("MSG").and_then(Value::as_str))
            .unwrap_or("已提交")
            .to_string();

        Ok(SignOutcome {
            message,
            success_like: true,
            http_status,
            server_status,
            raw_response,
        })
    }

    /// Builds the QR payload that the mobile client would normally scan.
    ///
    /// Why:
    /// The TUI and CLI expose a QR mode for users who want the timestamped sign
    /// URL without immediately firing the request. That keeps QR generation and
    /// direct sign submission aligned on the same URL shape.
    pub fn generate_sign_qr(&self, course_sched_id: &str, timestamp_ms: i64) -> Result<SignQrData> {
        if self.use_vpn {
            bail!("VPN 模式不支持生成二维码，请使用直接签到");
        }

        let course_sched_id = course_sched_id.trim();
        if course_sched_id.is_empty() {
            bail!("courseSchedId 不能为空");
        }

        let qr_timestamp = timestamp_ms;
        let sign_url = network_urls(self.use_vpn).scan_sign;
        let qr_url = format!(
            "{}?courseSchedId={}&timestamp={}",
            sign_url,
            encode_component(course_sched_id),
            encode_component(&qr_timestamp.to_string())
        );

        Ok(SignQrData {
            qr_url,
            course_sched_id: course_sched_id.to_string(),
            timestamp: qr_timestamp,
        })
    }

    /// Completes the BUAA VPN login flow and verifies that iClass is reachable.
    ///
    /// Why:
    /// A successful SSO form submission is not enough on its own. The VPN may
    /// still leave the session on a portal page, which later causes confusing
    /// iClass failures. This helper finishes the login and proves the cookies
    /// actually grant access to the target service.
    async fn vpn_login(&self, username: &str, password: &str) -> Result<()> {
        if username.trim().is_empty() || password.is_empty() {
            bail!("VPN 模式需要输入账号和密码");
        }

        let execution = self.fetch_execution().await?;
        let response = self
            .client
            .post(SSO_VPN_LOGIN)
            .header(REFERER, SSO_VPN_LOGIN)
            .form(&[
                ("username", username.trim()),
                ("password", password),
                ("submit", "登录"),
                ("type", "username_password"),
                ("execution", execution.as_str()),
                ("_eventId", "submit"),
            ])
            .send()
            .await
            .context("VPN 登录请求失败")?;

        if response.status() == reqwest::StatusCode::UNAUTHORIZED {
            bail!("登录失败：账号或密码错误，或密码过弱需先修改后再登录");
        }

        let final_url = response.url().to_string();
        if looks_like_iclass_url(&final_url) {
            return Ok(());
        }

        if looks_like_vpn_portal_home(&final_url) {
            let urls = network_urls(true);
            let probe = self
                .client
                .get(format!("{}/", urls.service_home.trim_end_matches('/')))
                .send()
                .await
                .context("进入 iClass 服务失败")?;
            let probe_final = probe.url().to_string();
            if looks_like_iclass_url(&probe_final) {
                return Ok(());
            }
            bail!("VPN 登录后进入 iClass 失败，最终 URL: {probe_final}");
        }

        bail!("登录失败，最终 URL: {final_url}");
    }

    /// Extracts the transient `execution` token required by BUAA SSO.
    ///
    /// Why:
    /// The login form is stateful and rejects submissions without the current
    /// hidden token, so this remains a dedicated pre-step instead of being
    /// inlined into the larger VPN login flow.
    async fn fetch_execution(&self) -> Result<String> {
        let body = self
            .client
            .get(SSO_VPN_LOGIN)
            .send()
            .await
            .context("获取 SSO 登录页失败")?
            .text()
            .await
            .context("读取 SSO 登录页失败")?;

        let document = Html::parse_document(&body);
        let selector = Selector::parse(r#"input[name="execution"]"#)
            .map_err(|_| anyhow!("SSO 页面选择器构造失败"))?;

        let execution = document
            .select(&selector)
            .next()
            .and_then(|node| node.value().attr("value"))
            .map(str::to_string)
            .ok_or_else(|| anyhow!("无法从 SSO 登录页面解析 execution 参数"))?;

        Ok(execution)
    }

    /// Fetches user info and derives `server_time_offset_ms` from the HTTP `Date` header.
    ///
    /// Why:
    /// iClass sign requests are time-sensitive, and the server clock can differ
    /// from the local machine. Capturing the offset once during login is the
    /// cheapest way to keep later sign timestamps aligned.
    async fn fetch_user_info(&self, username: &str) -> Result<(Value, i64)> {
        let urls = network_urls(self.use_vpn);
        let response = self
            .client
            .get(urls.user_login)
            .query(&[
                ("phone", username),
                ("password", ""),
                ("verificationType", "2"),
                ("verificationUrl", ""),
                ("userLevel", "1"),
            ])
            .send()
            .await
            .context("请求 iClass 用户信息失败")?;
        let server_time_offset_ms = response
            .headers()
            .get("date")
            .and_then(|value| value.to_str().ok())
            .and_then(|value| DateTime::parse_from_rfc2822(value).ok())
            .map(|server_time| {
                server_time
                    .timestamp_millis()
                    .saturating_sub(Utc::now().timestamp_millis())
            })
            .unwrap_or(0);

        // if self.use_vpn {
        //     server_time_offset_ms += VPN_OFFSET_CORRECTION_MS;
        // }

        if !response.status().is_success() {
            bail!("请求 iClass 用户信息失败，HTTP 状态: {}", response.status());
        }

        let mut data = parse_json(response).await?;
        ensure_status_ok(&data)?;
        let user_info = data
            .get_mut("result")
            .ok_or_else(|| anyhow!("iClass API 返回的用户信息格式异常"))?
            .take();

        Ok((user_info, server_time_offset_ms))
    }

    /// Resolves the semester code that downstream course APIs expect.
    ///
    /// Why:
    /// Most course endpoints need an explicit semester code, but the login
    /// response does not provide one. Preferring the row marked current keeps
    /// normal behavior correct while still falling back gracefully if the flag
    /// is absent.
    async fn get_current_semester(
        &self,
        user_id: &str,
        session_id: &str,
    ) -> Result<Option<String>> {
        let urls = network_urls(self.use_vpn);
        let data = parse_json(
            self.client
                .get(urls.semester_list)
                .query(&[("userId", user_id), ("type", "2")])
                .header("sessionId", session_id)
                .send()
                .await
                .context("请求学期列表失败")?,
        )
        .await?;

        ensure_status_ok(&data)?;

        let semesters = data
            .get("result")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let current = semesters
            .iter()
            .find(|item| item.get("yearStatus").and_then(Value::as_str) == Some("1"))
            .or_else(|| semesters.first())
            .and_then(|item| item.get("code"))
            .and_then(Value::as_str)
            .map(ToOwned::to_owned);

        Ok(current)
    }

    /// Loads the coarse course list for the current semester.
    ///
    /// Why:
    /// The detail API is keyed by course id, so we first need this lightweight
    /// list as the expansion seed for the richer schedule records.
    async fn get_courses(
        &self,
        user_id: &str,
        session_id: &str,
        semester_code: &str,
    ) -> Result<Vec<CourseItem>> {
        let urls = network_urls(self.use_vpn);
        let data = parse_json(
            self.client
                .get(urls.course_list)
                .query(&[
                    ("user_type", "1"),
                    ("id", user_id),
                    ("xq_code", semester_code),
                ])
                .header("sessionId", session_id)
                .send()
                .await
                .context("请求课程列表失败")?,
        )
        .await?;

        ensure_status_ok(&data)?;

        let result = data
            .get("result")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut courses = Vec::new();
        for item in result {
            let id = value_to_string(item.get("course_id"));
            if id.is_empty() {
                continue;
            }
            let course_name = value_to_string(item.get("course_name"));
            courses.push(CourseItem {
                name: if course_name.trim().is_empty() {
                    "未知课程".to_string()
                } else {
                    course_name
                },
                id,
            });
        }

        Ok(courses)
    }

    /// Expands each semester course into signable schedule rows.
    ///
    /// Why:
    /// The semester list only tells us which courses exist. The TUI and CLI act
    /// on concrete schedule rows, so we fan out here and normalize the result
    /// into one shared `CourseDetailItem` shape.
    ///
    /// How:
    /// The upstream detail API is per-course, so fetching dozens of rows
    /// serially wastes time on avoidable network latency. We fire all detail
    /// requests concurrently, then sort the flattened result to keep the final
    /// display stable for the TUI and CLI.
    async fn get_courses_detail(
        &self,
        user_id: &str,
        session_id: &str,
        courses: &[CourseItem],
    ) -> Result<Vec<CourseDetailItem>> {
        let urls = network_urls(self.use_vpn);
        let mut tasks = Vec::with_capacity(courses.len());
        for course in courses {
            let api = self.clone();
            let user_id = user_id.to_string();
            let session_id = session_id.to_string();
            let course = course.clone();
            tasks.push(tokio::spawn(async move {
                let url = format!(
                    "{}?id={}&courseId={}&sessionId={}",
                    urls.course_sign_detail, user_id, course.id, session_id
                );
                let data = parse_json(
                    api.client
                        .get(&url)
                        .send()
                        .await
                        .with_context(|| format!("请求课程详情失败: {}", course.name))?,
                )
                .await?;
                // ensure_status_ok(&data)
                //     .with_context(|| format!("课程详情返回业务错误: {}", course.name))?;

                let records = data
                    .get("result")
                    .and_then(Value::as_array)
                    .cloned()
                    .unwrap_or_default();

                let mut details = Vec::with_capacity(records.len());
                for record in records {
                    details.push(CourseDetailItem {
                        name: course.name.clone(),
                        id: course.id.clone(),
                        course_sched_id: value_to_string(record.get("courseSchedId")),
                        date: normalize_date_display(&value_to_string(record.get("teachTime"))),
                        start_time: normalize_time_display(&value_to_string(
                            record.get("classBeginTime"),
                        )),
                        end_time: normalize_time_display(&value_to_string(
                            record.get("classEndTime"),
                        )),
                        sign_status: value_to_string(record.get("signStatus")),
                    });
                }

                Ok::<Vec<CourseDetailItem>, anyhow::Error>(details)
            }));
        }

        let mut details = Vec::new();
        for task in tasks {
            details.extend(task.await.context("课程详情任务执行失败")??);
        }

        details.sort_by(|a, b| {
            (
                a.date.as_str(),
                a.start_time.as_str(),
                a.end_time.as_str(),
                a.name.as_str(),
                a.id.as_str(),
                a.course_sched_id.as_str(),
            )
                .cmp(&(
                    b.date.as_str(),
                    b.start_time.as_str(),
                    b.end_time.as_str(),
                    b.name.as_str(),
                    b.id.as_str(),
                    b.course_sched_id.as_str(),
                ))
        });

        Ok(details)
    }

    /// Loads schedule rows from the date-based API for one calendar day.
    ///
    /// Why:
    /// This endpoint often exposes imminent classes more accurately than the
    /// semester-detail flow. Keeping it separate lets the caller merge both data
    /// sources and prefer broader coverage over trusting only one endpoint.
    async fn get_course_by_date(
        &self,
        user_id: &str,
        session_id: &str,
        date_str: &str,
    ) -> Result<Vec<CourseDetailItem>> {
        let urls = network_urls(self.use_vpn);
        let data = parse_json(
            self.client
                .get(urls.course_schedule_by_date)
                .query(&[("id", user_id), ("dateStr", date_str)])
                .header("sessionId", session_id)
                .send()
                .await
                .with_context(|| format!("按日期获取课程失败: {date_str}"))?,
        )
        .await?;

        if data.get("STATUS").and_then(Value::as_str) == Some("2") {
            return Ok(Vec::new());
        }
        ensure_status_ok(&data)?;

        let records = data
            .get("result")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();

        let mut details = Vec::new();
        for record in records {
            let course_name = value_to_string(record.get("courseName"));
            let teach_time = value_to_string(record.get("teachTime"));
            details.push(CourseDetailItem {
                name: if course_name.trim().is_empty() {
                    "未知课程".to_string()
                } else {
                    course_name
                },
                id: value_to_string(record.get("courseId")),
                course_sched_id: value_to_string(record.get("id")),
                date: normalize_date_display(if teach_time.trim().is_empty() {
                    date_str
                } else {
                    teach_time.as_str()
                }),
                start_time: normalize_time_display(&value_to_string(record.get("classBeginTime"))),
                end_time: normalize_time_display(&value_to_string(record.get("classEndTime"))),
                sign_status: value_to_string(record.get("signStatus")),
            });
        }

        Ok(details)
    }
}

/// Builds the deduplication key used when merging multiple iClass data sources.
///
/// Why:
/// The semester-detail API and the date-based API overlap but do not always
/// expose the same identifiers. Prefer `course_sched_id` when present, then
/// fall back to a coarse identity so the merged list stays stable.
fn merged_key(item: &CourseDetailItem) -> String {
    if !item.course_sched_id.is_empty() {
        format!("sched:{}", item.course_sched_id)
    } else {
        format!("fallback:{}|{}|{}", item.id, item.date, item.name)
    }
}

/// Detects whether a redirect target has already landed inside iClass.
fn looks_like_iclass_url(url: &str) -> bool {
    url.contains("iclass.buaa.edu.cn") || url.contains("d.buaa.edu.cn/https-834")
}

/// Detects the generic VPN portal page that appears before entering iClass.
fn looks_like_vpn_portal_home(url: &str) -> bool {
    reqwest::Url::parse(url)
        .map(|parsed| {
            parsed.host_str() == Some("d.buaa.edu.cn") && !parsed.path().contains("/login")
        })
        .unwrap_or(false)
}

/// Parses JSON while preserving enough response context for debugging.
///
/// Why:
/// Both the TUI and CLI need actionable errors when the upstream service sends
/// HTML, truncated JSON, or other unexpected payloads. Centralizing the parsing
/// keeps those diagnostics consistent.
async fn parse_json(response: reqwest::Response) -> Result<Value> {
    let status = response.status();
    let body = response.text().await.context("读取响应失败")?;
    serde_json::from_str(&body).with_context(|| {
        format!(
            "响应不是合法 JSON，HTTP 状态: {status}, body: {}",
            body.chars().take(200).collect::<String>()
        )
    })
}

/// Interprets iClass' business-status convention.
///
/// Why:
/// Many iClass endpoints return HTTP 200 even when the operation failed, so the
/// JSON `STATUS` field is the real success signal.
fn ensure_status_ok(data: &Value) -> Result<()> {
    if data.get("STATUS").and_then(Value::as_str) == Some("0") {
        return Ok(());
    }
    bail!("iClass API 返回错误: {}", data);
}

/// Normalizes date-like fields into `YYYY-MM-DD` for stable display and merge keys.
fn normalize_date_display(raw: &str) -> String {
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 8 {
        format!("{}-{}-{}", &digits[0..4], &digits[4..6], &digits[6..8])
    } else {
        raw.trim().to_string()
    }
}

/// Normalizes time-like fields into `HH:MM` when the upstream payload is loose.
fn normalize_time_display(raw: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        return String::new();
    }

    let time_part = raw.split_once(' ').map(|(_, right)| right).unwrap_or(raw);
    let mut parts = time_part.split(':');
    let hour = parts.next().unwrap_or_default();
    let minute = parts.next().unwrap_or_default();
    if hour.is_empty() || minute.is_empty() {
        return time_part.to_string();
    }

    format!("{:0>2}:{}", hour, minute)
}

/// Converts a permissive JSON scalar into a displayable string.
///
/// Why:
/// The upstream APIs mix strings, numbers, and booleans for the same logical
/// fields, so callers use this helper to keep normalization code compact.
fn value_to_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(v)) => v.clone(),
        Some(Value::Number(v)) => v.to_string(),
        Some(Value::Bool(v)) => v.to_string(),
        _ => String::new(),
    }
}

/// Percent-encodes one query component for QR URL generation.
fn encode_component(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char);
            }
            _ => {
                encoded.push('%');
                encoded.push_str(&format!("{byte:02X}"));
            }
        }
    }
    encoded
}
