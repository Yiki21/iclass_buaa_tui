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
use chrono::TimeZone;
use rand::prelude::IndexedRandom;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::constants::to_webvpn_url;
use crate::iclass::IClassApi;

const BASE_URL: &str = "https://ygdk.buaa.edu.cn/api/Front";

/// How many days back a random clock-in may reach.
///
/// Why three:
/// The reference implementation draws from today and the two preceding days,
/// so aiming for the same window keeps a submitted record indistinguishable
/// from one made through the app. A wider reach would push records into a
/// window the service's own history no longer treats as plausible.

const RANDOM_DAY_RANGE_DAYS: u32 = 3;

/// Earliest hour a random clock-in may start at, in Beijing time.

const RANDOM_EARLIEST_HOUR: u32 = 8;

/// Latest hour a random clock-in's window may end at, in Beijing time.
///
/// Why:
/// The service treats a late-evening record as implausible, and the reference
/// implementation stops at 22:00. Ending later would make the record look
/// unlike a real one.

const RANDOM_LATEST_END_HOUR: u32 = 22;

/// Length of a randomly generated window.
///
/// Why fixed at one hour:
/// The reference implementation uses exactly one hour for every generated
/// window; this only mirrors that, it is not a service requirement.

const RANDOM_WINDOW_MINUTES: i64 = 60;

/// Place recorded with a one-tap clock-in, matching the reference app.

pub const DEFAULT_PLACE: &str = "操场";

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
    pub id:          i64,
    pub item_name:   String,
    pub place:       String,
    pub start:       Option<i64>,
    pub end:         Option<i64>,
    pub create_at:   String,
    /// Start time as the service formats it (HH:MM).
    pub start_label: String,
    /// End time as the service formats it (HH:MM).
    pub end_label:   String,
}

/// Result of a submitted clock-in.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct ClockinResult {
    pub record_id:  Option<i64>,
    pub term_count: i64,
    pub message:    String,
}

/// The image to attach to a clock-in.
///
/// Why:
/// The service requires a photo on every record. Holding the bytes here rather
/// than a path lets a caller pass either a file the user chose or an image the
/// program produced, without the submission path caring which it got.

#[derive(Clone, Debug, PartialEq, Eq)]

pub struct ClockinPhoto {
    pub bytes:     Vec<u8>,
    pub file_name: String,
}

impl ClockinPhoto {
    /// Reads a photo from disk.

    pub fn from_path(path: &std::path::Path) -> Result<Self> {

        let bytes =
            std::fs::read(path).with_context(|| format!("读取打卡照片失败: {}", path.display()))?;

        if bytes.is_empty() {

            bail!("打卡照片是空文件: {}", path.display());
        }

        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("clockin.jpg")
            .to_string();

        Ok(Self { bytes, file_name })
    }

    /// Builds a blank PNG to stand in for a photo nobody supplied.
    ///
    /// Why:
    /// The service rejects a clock-in with no image, so a caller that wants a
    /// one-step "just check in" needs some image to send. This one is
    /// deliberately unremarkable: a solid-colour frame, no embedded text or
    /// metadata suggesting a real photograph.
    ///
    /// What this is not:
    /// It is not a photo, and the caller must say so before submitting. A
    /// record carrying this image is one a reviewer can reject on sight.

    pub fn generated() -> Result<Self> {

        let (width, height) = (640u32, 480u32);

        let image = image::RgbImage::from_pixel(width, height, image::Rgb([0x1a, 0x1a, 0x1a]));

        let mut bytes = Vec::new();

        image::DynamicImage::ImageRgb8(image)
            .write_to(
                &mut std::io::Cursor::new(&mut bytes),
                image::ImageFormat::Png,
            )
            .context("生成打卡占位图片失败")?;

        Ok(Self {
            bytes,
            file_name: "clockin_auto.png".to_string(),
        })
    }
}

/// A randomly chosen clock-in window.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]

pub struct RandomSpan {
    pub start: chrono::DateTime<chrono::FixedOffset>,
    pub end:   chrono::DateTime<chrono::FixedOffset>,
}

/// Picks a plausible past window, aligned to the hour.
///
/// Why:
/// A record submitted for "right now" is one the user did not actually make.
/// Choosing a past hour inside the plausible range produces the same shape of
/// record as one submitted through the app at the time.
///
/// How:
/// Candidates are the hour-long windows beginning on the hour, between 08:00
/// and the 22:00 cutoff, over today and the two preceding days. Today only
/// contributes windows that have already finished, so a submission never claims
/// a window still in progress. One candidate is then drawn uniformly.

pub fn random_span(now: chrono::DateTime<chrono::FixedOffset>) -> RandomSpan {

    let mut candidates: Vec<chrono::DateTime<chrono::FixedOffset>> = Vec::new();

    let window = chrono::Duration::minutes(RANDOM_WINDOW_MINUTES);

    let midnight = now.date_naive().and_hms_opt(0, 0, 0).expect("午夜时刻有效");

    // The offset is fixed for Beijing time, so the conversion cannot be
    // ambiguous; falling back to `now` keeps the function total.
    let today_start = now
        .offset()
        .from_local_datetime(&midnight)
        .single()
        .unwrap_or(now);

    for offset in 0..RANDOM_DAY_RANGE_DAYS {

        let days_back = i64::from(offset);

        let date_start = today_start - chrono::Duration::days(days_back);

        let latest_end = if days_back == 0 {

            // Today's window has to have finished, so "end" cannot be later
            // than now.
            now.min(date_start + chrono::Duration::hours(i64::from(RANDOM_LATEST_END_HOUR)))
        } else {

            date_start + chrono::Duration::hours(i64::from(RANDOM_LATEST_END_HOUR))
        };

        let latest_start = latest_end - window;

        let earliest_start = date_start + chrono::Duration::hours(i64::from(RANDOM_EARLIEST_HOUR));

        let mut start = earliest_start;

        while start <= latest_start {

            candidates.push(start);

            start += window;
        }
    }

    let start = candidates
        .choose(&mut rand::rng())
        .copied()
        // Today may contribute nothing (before 09:00), but the two earlier days
        // always contribute 08:00..21:00, so this is unreachable; falling back
        // to an already-ended window keeps the function total.
        .unwrap_or(now - window);

    RandomSpan {
        start,
        end: start + window,
    }
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

        let data = value;

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
                            id:          flexible_i64(row.get("record_id")?)?,
                            // The service names this `item_fmt`, not `item_name`.
                            item_name:   row
                                .get("item_fmt")
                                .or_else(|| row.get("item_name"))
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            place:       row
                                .get("place")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            start:       row.get("start_time").and_then(flexible_i64),
                            end:         row.get("end_time").and_then(flexible_i64),
                            create_at:   row
                                .get("create_time_fmt")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            // Wall-clock labels the service formats itself.
                            start_label: row
                                .get("start_time_fmt")
                                .and_then(Value::as_str)
                                .unwrap_or_default()
                                .to_string(),
                            end_label:   row
                                .get("end_time_fmt")
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
    /// clock-in form references. The bytes are taken rather than a path so the
    /// same call serves a user-supplied photo and a generated stand-in.

    pub async fn ygdk_clockin(
        &self,
        session: &YgdkSession,
        classify_id: i64,
        item: &Item,
        start: chrono::DateTime<chrono::FixedOffset>,
        end: chrono::DateTime<chrono::FixedOffset>,
        place: &str,
        photo: &ClockinPhoto,
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
        photo: &ClockinPhoto,
    ) -> Result<String> {

        let file_name = photo.file_name.clone();

        let mime = mime_for(&file_name);

        let part = reqwest::multipart::Part::bytes(photo.bytes.clone())
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

    // The service nests payloads one level down, and the level's name varies:
    // list endpoints answer `result`, the login answers `result.data`. Both are
    // unwrapped so callers see the same shape.
    let result = value.get("result").cloned().unwrap_or(value);

    Ok(result
        .get("data")
        .cloned()
        .filter(|data| data.is_object() || data.is_array())
        .unwrap_or(result))
}

fn flexible_i64(value: &Value) -> Option<i64> {

    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
}

#[cfg(test)]

mod tests {

    use super::{ClockinPhoto, random_span};
    use super::{Count, decode_url_component, extract_oauth_code, format_span, mime_for, unwrap};
    use chrono::{TimeZone, Timelike};

    fn beijing(y: i32, m: u32, d: u32, h: u32, min: u32) -> chrono::DateTime<chrono::FixedOffset> {

        chrono::FixedOffset::east_opt(8 * 3600)
            .expect("东八区偏移有效")
            .with_ymd_and_hms(y, m, d, h, min, 0)
            .single()
            .expect("时刻有效")
    }

    #[test]

    fn random_span_stays_in_plausible_past_hours() {

        // Cover early morning (today contributes nothing), midday, and late
        // evening (today is capped at 22:00).
        for now in [
            beijing(2026, 3, 10, 7, 30),
            beijing(2026, 3, 10, 13, 20),
            beijing(2026, 3, 10, 23, 50),
        ] {

            let earliest_day = (now - chrono::Duration::days(2)).date_naive();

            for _ in 0..500 {

                let span = random_span(now);

                assert_eq!(span.end - span.start, chrono::Duration::hours(1));

                assert!(span.end <= now, "窗口不能晚于当前: {span:?} now={now}");

                assert_eq!(span.start.minute(), 0, "应对齐整点");

                assert!(span.start.hour() >= 8, "不早于 08:00: {span:?}");

                assert!(
                    span.end.hour() <= 22 && span.start.date_naive() == span.end.date_naive(),
                    "不晚于 22:00: {span:?}"
                );

                assert!(
                    span.start.date_naive() >= earliest_day,
                    "只覆盖近三天: {span:?}"
                );
            }
        }
    }

    #[test]

    fn generated_photo_is_a_png() {

        let photo = ClockinPhoto::generated().expect("应能生成占位图");

        assert!(photo.bytes.starts_with(b"\x89PNG\r\n\x1a\n"));

        assert!(photo.file_name.ends_with(".png"));
    }

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
