//! BYKC client implementation and request flow orchestration.

use anyhow::{Context, Result, anyhow, bail};
use chrono::Local;
use reqwest::cookie::Jar;
use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, HeaderMap, HeaderValue, REFERER, USER_AGENT};
use serde::Deserialize;
use serde_json::Value;
use std::sync::{Arc, Mutex};

use crate::constants::{BYKC_PAGE_SIZE, BYKC_VPN_BASE};
use crate::model::LoginInput;

use super::helpers::{
    bykc_base_url, calculate_course_status, decrypt_response, encrypt_request, extract_bykc_token,
    html_to_text, parse_sign_config, random_sign_location, resolve_attendance_availability,
    resolve_sign_in_unavailable_reason, resolve_sign_out_unavailable_reason,
    sanitize_bykc_error_message, vpn_login,
};
use super::raw::{
    BykcAllConfig, BykcApiResponse, BykcChosenCoursePayload, BykcChosenCourseRaw,
    BykcCourseActionResult, BykcCourseKind, BykcCoursePageResult, BykcCourseRaw,
};
use super::types::{BykcChosenCourse, BykcCourse, BykcCourseDetail};

/// BYKC client responsible for login, encrypted API calls, and course operations.
#[derive(Clone, Debug)]
pub struct BykcApi {
    client: reqwest::Client,
    login_client: reqwest::Client,
    login_input: LoginInput,
    auth_token: Arc<Mutex<Option<String>>>,
    current_semester_dates: Arc<Mutex<Option<(String, String)>>>,
}

impl BykcApi {
    /// Creates a BYKC client bound to the login form used by the TUI.
    pub fn new(login_input: LoginInput) -> Result<Self> {
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
            HeaderValue::from_static("application/json, text/plain;q=0.9, */*;q=0.8"),
        );

        let cookie_jar = Arc::new(Jar::default());
        let default_headers = headers.clone();
        let client = reqwest::Client::builder()
            .cookie_provider(cookie_jar.clone())
            .default_headers(headers)
            .build()
            .context("failed to build bykc reqwest client")?;
        let login_client = reqwest::Client::builder()
            .cookie_provider(cookie_jar)
            .default_headers(default_headers)
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .context("failed to build bykc login reqwest client")?;

        Ok(Self {
            client,
            login_client,
            login_input,
            auth_token: Arc::new(Mutex::new(None)),
            current_semester_dates: Arc::new(Mutex::new(None)),
        })
    }

    /// Loads every visible BYKC course page and maps it into the TUI course rows.
    pub async fn get_courses(&self, include_all: bool) -> Result<Vec<BykcCourse>> {
        self.ensure_login(false).await?;

        let mut courses = Vec::new();
        let first_page = self
            .query_student_semester_course_by_page(1, BYKC_PAGE_SIZE)
            .await?;
        let total_pages = first_page.total_pages.max(1) as usize;
        let mut page_results = vec![(1usize, first_page)];

        let mut tasks = Vec::with_capacity(total_pages.saturating_sub(1));
        for page_number in 2..=total_pages {
            let api = self.clone();
            tasks.push(tokio::spawn(async move {
                let result = api
                    .query_student_semester_course_by_page(page_number, BYKC_PAGE_SIZE)
                    .await?;
                Ok::<(usize, BykcCoursePageResult), anyhow::Error>((page_number, result))
            }));
        }

        for task in tasks {
            page_results.push(task.await.context("博雅课程分页任务执行失败")??);
        }
        page_results.sort_by_key(|(page_number, _)| *page_number);

        for (_, result) in page_results {
            for course in result.content {
                let status = calculate_course_status(&course);
                if !include_all
                    && matches!(
                        status,
                        super::helpers::BykcCourseStatus::Expired
                            | super::helpers::BykcCourseStatus::Ended
                    )
                {
                    continue;
                }
                let sign_config = parse_sign_config(course.course_sign_config.as_deref());
                let (category, sub_category) = course_kind_names(&course);
                courses.push(BykcCourse {
                    id: course.id,
                    course_name: course.course_name,
                    course_position: course.course_position.unwrap_or_default(),
                    course_teacher: course.course_teacher.unwrap_or_default(),
                    course_start_date: course.course_start_date.unwrap_or_default(),
                    course_end_date: course.course_end_date.unwrap_or_default(),
                    course_select_start_date: course.course_select_start_date.unwrap_or_default(),
                    course_select_end_date: course.course_select_end_date.unwrap_or_default(),
                    course_max_count: course.course_max_count,
                    course_current_count: course.course_current_count.unwrap_or_default(),
                    category,
                    sub_category,
                    has_sign_points: sign_config
                        .as_ref()
                        .is_some_and(|item| !item.sign_points.is_empty()),
                    status: status.display_name().to_string(),
                    selected: course.selected.unwrap_or(false),
                    course_desc: html_to_text(course.course_desc.as_deref()),
                });
            }
        }

        Ok(courses)
    }

    /// Loads the current user's chosen BYKC courses for the active semester.
    pub async fn get_chosen_courses(&self) -> Result<Vec<BykcChosenCourse>> {
        self.ensure_login(false).await?;
        let chosen_courses = self.query_current_semester_chosen_courses().await?;
        let now = Local::now().naive_local();

        Ok(chosen_courses
            .into_iter()
            .map(|chosen| {
                let course = chosen.course_info.unwrap_or_default();
                let sign_config = parse_sign_config(course.course_sign_config.as_deref());
                let availability = resolve_attendance_availability(
                    sign_config.as_ref(),
                    chosen.checkin,
                    chosen.pass,
                    now,
                );
                let (category, sub_category) = course_kind_names(&course);

                BykcChosenCourse {
                    id: chosen.id,
                    course_id: course.id,
                    course_name: course.course_name,
                    course_position: course.course_position.unwrap_or_default(),
                    course_teacher: course.course_teacher.unwrap_or_default(),
                    course_start_date: course.course_start_date.unwrap_or_default(),
                    course_end_date: course.course_end_date.unwrap_or_default(),
                    select_date: chosen.select_date.unwrap_or_default(),
                    course_cancel_end_date: course.course_cancel_end_date.unwrap_or_default(),
                    category,
                    sub_category,
                    checkin: chosen.checkin.unwrap_or_default(),
                    score: chosen.score,
                    pass: chosen.pass,
                    can_sign: availability.can_sign,
                    can_sign_out: availability.can_sign_out,
                    sign_config,
                    sign_info: chosen.sign_info.unwrap_or_default(),
                }
            })
            .collect())
    }

    /// Loads a single course detail and resolves sign-in availability.
    pub async fn get_course_detail(&self, course_id: i64) -> Result<BykcCourseDetail> {
        self.ensure_login(false).await?;
        let course = self.query_course_by_id(course_id).await?;
        let status = calculate_course_status(&course);
        let mut sign_config = parse_sign_config(course.course_sign_config.as_deref());
        let mut checkin = None;
        let mut pass = None;

        if course.selected.unwrap_or(false)
            && let Some(chosen) = self
                .find_chosen_course_for_current_semester(course_id)
                .await?
        {
            if sign_config.is_none() {
                sign_config = parse_sign_config(
                    chosen
                        .course_info
                        .as_ref()
                        .and_then(|item| item.course_sign_config.as_deref()),
                );
            }
            checkin = chosen.checkin;
            pass = chosen.pass;
        }

        let availability = resolve_attendance_availability(
            sign_config.as_ref(),
            checkin,
            pass,
            Local::now().naive_local(),
        );
        let (category, sub_category) = course_kind_names(&course);

        Ok(BykcCourseDetail {
            id: course.id,
            course_name: course.course_name,
            course_position: course.course_position.unwrap_or_default(),
            course_contact: course.course_contact.unwrap_or_default(),
            course_contact_mobile: course.course_contact_mobile.unwrap_or_default(),
            course_teacher: course.course_teacher.unwrap_or_default(),
            course_start_date: course.course_start_date.unwrap_or_default(),
            course_end_date: course.course_end_date.unwrap_or_default(),
            course_select_start_date: course.course_select_start_date.unwrap_or_default(),
            course_select_end_date: course.course_select_end_date.unwrap_or_default(),
            course_cancel_end_date: course.course_cancel_end_date.unwrap_or_default(),
            course_max_count: course.course_max_count,
            course_current_count: course.course_current_count.unwrap_or_default(),
            category,
            sub_category,
            status: status.display_name().to_string(),
            selected: course.selected.unwrap_or(false),
            course_desc: html_to_text(course.course_desc.as_deref()),
            sign_config,
            checkin,
            pass,
            can_sign: availability.can_sign,
            can_sign_out: availability.can_sign_out,
        })
    }

    /// Enrolls the user into a course.
    pub async fn select_course(&self, course_id: i64) -> Result<String> {
        self.ensure_login(false).await?;
        let request = format!(r#"{{"courseId":{course_id}}}"#);
        let response: BykcApiResponse<BykcCourseActionResult> =
            self.call_api("choseCourse", &request).await?;
        if !response.is_success() {
            bail!(sanitize_bykc_error_message(&response.errmsg, "选课失败"));
        }
        Ok("报名成功".to_string())
    }

    /// Cancels an existing enrollment for the chosen course.
    pub async fn deselect_course(&self, course_id: i64) -> Result<String> {
        self.ensure_login(false).await?;
        self.find_chosen_course_for_current_semester(course_id)
            .await?
            .ok_or_else(|| anyhow!("该课程未报名，无法退选"))?;
        let request = format!(r#"{{"id":{course_id}}}"#);
        let response: BykcApiResponse<BykcCourseActionResult> =
            self.call_api("delChosenCourse", &request).await?;
        if !response.is_success() {
            bail!(sanitize_bykc_error_message(&response.errmsg, "退选失败"));
        }
        Ok("退选成功".to_string())
    }

    /// Performs BYKC sign-in after resolving the configured sign window and location.
    pub async fn sign_in(&self, course_id: i64) -> Result<String> {
        self.sign_course(course_id, 1)
            .await
            .map(|_| "签到成功".to_string())
    }

    /// Performs BYKC sign-out after resolving the configured sign window and location.
    pub async fn sign_out(&self, course_id: i64) -> Result<String> {
        self.sign_course(course_id, 2)
            .await
            .map(|_| "签退成功".to_string())
    }

    async fn sign_course(&self, course_id: i64, sign_type: i32) -> Result<()> {
        self.ensure_login(false).await?;
        let chosen = self
            .find_chosen_course_for_current_semester(course_id)
            .await?
            .ok_or_else(|| anyhow!("该课程未选，无法执行签到操作"))?;

        let mut sign_config = parse_sign_config(
            chosen
                .course_info
                .as_ref()
                .and_then(|item| item.course_sign_config.as_deref()),
        );
        if sign_config.is_none() {
            let course = self.query_course_by_id(course_id).await?;
            sign_config = parse_sign_config(course.course_sign_config.as_deref());
        }

        let availability = resolve_attendance_availability(
            sign_config.as_ref(),
            chosen.checkin,
            chosen.pass,
            Local::now().naive_local(),
        );
        if sign_type == 1 && !availability.can_sign {
            bail!(resolve_sign_in_unavailable_reason(
                chosen.checkin,
                chosen.pass
            ));
        }
        if sign_type == 2 && !availability.can_sign_out {
            bail!(resolve_sign_out_unavailable_reason(
                chosen.checkin,
                chosen.pass
            ));
        }

        let (lat, lng) = random_sign_location(sign_config.as_ref())?;
        let request = format!(
            r#"{{"courseId":{course_id},"signLat":{lat},"signLng":{lng},"signType":{sign_type}}}"#
        );
        let response: BykcApiResponse<Value> = self.call_api("signCourseByUser", &request).await?;
        if !response.is_success() {
            bail!(sanitize_bykc_error_message(&response.errmsg, "签到失败"));
        }
        Ok(())
    }

    /// Ensures the BYKC auth token exists and refreshes it when the session expires.
    async fn ensure_login(&self, force_refresh: bool) -> Result<()> {
        if !self.login_input.use_vpn {
            bail!("博雅功能当前仅支持 VPN 模式登录");
        }

        if !force_refresh
            && self
                .auth_token
                .lock()
                .expect("token mutex poisoned")
                .is_some()
        {
            return Ok(());
        }

        vpn_login(
            &self.client,
            self.login_input.vpn_username.as_str(),
            self.login_input.vpn_password.as_str(),
        )
        .await?;

        let login_url = format!("{BYKC_VPN_BASE}/sscv/cas/login");
        let response = self
            .login_client
            .get(&login_url)
            .send()
            .await
            .context("博雅登录请求失败")?;

        let final_url = response.url().to_string();
        let header_location = response
            .headers()
            .get("Location")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        let token = extract_bykc_token(&final_url).or_else(|| extract_bykc_token(&header_location));

        let token = match token {
            Some(token) => token,
            None => {
                let fallback_response = self
                    .client
                    .get(&login_url)
                    .send()
                    .await
                    .context("博雅登录重试失败")?;
                let fallback_url = fallback_response.url().to_string();
                extract_bykc_token(&fallback_url)
                    .ok_or_else(|| anyhow!("博雅登录成功但未获取到 auth_token"))?
            }
        };

        *self.auth_token.lock().expect("token mutex poisoned") = Some(token);
        Ok(())
    }

    /// Calls the paged course-list API and validates the business response.
    async fn query_student_semester_course_by_page(
        &self,
        page_number: usize,
        page_size: usize,
    ) -> Result<BykcCoursePageResult> {
        let request = format!(r#"{{"pageNumber":{page_number},"pageSize":{page_size}}}"#);
        let response: BykcApiResponse<BykcCoursePageResult> = self
            .call_api("queryStudentSemesterCourseByPage", &request)
            .await?;
        if !response.is_success() {
            bail!(sanitize_bykc_error_message(
                &response.errmsg,
                "博雅课程列表加载失败"
            ));
        }
        response.data.ok_or_else(|| anyhow!("博雅课程列表返回为空"))
    }

    /// Loads BYKC global config, mainly to resolve the active semester window.
    async fn get_all_config(&self) -> Result<BykcAllConfig> {
        let response: BykcApiResponse<BykcAllConfig> = self.call_api("getAllConfig", "{}").await?;
        if !response.is_success() {
            bail!(sanitize_bykc_error_message(
                &response.errmsg,
                "博雅配置加载失败"
            ));
        }
        response.data.ok_or_else(|| anyhow!("博雅配置返回为空"))
    }

    /// Loads chosen courses within the given semester date range.
    async fn query_chosen_course(
        &self,
        start_date: &str,
        end_date: &str,
    ) -> Result<Vec<BykcChosenCourseRaw>> {
        let request = format!(r#"{{"startDate":"{start_date}","endDate":"{end_date}"}}"#);
        let response: BykcApiResponse<BykcChosenCoursePayload> =
            self.call_api("queryChosenCourse", &request).await?;
        if !response.is_success() {
            bail!(sanitize_bykc_error_message(
                &response.errmsg,
                "已选课程加载失败"
            ));
        }
        Ok(response.data.unwrap_or_default().course_list)
    }

    /// Loads a single raw course record from BYKC.
    async fn query_course_by_id(&self, id: i64) -> Result<BykcCourseRaw> {
        let request = format!(r#"{{"id":{id}}}"#);
        let response: BykcApiResponse<BykcCourseRaw> =
            self.call_api("queryCourseById", &request).await?;
        if !response.is_success() {
            bail!(sanitize_bykc_error_message(
                &response.errmsg,
                "课程详情加载失败"
            ));
        }
        response.data.ok_or_else(|| anyhow!("课程详情返回为空"))
    }

    /// Finds the chosen-course record matching a course in the active semester.
    async fn find_chosen_course_for_current_semester(
        &self,
        course_id: i64,
    ) -> Result<Option<BykcChosenCourseRaw>> {
        let chosen_courses = self.query_current_semester_chosen_courses().await?;
        Ok(chosen_courses
            .into_iter()
            .find(|item| item.course_info.as_ref().map(|course| course.id) == Some(course_id)))
    }

    /// Loads the chosen-course list for the active semester only.
    async fn query_current_semester_chosen_courses(&self) -> Result<Vec<BykcChosenCourseRaw>> {
        let (start_date, end_date) = self.current_semester_dates().await?;
        self.query_chosen_course(&start_date, &end_date).await
    }

    /// Resolves the current semester date range from the BYKC config payload.
    async fn current_semester_dates(&self) -> Result<(String, String)> {
        if let Some((start_date, end_date)) = self
            .current_semester_dates
            .lock()
            .expect("semester dates mutex poisoned")
            .clone()
        {
            return Ok((start_date, end_date));
        }

        let config = self.get_all_config().await?;
        let semester = config
            .semester
            .first()
            .ok_or_else(|| anyhow!("无法获取当前学期信息"))?;
        let start_date = semester
            .semester_start_date
            .as_deref()
            .ok_or_else(|| anyhow!("无法获取当前学期信息"))?;
        let end_date = semester
            .semester_end_date
            .as_deref()
            .ok_or_else(|| anyhow!("无法获取当前学期信息"))?;
        let semester_dates = (start_date.to_string(), end_date.to_string());
        *self
            .current_semester_dates
            .lock()
            .expect("semester dates mutex poisoned") = Some(semester_dates.clone());
        Ok(semester_dates)
    }

    /// Calls a BYKC API and deserializes the decrypted JSON payload.
    async fn call_api<T>(&self, api_name: &str, request_json: &str) -> Result<T>
    where
        T: for<'de> Deserialize<'de>,
    {
        let raw = self.call_api_raw(api_name, request_json).await?;
        serde_json::from_str(&raw)
            .with_context(|| format!("BYKC 响应 JSON 解析失败: {api_name}: {raw}"))
    }

    /// Calls a BYKC API and retries once after refreshing the auth token.
    async fn call_api_raw(&self, api_name: &str, request_json: &str) -> Result<String> {
        self.ensure_login(false).await?;
        match self.do_call_api_raw(api_name, request_json).await {
            Ok(value) => Ok(value),
            Err(error) => {
                *self.auth_token.lock().expect("token mutex poisoned") = None;
                self.ensure_login(true).await?;
                self.do_call_api_raw(api_name, request_json)
                    .await
                    .with_context(|| format!("{error}"))
            }
        }
    }

    /// Sends one encrypted BYKC request and returns the decrypted response body.
    async fn do_call_api_raw(&self, api_name: &str, request_json: &str) -> Result<String> {
        let encrypted = encrypt_request(request_json)?;
        let endpoint = format!(
            "{}/sscv/{}",
            bykc_base_url(self.login_input.use_vpn),
            api_name
        );
        let referer = format!(
            "{}/system/course-select",
            bykc_base_url(self.login_input.use_vpn)
        );
        let origin = bykc_base_url(self.login_input.use_vpn).to_string();

        let mut request = self
            .client
            .post(endpoint)
            .header(REFERER, referer)
            .header("Origin", origin)
            .header("Content-Type", "application/json;charset=UTF-8")
            .header("ak", encrypted.ak.as_str())
            .header("sk", encrypted.sk.as_str())
            .header("ts", encrypted.ts.as_str())
            .header(ACCEPT, "application/json")
            .body(encrypted.encrypted_data);

        if let Some(token) = self
            .auth_token
            .lock()
            .expect("token mutex poisoned")
            .clone()
        {
            request = request
                .header("auth_token", token.as_str())
                .header("authtoken", token);
        }

        let response = request.send().await.context("BYKC 请求失败")?;
        let status = response.status();
        let body = response.text().await.context("读取 BYKC 响应失败")?;
        if !status.is_success() {
            bail!("BYKC 服务返回异常 HTTP 状态: {status}: {body}");
        }

        let response_base64 = serde_json::from_str::<String>(&body).unwrap_or(body);
        let decoded = decrypt_response(&response_base64, &encrypted.aes_key)
            .unwrap_or_else(|_| response_base64.to_string());

        if decoded.contains("会话已失效") || decoded.contains("未登录") {
            bail!("博雅会话已失效");
        }

        Ok(decoded)
    }
}

/// Extracts the primary and secondary category names from a raw course record.
fn course_kind_names(course: &BykcCourseRaw) -> (String, String) {
    (
        course_kind_name(course.course_new_kind1.as_ref()),
        course_kind_name(course.course_new_kind2.as_ref()),
    )
}

/// Normalizes an optional BYKC category node into a plain display string.
fn course_kind_name(kind: Option<&BykcCourseKind>) -> String {
    kind.map(|item| item.kind_name.clone()).unwrap_or_default()
}
