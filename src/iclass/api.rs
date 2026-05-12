//! iClass client implementation, login flow, course loading, and sign requests.
//! Sorry for this big file because i don't wanna split it into multi shits
//! and that's make the maintain more difficult for me
//! maybe in the future i will carefully split it

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Duration, Local, Utc};
use reqwest::header::{
    ACCEPT, ACCEPT_LANGUAGE, HeaderMap, HeaderValue, ORIGIN, REFERER, USER_AGENT,
};
use scraper::{Html, Selector};
use serde_json::Value;
use std::{collections::HashSet, fs, net::ToSocketAddrs, path::PathBuf, time::Instant};

use crate::bykc::BykcApi;
use crate::constants::{BYKC_DIRECT_BASE, VPN_OFFSET_CORRECTION_MS, network_urls, sso_vpn_entry};
use crate::model::{
    CourseDetailItem, CourseItem, DoctorCheck, DoctorReport, LoginCaptchaChallenge,
    LoginDiagnostic, LoginFailureKind, LoginInput, LoginStart, Session, SignOutcome, SignQrData,
};

#[derive(Clone, Debug)]

pub struct IClassApi {
    client:  reqwest::Client,
    use_vpn: bool,
}

#[derive(Clone, Debug)]

struct LoginFormState {
    login_url:           String,
    action_url:          String,
    form:                Vec<(String, String)>,
    captcha_id:          Option<String>,
    page_hint:           String,
    captcha_field_names: Vec<String>,
}

impl IClassApi {
    /// Creates one iClass HTTP client with the headers shared by all requests.

    pub fn new(use_vpn: bool) -> Result<Self> {

        let mut headers = HeaderMap::new();

        headers.insert(
            USER_AGENT,
            HeaderValue::from_static(
                "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) \
                 Chrome/134.0.0.0 Safari/537.36",
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

        self.login_with_diagnostic(input)
            .await
            .map_err(|diagnostic| anyhow!(diagnostic.summary))
    }

    pub async fn start_login(
        &self,
        input: &LoginInput,
    ) -> std::result::Result<LoginStart, LoginDiagnostic> {

        let student_id = input.student_id.trim();

        if student_id.is_empty() && !self.use_vpn {

            return Err(LoginDiagnostic {
                kind:        LoginFailureKind::Validation,
                stage:       "input".to_string(),
                summary:     "学号不能为空".to_string(),
                error_chain: vec!["学号不能为空".to_string()],
                final_url:   None,
                http_status: None,
                page_hint:   None,
                suggestions: vec!["直连模式下请输入学号后重试".to_string()],
            });
        }

        if self.use_vpn {

            let form_state = self
                .fetch_login_form_state(&input.vpn_username, &input.vpn_password)
                .await
                .map_err(|error| diagnose_login_error("vpn_login_page", error, None, None, None))?;

            if let Some(captcha_id) = form_state.captcha_id.clone() {

                let captcha_path =
                    self.download_captcha_image(&captcha_id)
                        .await
                        .map_err(|error| {

                            diagnose_login_error("vpn_captcha", error, None, None, None)
                        })?;

                return Ok(LoginStart::Captcha(LoginCaptchaChallenge {
                    login_url: form_state.login_url,
                    action_url: form_state.action_url,
                    form: form_state.form,
                    captcha_id,
                    captcha_path: captcha_path.display().to_string(),
                    page_hint: form_state.page_hint,
                    captcha_field_names: form_state.captcha_field_names,
                }));
            }

            self.finish_vpn_login_submission(
                &form_state.login_url,
                &form_state.action_url,
                &form_state.form,
            )
            .await
            .map_err(|error| diagnose_login_error("vpn_login_submit", error, None, None, None))?;
        }

        self.finish_login_session(input)
            .await
            .map(LoginStart::Complete)
    }

    pub async fn login_with_diagnostic(
        &self,
        input: &LoginInput,
    ) -> std::result::Result<Session, LoginDiagnostic> {

        match self.start_login(input).await? {
            LoginStart::Complete(session) => Ok(session),
            LoginStart::Captcha(challenge) => {
                Err(LoginDiagnostic {
                    kind:        LoginFailureKind::Captcha,
                    stage:       "vpn_captcha".to_string(),
                    summary:     "当前 VPN 登录需要验证码，请在 TUI 中输入验证码后继续，CLI \
                                  请改用浏览器登录"
                        .to_string(),
                    error_chain: vec!["当前 VPN 登录需要验证码".to_string()],
                    final_url:   Some(challenge.login_url),
                    http_status: None,
                    page_hint:   Some(challenge.page_hint),
                    suggestions: vec![
                        format!("验证码图片路径: {}", challenge.captcha_path),
                        "TUI 中可继续输入验证码".to_string(),
                        "CLI 请先在浏览器完成一次登录".to_string(),
                    ],
                })
            }
        }
    }

    pub async fn continue_captcha_login(
        &self,
        input: &LoginInput,
        challenge: &LoginCaptchaChallenge,
        captcha: &str,
    ) -> std::result::Result<Session, LoginDiagnostic> {

        let captcha = captcha.trim();

        if captcha.is_empty() {

            return Err(LoginDiagnostic {
                kind:        LoginFailureKind::Validation,
                stage:       "vpn_captcha".to_string(),
                summary:     "验证码不能为空".to_string(),
                error_chain: vec!["验证码不能为空".to_string()],
                final_url:   Some(challenge.login_url.clone()),
                http_status: None,
                page_hint:   Some(challenge.page_hint.clone()),
                suggestions: vec!["输入验证码后重试".to_string()],
            });
        }

        let form = append_captcha_fields(&challenge.form, &challenge.captcha_field_names, captcha);

        self.finish_vpn_login_submission(&challenge.login_url, &challenge.action_url, &form)
            .await
            .map_err(|error| {

                diagnose_login_error(
                    "vpn_captcha_submit",
                    error,
                    Some(challenge.login_url.clone()),
                    None,
                    Some(challenge.page_hint.clone()),
                )
            })?;

        self.finish_login_session(input).await
    }

    pub async fn doctor(&self) -> DoctorReport {

        let urls = network_urls(self.use_vpn);

        let mut checks = Vec::new();

        checks.push(
            self.run_doctor_check("webvpn_home", "https://d.buaa.edu.cn/")
                .await,
        );

        checks.push(self.run_doctor_check("sso_login", &sso_vpn_entry()).await);

        checks.push(
            self.run_doctor_check("iclass_login_api", &urls.user_login)
                .await,
        );

        let bykc_login = if self.use_vpn {

            crate::constants::to_webvpn_url(&format!("{BYKC_DIRECT_BASE}/sscv/cas/login"))
        } else {

            format!("{BYKC_DIRECT_BASE}/sscv/cas/login")
        };

        checks.push(self.run_doctor_check("bykc_cas_entry", &bykc_login).await);

        DoctorReport {
            use_vpn: self.use_vpn,
            checks,
        }
    }

    async fn run_doctor_check(&self, name: &str, target: &str) -> DoctorCheck {

        let resolved_addrs = resolve_host_addrs(target);

        let dns_ok = !resolved_addrs.is_empty();

        let start = Instant::now();

        let result = self.client.get(target).send().await;

        let elapsed_ms = start.elapsed().as_millis();

        match result {
            Ok(response) => {

                let http_status = response.status().as_u16();

                let final_url = response.url().to_string();

                let ok = response.status().is_success() || response.status().is_redirection();

                DoctorCheck {
                    name: name.to_string(),
                    target: target.to_string(),
                    elapsed_ms,
                    ok: dns_ok && ok,
                    status: if ok {

                        "reachable".to_string()
                    } else {

                        format!("http_{}", http_status)
                    },
                    http_status: Some(http_status),
                    final_url: Some(final_url),
                    suggestion: doctor_suggestion(name, dns_ok, Some(http_status), None),
                    resolved_addrs,
                }
            }
            Err(error) => {
                DoctorCheck {
                    name: name.to_string(),
                    target: target.to_string(),
                    elapsed_ms,
                    ok: false,
                    status: classify_reqwest_error(&error).0,
                    http_status: None,
                    final_url: None,
                    suggestion: doctor_suggestion(name, dns_ok, None, Some(&error)),
                    resolved_addrs,
                }
            }
        }
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

    /// Submits one immediate iClass sign request using iClass' own sign timestamp.
    ///
    /// Why:
    /// iClass sign requests are sensitive to the timestamp format and clock used
    /// by the port-8081 sign service. UBAA's backend fetches that timestamp from
    /// `get_timestamp.action` immediately before posting the sign request, which
    /// avoids the VPN path sending a locally inferred millisecond value.
    ///
    /// How:
    /// Match the upstream request shape: `courseSchedId` and `timestamp` stay in
    /// the query string, while `id` is submitted as form data.

    pub async fn sign_now(&self, session: &Session, course_sched_id: &str) -> Result<SignOutcome> {

        let course_sched_id = course_sched_id.trim();

        if course_sched_id.is_empty() {

            bail!("courseSchedId 不能为空");
        }

        let urls = network_urls(self.use_vpn);

        let timestamp = self.fetch_sign_timestamp().await?;

        let response = self
            .client
            .post(urls.scan_sign)
            .query(&[
                ("courseSchedId", course_sched_id),
                ("timestamp", timestamp.as_str()),
            ])
            .header("sessionId", &session.session_id)
            .form(&[("id", session.user_id.as_str())])
            .send()
            .await
            .context("签到请求失败")?;

        let http_status = response.status().as_u16();

        let raw_response = parse_json(response).await.context("签到响应解析失败")?;

        let server_status = raw_response
            .get("STATUS")
            .map(|value| value_to_string(Some(value)))
            .unwrap_or_default();

        let stu_sign_status = raw_response
            .get("result")
            .and_then(|value| value.get("stuSignStatus"))
            .map(|value| value_to_string(Some(value)));

        let success_like = (200..300).contains(&http_status)
            && server_status == "0"
            && stu_sign_status
                .as_deref()
                .is_none_or(|status| status == "1");

        let message = sign_response_message(
            &raw_response,
            success_like,
            http_status,
            &server_status,
            stu_sign_status.as_deref(),
        );

        Ok(SignOutcome {
            message,
            success_like,
            http_status,
            server_status,
            raw_response,
        })
    }

    /// Loads the timestamp expected by iClass' port-8081 sign endpoint.

    async fn fetch_sign_timestamp(&self) -> Result<String> {

        let urls = network_urls(self.use_vpn);

        let data = parse_json(
            self.client
                .get(urls.sign_timestamp)
                .send()
                .await
                .context("获取 iClass 签到服务器时间失败")?,
        )
        .await
        .context("解析 iClass 签到服务器时间失败")?;

        let timestamp = value_to_string(data.get("timestamp")).trim().to_string();

        if timestamp.is_empty() {

            bail!("iClass 签到服务器时间响应格式异常: {data}");
        }

        Ok(timestamp)
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

    /// Extracts the transient `execution` token required by BUAA SSO.
    ///
    /// Why:
    /// The login form is stateful and rejects submissions without the current
    /// hidden token, so this remains a dedicated pre-step instead of being
    /// inlined into the larger VPN login flow.

    async fn fetch_login_form_state(
        &self,
        username: &str,
        password: &str,
    ) -> Result<LoginFormState> {

        let login_entry = sso_vpn_entry();

        let response = self
            .client
            .get(&login_entry)
            .send()
            .await
            .with_context(|| format!("获取 SSO 登录页失败，入口: {login_entry}"))?;

        let status = response.status();

        let login_url = response.url().to_string();

        let body = response
            .text()
            .await
            .with_context(|| format!("读取 SSO 登录页失败，最终 URL: {login_url}"))?;

        if !status.is_success() {

            bail!(
                "获取 SSO 登录页失败，HTTP 状态: {status}, 最终 URL: {login_url}, 页面线索: {}",
                summarize_login_page(&body)
            );
        }

        let document = Html::parse_document(&body);

        let action_url = resolve_login_form_action(&login_url, &document)
            .with_context(|| format!("解析 SSO 登录表单提交地址失败，最终 URL: {login_url}"))?;

        let form = build_cas_login_form(&document, username, password, None).ok_or_else(|| {

            anyhow!(
                "无法从 SSO 登录页面解析登录表单，最终 URL: {}, 页面线索: {}",
                login_url,
                summarize_login_page(&body)
            )
        })?;

        Ok(LoginFormState {
            login_url,
            action_url,
            form,
            captcha_id: detect_captcha_id(&body),
            page_hint: summarize_login_page(&body),
            captcha_field_names: collect_captcha_field_names(&document),
        })
    }

    async fn finish_vpn_login_submission(
        &self,
        login_url: &str,
        action_url: &str,
        form: &[(String, String)],
    ) -> Result<()> {

        let response = self
            .client
            .post(action_url)
            .header(ORIGIN, "https://d.buaa.edu.cn")
            .header(REFERER, login_url)
            .form(form)
            .send()
            .await
            .with_context(|| format!("VPN 登录请求失败，提交地址: {action_url}"))?;

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
                .with_context(|| {

                    format!("进入 iClass 服务失败，服务入口: {}", urls.service_home)
                })?;

            let probe_final = probe.url().to_string();

            if looks_like_iclass_url(&probe_final) {

                return Ok(());
            }

            let probe_body = probe.text().await.unwrap_or_default();

            return vpn_login_error(&probe_final, &probe_body);
        }

        let body = response.text().await.unwrap_or_default();

        vpn_login_error(&final_url, &body)
    }

    async fn finish_login_session(
        &self,
        input: &LoginInput,
    ) -> std::result::Result<Session, LoginDiagnostic> {

        let student_id = input.student_id.trim();

        let (user_info, server_time_offset_ms) =
            self.fetch_user_info(student_id).await.map_err(|error| {

                diagnose_login_error(
                    "iclass_user_info",
                    error,
                    Some(network_urls(self.use_vpn).user_login),
                    None,
                    None,
                )
            })?;

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

            return Err(LoginDiagnostic {
                kind:        LoginFailureKind::IclassApi,
                stage:       "iclass_user_info".to_string(),
                summary:     "登录成功但用户信息不完整，请重试".to_string(),
                error_chain: vec!["登录成功但用户信息不完整，请重试".to_string()],
                final_url:   Some(network_urls(self.use_vpn).user_login),
                http_status: None,
                page_hint:   Some("missing_user_id_or_session_id".to_string()),
                suggestions: vec![
                    "稍后重试".to_string(),
                    "若持续出现，请附上诊断信息提交 issue".to_string(),
                ],
            });
        }

        let bykc_api =
            if input.use_vpn {

                Some(BykcApi::new(input.clone()).map_err(|error| {

                    diagnose_login_error("bykc_bootstrap", error, None, None, None)
                })?)
            } else {

                None
            };

        Ok(Session {
            api: self.clone(),
            bykc_api,
            user_id,
            user_name,
            session_id,
            server_time_offset_ms,
            use_vpn: self.use_vpn,
        })
    }

    async fn download_captcha_image(&self, captcha_id: &str) -> Result<PathBuf> {

        let url = format!("https://sso.buaa.edu.cn/captcha?captchaId={captcha_id}");

        let bytes = self
            .client
            .get(url)
            .send()
            .await
            .context("获取验证码图片失败")?
            .bytes()
            .await
            .context("读取验证码图片失败")?;

        let path = std::env::temp_dir().join(format!("iclass-buaa-tui-captcha-{captcha_id}.jpg"));

        fs::write(&path, bytes)
            .with_context(|| format!("写入验证码图片失败: {}", path.display()))?;

        Ok(path)
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

        let mut server_time_offset_ms = response
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

        if self.use_vpn {

            // Match upstream WebVPN handling and bias away from future timestamps.
            server_time_offset_ms = server_time_offset_ms.saturating_add(VPN_OFFSET_CORRECTION_MS);
        }

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

            let course_sign_detail = urls.course_sign_detail.clone();

            tasks.push(tokio::spawn(async move {

                let url = format!(
                    "{}?id={}&courseId={}&sessionId={}",
                    course_sign_detail, user_id, course.id, session_id
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
                        name:            course.name.clone(),
                        id:              course.id.clone(),
                        course_sched_id: value_to_string(record.get("courseSchedId")),
                        date:            normalize_date_display(&value_to_string(
                            record.get("teachTime"),
                        )),
                        start_time:      normalize_time_display(&value_to_string(
                            record.get("classBeginTime"),
                        )),
                        end_time:        normalize_time_display(&value_to_string(
                            record.get("classEndTime"),
                        )),
                        sign_status:     value_to_string(record.get("signStatus")),
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

        if data
            .get("STATUS")
            .map(|value| value_to_string(Some(value)))
            .as_deref()
            == Some("2")
        {

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
                name:            if course_name.trim().is_empty() {

                    "未知课程".to_string()
                } else {

                    course_name
                },
                id:              value_to_string(record.get("courseId")),
                course_sched_id: value_to_string(record.get("id")),
                date:            normalize_date_display(if teach_time.trim().is_empty() {

                    date_str
                } else {

                    teach_time.as_str()
                }),
                start_time:      normalize_time_display(&value_to_string(
                    record.get("classBeginTime"),
                )),
                end_time:        normalize_time_display(&value_to_string(
                    record.get("classEndTime"),
                )),
                sign_status:     value_to_string(record.get("signStatus")),
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

fn diagnose_login_error(
    stage: &str,
    error: anyhow::Error,
    final_url: Option<String>,
    http_status: Option<u16>,
    page_hint: Option<String>,
) -> LoginDiagnostic {

    let error_chain = error.chain().map(ToString::to_string).collect::<Vec<_>>();

    let top = error_chain
        .first()
        .cloned()
        .unwrap_or_else(|| "未知错误".to_string());

    let joined = error_chain.join(" | ");

    let kind = classify_login_failure(&joined, http_status);

    let summary = match kind {
        LoginFailureKind::Dns => format!("登录失败：DNS 解析失败，阶段: {stage}"),
        LoginFailureKind::Timeout => format!("登录失败：请求超时，阶段: {stage}"),
        LoginFailureKind::Captcha => "登录失败：当前需要验证码".to_string(),
        LoginFailureKind::Credentials => "登录失败：账号或密码错误".to_string(),
        LoginFailureKind::SsoChanged => "登录失败：SSO 页面结构可能已变化".to_string(),
        LoginFailureKind::Http => {

            format!(
                "登录失败：HTTP 状态异常{}",
                http_status.map(|v| format!(" {v}")).unwrap_or_default()
            )
        }
        LoginFailureKind::IclassApi => format!("登录失败：iClass 接口异常，阶段: {stage}"),
        LoginFailureKind::Validation => top.clone(),
        LoginFailureKind::Network => format!("登录失败：网络异常，阶段: {stage}"),
        LoginFailureKind::Unknown => format!("登录失败：{top}"),
    };

    LoginDiagnostic {
        kind: kind.clone(),
        stage: stage.to_string(),
        summary,
        error_chain,
        final_url,
        http_status,
        page_hint,
        suggestions: login_suggestions(kind),
    }
}

fn classify_login_failure(message: &str, http_status: Option<u16>) -> LoginFailureKind {

    let lower = message.to_ascii_lowercase();

    if lower.contains("学号不能为空") || lower.contains("需要输入") {

        return LoginFailureKind::Validation;
    }

    if lower.contains("dns") || lower.contains("name or service not known") {

        return LoginFailureKind::Dns;
    }

    if lower.contains("timed out") || lower.contains("超时") {

        return LoginFailureKind::Timeout;
    }

    if lower.contains("验证码") || lower.contains("captcha") {

        return LoginFailureKind::Captcha;
    }

    if lower.contains("账号或密码错误")
        || lower.contains("invalid credentials")
        || lower.contains("密码过弱")
    {

        return LoginFailureKind::Credentials;
    }

    if lower.contains("登录表单")
        || lower.contains("execution")
        || lower.contains("无法从 sso 登录页面解析")
    {

        return LoginFailureKind::SsoChanged;
    }

    if lower.contains("iclass api 返回错误")
        || lower.contains("用户信息格式异常")
        || lower.contains("用户信息不完整")
    {

        return LoginFailureKind::IclassApi;
    }

    if http_status.is_some_and(|status| !(200..300).contains(&status)) {

        return LoginFailureKind::Http;
    }

    if lower.contains("connect") || lower.contains("network") || lower.contains("tls") {

        return LoginFailureKind::Network;
    }

    LoginFailureKind::Unknown
}

fn login_suggestions(kind: LoginFailureKind) -> Vec<String> {

    match kind {
        LoginFailureKind::Dns => {

            vec![
                "检查本机 DNS 与网络连接".to_string(),
                "若在校外，请先连接 WebVPN".to_string(),
            ]
        }
        LoginFailureKind::Timeout | LoginFailureKind::Network => {

            vec![
                "检查当前网络或稍后重试".to_string(),
                "可先执行 doctor 自检确认 WebVPN / SSO / iClass 连通性".to_string(),
            ]
        }
        LoginFailureKind::Captcha => {

            vec![
                "当前登录需要验证码".to_string(),
                "先在浏览器完成一次 WebVPN / SSO 登录后再重试".to_string(),
            ]
        }
        LoginFailureKind::Credentials => {

            vec![
                "确认账号密码正确".to_string(),
                "若提示密码过弱，请先在学校统一认证页面修改密码".to_string(),
            ]
        }
        LoginFailureKind::SsoChanged => {

            vec![
                "SSO 页面结构可能已变化".to_string(),
                "请附上诊断输出提交 issue".to_string(),
            ]
        }
        LoginFailureKind::Http => {

            vec![
                "上游服务返回了异常 HTTP 状态".to_string(),
                "可稍后重试，或附上诊断输出提交 issue".to_string(),
            ]
        }
        LoginFailureKind::IclassApi => {

            vec![
                "SSO 已通过，但 iClass 接口返回异常".to_string(),
                "刷新网络后重试；若持续失败，请附上诊断输出".to_string(),
            ]
        }
        LoginFailureKind::Validation => vec!["补全登录输入后重试".to_string()],
        LoginFailureKind::Unknown => {

            vec![
                "查看错误链、最终 URL 和页面线索".to_string(),
                "附上诊断输出提交 issue".to_string(),
            ]
        }
    }
}

fn classify_reqwest_error(error: &reqwest::Error) -> (String, LoginFailureKind) {

    if error.is_timeout() {

        return ("timeout".to_string(), LoginFailureKind::Timeout);
    }

    let lower = error.to_string().to_ascii_lowercase();

    if lower.contains("dns") || lower.contains("name or service not known") {

        return ("dns_error".to_string(), LoginFailureKind::Dns);
    }

    if lower.contains("certificate") || lower.contains("tls") {

        return ("tls_error".to_string(), LoginFailureKind::Network);
    }

    if error.is_connect() {

        return ("connect_error".to_string(), LoginFailureKind::Network);
    }

    ("request_error".to_string(), LoginFailureKind::Unknown)
}

fn resolve_host_addrs(target: &str) -> Vec<String> {

    let Some(host) = reqwest::Url::parse(target)
        .ok()
        .and_then(|url| url.host_str().map(ToOwned::to_owned))
    else {

        return Vec::new();
    };

    let port = reqwest::Url::parse(target)
        .ok()
        .and_then(|url| url.port_or_known_default())
        .unwrap_or(443);

    match (host.as_str(), port).to_socket_addrs() {
        Ok(addrs) => addrs.map(|addr| addr.ip().to_string()).collect(),
        Err(_) => Vec::new(),
    }
}

fn doctor_suggestion(
    name: &str,
    dns_ok: bool,
    http_status: Option<u16>,
    error: Option<&reqwest::Error>,
) -> String {

    if !dns_ok {

        return "DNS 解析失败，先检查网络或 DNS 配置".to_string();
    }

    if let Some(error) = error {

        if error.is_timeout() {

            return "请求超时，网络可能较差或上游服务不可达".to_string();
        }

        if error.is_connect() {

            return "连接失败，若在校外请先连接 WebVPN".to_string();
        }

        return "请求失败，建议稍后重试并保留诊断输出".to_string();
    }

    if let Some(status) = http_status {

        if (200..400).contains(&status) {

            return "检查通过".to_string();
        }

        if name == "iclass_login_api" {

            return "接口可达但返回异常状态，登录问题更可能是上游接口异常".to_string();
        }

        return format!("上游返回 HTTP {status}，建议稍后重试");
    }

    "检查未完成".to_string()
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

fn needs_vpn_captcha(body: &str) -> bool {

    body.contains("/captcha?captchaId=")
        || body.contains("captcha?captchaId=")
        || body.contains("config.captcha.id")
        || body.contains("config.captcha")
        || body.contains("\"captcha\":{\"id\"")
        || body.contains("\"captcha\": {")
        || body.contains("'captcha':{'id'")
}

fn looks_like_bad_vpn_credentials(body: &str) -> bool {

    [
        "Invalid credentials",
        "认证信息无效",
        "账号或密码错误",
        "用户名或密码错误",
        "password is invalid",
    ]
    .iter()
    .any(|marker| body.contains(marker))
}

fn resolve_login_form_action(login_url: &str, document: &Html) -> Result<String> {

    let selector = Selector::parse(r#"form#loginForm, form#fm1, form[action]"#)
        .map_err(|_| anyhow!("SSO 表单选择器构造失败"))?;

    let Some(action) = document
        .select(&selector)
        .next()
        .and_then(|node| node.value().attr("action"))
        .filter(|value| !value.trim().is_empty())
    else {

        return Ok(login_url.to_string());
    };

    reqwest::Url::parse(login_url)
        .and_then(|base| base.join(action))
        .map(|url| url.to_string())
        .with_context(|| format!("解析 SSO 登录表单提交地址失败: {action}"))
}

fn build_cas_login_form(
    document: &Html,
    username: &str,
    password: &str,
    captcha: Option<&str>,
) -> Option<Vec<(String, String)>> {

    let form_selector = Selector::parse(r#"form#loginForm, form#fm1, form[action]"#).ok()?;

    let input_selector = Selector::parse("input[name]").ok()?;

    let form = document.select(&form_selector).next()?;

    let mut fields = Vec::new();

    let mut present_names = HashSet::new();

    for input in form.select(&input_selector) {

        let name = input.value().attr("name")?.trim();

        if name.is_empty() {

            continue;
        }

        let input_type = input
            .value()
            .attr("type")
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();

        present_names.insert(name.to_string());

        if matches!(name, "username" | "password") {

            continue;
        }

        match input_type.as_str() {
            "submit" | "button" | "image" => {}
            "checkbox" => {
                if input.value().attr("checked").is_some() {

                    fields.push((
                        name.to_string(),
                        input.value().attr("value").unwrap_or("on").to_string(),
                    ));
                }
            }
            _ => {

                fields.push((
                    name.to_string(),
                    input.value().attr("value").unwrap_or_default().to_string(),
                ));
            }
        }
    }

    if !present_names.contains("execution") {

        return None;
    }

    fields.push(("username".to_string(), username.trim().to_string()));

    fields.push(("password".to_string(), password.to_string()));

    if let Some(captcha) = captcha.map(str::trim).filter(|value| !value.is_empty()) {

        if present_names.contains("captcha") {

            fields.push(("captcha".to_string(), captcha.to_string()));
        }

        if present_names.contains("captchaResponse") {

            fields.push(("captchaResponse".to_string(), captcha.to_string()));
        }
    }

    if !present_names.contains("submit") {

        fields.push(("submit".to_string(), "登录".to_string()));
    }

    if !present_names.contains("type") {

        fields.push(("type".to_string(), "username_password".to_string()));
    }

    if !present_names.contains("_eventId") {

        fields.push(("_eventId".to_string(), "submit".to_string()));
    }

    Some(fields)
}

fn detect_captcha_id(body: &str) -> Option<String> {

    let marker = "captchaId=";

    if let Some(index) = body.find(marker) {

        let rest = &body[index + marker.len()..];

        let value = rest
            .chars()
            .take_while(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_'))
            .collect::<String>();

        if !value.is_empty() {

            return Some(value);
        }
    }

    let marker_index = body
        .find("config.captcha")
        .or_else(|| body.find("\"captcha\""))?;

    let rest = &body[marker_index..];

    let id_index = rest.find("id")?;

    let after_id = &rest[id_index + "id".len()..];

    let colon_index = after_id.find(':')?;

    let after = after_id[colon_index + 1..].trim_start();

    let quote = after.chars().next()?;

    if !matches!(quote, '\'' | '"') {

        return None;
    }

    let content = &after[quote.len_utf8()..];

    let end = content.find(quote)?;

    Some(content[..end].to_string())
}

fn collect_captcha_field_names(document: &Html) -> Vec<String> {

    let Ok(input_selector) = Selector::parse("input[name]") else {

        return vec!["captcha".to_string(), "captchaResponse".to_string()];
    };

    let mut fields = document
        .select(&input_selector)
        .filter_map(|input| input.value().attr("name"))
        .filter(|name| matches!(*name, "captcha" | "captchaResponse"))
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();

    if fields.is_empty() {

        fields.push("captcha".to_string());

        fields.push("captchaResponse".to_string());
    }

    fields
}

fn append_captcha_fields(
    base_form: &[(String, String)],
    field_names: &[String],
    captcha: &str,
) -> Vec<(String, String)> {

    let mut form = base_form
        .iter()
        .filter(|(name, _)| name != "captcha" && name != "captchaResponse")
        .cloned()
        .collect::<Vec<_>>();

    for name in field_names {

        if matches!(name.as_str(), "captcha" | "captchaResponse") {

            form.push((name.clone(), captcha.to_string()));
        }
    }

    if !field_names.iter().any(|name| name == "captcha") {

        form.push(("captcha".to_string(), captcha.to_string()));
    }

    if !field_names.iter().any(|name| name == "captchaResponse") {

        form.push(("captchaResponse".to_string(), captcha.to_string()));
    }

    form
}

fn summarize_login_page(body: &str) -> String {

    let title = Html::parse_document(body)
        .select(&Selector::parse("title").expect("valid title selector"))
        .next()
        .map(|node| node.text().collect::<String>().trim().to_string())
        .filter(|value| !value.is_empty());

    let markers = [
        ("captcha", needs_vpn_captcha(body)),
        ("bad_credentials", looks_like_bad_vpn_credentials(body)),
        (
            "cas_form",
            body.contains("loginForm") || body.contains("统一身份认证"),
        ),
        (
            "portal",
            body.contains("wengine-vpn") || body.contains("免客户端VPN"),
        ),
    ]
    .into_iter()
    .filter_map(|(name, present)| present.then_some(name))
    .collect::<Vec<_>>()
    .join(",");

    format!(
        "title={}, markers={}, body_prefix={}",
        title.unwrap_or_else(|| "<none>".to_string()),
        if markers.is_empty() {

            "<none>"
        } else {

            &markers
        },
        body.chars()
            .take(120)
            .collect::<String>()
            .replace(char::is_whitespace, " ")
    )
}

fn vpn_login_error(final_url: &str, body: &str) -> Result<()> {

    if needs_vpn_captcha(body) {

        bail!("当前 VPN 登录需要验证码，请先在浏览器完成 WebVPN 登录后重试");
    }

    if looks_like_bad_vpn_credentials(body) {

        bail!("登录失败：账号或密码错误，或密码过弱需先修改后再登录");
    }

    bail!("登录失败，最终 URL: {final_url}");
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

    let status = data.get("STATUS").map(|value| value_to_string(Some(value)));

    if status.as_deref() == Some("0") {

        return Ok(());
    }

    bail!("iClass API 返回错误: {}", data);
}

/// Builds a user-facing explanation for iClass sign responses.

fn sign_response_message(
    data: &Value,
    success: bool,
    http_status: u16,
    server_status: &str,
    stu_sign_status: Option<&str>,
) -> String {

    let raw_message = data
        .get("ERRMSG")
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .or_else(|| {

            data.get("MSG")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
        })
        .or_else(|| {

            data.get("message")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
        });

    if success {

        return raw_message.unwrap_or("签到成功").to_string();
    }

    let raw_message = raw_message.unwrap_or_default().trim();

    if raw_message.contains("已签到") {

        return "您今天已经签到过了".to_string();
    }

    if raw_message.contains("未开始") {

        return "当前还未到签到时间".to_string();
    }

    if raw_message.contains("不是上课时间") {

        return "当前不是上课时间，无法签到".to_string();
    }

    if raw_message.contains("已结束") {

        return "本次签到已结束".to_string();
    }

    if raw_message.contains("范围") {

        return "当前不在可签到范围内".to_string();
    }

    if raw_message.contains("课程") && raw_message.contains("不存在") {

        return "未找到对应课程，请刷新后重试".to_string();
    }

    if !raw_message.is_empty() {

        return raw_message.to_string();
    }

    let server_status = empty_dash(server_status);

    let stu_sign_status = empty_dash(stu_sign_status.unwrap_or_default());

    format!(
        "签到失败，上游未返回具体原因 (HTTP {http_status}, STATUS {server_status}, stuSignStatus \
         {stu_sign_status})"
    )
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

fn empty_dash(value: &str) -> &str {

    if value.trim().is_empty() { "-" } else { value }
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

#[cfg(test)]

mod tests {

    use std::collections::HashMap;

    use scraper::Html;

    use super::{
        append_captcha_fields, build_cas_login_form, collect_captcha_field_names,
        detect_captcha_id, needs_vpn_captcha, resolve_login_form_action,
    };

    #[test]

    fn detects_captcha_from_cas_config_and_url_shapes() {

        let cas_config = r#"
            <html><script>
              config.captcha = { type: 'image', id: 'captcha-1' };
            </script></html>
        "#;

        let json_config = r#"
            <script>window.login = {"captcha":{"type":"image","id":"captcha_2"}}</script>
        "#;

        let image_tag = r#"<img src="/captcha?captchaId=captcha-3&ts=1">"#;

        assert!(needs_vpn_captcha(cas_config));

        assert_eq!(detect_captcha_id(cas_config).as_deref(), Some("captcha-1"));

        assert_eq!(detect_captcha_id(json_config).as_deref(), Some("captcha_2"));

        assert_eq!(detect_captcha_id(image_tag).as_deref(), Some("captcha-3"));
    }

    #[test]

    fn builds_cas_form_preserving_hidden_fields_and_replacing_credentials() {

        let document = Html::parse_document(
            r#"
            <form id="fm1" action="/login">
              <input type="hidden" name="execution" value="e1s1">
              <input type="hidden" name="lt" value="LT-123">
              <input type="checkbox" name="remember" value="on" checked>
              <input type="checkbox" name="unused" value="1">
              <input name="username" value="old-user">
              <input type="password" name="password" value="old-pass">
              <input type="text" name="captchaResponse">
            </form>
            "#,
        );

        let form = build_cas_login_form(&document, " 22330000 ", "secret", Some("abcd"))
            .expect("form should parse");

        let fields = form_map(&form);

        assert_eq!(fields.get("execution").map(String::as_str), Some("e1s1"));

        assert_eq!(fields.get("lt").map(String::as_str), Some("LT-123"));

        assert_eq!(fields.get("remember").map(String::as_str), Some("on"));

        assert!(!fields.contains_key("unused"));

        assert_eq!(fields.get("username").map(String::as_str), Some("22330000"));

        assert_eq!(fields.get("password").map(String::as_str), Some("secret"));

        assert_eq!(
            fields.get("captchaResponse").map(String::as_str),
            Some("abcd")
        );

        assert_eq!(fields.get("_eventId").map(String::as_str), Some("submit"));

        assert_eq!(
            fields.get("type").map(String::as_str),
            Some("username_password")
        );
    }

    #[test]

    fn captcha_fields_are_collected_and_appended_without_duplicate_old_values() {

        let document = Html::parse_document(
            r#"
            <form id="fm1">
              <input name="execution" value="e1s1">
              <input name="captcha">
              <input name="captchaResponse">
            </form>
            "#,
        );

        let field_names = collect_captcha_field_names(&document);

        assert_eq!(field_names, vec!["captcha", "captchaResponse"]);

        let base = vec![
            ("execution".to_string(), "e1s1".to_string()),
            ("captcha".to_string(), "old".to_string()),
            ("captchaResponse".to_string(), "old".to_string()),
        ];

        let form = append_captcha_fields(&base, &field_names, "new-code");

        let captcha_values = form
            .iter()
            .filter(|(name, _)| name == "captcha" || name == "captchaResponse")
            .map(|(name, value)| (name.as_str(), value.as_str()))
            .collect::<Vec<_>>();

        assert_eq!(
            captcha_values,
            vec![("captcha", "new-code"), ("captchaResponse", "new-code")]
        );
    }

    #[test]

    fn login_form_action_resolves_against_final_login_url() {

        let document = Html::parse_document(r#"<form id="fm1" action="/login?service=x"></form>"#);

        let action = resolve_login_form_action("https://d.buaa.edu.cn/login", &document)
            .expect("action should resolve");

        assert_eq!(action, "https://d.buaa.edu.cn/login?service=x");
    }

    fn form_map(form: &[(String, String)]) -> HashMap<String, String> {

        form.iter().cloned().collect()
    }
}
