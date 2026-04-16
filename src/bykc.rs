use aes::Aes128;
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::{Local, NaiveDateTime};
use ecb::cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyInit, block_padding::Pkcs7};
use rand::{RngExt, prelude::IndexedRandom, rng};
use reqwest::cookie::Jar;
use reqwest::header::{ACCEPT, ACCEPT_LANGUAGE, HeaderMap, HeaderValue, REFERER, USER_AGENT};
use rsa::{RsaPublicKey, pkcs8::DecodePublicKey, traits::PublicKeyParts};
use scraper::{Html, Selector};
use serde::Deserialize;
use serde_json::Value;
use sha1::{Digest, Sha1};
use std::f64::consts::PI;
use std::sync::{Arc, Mutex};

use crate::constants::{
    BYKC_DIRECT_BASE, BYKC_KEY_CHARS, BYKC_PAGE_SIZE, BYKC_RSA_PUBLIC_KEY_BASE64, BYKC_VPN_BASE,
    SSO_VPN_LOGIN,
};
use crate::model::LoginInput;

type Aes128EcbEnc = ecb::Encryptor<Aes128>;
type Aes128EcbDec = ecb::Decryptor<Aes128>;

/// Lightweight course record shown in the BYKC course list.
#[allow(dead_code)]
#[derive(Clone, Debug, Default)]
pub struct BykcCourse {
    pub id: i64,
    pub course_name: String,
    pub course_position: String,
    pub course_teacher: String,
    pub course_start_date: String,
    pub course_end_date: String,
    pub course_select_start_date: String,
    pub course_select_end_date: String,
    pub course_max_count: i32,
    pub course_current_count: i32,
    pub category: String,
    pub sub_category: String,
    pub has_sign_points: bool,
    pub status: String,
    pub selected: bool,
    pub course_desc: String,
}

/// Full course detail used by the bottom detail panel and sign actions.
#[allow(dead_code)]
#[derive(Clone, Debug, Default)]
pub struct BykcCourseDetail {
    pub id: i64,
    pub course_name: String,
    pub course_position: String,
    pub course_contact: String,
    pub course_contact_mobile: String,
    pub course_teacher: String,
    pub course_start_date: String,
    pub course_end_date: String,
    pub course_select_start_date: String,
    pub course_select_end_date: String,
    pub course_cancel_end_date: String,
    pub course_max_count: i32,
    pub course_current_count: i32,
    pub category: String,
    pub sub_category: String,
    pub status: String,
    pub selected: bool,
    pub course_desc: String,
    pub sign_config: Option<BykcSignConfig>,
    pub checkin: Option<i32>,
    pub pass: Option<i32>,
    pub can_sign: bool,
    pub can_sign_out: bool,
}

/// Chosen-course record shown in the "已选课程" view.
#[allow(dead_code)]
#[derive(Clone, Debug, Default)]
pub struct BykcChosenCourse {
    pub id: i64,
    pub course_id: i64,
    pub course_name: String,
    pub course_position: String,
    pub course_teacher: String,
    pub course_start_date: String,
    pub course_end_date: String,
    pub select_date: String,
    pub course_cancel_end_date: String,
    pub category: String,
    pub sub_category: String,
    pub checkin: i32,
    pub score: Option<i32>,
    pub pass: Option<i32>,
    pub can_sign: bool,
    pub can_sign_out: bool,
    pub sign_config: Option<BykcSignConfig>,
    pub sign_info: String,
}

/// Attendance time window and allowed sign points from BYKC.
#[derive(Clone, Debug, Default)]
pub struct BykcSignConfig {
    pub sign_start_date: String,
    pub sign_end_date: String,
    pub sign_out_start_date: String,
    pub sign_out_end_date: String,
    pub sign_points: Vec<BykcSignPoint>,
}

/// BYKC sign-point definition used to construct a valid check-in location.
#[derive(Clone, Debug, Default)]
pub struct BykcSignPoint {
    pub lat: f64,
    pub lng: f64,
    pub radius: f64,
}

/// BYKC client responsible for login, encrypted API calls, and course operations.
#[derive(Clone, Debug)]
pub struct BykcApi {
    client: reqwest::Client,
    login_client: reqwest::Client,
    login_input: LoginInput,
    auth_token: Arc<Mutex<Option<String>>>,
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
        })
    }

    /// Loads every visible BYKC course page and maps it into the TUI course rows.
    pub async fn get_courses(&self, include_all: bool) -> Result<Vec<BykcCourse>> {
        self.ensure_login(false).await?;

        let mut page_number = 1usize;
        let mut courses = Vec::new();

        loop {
            let result = self
                .query_student_semester_course_by_page(page_number, BYKC_PAGE_SIZE)
                .await?;

            for course in result.content {
                let status = calculate_course_status(&course);
                if !include_all
                    && matches!(status, BykcCourseStatus::Expired | BykcCourseStatus::Ended)
                {
                    continue;
                }
                let sign_config = parse_sign_config(course.course_sign_config.as_deref());
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
                    category: course
                        .course_new_kind1
                        .as_ref()
                        .map(|item| item.kind_name.clone())
                        .unwrap_or_default(),
                    sub_category: course
                        .course_new_kind2
                        .as_ref()
                        .map(|item| item.kind_name.clone())
                        .unwrap_or_default(),
                    has_sign_points: sign_config
                        .as_ref()
                        .is_some_and(|item| !item.sign_points.is_empty()),
                    status: status.display_name().to_string(),
                    selected: course.selected.unwrap_or(false),
                    course_desc: html_to_text(course.course_desc.as_deref()),
                });
            }

            if page_number >= result.total_pages.max(1) as usize {
                break;
            }
            page_number += 1;
        }

        Ok(courses)
    }

    /// Loads the current user's chosen BYKC courses for the active semester.
    pub async fn get_chosen_courses(&self) -> Result<Vec<BykcChosenCourse>> {
        self.ensure_login(false).await?;
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

        let chosen_courses = self.query_chosen_course(start_date, end_date).await?;
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
                    category: course
                        .course_new_kind1
                        .as_ref()
                        .map(|item| item.kind_name.clone())
                        .unwrap_or_default(),
                    sub_category: course
                        .course_new_kind2
                        .as_ref()
                        .map(|item| item.kind_name.clone())
                        .unwrap_or_default(),
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

        if course.selected.unwrap_or(false) {
            if let Some(chosen) = self
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
        }

        let availability = resolve_attendance_availability(
            sign_config.as_ref(),
            checkin,
            pass,
            Local::now().naive_local(),
        );

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
            category: course
                .course_new_kind1
                .as_ref()
                .map(|item| item.kind_name.clone())
                .unwrap_or_default(),
            sub_category: course
                .course_new_kind2
                .as_ref()
                .map(|item| item.kind_name.clone())
                .unwrap_or_default(),
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
            sign_config = self.get_course_detail(course_id).await?.sign_config;
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
        let chosen_courses = self.query_chosen_course(start_date, end_date).await?;
        Ok(chosen_courses
            .into_iter()
            .find(|item| item.course_info.as_ref().map(|course| course.id) == Some(course_id)))
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

/// Extracts the BYKC auth token from the login redirect URL.
fn extract_bykc_token(url: &str) -> Option<String> {
    url.split_once("?token=")
        .map(|(_, token)| token.to_string())
}

/// Resolves the correct BYKC base URL for direct and VPN modes.
fn bykc_base_url(use_vpn: bool) -> &'static str {
    if use_vpn {
        BYKC_VPN_BASE
    } else {
        BYKC_DIRECT_BASE
    }
}

/// Applies the same AES + RSA envelope used by the BYKC web client.
fn encrypt_request(json_data: &str) -> Result<EncryptedRequest> {
    let data_bytes = json_data.as_bytes();
    let aes_key = generate_aes_key();
    let ak = rsa_encrypt(&aes_key)?;

    let mut sha1 = Sha1::new();
    sha1.update(data_bytes);
    let data_sign = sha1.finalize();
    let sign_hex = data_sign
        .iter()
        .map(|value| format!("{value:02x}"))
        .collect::<String>();
    let sk = rsa_encrypt(sign_hex.as_bytes())?;

    let encrypted_data = Aes128EcbEnc::new_from_slice(&aes_key)
        .map_err(|_| anyhow!("无法初始化 BYKC AES 加密器"))?
        .encrypt_padded_vec::<Pkcs7>(data_bytes);

    Ok(EncryptedRequest {
        encrypted_data: BASE64.encode(encrypted_data),
        ak,
        sk,
        ts: chrono::Utc::now().timestamp_millis().to_string(),
        aes_key,
    })
}

/// Decrypts a BYKC response payload with the per-request AES key.
fn decrypt_response(response_base64: &str, aes_key: &[u8]) -> Result<String> {
    let encrypted_bytes = BASE64
        .decode(response_base64)
        .context("BYKC 响应 Base64 解码失败")?;
    let decrypted = Aes128EcbDec::new_from_slice(aes_key)
        .map_err(|_| anyhow!("无法初始化 BYKC AES 解密器"))?
        .decrypt_padded_vec::<Pkcs7>(&encrypted_bytes)
        .map_err(|_| anyhow!("BYKC 响应 AES 解密失败"))?;
    String::from_utf8(decrypted).context("BYKC 响应不是合法 UTF-8")
}

/// Generates the random AES key expected by the BYKC backend.
fn generate_aes_key() -> Vec<u8> {
    let mut rng = rng();
    (0..16)
        .map(|_| {
            *BYKC_KEY_CHARS
                .choose(&mut rng)
                .expect("BYKC_KEY_CHARS must not be empty")
        })
        .collect()
}

/// RSA-encrypts a BYKC header field and returns the Base64 form.
fn rsa_encrypt(data: &[u8]) -> Result<String> {
    let der = BASE64
        .decode(BYKC_RSA_PUBLIC_KEY_BASE64)
        .context("BYKC RSA 公钥解码失败")?;
    let public_key = RsaPublicKey::from_public_key_der(&der).context("BYKC RSA 公钥加载失败")?;
    let encrypted = rsa_pkcs1_encrypt(&public_key, data)?;
    Ok(BASE64.encode(encrypted))
}

/// Performs PKCS#1 v1.5 public-key encryption to match the original BYKC client.
fn rsa_pkcs1_encrypt(public_key: &RsaPublicKey, message: &[u8]) -> Result<Vec<u8>> {
    let size = public_key.size();
    if message.len() > size.saturating_sub(11) {
        bail!("BYKC RSA 加密消息过长");
    }

    let mut encoded = vec![0u8; size];
    encoded[1] = 0x02;

    let padding_len = size - message.len() - 3;
    let mut rng = rng();
    for index in 0..padding_len {
        let value = rng.random_range(1..=u8::MAX);
        encoded[2 + index] = value;
    }
    encoded[2 + padding_len] = 0x00;
    encoded[(3 + padding_len)..].copy_from_slice(message);

    let m = rsa::BigUint::from_bytes_be(&encoded);
    let c = m.modpow(public_key.e(), public_key.n());
    let mut output = c.to_bytes_be();
    if output.len() < size {
        let mut padded = vec![0u8; size - output.len()];
        padded.extend(output);
        output = padded;
    }
    Ok(output)
}

/// Parses the embedded sign config JSON attached to a course record.
fn parse_sign_config(config_json: Option<&str>) -> Option<BykcSignConfig> {
    let raw = config_json?.trim();
    if raw.is_empty() {
        return None;
    }

    let config = serde_json::from_str::<BykcSignConfigRaw>(raw).ok()?;
    Some(BykcSignConfig {
        sign_start_date: config.sign_start_date.unwrap_or_default(),
        sign_end_date: config.sign_end_date.unwrap_or_default(),
        sign_out_start_date: config.sign_out_start_date.unwrap_or_default(),
        sign_out_end_date: config.sign_out_end_date.unwrap_or_default(),
        sign_points: config
            .sign_point_list
            .into_iter()
            .map(|point| BykcSignPoint {
                lat: point.lat,
                lng: point.lng,
                radius: point.radius,
            })
            .collect(),
    })
}

/// Resolves whether a chosen course can currently sign in or sign out.
fn resolve_attendance_availability(
    sign_config: Option<&BykcSignConfig>,
    checkin: Option<i32>,
    pass: Option<i32>,
    now: NaiveDateTime,
) -> AttendanceAvailability {
    AttendanceAvailability {
        can_sign: pass != Some(1)
            && is_unsigned_checkin(checkin)
            && is_within_window(
                sign_config.map(|item| item.sign_start_date.as_str()),
                sign_config.map(|item| item.sign_end_date.as_str()),
                now,
            ),
        can_sign_out: pass != Some(1)
            && is_signed_awaiting_sign_out(checkin)
            && is_within_window(
                sign_config.map(|item| item.sign_out_start_date.as_str()),
                sign_config.map(|item| item.sign_out_end_date.as_str()),
                now,
            ),
    }
}

fn is_unsigned_checkin(checkin: Option<i32>) -> bool {
    checkin.is_none() || checkin == Some(0)
}

fn is_signed_awaiting_sign_out(checkin: Option<i32>) -> bool {
    matches!(checkin, Some(5) | Some(6))
}

fn resolve_sign_in_unavailable_reason(checkin: Option<i32>, pass: Option<i32>) -> &'static str {
    if pass == Some(1) {
        "课程已考核完成，无需签到"
    } else if !is_unsigned_checkin(checkin) {
        "当前考勤状态不可签到"
    } else {
        "当前不在签到时间窗口"
    }
}

fn resolve_sign_out_unavailable_reason(checkin: Option<i32>, pass: Option<i32>) -> &'static str {
    if pass == Some(1) {
        "课程已考核完成，无需签退"
    } else if !is_signed_awaiting_sign_out(checkin) {
        "当前考勤状态不可签退"
    } else {
        "当前不在签退时间窗口"
    }
}

/// Checks whether the current time falls inside the configured attendance window.
fn is_within_window(start_date: Option<&str>, end_date: Option<&str>, now: NaiveDateTime) -> bool {
    let Some(start) = parse_date_time(start_date) else {
        return false;
    };
    let Some(end) = parse_date_time(end_date) else {
        return false;
    };
    now >= start && now <= end
}

/// Parses the BYKC `yyyy-MM-dd HH:mm:ss` timestamp format.
fn parse_date_time(value: Option<&str>) -> Option<NaiveDateTime> {
    let value = value?.trim();
    if value.is_empty() {
        return None;
    }
    NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S").ok()
}

/// Chooses a legal sign-in point and jitters it within the configured radius.
fn random_sign_location(sign_config: Option<&BykcSignConfig>) -> Result<(f64, f64)> {
    let point = sign_config
        .and_then(|config| {
            let mut rng = rng();
            config.sign_points.choose(&mut rng).cloned()
        })
        .ok_or_else(|| anyhow!("未找到可用的签到地点配置"))?;

    if point.radius > 0.0 {
        let mut rng = rng();
        let dist = point.radius * rng.random_range(0.0..1.0f64).sqrt();
        let angle = rng.random_range(0.0..1.0f64) * 2.0 * PI;
        Ok(destination_point(point.lat, point.lng, dist, angle))
    } else {
        Ok((point.lat, point.lng))
    }
}

/// Computes the destination coordinates from a start point, distance, and angle.
fn destination_point(lat: f64, lng: f64, dist: f64, angle: f64) -> (f64, f64) {
    let radius = dist / 6_371_000.0;
    let lat_radians = lat.to_radians();
    let lng_radians = lng.to_radians();
    let dest_lat =
        (lat_radians.sin() * radius.cos() + lat_radians.cos() * radius.sin() * angle.cos()).asin();
    let dest_lng = lng_radians
        + (angle.sin() * radius.sin() * lat_radians.cos())
            .atan2(radius.cos() - lat_radians.sin() * dest_lat.sin());
    (dest_lat.to_degrees(), dest_lng.to_degrees())
}

/// Normalizes BYKC business error messages before surfacing them in the TUI.
fn sanitize_bykc_error_message(raw: &str, fallback: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        fallback.to_string()
    } else {
        raw.replace("签到失败:", "").trim().to_string()
    }
}

/// Converts BYKC rich-text HTML into plain text for terminal rendering.
fn html_to_text(raw: Option<&str>) -> String {
    let raw = raw.unwrap_or_default().trim();
    if raw.is_empty() {
        return String::new();
    }

    let fragment = Html::parse_fragment(raw);
    let text = fragment
        .root_element()
        .text()
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join(" ");

    if text.is_empty() {
        raw.to_string()
    } else {
        text
    }
}

/// Derives the list status shown in the BYKC course list.
fn calculate_course_status(course: &BykcCourseRaw) -> BykcCourseStatus {
    let now = Local::now().naive_local();
    let course_start = parse_date_time(course.course_start_date.as_deref());
    let select_start = parse_date_time(course.course_select_start_date.as_deref());
    let select_end = parse_date_time(course.course_select_end_date.as_deref());

    match () {
        _ if course_start.is_some_and(|value| now > value) => BykcCourseStatus::Expired,
        _ if course.selected.unwrap_or(false) => BykcCourseStatus::Selected,
        _ if select_end.is_some_and(|value| now > value) => BykcCourseStatus::Ended,
        _ if course
            .course_current_count
            .is_some_and(|value| value >= course.course_max_count) =>
        {
            BykcCourseStatus::Full
        }
        _ if select_start.is_some_and(|value| now < value) => BykcCourseStatus::Preview,
        _ => BykcCourseStatus::Available,
    }
}

/// Logs into BUAA VPN and establishes the cookie session needed by BYKC.
async fn vpn_login(client: &reqwest::Client, username: &str, password: &str) -> Result<()> {
    if username.trim().is_empty() || password.is_empty() {
        bail!("博雅功能需要 VPN 账号和密码");
    }

    let body = client
        .get(SSO_VPN_LOGIN)
        .send()
        .await
        .context("获取 SSO 登录页失败")?
        .text()
        .await
        .context("读取 SSO 登录页失败")?;
    let execution = {
        let document = Html::parse_document(&body);
        let selector = Selector::parse(r#"input[name="execution"]"#)
            .map_err(|_| anyhow!("SSO 页面选择器构造失败"))?;
        document
            .select(&selector)
            .next()
            .and_then(|node| node.value().attr("value"))
            .map(|value| value.to_string())
            .ok_or_else(|| anyhow!("无法从 SSO 登录页面解析 execution 参数"))?
    };

    let response = client
        .post(SSO_VPN_LOGIN)
        .header(REFERER, SSO_VPN_LOGIN)
        .form(&[
            ("username", username.trim()),
            ("password", password),
            ("submit", "登录"),
            ("type", "username_password"),
            ("execution", &execution),
            ("_eventId", "submit"),
        ])
        .send()
        .await
        .context("VPN 登录请求失败")?;

    if response.status() == reqwest::StatusCode::UNAUTHORIZED {
        bail!("VPN 登录失败：账号或密码错误");
    }
    Ok(())
}

/// Per-request encrypted payload plus the headers derived from it.
#[derive(Clone)]
struct EncryptedRequest {
    encrypted_data: String,
    ak: String,
    sk: String,
    ts: String,
    aes_key: Vec<u8>,
}

/// Course status shown in the selectable-course list.
#[derive(Clone, Copy, Debug)]
enum BykcCourseStatus {
    Expired,
    Selected,
    Preview,
    Ended,
    Full,
    Available,
}

impl BykcCourseStatus {
    fn display_name(self) -> &'static str {
        match self {
            Self::Expired => "已过期",
            Self::Selected => "已选",
            Self::Preview => "预告",
            Self::Ended => "已结束",
            Self::Full => "人数已满",
            Self::Available => "可选",
        }
    }
}

/// Computed attendance availability for a chosen course at the current time.
#[derive(Clone, Copy, Debug, Default)]
struct AttendanceAvailability {
    can_sign: bool,
    can_sign_out: bool,
}

/// Generic outer response envelope used by BYKC APIs.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BykcApiResponse<T> {
    status: String,
    #[serde(default)]
    data: Option<T>,
    #[serde(default)]
    errmsg: String,
}

impl<T> BykcApiResponse<T> {
    fn is_success(&self) -> bool {
        self.status == "0"
    }
}

/// Paged payload returned by the course-list query.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BykcCoursePageResult {
    #[serde(default)]
    content: Vec<BykcCourseRaw>,
    #[serde(default)]
    total_pages: i32,
}

#[derive(Clone, Debug, Default, Deserialize)]
struct BykcCourseActionResult {}

/// Global BYKC configuration containing the active semester range.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BykcAllConfig {
    #[serde(default)]
    semester: Vec<BykcSemester>,
}

/// Semester definition extracted from the BYKC config response.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BykcSemester {
    #[serde(default)]
    semester_start_date: Option<String>,
    #[serde(default)]
    semester_end_date: Option<String>,
}

/// Payload wrapper around the chosen-course list.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BykcChosenCoursePayload {
    #[serde(default)]
    course_list: Vec<BykcChosenCourseRaw>,
}

/// Raw course record returned by multiple BYKC APIs.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BykcCourseRaw {
    id: i64,
    #[serde(default)]
    course_name: String,
    #[serde(default)]
    course_position: Option<String>,
    #[serde(default)]
    course_teacher: Option<String>,
    #[serde(default)]
    course_start_date: Option<String>,
    #[serde(default)]
    course_end_date: Option<String>,
    #[serde(default)]
    course_select_start_date: Option<String>,
    #[serde(default)]
    course_select_end_date: Option<String>,
    #[serde(default)]
    course_cancel_end_date: Option<String>,
    #[serde(default)]
    course_max_count: i32,
    #[serde(default)]
    course_current_count: Option<i32>,
    #[serde(default)]
    course_new_kind1: Option<BykcCourseKind>,
    #[serde(default)]
    course_new_kind2: Option<BykcCourseKind>,
    #[serde(default)]
    selected: Option<bool>,
    #[serde(default)]
    course_desc: Option<String>,
    #[serde(default)]
    course_contact: Option<String>,
    #[serde(default)]
    course_contact_mobile: Option<String>,
    #[serde(default)]
    course_sign_config: Option<String>,
}

/// Raw category node embedded in a BYKC course record.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BykcCourseKind {
    #[serde(default)]
    kind_name: String,
}

/// Raw chosen-course record returned by BYKC.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BykcChosenCourseRaw {
    id: i64,
    #[serde(default)]
    select_date: Option<String>,
    #[serde(default)]
    course_info: Option<BykcCourseRaw>,
    #[serde(default)]
    checkin: Option<i32>,
    #[serde(default)]
    score: Option<i32>,
    #[serde(default)]
    pass: Option<i32>,
    #[serde(default)]
    sign_info: Option<String>,
}

/// Raw JSON structure stored inside `courseSignConfig`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BykcSignConfigRaw {
    #[serde(default)]
    sign_start_date: Option<String>,
    #[serde(default)]
    sign_end_date: Option<String>,
    #[serde(default)]
    sign_out_start_date: Option<String>,
    #[serde(default)]
    sign_out_end_date: Option<String>,
    #[serde(default)]
    sign_point_list: Vec<BykcSignPointRaw>,
}

/// Raw sign-point entry embedded in the sign config JSON.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
struct BykcSignPointRaw {
    lat: f64,
    lng: f64,
    #[serde(default)]
    radius: f64,
}
