//! Internal BYKC helpers for crypto, parsing, HTML cleanup, and availability checks.

use aes::Aes128;
use anyhow::{Context, Result, anyhow, bail};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64};
use chrono::{Local, NaiveDateTime};
use ecb::cipher::{BlockModeDecrypt, BlockModeEncrypt, KeyInit, block_padding::Pkcs7};
use rand::{RngExt, prelude::IndexedRandom, rng};
use reqwest::header::REFERER;
use rsa::{RsaPublicKey, pkcs8::DecodePublicKey, traits::PublicKeyParts};
use scraper::{Html, Selector};
use sha1::{Digest, Sha1};
use std::f64::consts::PI;

use crate::constants::{
    BYKC_DIRECT_BASE, BYKC_KEY_CHARS, BYKC_RSA_PUBLIC_KEY_BASE64, BYKC_VPN_BASE, SSO_VPN_LOGIN,
};

use super::raw::{BykcCourseRaw, BykcSignConfigRaw};
use super::types::{BykcSignConfig, BykcSignPoint};

type Aes128EcbEnc = ecb::Encryptor<Aes128>;
type Aes128EcbDec = ecb::Decryptor<Aes128>;

/// Extracts the BYKC auth token from the login redirect URL.
pub(super) fn extract_bykc_token(url: &str) -> Option<String> {
    url.split_once("?token=")
        .map(|(_, token)| token.to_string())
}

/// Resolves the correct BYKC base URL for direct and VPN modes.
pub(super) fn bykc_base_url(use_vpn: bool) -> &'static str {
    if use_vpn {
        BYKC_VPN_BASE
    } else {
        BYKC_DIRECT_BASE
    }
}

/// Applies the same AES + RSA envelope used by the BYKC web client.
///
/// Why:
/// BYKC does not accept plain JSON bodies. The request payload, AES key, and
/// payload digest all have to be wrapped the same way as the browser client, or
/// the server rejects the call before business validation even starts.
///
/// How:
/// Generate one short-lived AES key, encrypt the JSON body with it, then RSA
/// encrypt both the AES key and the SHA-1 digest of the plaintext. The caller
/// sends the returned fields as BYKC's expected request envelope.
pub(super) fn encrypt_request(json_data: &str) -> Result<EncryptedRequest> {
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
///
/// Why:
/// BYKC mirrors the same symmetric key back into the response path, so callers
/// cannot inspect the returned business payload unless they keep the request's
/// AES key and use it here.
pub(super) fn decrypt_response(response_base64: &str, aes_key: &[u8]) -> Result<String> {
    let encrypted_bytes = BASE64
        .decode(response_base64)
        .context("BYKC 响应 Base64 解码失败")?;
    let decrypted = Aes128EcbDec::new_from_slice(aes_key)
        .map_err(|_| anyhow!("无法初始化 BYKC AES 解密器"))?
        .decrypt_padded_vec::<Pkcs7>(&encrypted_bytes)
        .map_err(|_| anyhow!("BYKC 响应 AES 解密失败"))?;
    String::from_utf8(decrypted).context("BYKC 响应不是合法 UTF-8")
}

/// Parses the embedded sign config JSON attached to a course record.
///
/// Why:
/// The course list API embeds sign windows and legal sign-in locations as one
/// JSON string field instead of a typed nested object. Converting it once keeps
/// later attendance logic focused on business rules rather than JSON plumbing.
pub(super) fn parse_sign_config(config_json: Option<&str>) -> Option<BykcSignConfig> {
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
///
/// Why:
/// BYKC models sign-in and sign-out as two separate windows layered on top of a
/// coarse `checkin`/`pass` state machine. The TUI, CLI, and autologin all need
/// one consistent answer about what is actionable right now.
pub(super) fn resolve_attendance_availability(
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

pub(super) fn resolve_sign_in_unavailable_reason(
    checkin: Option<i32>,
    pass: Option<i32>,
) -> &'static str {
    if pass == Some(1) {
        "课程已考核完成，无需签到"
    } else if !is_unsigned_checkin(checkin) {
        "当前考勤状态不可签到"
    } else {
        "当前不在签到时间窗口"
    }
}

pub(super) fn resolve_sign_out_unavailable_reason(
    checkin: Option<i32>,
    pass: Option<i32>,
) -> &'static str {
    if pass == Some(1) {
        "课程已考核完成，无需签退"
    } else if !is_signed_awaiting_sign_out(checkin) {
        "当前考勤状态不可签退"
    } else {
        "当前不在签退时间窗口"
    }
}

/// Chooses a legal sign-in point and jitters it within the configured radius.
///
/// Why:
/// BYKC sign requests need coordinates that fall inside an allowed area. Using
/// the exact center every time is unnecessary and makes automated requests look
/// artificially rigid, so we randomize within the permitted radius.
pub(super) fn random_sign_location(sign_config: Option<&BykcSignConfig>) -> Result<(f64, f64)> {
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

/// Normalizes BYKC business error messages before surfacing them in the TUI.
pub(super) fn sanitize_bykc_error_message(raw: &str, fallback: &str) -> String {
    let raw = raw.trim();
    if raw.is_empty() {
        fallback.to_string()
    } else {
        raw.replace("签到失败:", "").trim().to_string()
    }
}

/// Converts BYKC rich-text HTML into plain text for terminal rendering.
///
/// Why:
/// BYKC details often contain editor-generated markup. The terminal view needs
/// readable text, but we still keep the original snippet as a fallback when the
/// HTML parser cannot extract anything useful.
pub(super) fn html_to_text(raw: Option<&str>) -> String {
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
///
/// Why:
/// The selectable-course tab needs one stable label that matches the user's
/// mental model: already selected, full, preview only, expired, or actionable.
/// BYKC exposes the required facts separately, so the UI derives that summary
/// here instead of scattering the priority rules across the renderer.
pub(super) fn calculate_course_status(course: &BykcCourseRaw) -> BykcCourseStatus {
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
///
/// Why:
/// BYKC reuses BUAA SSO/VPN cookies rather than offering a separate API token.
/// Running this before BYKC requests lets the later API calls stay simple and
/// assume the shared `reqwest::Client` already carries the required session.
pub(super) async fn vpn_login(
    client: &reqwest::Client,
    username: &str,
    password: &str,
) -> Result<()> {
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

    let final_url = response.url().to_string();
    let body = response.text().await.context("读取 VPN 登录响应失败")?;
    if final_url.contains("/login")
        || body.contains(r#"name="execution""#)
        || body.contains("统一身份认证")
    {
        bail!("VPN 登录失败，仍停留在统一认证页面");
    }
    Ok(())
}

/// Course status shown in the selectable-course list.
#[derive(Clone, Copy, Debug)]
pub(super) enum BykcCourseStatus {
    Expired,
    Selected,
    Preview,
    Ended,
    Full,
    Available,
}

impl BykcCourseStatus {
    pub(super) fn display_name(self) -> &'static str {
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
pub(super) struct AttendanceAvailability {
    pub(super) can_sign: bool,
    pub(super) can_sign_out: bool,
}

/// Per-request encrypted payload plus the headers derived from it.
#[derive(Clone)]
pub(super) struct EncryptedRequest {
    pub(super) encrypted_data: String,
    pub(super) ak: String,
    pub(super) sk: String,
    pub(super) ts: String,
    pub(super) aes_key: Vec<u8>,
}

/// Generates the 16-byte AES key format accepted by BYKC's frontend protocol.
///
/// Why:
/// The web client restricts key characters to a custom alphanumeric alphabet.
/// Mirroring that behavior avoids protocol drift from the browser reference.
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

/// Encrypts one short protocol field with the BYKC RSA public key.
///
/// Why:
/// The server expects the AES key and SHA-1 digest as Base64-encoded RSA
/// ciphertext. Keeping the public-key loading here avoids repeating that setup
/// in every caller.
fn rsa_encrypt(data: &[u8]) -> Result<String> {
    let der = BASE64
        .decode(BYKC_RSA_PUBLIC_KEY_BASE64)
        .context("BYKC RSA 公钥解码失败")?;
    let public_key = RsaPublicKey::from_public_key_der(&der).context("BYKC RSA 公钥加载失败")?;
    let encrypted = rsa_pkcs1_encrypt(&public_key, data)?;
    Ok(BASE64.encode(encrypted))
}

/// Performs RSAES-PKCS1-v1_5 style public-key encryption.
///
/// Why:
/// BYKC follows the legacy browser-side RSA convention instead of a modern
/// hybrid library format. This helper keeps that compatibility logic in one
/// place so the higher-level request builder stays readable.
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

fn is_unsigned_checkin(checkin: Option<i32>) -> bool {
    checkin.is_none() || checkin == Some(0)
}

fn is_signed_awaiting_sign_out(checkin: Option<i32>) -> bool {
    matches!(checkin, Some(5) | Some(6))
}

/// Checks whether the current time falls inside the configured attendance window.
///
/// Why:
/// Missing or malformed timestamps should behave as "not open yet" rather than
/// accidentally allowing an action. Returning `false` on any parse failure keeps
/// the caller conservative.
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

/// Computes the destination coordinates from a start point, distance, and angle.
///
/// How:
/// Treat the Earth as a sphere, convert the distance into an angular radius,
/// then project the random offset from the configured sign center.
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
