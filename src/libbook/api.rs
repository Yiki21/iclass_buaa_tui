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

/// User agent the service expects.
///
/// Why:
/// It answers requests from a browser-like client and appears to reject some
/// others outright, so the value matches what the site itself is served to.

const USER_AGENT: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, \
                          like Gecko) Chrome/134.0.0.0 Safari/537.36";

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

impl IClassApi {
    /// Sends an authenticated POST to the booking service.
    ///
    /// How:
    /// The bearer token is prefixed without a space, matching what the service
    /// expects (`bearer<token>`), and business failures are read from `code`
    /// rather than from the HTTP status.

    /// A client that additionally trusts the library's missing intermediate.
    ///
    /// Why:
    /// The relaxation is scoped to this host rather than applied globally, so
    /// no other service is affected by the library's broken chain.

    fn libbook_client(&self) -> Result<reqwest::Client> {

        let certificate = reqwest::Certificate::from_pem(LIBRARY_INTERMEDIATE_PEM)
            .context("内置的图书馆中间证书无法解析")?;

        reqwest::Client::builder()
            // The shared cookie jar is required: the CAS handshake deposits the
            // session cookies the login POST must present, and a fresh jar
            // would arrive without them.
            .cookie_provider(self.session_cookie_jar())
            .user_agent(USER_AGENT)
            .add_root_certificate(certificate)
            .build()
            .context("构建图书馆专用 HTTP 客户端失败")
    }

    async fn libbook_post(&self, path: &str, body: Value, token: Option<&str>) -> Result<Value> {

        let url = libbook_url(self.use_vpn, &format!("{BASE_URL}/v4/{path}"));

        // The service rejects requests that do not look like its own XHR:
        // without these headers the login answers `{"code":1,"message":
        // "操作失败"}` even with a valid ticket.
        let mut request = self
            .libbook_client()?
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

        let value: Value = serde_json::from_str(&body_text).with_context(|| {

            let preview: String = body_text.chars().take(120).collect();

            format!("图书馆返回了非 JSON 响应: {preview}")
        })?;

        // 401 or an explicit login error means the token must be renewed.
        if status == 401 {

            bail!("图书馆登录状态已失效，请重新登录");
        }

        if let Some(code) = value.get("code").and_then(Value::as_i64)
            && !matches!(code, 0 | 1)
        {

            let message = value
                .get("message")
                .or_else(|| value.get("msg"))
                .and_then(Value::as_str)
                .unwrap_or("图书馆接口请求失败");

            bail!("{message}");
        }

        Ok(value)
    }

    /// Exchanges the SSO session for a library bearer token.

    pub async fn libbook_login(&self) -> Result<String> {

        let ticket = self.libbook_cas_ticket().await?;

        if std::env::var_os("ICLASS_LIBBOOK_DEBUG").is_some() {

            eprintln!("图书馆 CAS ticket 长度: {}", ticket.len());
        }

        // The ticket arrives percent-encoded from the redirect; the endpoint
        // expects the decoded value.
        let decoded = percent_decode(&ticket);

        let response = self
            .libbook_post("login/user", json!({ "cas": decoded }), None)
            .await
            .context("图书馆登录失败")?;

        if std::env::var_os("ICLASS_LIBBOOK_DEBUG").is_some() {

            let full = serde_json::to_string(&response).unwrap_or_default();

            eprintln!("图书馆登录响应: {}", &full[..full.len().min(600)]);
        }

        response
            .pointer("/data/member/token")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("图书馆登录成功但未返回 token"))
    }

    /// Walks the CAS chain and returns the ticket from the redirect URL.
    ///
    /// Why:
    /// The service authenticates by ticket, not by password. Following the
    /// redirect chain with the shared session is what produces one.

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

            let url = response.url().to_string();

            if let Some(ticket) = extract_cas_ticket(&url) {

                return Ok(ticket);
            }

            let Some(location) = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
            else {

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

            current = reqwest::Url::parse(&url)
                .and_then(|base| base.join(location))
                .map(|next| next.to_string())
                .context("解析图书馆 CAS 跳转失败")?;

            if let Some(ticket) = extract_cas_ticket(&current) {

                return Ok(ticket);
            }
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
                    free_num:  row.get("freeNum").and_then(flexible_i64).unwrap_or(0),
                    total_num: row.get("totalNum").and_then(flexible_i64).unwrap_or(0),
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
                    free_num:  row.get("freeNum").and_then(flexible_i64).unwrap_or(0),
                    total_num: row.get("totalNum").and_then(flexible_i64).unwrap_or(0),
                })
            })
            .collect())
    }

    /// Loads an area's available dates and time segments.

    pub async fn libbook_area_detail(&self, token: &str, area_id: &str) -> Result<AreaDetail> {

        let response = self
            .libbook_post("Space/map", json!({ "id": area_id }), Some(token))
            .await?;

        let data = response.get("data").cloned().unwrap_or(Value::Null);

        let available_dates = data
            .get("availableDates")
            .and_then(Value::as_array)
            .map(|rows| {

                rows.iter()
                    .filter_map(|value| value.as_str().map(str::to_string))
                    .collect()
            })
            .unwrap_or_default();

        let time_slots = data
            .get("timeSlots")
            .and_then(Value::as_array)
            .map(|rows| {

                rows.iter()
                    .filter_map(|row| {

                        let start = row
                            .get("start")
                            .and_then(flexible_string)
                            .unwrap_or_default();

                        let end = row.get("end").and_then(flexible_string).unwrap_or_default();

                        Some(TimeSlot {
                            id: row.get("id").and_then(flexible_string)?,
                            label: row
                                .get("label")
                                .and_then(flexible_string)
                                .unwrap_or_else(|| format!("{start}-{end}")),
                            start,
                            end,
                        })
                    })
                    .collect()
            })
            .unwrap_or_default();

        Ok(AreaDetail {
            id: area_id.to_string(),
            name: data
                .get("name")
                .and_then(flexible_string)
                .unwrap_or_default(),
            available_dates,
            time_slots,
        })
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

        Ok(rows
            .iter()
            .filter_map(|row| {

                let status = row
                    .get("status")
                    .and_then(flexible_string)
                    .unwrap_or_default();

                Some(Seat {
                    id:           row.get("id").and_then(flexible_string)?,
                    name:         row
                        .get("name")
                        .and_then(flexible_string)
                        .unwrap_or_default(),
                    no:           row.get("no").and_then(flexible_string).unwrap_or_default(),
                    status_name:  row
                        .get("statusName")
                        .and_then(flexible_string)
                        .unwrap_or_default(),
                    // The service encodes availability as "1"; the explicit
                    // flag is preferred when present.
                    is_available: row
                        .get("isAvailable")
                        .and_then(Value::as_bool)
                        .unwrap_or(status == "1"),
                })
            })
            .collect())
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

        // The list may sit under `data.list` or be the whole `data`.
        let rows = data_list(&response, "list")
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
    /// then posted as `aesjson`.

    pub async fn libbook_reserve(
        &self,
        token: &str,
        seat_id: &str,
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

        Ok(response
            .get("data")
            .and_then(parse_booking)
            .unwrap_or_default())
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

fn libbook_url(use_vpn: bool, raw: &str) -> String {

    if use_vpn {

        to_webvpn_url(raw)
    } else {

        raw.to_string()
    }
}

/// Decodes percent escapes, leaving other characters alone.
///
/// Why:
/// The ticket comes back percent-encoded in the redirect URL. Posting the
/// encoded form makes the service reject an otherwise valid ticket.

fn percent_decode(value: &str) -> String {

    let bytes = value.as_bytes();

    let mut output = Vec::with_capacity(bytes.len());

    let mut index = 0;

    while index < bytes.len() {

        if bytes[index] == b'%' && index + 2 < bytes.len() {

            let hex = &value[index + 1..index + 3];

            if let Ok(byte) = u8::from_str_radix(hex, 16) {

                output.push(byte);

                index += 3;

                continue;
            }
        }

        output.push(bytes[index]);

        index += 1;
    }

    String::from_utf8_lossy(&output).into_owned()
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

        let preview: String = body.chars().take(80).collect();

        return format!("无法识别（{}）", preview.replace('\n', " "));
    }

    clues.join(", ")
}

fn extract_cas_ticket(url: &str) -> Option<String> {

    let parsed = reqwest::Url::parse(url).ok()?;

    parsed
        .query_pairs()
        .find(|(key, _)| key == "cas" || key == "ticket")
        .map(|(_, value)| value.to_string())
        .filter(|value| !value.is_empty())
        .or_else(|| {

            // Fall back to a plain text search, but only for a query-style
            // parameter: the SSO service URL itself contains `login%2Fcas`, so
            // a bare `cas=` substring match returns that path fragment as if it
            // were a ticket, and the login then fails with an opaque
            // "操作失败".
            ["?cas=", "&cas=", "?ticket=", "&ticket="]
                .iter()
                .find_map(|marker| {

                    let rest = url.split(marker).nth(1)?;

                    let value: String = rest
                        .chars()
                        .take_while(|c| !matches!(c, '&' | '#'))
                        .collect();

                    (!value.is_empty()).then_some(value)
                })
        })
}

fn data_list(response: &Value, key: &str) -> Option<Vec<Value>> {

    response
        .get("data")?
        .get(key)?
        .as_array()
        .cloned()
        .or_else(|| response.get("data").and_then(Value::as_array).cloned())
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

fn parse_booking(row: &Value) -> Option<Booking> {

    Some(Booking {
        id:          row.get("id").and_then(flexible_string)?,
        area_name:   row
            .get("areaName")
            .or_else(|| row.get("nameMerge"))
            .and_then(flexible_string)
            .unwrap_or_default(),
        seat_no:     row
            .get("seatNo")
            .and_then(flexible_string)
            .unwrap_or_default(),
        day:         row.get("day").and_then(flexible_string).unwrap_or_default(),
        begin_time:  row
            .get("beginTime")
            .and_then(flexible_string)
            .unwrap_or_default(),
        end_time:    row
            .get("endTime")
            .and_then(flexible_string)
            .unwrap_or_default(),
        status_name: row
            .get("statusName")
            .and_then(flexible_string)
            .unwrap_or_default(),
    })
}

#[cfg(test)]

mod tests {

    use super::{extract_cas_ticket, parse_booking};
    use serde_json::json;

    #[test]

    fn reads_the_cas_ticket_from_a_redirect_url() {

        // The booking service names this parameter `cas`, not `ticket`.
        let url = "https://booking.lib.buaa.edu.cn/v4/login/cas?cas=ST-123-abc";

        assert_eq!(extract_cas_ticket(url).as_deref(), Some("ST-123-abc"));

        // The standard name still works, for robustness.
        assert_eq!(
            extract_cas_ticket("https://x/v4/login/cas?ticket=ST-9").as_deref(),
            Some("ST-9")
        );

        // Missing or empty must not be treated as success.
        assert!(extract_cas_ticket("https://booking.lib.buaa.edu.cn/v4/login/cas").is_none());

        assert!(
            extract_cas_ticket("https://x/?cas=").is_none(),
            "空 cas 不应视为成功"
        );

        // The SSO entry URL embeds the service path, which contains
        // "login/cas"; a loose substring search returns that fragment as the
        // ticket and the login then fails with an opaque "操作失败".
        assert!(
            extract_cas_ticket(
                "https://sso.buaa.edu.cn/login?service=https%3A%2F%2Fbooking.lib.buaa.edu.cn%2Fv4%2Flogin%2Fcas"
            )
            .is_none(),
            "服务地址不应被当成 ticket"
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
}
