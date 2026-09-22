//! Sunshine clock-in (阳光打卡) for `ygdk.buaa.edu.cn`.
//!
//! Why:
//! Sports clock-in is tracked by its own service with its own token, reached
//! through an app OAuth redirect rather than the unified-auth password. The
//! overview and history are read-only; submitting a clock-in is a real record
//! and is gated behind explicit confirmation at the CLI.
//!
//! How:
//! OAuth yields a `uid` and `token`, which every later call carries as form
//! fields. The token is URL-encoded in some responses, so it is decoded once
//! before use.

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::constants::to_webvpn_url;
use crate::iclass::IClassApi;

const BASE_URL: &str = "https://ygdk.buaa.edu.cn/api/Front";

/// OAuth entry point that redirects back with a `code`.

const OAUTH_URL: &str = "https://app.buaa.edu.cn/uc/api/oauth/index?redirect=https%3A%2F%2Fygdk.buaa.edu.cn%2F%23%2Fhome&appid=200230221144501510&state=STATE&qrcode=1";

/// Authenticated session, carried as form fields on every call.

#[derive(Clone, Debug, Default)]

pub struct YgdkSession {
    pub uid:   i64,
    pub token: String,
}

/// A clock-in category, such as 晨跑 or 课外锻炼.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct Classify {
    pub id:        i64,
    pub name:      String,
    pub term_num:  Option<i64>,
    pub week_num:  Option<i64>,
    pub month_num: Option<i64>,
}

/// A clock-in item within a category.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct Item {
    pub id:   i64,
    pub name: String,
}

/// Completion counters for a category.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct Count {
    pub term_count:      i64,
    /// What the service displays, which can differ from the raw count.
    pub term_count_show: i64,
    pub term_good_count: i64,
    pub week_count:      i64,
    pub week_num:        i64,
    pub month_count:     i64,
    pub month_num:       i64,
    pub day_count:       i64,
    /// Minimum required for the term.
    pub term_num:        i64,
}

impl Count {
    /// Whether the term requirement is met.

    pub fn satisfied(&self) -> bool {

        self.term_num > 0 && self.term_count >= self.term_num
    }

    /// How many more are needed, if any.

    pub fn remaining(&self) -> i64 {

        (self.term_num - self.term_count).max(0)
    }
}

/// One recorded clock-in.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct Record {
    pub id:        i64,
    pub item_name: String,
    pub place:     String,
    pub start:     Option<i64>,
    pub end:       Option<i64>,
    pub create_at: String,
}

/// Result of a submitted clock-in.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct ClockinResult {
    pub record_id:  Option<i64>,
    pub term_count: i64,
    pub message:    String,
}

impl IClassApi {
    /// Performs the OAuth handshake and returns a session.
    ///
    /// How:
    /// The OAuth entry redirects a few times before yielding a `code`, which is
    /// then exchanged at `campusAppLogin` for `uid` and `token`.

    pub async fn ygdk_login(&self) -> Result<YgdkSession> {

        let code = self.ygdk_oauth_code().await?;

        let response = self
            .client
            .get(ygdk_url(
                self.use_vpn,
                &format!("{BASE_URL}/Clockin/User/campusAppLogin"),
            ))
            .query(&[("code", code)])
            .send()
            .await
            .context("阳光打卡登录失败")?;

        let body = response.text().await.context("读取阳光打卡登录响应失败")?;

        let value = unwrap(&body)?;

        let data = value.get("data").cloned().unwrap_or(value);

        let uid = data
            .get("uid")
            .and_then(flexible_i64)
            .ok_or_else(|| anyhow!("阳光打卡登录未返回 uid"))?;

        // The service URL-encodes the token in some responses.
        let token = data
            .get("token")
            .and_then(Value::as_str)
            .map(decode_url_component)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("阳光打卡登录未返回 token"))?;

        Ok(YgdkSession { uid, token })
    }

    /// Follows the OAuth redirect chain until a `code` appears.

    async fn ygdk_oauth_code(&self) -> Result<String> {

        let mut current = ygdk_url(self.use_vpn, OAUTH_URL);

        for _ in 0..10 {

            let response = self
                .no_redirect_client
                .get(&current)
                .send()
                .await
                .context("阳光打卡 OAuth 跳转失败")?;

            let url = response.url().to_string();

            if let Some(code) = extract_oauth_code(&url) {

                return Ok(code);
            }

            let Some(location) = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
            else {

                break;
            };

            current = reqwest::Url::parse(&url)
                .and_then(|base| base.join(location))
                .map(|next| next.to_string())
                .context("解析阳光打卡 OAuth 跳转失败")?;

            if let Some(code) = extract_oauth_code(&current) {

                return Ok(code);
            }
        }

        bail!("阳光打卡未取得登录 code，请确认统一认证登录有效（需要先登录 app.buaa.edu.cn）")
    }

    /// Sends an authenticated form POST.

    async fn ygdk_post(
        &self,
        session: &YgdkSession,
        path: &str,
        form: &[(String, String)],
    ) -> Result<Value> {

        let url = ygdk_url(self.use_vpn, &format!("{BASE_URL}/{path}"));

        let mut fields: Vec<(String, String)> = form.to_vec();

        fields.push(("uid".to_string(), session.uid.to_string()));

        fields.push(("token".to_string(), session.token.clone()));

        let response = self
            .client
            .post(&url)
            .header("X-Requested-With", "XMLHttpRequest")
            .header(
                "Content-Type",
                "application/x-www-form-urlencoded; charset=UTF-8",
            )
            .form(&fields)
            .send()
            .await
            .with_context(|| format!("阳光打卡请求失败: {path}"))?;

        let body = response.text().await.context("读取阳光打卡响应失败")?;

        unwrap(&body)
    }

    /// Lists clock-in categories.

    pub async fn ygdk_classifies(&self, session: &YgdkSession) -> Result<Vec<Classify>> {

        let value = self
            .ygdk_post(session, "Clockin/Classify/getList", &[])
            .await?;

        Ok(value
            .get("list")
            .and_then(Value::as_array)
            .map(|rows| {

                rows.iter()
                    .filter_map(|row| {

                        Some(Classify {
                            id:        flexible_i64(row.get("classify_id")?)?,
                            name:      row.get("name").and_then(Value::as_str)?.to_string(),
                            term_num:  row.get("term_num").and_then(flexible_i64),
                            week_num:  row.get("week_num").and_then(flexible_i64),
                            month_num: row.get("month_num").and_then(flexible_i64),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Lists the items within a category.

    pub async fn ygdk_items(&self, session: &YgdkSession, classify_id: i64) -> Result<Vec<Item>> {

        let value = self
            .ygdk_post(
                session,
                "Clockin/Item/getList",
                &[
                    ("page".to_string(), "1".to_string()),
                    ("limit".to_string(), "1000".to_string()),
                    ("classify_id".to_string(), classify_id.to_string()),
                ],
            )
            .await?;

        Ok(value
            .get("list")
            .and_then(Value::as_array)
            .map(|rows| {

                rows.iter()
                    .filter_map(|row| {

                        Some(Item {
                            id:   flexible_i64(row.get("item_id")?)?,
                            name: row.get("name").and_then(Value::as_str)?.to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Reads completion counters for a category.

    pub async fn ygdk_count(&self, session: &YgdkSession, classify_id: i64) -> Result<Count> {

        let value = self
            .ygdk_post(
                session,
                "Clockin/Clockin/getCount",
                &[("classify_id".to_string(), classify_id.to_string())],
            )
            .await?;

        Ok(Count {
            term_count:      value.get("term_count").and_then(flexible_i64).unwrap_or(0),
            term_count_show: value
                .get("term_count_show")
                .and_then(flexible_i64)
                .unwrap_or(0),
            term_good_count: value
                .get("term_good_count")
                .and_then(flexible_i64)
                .unwrap_or(0),
            week_count:      value.get("week_count").and_then(flexible_i64).unwrap_or(0),
            week_num:        value.get("week_num").and_then(flexible_i64).unwrap_or(0),
            month_count:     value.get("month_count").and_then(flexible_i64).unwrap_or(0),
            month_num:       value.get("month_num").and_then(flexible_i64).unwrap_or(0),
            day_count:       value.get("day_count").and_then(flexible_i64).unwrap_or(0),
            term_num:        value.get("term_num").and_then(flexible_i64).unwrap_or(0),
        })
    }

    /// Lists past clock-in records.

    pub async fn ygdk_records(
        &self,
        session: &YgdkSession,
        classify_id: i64,
        page: i64,
        limit: i64,
    ) -> Result<Vec<Record>> {

        let value = self
            .ygdk_post(
                session,
                "Clockin/Clockin/getList",
                &[
                    ("page".to_string(), page.to_string()),
                    ("limit".to_string(), limit.to_string()),
                    ("classify_id".to_string(), classify_id.to_string()),
                ],
            )
            .await?;

        Ok(value
            .get("list")
            .and_then(Value::as_array)
            .map(|rows| {

                rows.iter()
                    .filter_map(|row| {

                        Some(Record {
                            id:        flexible_i64(row.get("record_id")?)?,
                            item_name: row
                                .get("item_name")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            place:     row
                                .get("place")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            start:     row.get("start_time").and_then(flexible_i64),
                            end:       row.get("end_time").and_then(flexible_i64),
                            create_at: row
                                .get("create_time_fmt")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                        })
                    })
                    .collect()
            })
            .unwrap_or_default())
    }

    /// Submits a clock-in.
    ///
    /// Why:
    /// This creates a real sports record under the user's name. It requires an
    /// uploaded photo, so it is only reachable with an explicit image path at
    /// the CLI, plus `--yes`.
    ///
    /// How:
    /// The photo is uploaded first to obtain a server-side file name, which the
    /// clock-in form references.

    pub async fn ygdk_clockin(
        &self,
        session: &YgdkSession,
        classify_id: i64,
        item: &Item,
        start: chrono::DateTime<chrono::FixedOffset>,
        end: chrono::DateTime<chrono::FixedOffset>,
        place: &str,
        photo: &std::path::Path,
    ) -> Result<ClockinResult> {

        let image_name = self.ygdk_upload_photo(session, photo).await?;

        let form = vec![
            ("start_time".to_string(), start.timestamp().to_string()),
            ("end_time".to_string(), end.timestamp().to_string()),
            ("place_type".to_string(), "1".to_string()),
            ("place".to_string(), place.to_string()),
            ("isopen".to_string(), "0".to_string()),
            ("form_time_fmt".to_string(), format_span(start, end)),
            ("images".to_string(), format!("[\"{image_name}\"]")),
            ("classify_id".to_string(), classify_id.to_string()),
            ("item_id".to_string(), item.id.to_string()),
            ("item_name".to_string(), item.name.clone()),
        ];

        let value = self
            .ygdk_post(session, "Clockin/Clockin/clockin", &form)
            .await
            .context("提交打卡失败")?;

        Ok(ClockinResult {
            record_id:  value.get("record_id").and_then(flexible_i64),
            term_count: value.get("term_count").and_then(flexible_i64).unwrap_or(0),
            message:    value
                .get("msg")
                .or_else(|| value.get("message"))
                .and_then(Value::as_str)
                .unwrap_or("打卡成功")
                .to_string(),
        })
    }

    /// Uploads a clock-in photo and returns its server-side file name.

    async fn ygdk_upload_photo(
        &self,
        session: &YgdkSession,
        photo: &std::path::Path,
    ) -> Result<String> {

        let bytes = std::fs::read(photo)
            .with_context(|| format!("读取打卡照片失败: {}", photo.display()))?;

        let file_name = photo
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("clockin.jpg")
            .to_string();

        let mime = mime_for(&file_name);

        let part = reqwest::multipart::Part::bytes(bytes)
            .file_name(file_name)
            .mime_str(mime)
            .context("构造照片上传请求失败")?;

        let form = reqwest::multipart::Form::new()
            .text("uid", session.uid.to_string())
            .text("token", session.token.clone())
            .part("file", part);

        let response = self
            .client
            .post(ygdk_url(
                self.use_vpn,
                &format!("{BASE_URL}/Upload/File/post"),
            ))
            .header("X-Requested-With", "XMLHttpRequest")
            .multipart(form)
            .send()
            .await
            .context("上传打卡照片失败")?;

        let body = response.text().await.context("读取照片上传响应失败")?;

        let value = unwrap(&body)?;

        value
            .get("file_name")
            .and_then(Value::as_str)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("照片上传成功但未返回文件名"))
    }
}

/// Formats the human-readable span the service stores alongside the timestamps.

fn format_span(
    start: chrono::DateTime<chrono::FixedOffset>,
    end: chrono::DateTime<chrono::FixedOffset>,
) -> String {

    format!(
        "{} - {}",
        start.format("%Y-%m-%d %H:%M"),
        end.format("%Y-%m-%d %H:%M")
    )
}

/// Best-effort MIME type from a file extension.

fn mime_for(file_name: &str) -> &'static str {

    let lower = file_name.to_ascii_lowercase();

    if lower.ends_with(".png") {

        "image/png"
    } else if lower.ends_with(".webp") {

        "image/webp"
    } else if lower.ends_with(".gif") {

        "image/gif"
    } else {

        "image/jpeg"
    }
}

fn ygdk_url(use_vpn: bool, raw: &str) -> String {

    if use_vpn {

        to_webvpn_url(raw)
    } else {

        raw.to_string()
    }
}

/// Pulls `code` out of a URL, checking both the query and the fragment.
///
/// Why:
/// The OAuth entry redirects to a URL whose parameters follow the `#` of a
/// single-page route (`/#/home?code=...`). `Url::query_pairs` only reads the
/// part before the fragment, so it would miss the code entirely and the login
/// would fail with no explanation.

fn extract_oauth_code(url: &str) -> Option<String> {

    let parsed = reqwest::Url::parse(url).ok()?;

    let from_query = parsed
        .query_pairs()
        .find(|(key, _)| key == "code")
        .map(|(_, value)| value.to_string());

    from_query
        .or_else(|| params_from_fragment(parsed.fragment().unwrap_or_default()))
        .filter(|value| !value.is_empty())
}

/// Reads a `code` parameter out of a fragment such as `/home?code=abc`.

fn params_from_fragment(fragment: &str) -> Option<String> {

    let query = fragment.split_once('?').map(|(_, rest)| rest)?;

    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == "code")
        .map(|(_, value)| decode_url_component(value))
}

/// Percent-decodes a value, leaving it unchanged when it is not encoded.
///
/// Why:
/// The service URL-encodes the token in some responses. Passing the encoded
/// form through would authenticate with the wrong value.

fn decode_url_component(value: &str) -> String {

    let mut output = String::with_capacity(value.len());

    let bytes = value.as_bytes();

    let mut index = 0;

    while index < bytes.len() {

        if bytes[index] == b'%' && index + 2 < bytes.len() {

            let hex = &value[index + 1..index + 3];

            if let Ok(byte) = u8::from_str_radix(hex, 16) {

                output.push(byte as char);

                index += 3;

                continue;
            }
        }

        if bytes[index] == b'+' {

            output.push(' ');

            index += 1;

            continue;
        }

        output.push(bytes[index] as char);

        index += 1;
    }

    output
}

/// Unwraps the service's envelope.
///
/// Why:
/// Failures arrive as HTTP 200 with a negative or non-zero code, so the status
/// alone is not enough to tell success from rejection.

fn unwrap(body: &str) -> Result<Value> {

    let value: Value = serde_json::from_str(body).with_context(|| {

        let preview: String = body.chars().take(120).collect();

        format!("阳光打卡返回了非 JSON 响应: {preview}")
    })?;

    if let Some(code) = value.get("code").and_then(flexible_i64)
        && code != 0
        && code != 1
    {

        let message = value
            .get("msg")
            .or_else(|| value.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("阳光打卡接口返回错误");

        bail!("{message} (code={code})");
    }

    // Some responses nest the payload; callers read from the unwrapped value.
    Ok(value.get("data").cloned().unwrap_or(value))
}

fn flexible_i64(value: &Value) -> Option<i64> {

    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
}

#[cfg(test)]

mod tests {

    use super::{Count, decode_url_component, extract_oauth_code, format_span, mime_for, unwrap};

    #[test]

    fn reads_the_oauth_code_from_a_redirect() {

        assert_eq!(
            extract_oauth_code("https://ygdk.buaa.edu.cn/#/home?code=abc123").as_deref(),
            Some("abc123")
        );

        assert!(extract_oauth_code("https://ygdk.buaa.edu.cn/#/home").is_none());

        assert!(
            extract_oauth_code("https://x/?code=").is_none(),
            "空 code 不应视为成功"
        );
    }

    #[test]

    fn decodes_a_url_encoded_token() {

        // Tokens come back encoded in some responses; using the raw form would
        // authenticate with the wrong value.
        assert_eq!(decode_url_component("a%2Bb%2Fc%3D"), "a+b/c=");

        assert_eq!(decode_url_component("plain-token"), "plain-token");

        assert_eq!(decode_url_component("a+b"), "a b");

        // A truncated escape must not panic or drop characters.
        assert_eq!(decode_url_component("a%"), "a%");

        assert_eq!(decode_url_component("a%zz"), "a%zz");
    }

    #[test]

    fn envelope_errors_are_surfaced() {

        assert!(unwrap(r#"{"code":0,"data":{"x":1}}"#).is_ok());

        let error = unwrap(r#"{"code":-1,"msg":"未登录"}"#)
            .unwrap_err()
            .to_string();

        assert!(error.contains("未登录"), "应保留服务端消息: {error}");
    }

    #[test]

    fn nested_data_is_unwrapped() {

        let value = unwrap(r#"{"code":0,"data":{"uid":7}}"#).expect("应能解析");

        assert_eq!(value.get("uid").and_then(|v| v.as_i64()), Some(7));
    }

    #[test]

    fn count_reports_progress_against_the_requirement() {

        let met = Count {
            term_count: 12,
            term_num: 12,
            ..Count::default()
        };

        assert!(met.satisfied());

        assert_eq!(met.remaining(), 0);

        let short = Count {
            term_count: 8,
            term_num: 12,
            ..Count::default()
        };

        assert!(!short.satisfied());

        assert_eq!(short.remaining(), 4);

        // No stated requirement means nothing to report.
        let unknown = Count::default();

        assert!(!unknown.satisfied());

        assert_eq!(unknown.remaining(), 0);
    }

    #[test]

    fn mime_type_follows_the_extension() {

        assert_eq!(mime_for("run.PNG"), "image/png");

        assert_eq!(mime_for("run.jpg"), "image/jpeg");

        assert_eq!(mime_for("run"), "image/jpeg");
    }

    #[test]

    fn span_is_formatted_from_local_timestamps() {

        let zone = chrono::FixedOffset::east_opt(8 * 3600).expect("东八区应有效");

        let start = chrono::DateTime::parse_from_rfc3339("2026-03-10T06:00:00+08:00")
            .expect("应能解析")
            .with_timezone(&zone);

        let end = chrono::DateTime::parse_from_rfc3339("2026-03-10T06:40:00+08:00")
            .expect("应能解析")
            .with_timezone(&zone);

        assert_eq!(
            format_span(start, end),
            "2026-03-10 06:00 - 2026-03-10 06:40"
        );
    }
}
