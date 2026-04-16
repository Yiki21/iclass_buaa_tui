use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Duration, Local, Utc};
use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, HeaderMap, HeaderValue, REFERER, USER_AGENT};
use scraper::{Html, Selector};
use serde_json::Value;
use std::collections::HashSet;

use crate::bykc::BykcApi;
use crate::constants::{SSO_VPN_LOGIN, network_urls};
use crate::model::{CourseDetailItem, CourseItem, LoginInput, Session, SignOutcome, SignQrData};

#[derive(Clone, Debug)]
pub struct IClassApi {
    client: reqwest::Client,
    use_vpn: bool,
}

impl IClassApi {
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
        if student_id.is_empty() {
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

        for offset in 0..=future_days {
            let date_str = (Local::now().date_naive() + Duration::days(offset as i64))
                .format("%Y%m%d")
                .to_string();
            for item in self
                .get_course_by_date(&session.user_id, &session.session_id, &date_str)
                .await?
            {
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

        let status_code = response.status().as_u16();
        let raw_text = response.text().await.context("签到响应读取失败")?;
        let raw = serde_json::from_str::<Value>(&raw_text).unwrap_or_else(|_| {
            serde_json::json!({
                "STATUS": "1",
                "ERRMSG": "签到接口返回非 JSON",
                "statusCode": status_code,
                "raw": raw_text.chars().take(200).collect::<String>(),
            })
        });

        let status = raw
            .get("STATUS")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();

        let message = raw
            .get("ERRMSG")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or("已提交")
            .to_string();

        Ok(SignOutcome {
            message,
            success_like: status == "0",
            http_status: status_code,
            server_status: status,
            raw_response: raw,
        })
    }

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

        if !response.status().is_success() {
            bail!("请求 iClass 用户信息失败，HTTP 状态: {}", response.status());
        }

        let data = parse_json(response).await?;
        ensure_status_ok(&data)?;
        let user_info = data
            .get("result")
            .cloned()
            .ok_or_else(|| anyhow!("iClass API 返回的用户信息格式异常"))?;

        Ok((user_info, server_time_offset_ms))
    }

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
            courses.push(CourseItem {
                name: value_to_string(item.get("course_name")).if_empty("未知课程"),
                id,
            });
        }

        Ok(courses)
    }

    async fn get_courses_detail(
        &self,
        user_id: &str,
        session_id: &str,
        courses: &[CourseItem],
    ) -> Result<Vec<CourseDetailItem>> {
        let urls = network_urls(self.use_vpn);
        let mut details = Vec::new();

        for course in courses {
            let url = format!(
                "{}?id={}&courseId={}&sessionId={}",
                urls.course_sign_detail, user_id, course.id, session_id
            );
            let data = parse_json(
                self.client
                    .get(&url)
                    .send()
                    .await
                    .with_context(|| format!("请求课程详情失败: {}", course.name))?,
            )
            .await?;

            let records = data
                .get("result")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default();

            for record in records {
                details.push(CourseDetailItem {
                    name: course.name.clone(),
                    id: course.id.clone(),
                    course_sched_id: value_to_string(record.get("courseSchedId")),
                    date: normalize_date_display(&value_to_string(record.get("teachTime"))),
                    start_time: normalize_time_display(&value_to_string(
                        record.get("classBeginTime"),
                    )),
                    end_time: normalize_time_display(&value_to_string(record.get("classEndTime"))),
                    sign_status: value_to_string(record.get("signStatus")),
                });
            }
        }

        Ok(details)
    }

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
            details.push(CourseDetailItem {
                name: value_to_string(record.get("courseName")).if_empty("未知课程"),
                id: value_to_string(record.get("courseId")),
                course_sched_id: value_to_string(record.get("id")),
                date: normalize_date_display(
                    &value_to_string(record.get("teachTime")).if_empty(date_str),
                ),
                start_time: normalize_time_display(&value_to_string(record.get("classBeginTime"))),
                end_time: normalize_time_display(&value_to_string(record.get("classEndTime"))),
                sign_status: value_to_string(record.get("signStatus")),
            });
        }

        Ok(details)
    }
}

fn merged_key(item: &CourseDetailItem) -> String {
    if !item.course_sched_id.is_empty() {
        format!("sched:{}", item.course_sched_id)
    } else {
        format!("fallback:{}|{}|{}", item.id, item.date, item.name)
    }
}

fn looks_like_iclass_url(url: &str) -> bool {
    url.contains("iclass.buaa.edu.cn") || url.contains("d.buaa.edu.cn/https-834")
}

fn looks_like_vpn_portal_home(url: &str) -> bool {
    reqwest::Url::parse(url)
        .map(|parsed| {
            parsed.host_str() == Some("d.buaa.edu.cn") && !parsed.path().contains("/login")
        })
        .unwrap_or(false)
}

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

fn ensure_status_ok(data: &Value) -> Result<()> {
    if data.get("STATUS").and_then(Value::as_str) == Some("0") {
        return Ok(());
    }
    bail!("iClass API 返回错误: {}", data);
}

fn normalize_date_display(raw: &str) -> String {
    let digits: String = raw.chars().filter(|c| c.is_ascii_digit()).collect();
    if digits.len() >= 8 {
        format!("{}-{}-{}", &digits[0..4], &digits[4..6], &digits[6..8])
    } else {
        raw.trim().to_string()
    }
}

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

fn value_to_string(value: Option<&Value>) -> String {
    match value {
        Some(Value::String(v)) => v.clone(),
        Some(Value::Number(v)) => v.to_string(),
        Some(Value::Bool(v)) => v.to_string(),
        _ => String::new(),
    }
}

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

trait StringFallback {
    fn if_empty(self, fallback: &str) -> String;
}

impl StringFallback for String {
    fn if_empty(self, fallback: &str) -> String {
        if self.trim().is_empty() {
            fallback.to_string()
        } else {
            self
        }
    }
}
