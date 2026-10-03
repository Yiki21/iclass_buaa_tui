//! Library seat client for `booking.lib.buaa.edu.cn`.
//!
//! Why:
//! Seat booking needs its own login: the shared SSO session is exchanged
//! through CAS for a bearer token, which every later call carries. Reservations
//! additionally have to be sent as an encrypted payload.
//!
//! How:
//! The CAS handshake follows redirects without a browser and reads the `cas`
//! ticket out of the final URL, which is then posted to `login/user`.

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::crypto::{EncryptedReserveBody, encrypt_reserve};
use crate::constants::to_webvpn_url;
use crate::iclass::IClassApi;

const BASE_URL: &str = "https://booking.lib.buaa.edu.cn";

/// Intermediate certificate the library server fails to send.
///
/// Why:
/// `booking.lib.buaa.edu.cn` presents only its leaf certificate, with no
/// intermediate and no AIA URL to fetch one from, so no standard client can
/// build a chain: `curl` fails with "unable to get local issuer certificate"
/// and rustls reports `UnknownIssuer`. The intermediate is not in the system
/// trust store either, and installing it there needs root.
///
/// How:
/// The missing intermediate is bundled and trusted explicitly for this one
/// host. Verification is not disabled — the chain is still fully validated,
/// just against a store that contains the certificate the server should have
/// sent. The reference implementation takes the same approach, scoped to the
/// library client.

const LIBRARY_INTERMEDIATE_PEM: &[u8] =
    include_bytes!("../../assets/certs/globalsign-gcc-r3-dv-tls-ca-2020.pem");

/// CAS entry point that yields a ticket for the booking service.

const CAS_LOGIN_URL: &str =
    "https://sso.buaa.edu.cn/login?service=https%3A%2F%2Fbooking.lib.buaa.edu.cn%2Fv4%2Flogin%2Fcas";

/// A library building.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct Library {
    pub id:        String,
    pub name:      String,
    pub free_num:  i64,
    pub total_num: i64,
}

/// A floor within a library.
///
/// Part of the upstream model; the availability list reports floors nested
/// inside a library, which this client flattens, so it is not read yet.

#[allow(dead_code)]
#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct Storey {
    pub id:        String,
    pub name:      String,
    pub free_num:  i64,
    pub total_num: i64,
}

/// A bookable area (reading room) inside a library.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct Area {
    pub id:        String,
    pub name:      String,
    pub free_num:  i64,
    pub total_num: i64,
}

/// A selectable time segment for a seat.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct TimeSlot {
    pub id:    String,
    pub start: String,
    pub end:   String,
    pub label: String,
}

/// An area's available dates and segments.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct AreaDetail {
    pub id:              String,
    pub name:            String,
    pub available_dates: Vec<String>,
    pub time_slots:      Vec<TimeSlot>,
}

/// A seat and whether it can be taken.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct Seat {
    pub id:           String,
    pub name:         String,
    pub no:           String,
    pub status_name:  String,
    pub is_available: bool,
}

/// One of the caller's seat reservations.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct Booking {
    pub id:          String,
    pub area_name:   String,
    pub seat_no:     String,
    pub day:         String,
    pub begin_time:  String,
    pub end_time:    String,
    pub status_name: String,
}

pub(crate) fn library_intermediate_certificate() -> &'static [u8] {

    LIBRARY_INTERMEDIATE_PEM
}

impl IClassApi {
    /// Sends an authenticated POST to the booking service.
    ///
    /// The bearer token is prefixed without a space, matching what the service
    /// expects (`bearer<token>`).

    async fn libbook_post(&self, path: &str, body: Value, token: Option<&str>) -> Result<Value> {

        let url = libbook_url(self.use_vpn, &format!("{BASE_URL}/v4/{path}"));

        // The service rejects requests that do not look like its own XHR:
        // without these headers the login answers `{"code":1,"message":
        // "操作失败"}` even with a valid ticket.
        let mut request = self
            .library_client()
            .post(&url)
            .header("Accept", "application/json, text/plain, */*")
            .header("X-Requested-With", "XMLHttpRequest")
            .header("Referer", libbook_url(self.use_vpn, BASE_URL))
            .header("Origin", libbook_url(self.use_vpn, BASE_URL))
            .json(&body);

        if let Some(token) = token {

            request = request.header("Authorization", format!("bearer{token}"));
        }

        let response = request
            .send()
            .await
            .with_context(|| format!("图书馆请求失败: {path}"))?;

        let status = response.status().as_u16();

        let body_text = response.text().await.context("读取图书馆响应失败")?;

        let result = validate_library_response(status, &body_text, path);

        if result.is_err() && token.is_some() {

            *self.libbook_token.lock().await = None;
        }

        result
    }

    /// Exchanges the SSO session for a library bearer token.

    pub async fn libbook_login(&self) -> Result<String> {

        // Holding this per-API lock coalesces concurrent token refreshes.
        let mut cached = self.libbook_token.lock().await;

        if let Some(token) = cached.as_ref().and_then(crate::iclass::CachedToken::valid) {

            return Ok(token);
        }

        *cached = None;

        let ticket = self.libbook_cas_ticket().await?;

        if std::env::var_os("ICLASS_LIBBOOK_DEBUG").is_some() {

            eprintln!("图书馆 CAS ticket 长度: {}", ticket.len());
        }

        // The ticket arrives percent-encoded from the redirect; the endpoint
        // expects the decoded value.
        let decoded = ticket;

        let response = self
            .libbook_post("login/user", json!({ "cas": decoded }), None)
            .await
            .context("图书馆登录失败")?;

        let token = response
            .pointer("/data/member/token")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("图书馆登录成功但未返回 token"))?;

        if std::env::var_os("ICLASS_LIBBOOK_DEBUG").is_some() {

            eprintln!("图书馆登录响应字段: token_present=true");
        }

        *cached = Some(crate::iclass::CachedToken::new(token.to_string()));

        Ok(token.to_string())
    }

    /// Walks the CAS chain and returns the service-issued `cas` token.
    ///
    /// Why the chain has to be walked to the end:
    /// The shared SSO session answers the first hop with a redirect carrying a
    /// *service ticket* (`?ticket=ST-...`). That value is not what the login
    /// endpoint accepts: it has to be presented back to the service's own
    /// callback (`/v4/login/cas`), which validates it against SSO and then
    /// redirects to the frontend with its own `?cas=`. Posting the service
    /// ticket to `login/user` instead is answered with
    /// `{"code":1,"message":"操作失败"}`, which reads like an auth failure
    /// rather than a missed hop.

    async fn libbook_cas_ticket(&self) -> Result<String> {

        let mut current = libbook_url(self.use_vpn, CAS_LOGIN_URL);

        for _ in 0..8 {

            let response = self
                .no_redirect_client
                .get(&current)
                .header("Referer", libbook_url(self.use_vpn, BASE_URL))
                .send()
                .await
                .context("图书馆 CAS 跳转失败")?;

            let status = response.status();

            if !status.is_success() && !status.is_redirection() {

                bail!("图书馆 CAS 请求失败: HTTP {status}");
            }

            // Only `cas` counts here: a `ticket` is the SSO service ticket, an
            // intermediate value rather than a credential.
            let url = response.url().to_string();

            if let Some(ticket) = extract_cas_token(&url) {

                return Ok(ticket);
            }

            // The fragment is not transmitted in HTTP, but it appears in the
            // Location header value and can be parsed there.
            let location_header = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok());

            if let Some(location_value) = location_header {

                if let Some(ticket) = extract_cas_token(location_value) {

                    return Ok(ticket);
                }
            }

            let Some(location) = location_header else {

                // Report what the SSO page actually is: a login form here means
                // the shared session is not authenticated for this service,
                // which is a different problem from a missing redirect.
                let status = response.status();

                let body = response.text().await.unwrap_or_default();

                bail!(
                    "图书馆 CAS 未返回 ticket，请确认统一认证登录有效：HTTP {status}，                     页面线索: {}",
                    summarize_cas_page(&body)
                );
            };

            if !status.is_redirection() {

                bail!("图书馆 CAS 返回非跳转响应，未取得 ticket");
            }

            current = reqwest::Url::parse(&url)
                .and_then(|base| base.join(location))
                .map(|next| next.to_string())
                .context("解析图书馆 CAS 跳转失败")?;
        }

        bail!("图书馆 CAS 跳转次数过多，未取得 ticket")
    }

    /// Lists libraries with their free-seat counts.

    pub async fn libbook_libraries(&self, token: &str, day: &str) -> Result<Vec<Library>> {

        let response = self
            .libbook_post("space/pcTopFor", json!({ "day": day }), Some(token))
            .await?;

        let rows = data_list(&response, "list").unwrap_or_default();

        Ok(rows
            .iter()
            .filter_map(|row| {

                Some(Library {
                    id:        row.get("id").and_then(flexible_string)?,
                    name:      row
                        .get("name")
                        .and_then(flexible_string)
                        .unwrap_or_default(),
                    free_num:  field_i64(row, &["free_num", "freeNum"]),
                    total_num: field_i64(row, &["total_num", "totalNum"]),
                })
            })
            .collect())
    }

    /// Lists bookable areas inside a library.

    pub async fn libbook_areas(
        &self,
        token: &str,
        premises_id: &str,
        day: &str,
    ) -> Result<Vec<Area>> {

        let response = self
            .libbook_post(
                "space/pick",
                json!({
                    "premisesIds": premises_id,
                    "categoryIds": [],
                    "storeyIds": [],
                    "boutiqueIds": [],
                    "date": day,
                }),
                Some(token),
            )
            .await?;

        let rows = data_list(&response, "area")
            .or_else(|| data_list(&response, "list"))
            .unwrap_or_default();

        Ok(rows
            .iter()
            .filter_map(|row| {

                Some(Area {
                    id:        row.get("id").and_then(flexible_string)?,
                    name:      row
                        .get("name")
                        .or_else(|| row.get("areaName"))
                        .and_then(flexible_string)
                        .unwrap_or_default(),
                    free_num:  field_i64(row, &["free_num", "freeNum"]),
                    total_num: field_i64(row, &["total_num", "totalNum"]),
                })
            })
            .collect())
    }

    /// Loads an area's available dates and time segments.

    pub async fn libbook_area_detail(&self, token: &str, area_id: &str) -> Result<AreaDetail> {

        let response = self
            .libbook_post("Space/map", json!({ "id": area_id }), Some(token))
            .await?;

        Ok(parse_area_detail(area_id, &response))
    }

    /// Lists seats in an area for a segment, with availability.

    pub async fn libbook_seats(
        &self,
        token: &str,
        area_id: &str,
        day: &str,
        start: &str,
        end: &str,
    ) -> Result<Vec<Seat>> {

        let response = self
            .libbook_post(
                "Space/seat",
                json!({
                    "id": area_id,
                    "day": day,
                    "label_id": [],
                    "start_time": start,
                    "end_time": end,
                    "begdate": "",
                    "enddate": "",
                }),
                Some(token),
            )
            .await?;

        let rows = data_list(&response, "list").unwrap_or_default();

        Ok(rows.iter().filter_map(parse_seat).collect())
    }

    /// Lists the caller's seat reservations.

    pub async fn libbook_bookings(
        &self,
        token: &str,
        page: i64,
        limit: i64,
    ) -> Result<Vec<Booking>> {

        let response = self
            .libbook_post(
                "member/seat",
                json!({ "type": "1", "page": page, "limit": limit }),
                Some(token),
            )
            .await?;

        // The list may sit under `data.list`, `data.data`, or be the whole `data`.
        let rows = data_list(&response, "list")
            .or_else(|| data_list(&response, "data"))
            .or_else(|| response.get("data").and_then(Value::as_array).cloned())
            .unwrap_or_default();

        Ok(rows.iter().filter_map(parse_booking).collect())
    }

    /// Reserves one seat.
    ///
    /// Why:
    /// This claims a physical seat. The caller must have confirmed explicitly;
    /// see the CLI layer, which refuses without `--yes`.
    ///
    /// How:
    /// The payload is encrypted with a key derived from the reservation date,
    /// then posted as `aesjson`. A successful reply may omit the booking, so
    /// when it carries no id the booking list is read back and the new
    /// reservation is found by seat number and day.

    pub async fn libbook_reserve(
        &self,
        token: &str,
        seat_id: &str,
        seat_no: &str,
        segment: &str,
        day: &str,
    ) -> Result<Booking> {

        let encrypted = encrypt_reserve(&EncryptedReserveBody {
            seat_id:    seat_id.to_string(),
            segment:    segment.to_string(),
            day:        day.to_string(),
            start_time: String::new(),
            end_time:   String::new(),
        })?;

        let response = self
            .libbook_post(
                "space/confirm",
                json!({ "aesjson": encrypted }),
                Some(token),
            )
            .await
            .context("预约座位失败")?;

        if let Some(booking) = reserved_booking(&response).filter(|b| !b.id.is_empty()) {

            return Ok(booking);
        }

        // The service said yes without naming the booking. Read it back; the
        // write already happened, so a failed read must not look like a
        // refusal that invites a retry.
        let bookings = self
            .libbook_bookings(token, 1, 20)
            .await
            .context("图书馆已接受预约，但读取预约记录失败，请稍后查看预约记录，勿直接重试")?;

        find_new_booking(&bookings, seat_no, day).ok_or_else(|| {

            anyhow!(
                "图书馆已接受预约，但预约记录中未找到座位 {seat_no}，请查看预约记录，勿直接重试"
            )
        })
    }

    /// Cancels a seat reservation.

    pub async fn libbook_cancel(&self, token: &str, booking_id: &str) -> Result<()> {

        let _ = self
            .libbook_post("space/cancel", json!({ "id": booking_id }), Some(token))
            .await
            .context("取消座位预约失败")?;

        Ok(())
    }
}

fn validate_library_response(status: u16, body: &str, path: &str) -> Result<Value> {

    if status == 401 || status == 403 {

        bail!("图书馆登录状态已失效，请重新登录");
    }

    if !(200..300).contains(&status) {

        bail!("图书馆请求失败: HTTP {status}");
    }

    let value: Value = serde_json::from_str(body)
        .with_context(|| format!("图书馆返回了非 JSON 响应: bytes={}", body.len()))?;

    let code = value.get("code").and_then(flexible_i64);

    let message = value
        .get("message")
        .or_else(|| value.get("msg"))
        .and_then(Value::as_str)
        .unwrap_or_default();

    let is_write = matches!(path, "space/confirm" | "space/cancel");

    // Words the service uses to refuse a write while still answering code 1;
    // the reference client treats the same list as failure.
    const WRITE_REFUSALS: [&str; 8] = [
        "不可",
        "已被",
        "不能取消",
        "无法取消",
        "已取消",
        "用户取消",
        "已结束",
        "已完成",
    ];

    if value.get("success") == Some(&Value::Bool(false))
        || value.get("success").and_then(Value::as_str) == Some("false")
        || message.contains("失败")
        || (is_write && WRITE_REFUSALS.iter().any(|word| message.contains(word)))
        || code.is_some_and(|code| !matches!(code, 0 | 1))
    {

        if is_write && !message.is_empty() {

            bail!("图书馆拒绝了请求：{message}");
        }

        bail!("图书馆接口明确报告失败（code={code:?}）");
    }

    let has_rows = |keys: &[&str]| keys.iter().any(|key| data_list(&value, key).is_some());

    let valid = match path {
        "login/user" => {
            value
                .pointer("/data/member/token")
                .and_then(Value::as_str)
                .is_some_and(|v| !v.trim().is_empty())
        }
        "space/pcTopFor" => has_rows(&["list"]),
        "space/pick" => has_rows(&["area", "list"]),
        // Upstream nests segments as `data.date.list[].times`; the flat
        // `data.timeSlots` form is kept for older responses.
        "Space/map" => {
            value
                .pointer("/data/date/list")
                .is_some_and(Value::is_array)
                || value
                    .pointer("/data/timeSlots")
                    .is_some_and(Value::is_array)
        }
        "Space/seat" => has_rows(&["list"]),
        "member/seat" => has_rows(&["list", "data"]),
        // The confirm reply does not reliably carry the booking: a successful
        // reservation can answer with only `code`/`message`. A business-success
        // envelope with an affirmative code passes here; the caller then proves
        // the booking exists by reading it back when no id came with it.
        // Cancellation has no verified success envelope yet.
        "space/confirm" => code.is_some() || reserved_booking(&value).is_some(),
        _ => false,
    };

    if !valid {

        if matches!(path, "space/confirm" | "space/cancel") {

            bail!("图书馆写入结果未知，请查询预约记录确认，勿直接重试");
        }

        bail!("图书馆响应缺少预期数据，不能确认请求成功");
    }

    Ok(value)
}

fn libbook_url(use_vpn: bool, raw: &str) -> String {

    if use_vpn {

        to_webvpn_url(raw)
    } else {

        raw.to_string()
    }
}

/// Pulls the CAS ticket out of a URL.
///
/// Why the parameter is called `cas`:
/// The booking service does not use the standard `ticket` name for the value it
/// passes to its login endpoint. Looking for `ticket` finds nothing, the login
/// is posted without a valid ticket, and the service answers
/// `{"code":1,"message":"操作失败"}` — which looks like an auth failure rather
/// than a parsing mistake.
///
/// The value also arrives URL-encoded and possibly inside a WebVPN-wrapped URL,
/// so both the decoded form and the raw string are checked.

/// Short description of what a CAS page contains, for error messages.

fn summarize_cas_page(body: &str) -> String {

    let lower = body.to_ascii_lowercase();

    let mut clues = Vec::new();

    if lower.contains("name=\"execution\"") {

        clues.push("登录表单");
    }

    if body.contains("统一身份认证") {

        clues.push("统一身份认证");
    }

    if lower.contains("captcha") {

        clues.push("验证码");
    }

    if body.contains("锁定") || lower.contains("locked") {

        clues.push("账号锁定");
    }

    if clues.is_empty() {

        return format!("无法识别（bytes={}）", body.len());
    }

    clues.join(", ")
}

/// Pulls the service-issued `cas` token out of a URL.
///
/// Why only `cas`:
/// The booking service does not use the standard `ticket` name for the value
/// its login endpoint accepts. SSO's `?ticket=ST-...` is a service ticket that
/// still has to be redeemed at the service's own `/v4/login/cas` callback;
/// accepting it here ended the walk one hop early and posted a value the login
/// endpoint rejects with `{"code":1,"message":"操作失败"}`.
///
/// Why the fragment is read too:
/// The callback's final redirect carries the token inside a single-page route
/// (`.../h5/index.html#/cas/?cas=<token>`), and a fragment is never sent to the
/// server. `query_pairs` only sees the part before `#`, so the token would be
/// missed and the walk would end on a page that looks like a dead end.

fn extract_cas_token(url: &str) -> Option<String> {

    let parsed = reqwest::Url::parse(url).ok()?;

    token_from_query_pairs(&parsed)
        .or_else(|| token_from_query_pairs_in_fragment(parsed.fragment().unwrap_or_default()))
}

fn token_from_query_pairs(parsed: &reqwest::Url) -> Option<String> {

    parsed
        .query_pairs()
        .find(|(key, _)| key == "cas")
        .map(|(_, value)| value.to_string())
        .filter(|value| !value.is_empty())
}

/// Reads `cas` out of a fragment such as `/cas/?cas=abc`.

fn token_from_query_pairs_in_fragment(fragment: &str) -> Option<String> {

    let query = fragment.split_once('?').map(|(_, rest)| rest)?;

    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .find(|(key, _)| *key == "cas")
        .map(|(_, value)| value.to_string())
        .filter(|value| !value.is_empty())
}

fn data_list(response: &Value, key: &str) -> Option<Vec<Value>> {

    let data = response.get("data")?;

    data.get(key)
        .and_then(Value::as_array)
        .or_else(|| data.as_array())
        .cloned()
}

fn flexible_i64(value: &Value) -> Option<i64> {

    value
        .as_i64()
        .or_else(|| value.as_str().and_then(|text| text.trim().parse().ok()))
}

fn flexible_string(value: &Value) -> Option<String> {

    value
        .as_str()
        .map(str::to_string)
        .or_else(|| value.as_i64().map(|number| number.to_string()))
}

/// Reads the first present key as a string.
///
/// The service mixes snake_case (`free_num`, `status_name`) and camelCase
/// across endpoints and versions, so each field lists every spelling seen.

fn field_string(row: &Value, keys: &[&str]) -> String {

    keys.iter()
        .filter_map(|key| row.get(*key).and_then(flexible_string))
        .find(|text| !text.is_empty())
        .unwrap_or_default()
}

fn field_i64(row: &Value, keys: &[&str]) -> i64 {

    keys.iter()
        .find_map(|key| row.get(*key).and_then(flexible_i64))
        .unwrap_or(0)
}

fn parse_time_slot(row: &Value) -> Option<TimeSlot> {

    let id = row.get("id").and_then(flexible_string)?;

    let start = field_string(row, &["start", "start_time", "beginTime"]);

    let end = field_string(row, &["end", "end_time", "endTime"]);

    let label = field_string(row, &["label"]);

    Some(TimeSlot {
        id,
        label: if label.is_empty() {

            format!("{start}-{end}")
        } else {

            label
        },
        start,
        end,
    })
}

/// Parses `Space/map`.
///
/// Upstream shape: `data.area{id,name}` and `data.date.list[{day, times[]}]`.
/// Segments come from the first listed day, matching the UBAA client. The
/// older flat `data.availableDates` / `data.timeSlots` form is still accepted.

fn parse_area_detail(area_id: &str, response: &Value) -> AreaDetail {

    let data = response.get("data").unwrap_or(&Value::Null);

    let area = data.get("area").filter(|value| value.is_object());

    let days = data
        .pointer("/date/list")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();

    let mut available_dates: Vec<String> = days
        .iter()
        .map(|day| field_string(day, &["day", "date"]))
        .filter(|day| !day.is_empty())
        .collect();

    let mut time_slots: Vec<TimeSlot> = days
        .first()
        .and_then(|day| day.get("times"))
        .and_then(Value::as_array)
        .map(|rows| rows.iter().filter_map(parse_time_slot).collect())
        .unwrap_or_default();

    if available_dates.is_empty() {

        available_dates = data
            .get("availableDates")
            .and_then(Value::as_array)
            .map(|rows| rows.iter().filter_map(flexible_string).collect())
            .unwrap_or_default();
    }

    if time_slots.is_empty() {

        time_slots = data
            .get("timeSlots")
            .and_then(Value::as_array)
            .map(|rows| rows.iter().filter_map(parse_time_slot).collect())
            .unwrap_or_default();
    }

    let id = area
        .map(|area| field_string(area, &["id"]))
        .unwrap_or_default();

    let name = area
        .map(|area| field_string(area, &["name"]))
        .filter(|name| !name.is_empty())
        .unwrap_or_else(|| field_string(data, &["name"]));

    AreaDetail {
        id: if id.is_empty() {

            area_id.to_string()
        } else {

            id
        },
        name,
        available_dates,
        time_slots,
    }
}

fn parse_seat(row: &Value) -> Option<Seat> {

    let status = field_string(row, &["status"]);

    Some(Seat {
        id:           row.get("id").and_then(flexible_string)?,
        name:         field_string(row, &["name"]),
        no:           field_string(row, &["no"]),
        status_name:  field_string(row, &["status_name", "statusName"]),
        // The service encodes availability as "1"; the explicit flag is
        // preferred when present.
        is_available: row
            .get("isAvailable")
            .and_then(Value::as_bool)
            .unwrap_or(status == "1"),
    })
}

/// Reads the booking returned by `space/confirm`.
///
/// Upstream nests it as `data.bookInfo` (or `data.booking`); a flat `data`
/// row is accepted as a fallback.

/// Finds the reservation just made among the caller's bookings.
///
/// Why seat number and day:
/// The confirm reply may carry no id, and the seat id sent is not echoed in
/// the list. Seat number plus day identifies one seat on one date; the day is
/// compared on its date part because the list may add a time.

fn find_new_booking(bookings: &[Booking], seat_no: &str, day: &str) -> Option<Booking> {

    let seat_no = seat_no.trim();

    bookings
        .iter()
        .find(|booking| {

            booking.seat_no.trim() == seat_no
                && (booking.day.is_empty() || booking.day.starts_with(day))
        })
        .cloned()
}

fn reserved_booking(response: &Value) -> Option<Booking> {

    let data = response.get("data")?;

    data.get("bookInfo")
        .or_else(|| data.get("booking"))
        .filter(|value| value.is_object())
        .and_then(parse_booking)
        .or_else(|| parse_booking(data))
}

fn parse_booking(row: &Value) -> Option<Booking> {

    Some(Booking {
        id:          row.get("id").and_then(flexible_string)?,
        area_name:   field_string(
            row,
            &["areaName", "nameMerge", "name_merge", "area_name", "name"],
        ),
        seat_no:     field_string(row, &["seatNo", "seat_no", "no"]),
        day:         field_string(row, &["day", "date"]),
        begin_time:  field_string(row, &["beginTime", "begin_time"]),
        end_time:    field_string(row, &["endTime", "end_time"]),
        status_name: field_string(row, &["statusName", "status_name"]),
    })
}

#[cfg(test)]

mod tests {

    use super::{
        Booking, data_list, extract_cas_token, field_i64, find_new_booking, parse_area_detail,
        parse_booking, parse_seat, reserved_booking, validate_library_response,
    };

    #[test]

    fn reservation_success_without_booking_is_accepted() {

        // A real successful confirm reply that names no booking must not be
        // reported as an unknown write.
        for body in [
            r#"{"code":1,"message":"预约成功"}"#,
            r#"{"code":1,"message":"操作成功","data":[]}"#,
            r#"{"code":0,"msg":"ok","data":null}"#,
        ] {

            let value = validate_library_response(200, body, "space/confirm")
                .unwrap_or_else(|error| panic!("{body}: {error:#}"));

            assert!(reserved_booking(&value).is_none_or(|b| b.id.is_empty()));
        }
    }

    #[test]

    fn reservation_refusal_reports_the_service_message() {

        let body = r#"{"code":1,"message":"该座位已被预约"}"#;

        let error = validate_library_response(200, body, "space/confirm").unwrap_err();

        assert!(format!("{error:#}").contains("该座位已被预约"));
    }

    #[test]

    fn finds_the_new_booking_by_seat_and_day() {

        let booking = |id: &str, no: &str, day: &str| {

            Booking {
                id: id.to_string(),
                seat_no: no.to_string(),
                day: day.to_string(),
                ..Booking::default()
            }
        };

        let bookings = [
            booking("1", "495", "2026-10-02"),
            booking("2", "494", "2026-10-03"),
            booking("3", "495", "2026-10-03 08:00"),
        ];

        assert_eq!(
            find_new_booking(&bookings, "495", "2026-10-03").map(|b| b.id),
            Some("3".to_string())
        );

        assert!(find_new_booking(&bookings, "496", "2026-10-03").is_none());
    }

    use serde_json::json;

    #[test]

    fn reads_the_cas_token_from_the_service_callback() {

        // The booking service names this parameter `cas`, not `ticket`.
        let url = "https://booking.lib.buaa.edu.cn/v4/login/cas?cas=abc123";

        assert_eq!(extract_cas_token(url).as_deref(), Some("abc123"));

        // The final redirect is a single-page route carrying the token in the
        // fragment, which is never transmitted to the server but can be parsed
        // from the Location header or the reqwest URL.
        let fragment_url = "https://booking.lib.buaa.edu.cn/h5/index.html#/cas/?cas=def456";

        assert_eq!(
            extract_cas_token(fragment_url).as_deref(),
            Some("def456"),
            "fragment 里的 cas 应该被识别"
        );

        // The SSO service ticket is an intermediate value. Accepting it here
        // ends the redirect walk one hop early and the login is answered with
        // an opaque "操作失败".
        assert!(
            extract_cas_token("https://x/v4/login/cas?ticket=ST-9").is_none(),
            "SSO 服务票据不是最终凭证"
        );

        // Missing or empty must not be treated as success.
        assert!(extract_cas_token("https://booking.lib.buaa.edu.cn/v4/login/cas").is_none());

        assert!(
            extract_cas_token("https://x/?cas=").is_none(),
            "空 cas 不应视为成功"
        );

        assert!(
            extract_cas_token("https://x/h5/#/cas/?cas=").is_none(),
            "fragment 里的空 cas 也不应视为成功"
        );

        // The SSO entry URL embeds the service path, which contains
        // "login/cas"; a loose substring search returns that fragment as the
        // token and the login then fails with an opaque "操作失败".
        assert!(
            extract_cas_token(
                "https://sso.buaa.edu.cn/login?service=https%3A%2F%2Fbooking.lib.buaa.edu.cn%2Fv4%2Flogin%2Fcas"
            )
            .is_none(),
            "服务地址不应被当成 cas"
        );
    }

    #[test]

    fn reads_a_booking_row() {

        let booking = parse_booking(&json!({
            "id": "b1",
            "areaName": "三层阅览区",
            "seatNo": "A-12",
            "day": "2026-03-10",
            "beginTime": "08:00",
            "endTime": "12:00",
            "statusName": "预约成功"
        }))
        .expect("应能解析预约");

        assert_eq!(booking.id, "b1");

        assert_eq!(booking.seat_no, "A-12");

        assert_eq!(booking.status_name, "预约成功");
    }

    #[test]

    fn booking_ids_may_arrive_as_numbers() {

        let booking = parse_booking(&json!({"id": 42})).expect("数字 id 也应可解析");

        assert_eq!(booking.id, "42");
    }

    // Fixture shapes below mirror UBAA's LocalLibBookApiBackendTest, which
    // records the live service's snake_case and nested envelopes.

    #[test]

    fn library_counts_are_snake_case() {

        let row = json!({"id": "9", "free_num": 12, "total_num": 100});

        assert_eq!(field_i64(&row, &["free_num", "freeNum"]), 12);

        assert_eq!(field_i64(&row, &["total_num", "totalNum"]), 100);
    }

    #[test]

    fn area_detail_reads_nested_date_list() {

        let body = r#"{"code":1,"data":{
            "area":{"id":"8","name":"一层西阅学空间"},
            "date":{"list":[{"day":"2026-05-08",
                "times":[{"id":"seg-1","start":"08:00","end":"23:00"}]}]}}}"#;

        let value = validate_library_response(200, body, "Space/map")
            .expect("嵌套 date.list 应被视为有效响应");

        let detail = parse_area_detail("8", &value);

        assert_eq!(detail.name, "一层西阅学空间");

        assert_eq!(detail.available_dates, vec!["2026-05-08"]);

        assert_eq!(detail.time_slots.len(), 1);

        assert_eq!(detail.time_slots[0].id, "seg-1");

        assert_eq!(detail.time_slots[0].label, "08:00-23:00");
    }

    #[test]

    fn area_detail_without_dates_is_rejected() {

        let body = r#"{"code":1,"data":{"area":{"id":"8"}}}"#;

        assert!(validate_library_response(200, body, "Space/map").is_err());
    }

    #[test]

    fn seat_reads_snake_case_status() {

        let seat = parse_seat(&json!({"id":"101","no":"101","status":"1","status_name":"空闲"}))
            .expect("座位应可解析");

        assert!(seat.is_available);

        assert_eq!(seat.status_name, "空闲");
    }

    #[test]

    fn bookings_list_under_data_data_is_valid() {

        let body = r#"{"code":1,"data":{"data":[{"id":"b1","nameMerge":"一层 / 101","status_name":"已预约"}],"total":1}}"#;

        let value = validate_library_response(200, body, "member/seat").expect("应有效");

        let rows = data_list(&value, "list")
            .or_else(|| data_list(&value, "data"))
            .unwrap();

        let booking = parse_booking(&rows[0]).unwrap();

        assert_eq!(booking.area_name, "一层 / 101");

        assert_eq!(booking.status_name, "已预约");
    }

    #[test]

    fn reservation_reads_book_info() {

        let body = r#"{"code":1,"message":"操作成功","data":{"bookInfo":{"id":"b1","no":"101"}}}"#;

        let value = validate_library_response(200, body, "space/confirm").expect("应有效");

        assert_eq!(reserved_booking(&value).unwrap().id, "b1");
    }
}
