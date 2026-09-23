//! iClass client implementation, login flow, course loading, and sign requests.
//! Sorry for this big file because i don't wanna split it into multi shits
//! and that's make the maintain more difficult for me
//! maybe in the future i will carefully split it

use anyhow::{Context, Result, anyhow, bail};
use chrono::{DateTime, Duration, Local, Utc};
use reqwest::header::{
    ACCEPT, ACCEPT_LANGUAGE, HeaderMap, HeaderValue, LOCATION, ORIGIN, REFERER, USER_AGENT,
};
use scraper::{Html, Selector};
use serde_json::Value;
use std::{collections::HashSet, fs, net::ToSocketAddrs, path::PathBuf, sync::Arc, time::Instant};
use tokio::sync::Semaphore;

use crate::bykc::BykcApi;
use crate::constants::{
    BYKC_DIRECT_BASE, VPN_OFFSET_CORRECTION_MS, WEBVPN_CAS_LOGIN_URL, network_urls,
    sso_login_entry, to_webvpn_url,
};
use crate::model::{
    CourseDetailItem, CourseItem, DoctorCheck, DoctorReport, LoginCaptchaChallenge,
    LoginDiagnostic, LoginFailureKind, LoginInput, LoginStart, Session, SignOutcome, SignQrData,
};

#[derive(Clone, Debug)]

pub struct IClassApi {
    pub(crate) client:              reqwest::Client,
    pub(crate) no_redirect_client:  reqwest::Client,
    pub(crate) use_vpn:             bool,
    /// Cookie jar shared with every client built from this API.
    ///
    /// Why:
    /// BYKC reuses the same BUAA SSO session instead of authenticating on its
    /// own. Building it with a fresh jar left its CAS request unauthenticated,
    /// so no token ever came back.
    pub(crate) cookie_jar:          Arc<reqwest::cookie::Jar>,
    /// Reused client for the library service, with its scoped intermediate
    /// certificate and the same cookie jar as the authentication clients.
    pub(crate) libbook_client:      reqwest::Client,
    pub(crate) libbook_token:       Arc<tokio::sync::Mutex<Option<super::CachedToken>>>,
    #[cfg(test)]
    sso_entry_override:             Option<String>,
    #[cfg(test)]
    pub(crate) venue_base_override: Option<String>,
}

/// Outcome of the unified-auth phase on its own.

enum SsoPhase {
    SessionEstablished,

    Captcha(LoginCaptchaChallenge),
}

#[derive(Clone)]

struct LoginFormState {
    login_url:           String,
    action_url:          String,
    form:                Vec<(String, String)>,
    captcha_id:          Option<String>,
    page_hint:           String,
    captcha_field_names: Vec<String>,
}

#[derive(Clone, Debug)]

struct LoginFlowResult {
    final_url:   String,
    body:        String,
    http_status: reqwest::StatusCode,
}

impl IClassApi {
    /// Creates shared-cookie iClass clients. The normal client follows redirects,
    /// while the no-redirect client inspects 302 Location headers.

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

        let cookie_jar = Arc::new(reqwest::cookie::Jar::default());

        let client = super::http::client_builder()
            .cookie_provider(cookie_jar.clone())
            .default_headers(headers.clone())
            .build()
            .context("failed to build reqwest client")?;

        let no_redirect_client = super::http::client_builder()
            .cookie_provider(cookie_jar.clone())
            .default_headers(headers.clone())
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("failed to build no-redirect reqwest client")?;

        let certificate =
            reqwest::Certificate::from_pem(crate::libbook::library_intermediate_certificate())
                .context("内置的图书馆中间证书无法解析")?;

        let libbook_client = super::http::client_builder()
            .cookie_provider(cookie_jar.clone())
            .default_headers(headers)
            .add_root_certificate(certificate)
            .build()
            .context("failed to build library reqwest client")?;

        Ok(Self {
            client,
            no_redirect_client,
            use_vpn,
            cookie_jar,
            libbook_client,
            libbook_token: Arc::new(tokio::sync::Mutex::new(None)),
            #[cfg(test)]
            sso_entry_override: None,
            #[cfg(test)]
            venue_base_override: None,
        })
    }

    /// Builds a direct client for the seminar-room service and authenticates
    /// only through SSO. It deliberately does not call any iClass endpoint.

    pub async fn for_venue(input: &LoginInput) -> Result<Self> {

        let api = Self::new(false)?;

        Self::authenticate_venue_client(api, input).await
    }

    async fn authenticate_venue_client(api: Self, input: &LoginInput) -> Result<Self> {

        api.login_sso_only(input)
            .await
            .map_err(login_diagnostic_error)?;

        Ok(api)
    }

    #[cfg(test)]

    pub(crate) async fn for_venue_fixture(input: &LoginInput, origin: &str) -> Result<Self> {

        let mut api = Self::new(false)?;

        api.client = super::http::client_builder()
            .no_proxy()
            .cookie_provider(api.cookie_jar.clone())
            .build()?;

        api.no_redirect_client = super::http::client_builder()
            .no_proxy()
            .cookie_provider(api.cookie_jar.clone())
            .redirect(reqwest::redirect::Policy::none())
            .build()?;

        api.sso_entry_override = Some(format!("{origin}/login"));

        api.venue_base_override = Some(format!("{origin}/venue-zhjs-server/"));

        Self::authenticate_venue_client(api, input).await
    }

    /// Logs in and captures the server clock offset needed by later sign requests.

    pub async fn login(&self, input: &LoginInput) -> Result<Session> {

        self.login_with_diagnostic(input)
            .await
            .map_err(login_diagnostic_error)
    }

    pub(crate) fn session_cookie_jar(&self) -> Arc<reqwest::cookie::Jar> {

        self.cookie_jar.clone()
    }

    pub(crate) fn library_client(&self) -> reqwest::Client {

        self.libbook_client.clone()
    }

    pub async fn start_login(
        &self,
        input: &LoginInput,
    ) -> std::result::Result<LoginStart, LoginDiagnostic> {

        *self.libbook_token.lock().await = None;

        match self.authenticate_sso(input).await? {
            SsoPhase::SessionEstablished => {}

            SsoPhase::Captcha(challenge) => return Ok(LoginStart::Captcha(challenge)),
        }

        self.finish_login_session(input)
            .await
            .map(LoginStart::Complete)
    }

    /// Runs the unified-auth phase, leaving an SSO session in the cookie jar.
    ///
    /// Why this is its own step:
    /// The full login continues into iClass, which is campus-only. Services
    /// that authenticate purely off the SSO session — the seminar-room service
    /// is one — must not be made to depend on that later phase, which fails
    /// whenever the machine is off campus or behind a proxy even though the
    /// session they need was established successfully.

    async fn authenticate_sso(
        &self,
        input: &LoginInput,
    ) -> std::result::Result<SsoPhase, LoginDiagnostic> {

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

        // A direct venue client still authenticates with the same unified
        // account. VPN mode normally supplies an explicit username; direct
        // mode falls back to the student id when it is omitted.
        let username = if input.vpn_username.trim().is_empty() {

            student_id
        } else {

            input.vpn_username.trim()
        };

        if username.is_empty() {

            return Err(LoginDiagnostic {
                kind:        LoginFailureKind::Validation,
                stage:       "input".to_string(),
                summary:     "统一认证账号不能为空".to_string(),
                error_chain: vec!["统一认证账号不能为空".to_string()],
                final_url:   None,
                http_status: None,
                page_hint:   None,
                suggestions: vec!["请输入学号或统一认证账号后重试".to_string()],
            });
        }

        if input.vpn_password.is_empty() {

            return Err(LoginDiagnostic {
                kind:        LoginFailureKind::Validation,
                stage:       "input".to_string(),
                summary:     "统一认证密码不能为空".to_string(),
                error_chain: vec!["统一认证密码不能为空".to_string()],
                final_url:   None,
                http_status: None,
                page_hint:   None,
                suggestions: vec!["请输入统一认证密码后重试".to_string()],
            });
        }

        self.authenticate_sso_as(username, &input.vpn_password)
            .await
    }

    /// Performs the form fetch and submission for one username/password pair.

    async fn authenticate_sso_as(
        &self,
        username: &str,
        password: &str,
    ) -> std::result::Result<SsoPhase, LoginDiagnostic> {

        let form_state = self
            .fetch_login_form_state(username, password)
            .await
            .map_err(|error| diagnose_login_error("sso_login_page", error, None, None, None))?;

        if let Some(captcha_id) = form_state.captcha_id.clone() {

            let captcha_path = self
                .download_captcha_image(&captcha_id)
                .await
                .map_err(|error| diagnose_login_error("sso_captcha", error, None, None, None))?;

            return Ok(SsoPhase::Captcha(LoginCaptchaChallenge {
                login_url: form_state.login_url,
                action_url: form_state.action_url,
                form: form_state.form,
                captcha_id,
                captcha_path: captcha_path.display().to_string(),
                page_hint: form_state.page_hint,
                captcha_field_names: form_state.captcha_field_names,
            }));
        }

        self.finish_sso_login_submission(
            &form_state.login_url,
            &form_state.action_url,
            &form_state.form,
        )
        .await
        .map_err(|error| diagnose_login_error("sso_login_submit", error, None, None, None))?;

        Ok(SsoPhase::SessionEstablished)
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
                    final_url:   Some(super::http::diagnostic_url(&challenge.login_url)),
                    http_status: None,
                    page_hint:   Some(challenge.page_hint),
                    suggestions: vec![
                        format!("验证码图片路径: {}", challenge.captcha_path),
                        "TUI 中可继续输入验证码".to_string(),
                        "CLI 不支持验证码；浏览器或其他进程的会话不会共享".to_string(),
                    ],
                })
            }
        }
    }

    /// Logs in far enough to hold a unified-auth session, then stops.
    ///
    /// Why:
    /// Some services authenticate purely off the SSO session and never touch
    /// iClass. The seminar-room service is one: its login only needs the
    /// unified-auth cookies, and it is reachable directly.
    ///
    /// The full login continues into iClass, which is campus-only. When that
    /// phase fails the caller sees a login error even though the SSO session —
    /// everything the seminar-room service needs — was established first. This
    /// stops after the SSO phase so that session can be used on its own.
    ///
    /// A captcha challenge is an error: reaching a form is not authentication.

    pub async fn login_sso_only(
        &self,
        input: &LoginInput,
    ) -> std::result::Result<(), LoginDiagnostic> {

        match self.authenticate_sso(input).await? {
            SsoPhase::SessionEstablished => Ok(()),

            SsoPhase::Captcha(challenge) => {
                Err(LoginDiagnostic {
                    kind:        LoginFailureKind::Captcha,
                    stage:       "sso_captcha".to_string(),
                    summary:     "统一认证要求验证码，当前研讨室会话未建立；\
                                  其他进程的登录不会共享会话"
                        .to_string(),
                    error_chain: vec!["统一认证要求验证码".to_string()],
                    final_url:   Some(super::http::diagnostic_url(&challenge.login_url)),
                    http_status: None,
                    page_hint:   Some(challenge.page_hint),
                    suggestions: vec![format!("验证码图片路径: {}", challenge.captcha_path)],
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
                final_url:   Some(super::http::diagnostic_url(&challenge.login_url)),
                http_status: None,
                page_hint:   Some(challenge.page_hint.clone()),
                suggestions: vec!["输入验证码后重试".to_string()],
            });
        }

        let form = append_captcha_fields(&challenge.form, &challenge.captcha_field_names, captcha);

        self.finish_sso_login_submission(&challenge.login_url, &challenge.action_url, &form)
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

        checks.push(
            self.run_doctor_check("sso_login", &sso_login_entry(self.use_vpn))
                .await,
        );

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

        let mut merged = Vec::new();

        let mut seen = HashSet::new();

        let mut source_errors = Vec::new();

        match self
            .get_current_semester(&session.user_id, &session.session_id)
            .await
        {
            Ok(Some(semester_code)) => {

                match self
                    .get_courses(&session.user_id, &session.session_id, &semester_code)
                    .await
                {
                    Ok(courses) => {

                        match self
                            .get_courses_detail(&session.user_id, &session.session_id, &courses)
                            .await
                        {
                            Ok(detail_data) => {
                                for item in detail_data {

                                    let key = merged_key(&item);

                                    if seen.insert(key) {

                                        merged.push(item);
                                    }
                                }
                            }
                            Err(error) => {

                                source_errors
                                    .push(format!("课程详情: {}", format_anyhow_chain(&error)));
                            }
                        }
                    }
                    Err(error) => {

                        source_errors.push(format!("课程列表: {}", format_anyhow_chain(&error)));
                    }
                }
            }
            Ok(None) => {

                source_errors.push("学期列表: 未获取到当前学期".to_string());
            }
            Err(error) => {

                source_errors.push(format!("学期列表: {}", format_anyhow_chain(&error)));
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

            match task.await.context("按日期获取课程任务执行失败")? {
                Ok(items) => {
                    for item in items {

                        let key = merged_key(&item);

                        if seen.insert(key) {

                            merged.push(item);
                        }
                    }
                }
                Err(error) => {

                    source_errors.push(format!("按日期课程: {}", format_anyhow_chain(&error)));
                }
            }
        }

        if merged.is_empty() && !source_errors.is_empty() {

            bail!("拉取课程失败: {}", source_errors.join(" | "));
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

            bail!("iClass 签到服务器时间响应格式异常");
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

        let login_entry = if self.use_vpn {

            to_webvpn_url(WEBVPN_CAS_LOGIN_URL)
        } else {

            sso_login_entry(false)
        };

        #[cfg(test)]
        let login_entry = self.sso_entry_override.clone().unwrap_or(login_entry);

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

    async fn finish_sso_login_submission(
        &self,
        login_url: &str,
        action_url: &str,
        form: &[(String, String)],
    ) -> Result<()> {

        let response = self
            .no_redirect_client
            .post(action_url)
            .header(REFERER, login_url)
            .form(form)
            .send()
            .await
            .with_context(|| format!("VPN 登录请求失败，提交地址: {action_url}"))?;

        let login_flow = self.follow_sso_login_flow(response).await?;

        if !login_flow.http_status.is_success() {

            bail!(
                "登录失败，HTTP 状态: {}, 最终 URL: {}",
                login_flow.http_status,
                login_flow.final_url
            );
        }

        if looks_like_login_form_page(&login_flow.body) {

            return vpn_login_error(&login_flow.final_url, &login_flow.body);
        }

        Ok(())
    }

    async fn follow_sso_login_flow(
        &self,
        initial_response: reqwest::Response,
    ) -> Result<LoginFlowResult> {

        let mut current_response = initial_response;

        let mut password_expiry_ignored = false;

        for _ in 0..16 {

            if current_response.status().is_redirection() {

                let current_url = current_response.url().to_string();

                let location = current_response
                    .headers()
                    .get(LOCATION)
                    .and_then(|value| value.to_str().ok())
                    .ok_or_else(|| anyhow!("登录跳转缺少重定向地址，当前 URL: {current_url}"))?;

                let next_url = resolve_redirect_url(&current_url, location)?;

                current_response = self
                    .no_redirect_client
                    .get(&next_url)
                    .send()
                    .await
                    .map_err(reqwest::Error::without_url)
                    .context("跟随 SSO 登录跳转失败")?;

                continue;
            }

            let http_status = current_response.status();

            let final_url = current_response.url().to_string();

            let body = current_response
                .text()
                .await
                .with_context(|| format!("读取 SSO 登录响应失败，最终 URL: {final_url}"))?;

            if is_ignorable_password_expiry_page(&body) {

                if password_expiry_ignored {

                    bail!("登录失败：忽略密码风险提示后仍停留在风险提示页");
                }

                let execution = extract_execution_value(&body)
                    .ok_or_else(|| anyhow!("密码风险提示页缺少 execution 参数"))?;

                let ignore_url = strip_query(&final_url);

                let ignore_form = build_ignore_password_expiry_form(&execution);

                let mut request = self
                    .no_redirect_client
                    .post(&ignore_url)
                    .header(REFERER, &final_url)
                    .form(&ignore_form);

                if let Some(origin) = origin_for_url(&ignore_url) {

                    request = request.header(ORIGIN, origin);
                }

                current_response = request
                    .send()
                    .await
                    .with_context(|| format!("提交忽略密码风险提示失败: {ignore_url}"))?;

                password_expiry_ignored = true;

                continue;
            }

            if let Some(message) = extract_exception_message_from_url(&final_url) {

                bail!("登录失败：{message}");
            }

            if http_status == reqwest::StatusCode::UNAUTHORIZED {

                bail!("登录失败：账号或密码错误，或密码过弱需先修改后再登录");
            }

            if let Some(message) = find_login_error(&body) {

                bail!("登录失败：{message}");
            }

            if looks_like_login_form_page(&body) {

                bail!("登录失败：账号或密码错误");
            }

            return Ok(LoginFlowResult {
                final_url,
                body,
                http_status,
            });
        }

        bail!("SSO 登录重定向超过 16 次仍未完成")
    }

    async fn finish_login_session(
        &self,
        input: &LoginInput,
    ) -> std::result::Result<Session, LoginDiagnostic> {

        let student_id = input.student_id.trim();

        let iclass_login_name = self.resolve_iclass_login_name().await.map_err(|error| {

            diagnose_login_error(
                "iclass_login_name",
                error,
                Some(network_urls(self.use_vpn).my_center),
                None,
                None,
            )
        })?;

        let (user_info, server_time_offset_ms) = self
            .fetch_user_info(&iclass_login_name)
            .await
            .map_err(|error| {

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

        let bykc_api = if input.use_vpn {

            Some(
                BykcApi::with_cookie_jar(input.clone(), self.session_cookie_jar()).map_err(
                    |error| diagnose_login_error("bykc_bootstrap", error, None, None, None),
                )?,
            )
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

    /// Follows the browser MyCenter flow step by step and extracts the transient loginName.

    async fn resolve_iclass_login_name(&self) -> Result<String> {

        let mut current_url = network_urls(self.use_vpn).my_center;

        for _ in 0..8 {

            let response = self
                .no_redirect_client
                .get(&current_url)
                .send()
                .await
                .with_context(|| format!("访问 iClass MyCenter 跳转入口失败: {current_url}"))?;

            let final_url = response.url().to_string();

            if let Some(login_name) = extract_iclass_login_name(&final_url) {

                return Ok(login_name);
            }

            let location = response
                .headers()
                .get(LOCATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);

            if let Some(location) = location {

                if let Some(login_name) = extract_iclass_login_name(&location) {

                    return Ok(login_name);
                }

                current_url = reqwest::Url::parse(&final_url)
                    .and_then(|base| base.join(&location))
                    .map(|url| url.to_string())
                    .with_context(|| {

                        format!(
                            "解析 iClass MyCenter 跳转地址失败，当前 URL: {final_url}, Location: \
                             {location}"
                        )
                    })?;

                continue;
            }

            let status = response.status();

            let body = response.text().await.unwrap_or_default();

            if let Some(login_name) = extract_iclass_login_name(&body) {

                return Ok(login_name);
            }

            bail!(
                "iClass MyCenter 未返回 loginName，HTTP 状态: {status}, 最终 URL: {final_url}, \
                 页面线索: {}",
                summarize_login_page(&body)
            );
        }

        bail!("iClass MyCenter 跳转超过 8 次仍未返回 loginName");
    }

    async fn download_captcha_image(&self, captcha_id: &str) -> Result<PathBuf> {

        let raw_url = format!("https://sso.buaa.edu.cn/captcha?captchaId={captcha_id}");

        let url = if self.use_vpn {

            to_webvpn_url(&raw_url)
        } else {

            raw_url
        };

        let bytes = self
            .client
            .get(url)
            .send()
            .await
            .context("获取验证码图片失败")?
            .bytes()
            .await
            .context("读取验证码图片失败")?;

        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join(".tmp")
            .join("auth-captchas")
            .join(format!("iclass-buaa-tui-captcha-{captcha_id}.jpg"));

        if let Some(parent) = path.parent() {

            fs::create_dir_all(parent).context("创建项目内验证码缓存目录失败")?;
        }

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

        if status_means_no_data(&data) {

            return Ok(Vec::new());
        }

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

        let semaphore = Arc::new(Semaphore::new(8));

        for course in courses {

            let api = self.clone();

            let user_id = user_id.to_string();

            let session_id = session_id.to_string();

            let course = course.clone();

            let course_sign_detail = urls.course_sign_detail.clone();

            let semaphore = semaphore.clone();

            tasks.push(tokio::spawn(async move {

                let _permit = semaphore
                    .acquire_owned()
                    .await
                    .context("课程详情并发限流器关闭")?;

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

fn format_anyhow_chain(error: &anyhow::Error) -> String {

    let mut parts = error.chain().map(ToString::to_string);

    let Some(first) = parts.next() else {

        return "未知错误".to_string();
    };

    parts.fold(first, |mut output, cause| {

        if !output.contains(&cause) {

            output.push_str(": ");

            output.push_str(&cause);
        }

        output
    })
}

fn login_diagnostic_error(diagnostic: LoginDiagnostic) -> anyhow::Error {

    let mut error = anyhow!(diagnostic.summary);

    for cause in diagnostic.error_chain.into_iter().rev() {

        error = error.context(cause);
    }

    error
}

fn diagnose_login_error(
    stage: &str,
    error: anyhow::Error,
    final_url: Option<String>,
    http_status: Option<u16>,
    page_hint: Option<String>,
) -> LoginDiagnostic {

    let error_chain = error
        .chain()
        .map(|cause| super::http::diagnostic_text(&cause.to_string()))
        .collect::<Vec<_>>();

    let top = error_chain
        .first()
        .cloned()
        .unwrap_or_else(|| "未知错误".to_string());

    let joined = error_chain.join(" | ");

    let kind = classify_login_failure(&joined, http_status);

    let summary = match kind {
        LoginFailureKind::Dns => format!("登录失败：DNS 解析失败，阶段: {stage}"),
        LoginFailureKind::Timeout => format!("登录失败：请求超时，阶段: {stage}"),
        LoginFailureKind::Unreachable => {

            format!(
                "登录失败：到 iClass 的加密连接在 TLS 握手阶段就被断开，阶段: \
                 {stage}。最常见的原因是本机代理 / VPN（Clash、Mihomo、Surge \
                 等）把校内网段也接管了：iclass.buaa.edu.cn 解析到 10.x \
                 校内地址，本应直连。请把这些网段设为直连后重试。"
            )
        }
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
        final_url: final_url.map(|url| super::http::diagnostic_url(&url)),
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

    // A TLS handshake that is reset or truncated means the host is not
    // reachable from this network — a different problem from a transient
    // fault, with a different remedy.
    if lower.contains("unexpected eof")
        || lower.contains("tls connect error")
        || lower.contains("ssl routines")
        || lower.contains("connection reset")
        || lower.contains("os error 104")
    {

        return LoginFailureKind::Unreachable;
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
        LoginFailureKind::Unreachable => {

            vec![
                "检查本机代理 / VPN 是否把校内网段也走了代理（Clash / Mihomo 的 tun 模式常见）"
                    .to_string(),
                "把 10.0.0.0/8、172.16.0.0/12、192.168.0.0/16 与 *.buaa.edu.cn 设为直连"
                    .to_string(),
                "用 `ip route get 10.20.11.166` 看是否走到了 Mihomo / tun0 这类代理网卡"
                    .to_string(),
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
                "请在当前 TUI 登录流程输入验证码；其他进程的会话不会共享".to_string(),
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

fn resolve_redirect_url(base_url: &str, location: &str) -> Result<String> {

    reqwest::Url::parse(base_url)
        .and_then(|base| base.join(location))
        .map(|url| url.to_string())
        .with_context(|| {

            format!("解析 SSO 重定向地址失败，当前 URL: {base_url}, Location: {location}")
        })
}

fn strip_query(url: &str) -> String {

    url.split_once('?')
        .map(|(left, _)| left.to_string())
        .unwrap_or_else(|| url.to_string())
}

fn origin_for_url(url: &str) -> Option<String> {

    let parsed = reqwest::Url::parse(url).ok()?;

    let host = parsed.host_str()?;

    let mut origin = format!("{}://{}", parsed.scheme(), host);

    if let Some(port) = parsed.port() {

        origin.push(':');

        origin.push_str(&port.to_string());
    }

    Some(origin)
}

fn extract_exception_message_from_url(url: &str) -> Option<String> {

    let query = reqwest::Url::parse(url).ok()?.query()?.to_string();

    for part in query.split('&') {

        let Some((key, value)) = part.split_once('=') else {

            continue;
        };

        if key == "exception.message" {

            return Some(decode_url_query_component(value));
        }
    }

    None
}

fn decode_url_query_component(value: &str) -> String {

    let value = value.replace('+', " ");

    percent_decode(&value).unwrap_or(value)
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

            // The advice has to account for WebVPN itself being unreachable:
            // telling someone to connect to it is useless when the check for it
            // also failed, which is the common case off campus.
            // Campus hosts are normally reachable directly from the campus
            // network, so a connection failure here usually means a local proxy
            // captured the campus ranges rather than the host being down.
            return "连接失败：校内地址通常在校园网内可直连，请检查本机代理 / VPN 是否把 \
                    10.0.0.0/8 等校内网段也走了代理。"
                .to_string();
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

/// Detects the generic VPN portal page that appears before entering iClass.

/// Extracts loginName from redirect URLs or HTML snippets, accepting case-insensitive keys.

fn extract_iclass_login_name(value: &str) -> Option<String> {

    let lower = value.to_ascii_lowercase();

    let marker = "loginname=";

    let mut search_start = 0;

    while let Some(relative_index) = lower[search_start..].find(marker) {

        let value_start = search_start + relative_index + marker.len();

        let raw_value = value[value_start..]
            .chars()
            .take_while(|ch| !matches!(*ch, '&' | '#' | '"' | '\'' | '<' | '>' | ' ' | '\n' | '\r'))
            .collect::<String>();

        if !raw_value.is_empty() {

            return percent_decode(&raw_value);
        }

        search_start = value_start;
    }

    None
}

/// Decodes only `%XX` escapes so `+` inside loginName/base64 is preserved.

fn percent_decode(value: &str) -> Option<String> {

    let mut bytes = Vec::with_capacity(value.len());

    let mut chars = value.as_bytes().iter().copied().peekable();

    while let Some(byte) = chars.next() {

        if byte == b'%' {

            let high = chars.next()?;

            let low = chars.next()?;

            let high = (high as char).to_digit(16)?;

            let low = (low as char).to_digit(16)?;

            bytes.push(((high << 4) | low) as u8);
        } else {

            bytes.push(byte);
        }
    }

    String::from_utf8(bytes).ok()
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

fn extract_execution_value(body: &str) -> Option<String> {

    let document = Html::parse_document(body);

    let selector = Selector::parse(r#"input[name="execution"]"#).ok()?;

    document
        .select(&selector)
        .next()
        .and_then(|node| node.value().attr("value"))
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToOwned::to_owned)
}

fn is_ignorable_password_expiry_page(body: &str) -> bool {

    if body.is_empty() || extract_execution_value(body).is_none() {

        return false;
    }

    body.contains("continueForm")
        || body.contains("ignoreAndContinue")
        || body.contains("账号存在安全风险")
        || body.contains("密码过期")
}

fn build_ignore_password_expiry_form(execution: &str) -> Vec<(String, String)> {

    vec![
        ("execution".to_string(), execution.to_string()),
        ("_eventId".to_string(), "ignoreAndContinue".to_string()),
    ]
}

fn find_login_error(body: &str) -> Option<String> {

    if body.trim().is_empty() {

        return None;
    }

    let document = Html::parse_document(body);

    let selectors = [
        "#errorDiv.alert.alert-danger p",
        "#errorDiv.alert.alert-danger",
        "div.errors",
        "p.errors",
        "span.errors",
        ".tip-text",
        ".login-error",
    ];

    for selector in selectors {

        let Ok(selector) = Selector::parse(selector) else {

            continue;
        };

        if let Some(message) = document
            .select(&selector)
            .map(|node| node.text().collect::<String>())
            .map(|text| text.trim().to_string())
            .find(|text| !text.is_empty())
        {

            return Some(message);
        }
    }

    None
}

fn looks_like_login_form_page(body: &str) -> bool {

    extract_execution_value(body).is_some()
        && (body.contains("loginForm") || body.contains("fm1") || body.contains("统一身份认证"))
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

        if matches!(name, "username" | "password") {

            present_names.insert(name.to_string());

            continue;
        }

        match input_type.as_str() {
            "submit" | "button" | "image" => {}
            "checkbox" => {

                present_names.insert(name.to_string());

                if input.value().attr("checked").is_some() {

                    fields.push((
                        name.to_string(),
                        input.value().attr("value").unwrap_or("on").to_string(),
                    ));
                }
            }
            "hidden" => {

                present_names.insert(name.to_string());

                fields.push((
                    name.to_string(),
                    input.value().attr("value").unwrap_or_default().to_string(),
                ));
            }
            _ => {

                present_names.insert(name.to_string());

                let value = input.value().attr("value").unwrap_or_default();

                if !value.is_empty() {

                    fields.push((name.to_string(), value.to_string()));
                }
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

    fields.push(("submit".to_string(), "登录".to_string()));

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

    format!("markers={markers}, bytes={}", body.len())
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
            "响应不是合法 JSON，HTTP 状态: {status}, bytes={}",
            body.len()
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

    bail!(
        "iClass API 返回错误: STATUS={}",
        status.as_deref().unwrap_or("missing")
    );
}

/// Reports whether iClass signalled "no data" rather than a real failure.
///
/// Why:
/// The course-list endpoint answers `STATUS=2` when the semester legitimately
/// has no rows. Treating that as an API error made a normal empty term look like
/// a broken course fetch, and the merge then reported failure even when the
/// date-based source had already returned data.

fn status_means_no_data(data: &Value) -> bool {

    data.get("STATUS")
        .map(|value| value_to_string(Some(value)))
        .as_deref()
        == Some("2")
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

mod unreachable_tests {

    use super::{LoginFailureKind, classify_login_failure};

    #[test]

    fn a_reset_tls_handshake_is_unreachable_not_transient() {

        // The exact error curl and rustls report when a campus-only host is
        // contacted from a network that cannot reach it.
        let cases = [
            "error:0A000126:SSL routines::unexpected eof while reading",
            "tls connect error",
            "connection reset by peer",
        ];

        for case in cases {

            assert_eq!(
                classify_login_failure(case, None),
                LoginFailureKind::Unreachable,
                "应识别为不可达: {case}"
            );
        }
    }

    #[test]

    fn ordinary_timeouts_keep_their_own_kind() {

        assert_eq!(
            classify_login_failure("operation timed out", None),
            LoginFailureKind::Timeout
        );
    }
}

#[cfg(test)]

mod tests {

    use std::collections::HashMap;

    use scraper::Html;

    use super::{
        append_captcha_fields, build_cas_login_form, build_ignore_password_expiry_form,
        collect_captcha_field_names, detect_captcha_id, extract_execution_value,
        extract_iclass_login_name, find_login_error, is_ignorable_password_expiry_page,
        needs_vpn_captcha, resolve_login_form_action, status_means_no_data, summarize_login_page,
    };

    #[test]

    fn treats_status_two_as_empty_semester_not_api_error() {

        assert!(status_means_no_data(&serde_json::json!({ "STATUS": "2" })));

        assert!(status_means_no_data(&serde_json::json!({ "STATUS": 2 })));

        assert!(!status_means_no_data(&serde_json::json!({ "STATUS": "0" })));

        assert!(!status_means_no_data(&serde_json::json!({ "result": [] })));
    }

    #[test]

    fn extracts_iclass_login_name_from_redirect_shapes() {

        assert_eq!(
            extract_iclass_login_name(
                "https://d.buaa.edu.cn/https-8346/encrypted/?loginName=abc%2Bdef%2Fghi%3D&type=jumpMyCenter#/MyCenter"
            )
            .as_deref(),
            Some("abc+def/ghi=")
        );

        assert_eq!(
            extract_iclass_login_name(
                r#"<a href="/?loginName=Rjc1QkJDMUMxNzVENkY0NkZCNzFDMEM5RjYwNzg4RDg=&type=jumpMyCenter">center</a>"#
            )
            .as_deref(),
            Some("Rjc1QkJDMUMxNzVENkY0NkZCNzFDMEM5RjYwNzg4RDg=")
        );

        assert_eq!(
            extract_iclass_login_name("https://iclass.buaa.edu.cn:8346/?type=jumpMyCenter"),
            None
        );
    }

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

    #[tokio::test]

    async fn venue_fixture_uses_direct_sso_and_cgyy_cookie_without_iclass() {

        use crate::model::LoginInput;
        use tokio::net::TcpListener;
        use tokio::task::JoinHandle;

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();

        let origin = format!("http://{}", listener.local_addr().unwrap());

        let server: JoinHandle<()> = tokio::spawn(async move {

            loop {

                let Ok((mut stream, _)) = listener.accept().await else {

                    return;
                };

                tokio::spawn(async move {

                    use tokio::io::{AsyncReadExt, AsyncWriteExt};

                    let mut request = [0_u8; 4096];

                    let size = stream.read(&mut request).await.unwrap_or(0);

                    let text = String::from_utf8_lossy(&request[..size]);

                    let response = if text.starts_with("GET /login") {

                        let body = r#"<form id="fm1" action="/login" method="post"><input type="hidden" name="execution" value="fixture"><input name="username"><input name="password" type="password"><input type="submit" name="submit" value="登录"></form>"#;

                        format!(
                            "HTTP/1.1 200 OK\r\nContent-Type: text/html\r\nContent-Length: \
                             {}\r\n\r\n{}",
                            body.len(),
                            body
                        )
                    } else if text.starts_with("POST /login") {

                        "HTTP/1.1 302 Found\r\nLocation: /sso/callback\r\nSet-Cookie: SSO=ok; \
                         Path=/\r\nContent-Length: 0\r\n\r\n"
                            .to_string()
                    } else if text.starts_with("GET /sso/callback") {

                        "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_string()
                    } else if text.starts_with("GET /venue-zhjs-server/sso/manageLogin") {

                        "HTTP/1.1 302 Found\r\nLocation: \
                         /venue-zhjs-server/sso/landing\r\nSet-Cookie: \
                         sso_buaa_zhjs_token=fixture; Path=/\r\nContent-Length: 0\r\n\r\n"
                            .to_string()
                    } else if text.starts_with("GET /venue-zhjs-server/sso/landing") {

                        "HTTP/1.1 200 OK\r\nContent-Length: 0\r\n\r\n".to_string()
                    } else if text.starts_with("POST /venue-zhjs-server/api/login") {

                        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: \
                         62\r\n\r\n{\"code\":0,\"data\":{\"token\":{\"access_token\":\"fixture\"}}}"
                            .to_string()
                    } else {

                        "HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\n\r\n".to_string()
                    };

                    let _ = stream.write_all(response.as_bytes()).await;
                });
            }
        });

        let input = LoginInput {
            student_id: "23371544".into(),
            vpn_password: "secret".into(),
            ..Default::default()
        };

        let result = super::IClassApi::for_venue_fixture(&input, &origin).await;

        server.abort();

        assert!(
            result.is_ok(),
            "fixture venue SSO must complete: {result:?}"
        );
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

    fn builds_cas_form_always_submitting_login_button_value() {

        let document = Html::parse_document(
            r#"
            <form id="fm1" action="/login">
              <input type="hidden" name="execution" value="e1s1">
              <input name="username" value="old-user">
              <input type="password" name="password" value="old-pass">
              <input type="submit" name="submit" value="旧值">
            </form>
            "#,
        );

        let form =
            build_cas_login_form(&document, "22330000", "secret", None).expect("form should parse");

        let fields = form_map(&form);

        assert_eq!(fields.get("submit").map(String::as_str), Some("登录"));
    }

    #[test]

    fn password_expiry_warning_page_builds_ignore_form() {

        let body = r#"
            <html>
              <body>
                <form id="continueForm" action="/login" method="post">
                  <div>账号存在安全风险，请修改密码</div>
                  <input type="hidden" name="execution" value="e2s2">
                  <button type="submit" name="_eventId" value="ignoreAndContinue">忽略提示</button>
                </form>
              </body>
            </html>
        "#;

        assert!(is_ignorable_password_expiry_page(body));

        assert_eq!(extract_execution_value(body).as_deref(), Some("e2s2"));

        let form = build_ignore_password_expiry_form("e2s2");

        let fields = form_map(&form);

        assert_eq!(fields.get("execution").map(String::as_str), Some("e2s2"));

        assert_eq!(
            fields.get("_eventId").map(String::as_str),
            Some("ignoreAndContinue")
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

    #[test]

    fn cas_fixture_covers_action_hidden_captcha_and_summary_markers() {

        let body = include_str!("../../tests/fixtures/cas_login_with_captcha.html");

        let document = Html::parse_document(body);

        let action = resolve_login_form_action("https://sso.buaa.edu.cn/login", &document).unwrap();

        assert_eq!(
            action,
            "https://sso.buaa.edu.cn/login?service=https%3A%2F%2Ficlass.example.invalid%2Fcas"
        );

        let form = build_cas_login_form(&document, "22330000", "secret", None)
            .expect("fixture form should parse");

        let fields = form_map(&form);

        assert_eq!(fields.get("execution").map(String::as_str), Some("e1s1"));

        assert_eq!(fields.get("lt").map(String::as_str), Some("LT-REDACTED"));

        assert_eq!(fields.get("username").map(String::as_str), Some("22330000"));

        assert_eq!(fields.get("password").map(String::as_str), Some("secret"));

        assert!(needs_vpn_captcha(body));

        assert_eq!(
            detect_captcha_id(body).as_deref(),
            Some("fixture-captcha-1")
        );

        assert_eq!(
            collect_captcha_field_names(&document),
            vec!["captchaResponse"]
        );

        let summary = summarize_login_page(body);

        assert!(summary.contains("bytes="));

        assert!(summary.contains("captcha"));

        assert!(summary.contains("cas_form"));
    }

    #[test]

    fn bad_credentials_fixture_summary_exposes_page_markers_without_secrets() {

        let body = include_str!("../../tests/fixtures/cas_bad_credentials.html");

        let summary = summarize_login_page(body);

        assert!(summary.contains("bytes="));

        assert!(summary.contains("bad_credentials"));

        assert!(summary.contains("cas_form"));

        assert!(!summary.contains("secret"));

        assert!(!summary.contains("password="));

        assert_eq!(find_login_error(body).as_deref(), Some("账号或密码错误"));
    }

    fn form_map(form: &[(String, String)]) -> HashMap<String, String> {

        form.iter().cloned().collect()
    }
}
