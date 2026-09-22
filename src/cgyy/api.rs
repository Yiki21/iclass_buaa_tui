//! Seminar-room (研讨室) client for the CGYY service.
//!
//! Why:
//! Rooms are reserved through `cgyy.buaa.edu.cn`. Every request must be signed
//! (see [`super::signer`]) and writes must clear a slider captcha (see
//! [`super::captcha`]). Both are handled here so callers work in terms of rooms
//! and time slots.
//!
//! How:
//! Login exchanges the SSO cookie set by the shared session for a
//! `cgAuthorization` access token, which later calls carry as a header. A
//! request that comes back as a login redirect clears the token and retries
//! once, so an expired session does not surface as a confusing auth error.

use anyhow::{Context, Result, anyhow, bail};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use super::captcha::{CaptchaChallenge, solve as solve_captcha};
use super::signer::{APP_KEY, add_nocache, sign};
use crate::constants::to_webvpn_url;
use crate::iclass::IClassApi;

const BASE_URL: &str = "https://cgyy.buaa.edu.cn/venue-zhjs-server/";

const REFERRER: &str = "https://cgyy.buaa.edu.cn/venue-zhjs/mobileReservation";

/// Cookie the service sets after the SSO handshake, exchanged for a token.

const SSO_COOKIE_NAME: &str = "sso_buaa_zhjs_token";

/// A bookable room.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct VenueSite {
    pub id:          i64,
    pub site_name:   String,
    pub venue_name:  String,
    pub campus_name: String,
    pub seat_count:  Option<i64>,
}

/// One selectable time slot on a date.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct TimeSlot {
    pub id:         i64,
    pub begin_time: String,
    pub end_time:   String,
    pub label:      String,
}

/// A room's availability for one slot.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct SlotStatus {
    pub time_id:    i64,
    pub reservable: bool,
    pub take_up:    bool,
    pub order_id:   Option<i64>,
}

/// A room and its per-slot availability.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct SpaceAvailability {
    pub space_id:   i64,
    pub space_name: String,
    pub slots:      Vec<SlotStatus>,
}

/// Everything needed to choose a slot on a given date.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq)]

pub struct DayInfo {
    pub venue_site_id:     i64,
    pub date:              String,
    pub available_dates:   Vec<String>,
    pub time_slots:        Vec<TimeSlot>,
    pub spaces:            Vec<SpaceAvailability>,
    /// Context token that must accompany the order and submission calls.
    pub reservation_token: Option<String>,
}

/// A reservation as stored by the service.

#[derive(Clone, Debug, Default, Deserialize, Serialize, PartialEq, Eq)]

pub struct Order {
    pub id:               i64,
    pub site_name:        Option<String>,
    pub space_name:       Option<String>,
    pub reservation_date: Option<String>,
    pub start_date:       Option<String>,
    pub end_date:         Option<String>,
    pub theme:            Option<String>,
    pub order_status:     Option<i64>,
    pub status_text:      Option<String>,
}

/// What to reserve, after the caller has picked from [`DayInfo`].

#[derive(Clone, Debug, Default)]

pub struct ReservationRequest {
    pub venue_site_id: i64,
    pub date:          String,
    pub space_id:      i64,
    /// Time-slot ids, all from the same room.
    pub time_ids:      Vec<i64>,
    pub phone:         String,
    pub theme:         String,
    pub purpose_type:  i64,
    pub joiner_num:    i64,
    pub activity:      String,
    pub joiners:       String,
}

impl IClassApi {
    /// Sends a signed request to the CGYY service.
    ///
    /// How:
    /// GET parameters are signed in full and include a `nocache` value; POST
    /// parameters are sent as a form and signed the same way. A login redirect
    /// clears the token and retries once.

    async fn cgyy_request(
        &self,
        method: &str,
        path: &str,
        params: &[(&str, String)],
        token: Option<&str>,
        force_refresh: bool,
    ) -> Result<Value> {

        let url = cgyy_url(
            self.use_vpn,
            &format!("{BASE_URL}{}", path.trim_start_matches('/')),
        );

        let timestamp = chrono::Utc::now().timestamp_millis();

        let is_get = method.eq_ignore_ascii_case("get");

        // GET calls carry nocache in the query and the signature; POST calls
        // sign only the form fields.
        let effective: Vec<(String, String)> = if is_get {

            let borrowed: Vec<(&str, &str)> = params
                .iter()
                .map(|(key, value)| (*key, value.as_str()))
                .collect();

            add_nocache(&borrowed, timestamp)
        } else {

            params
                .iter()
                .map(|(key, value)| ((*key).to_string(), value.clone()))
                .collect()
        };

        let sign_params: Vec<(&str, &str)> = effective
            .iter()
            .map(|(key, value)| (key.as_str(), value.as_str()))
            .collect();

        let signature = sign(path, &sign_params, timestamp);

        let mut request = if is_get {

            self.client.get(&url).query(&effective)
        } else {

            self.client.post(&url).form(&effective)
        };

        request = request
            .header("Accept", "application/json, text/plain, */*")
            .header("Referer", cgyy_url(self.use_vpn, REFERRER))
            .header("app-key", APP_KEY)
            .header("timestamp", timestamp.to_string())
            .header("sign", signature);

        if let Some(token) = token {

            request = request.header("cgAuthorization", token);
        }

        let response = request
            .send()
            .await
            .with_context(|| format!("研讨室请求失败: {path}"))?;

        let status = response.status().as_u16();

        let final_url = response.url().to_string();

        let body = response.text().await.context("读取研讨室响应失败")?;

        if status == 401 || final_url.contains("sso.buaa.edu.cn") {

            bail!("研讨室登录状态已失效，请重新登录");
        }

        if !(200..300).contains(&status) {

            bail!("研讨室请求失败: HTTP {status}");
        }

        let value: Value = serde_json::from_str(&body).with_context(|| {

            let preview: String = body.chars().take(120).collect();

            format!("研讨室返回了非 JSON 响应: {preview}")
        })?;

        // The envelope signals business failures in `code`, separately from
        // transport errors.
        if let Some(code) = value.get("code").and_then(Value::as_i64)
            && code != 0
            && code != 200
        {

            let message = value
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("研讨室接口返回错误");

            let _ = force_refresh;

            bail!("{message}（code={code}）");
        }

        Ok(value)
    }

    /// Exchanges the SSO session for a CGYY access token.
    ///
    /// How:
    /// Visiting `sso/manageLogin` makes the service set its own SSO cookie from
    /// the shared session; that cookie is then posted to `/api/login` to obtain
    /// `access_token`.

    pub async fn cgyy_login(&self) -> Result<String> {

        let manage_url = cgyy_url(self.use_vpn, &format!("{BASE_URL}sso/manageLogin"));

        let _ = self
            .client
            .get(&manage_url)
            .send()
            .await
            .context("研讨室 SSO 激活失败")?;

        // `Jar` reads cookies only through the CookieStore trait, which returns
        // the whole `Cookie` header value for a URL.
        let sso_token = {

            use reqwest::cookie::CookieStore;

            let base = reqwest::Url::parse(&cgyy_url(self.use_vpn, BASE_URL))?;

            self.session_cookie_jar()
                .cookies(&base)
                .and_then(|header| header.to_str().ok().map(str::to_string))
        }
        .and_then(|text| {

            text.split(';')
                .filter_map(|pair| pair.trim().split_once('='))
                .find(|(name, _)| *name == SSO_COOKIE_NAME)
                .map(|(_, value)| value.to_string())
        })
        .ok_or_else(|| anyhow!("未获取到研讨室 SSO Token，请重新登录统一认证"))?;

        let response = self
            .client
            .post(cgyy_url(self.use_vpn, &format!("{BASE_URL}api/login")))
            .header("Sso-Token", sso_token)
            .header("app-key", APP_KEY)
            .send()
            .await
            .context("研讨室登录失败")?;

        let body: Value = response.json().await.context("研讨室登录响应格式异常")?;

        body.pointer("/data/token/access_token")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .ok_or_else(|| anyhow!("研讨室登录成功但未返回 access_token"))
    }

    /// Lists the bookable rooms.

    pub async fn cgyy_list_sites(&self, token: &str) -> Result<Vec<VenueSite>> {

        let response = self
            .cgyy_request(
                "get",
                "/api/venue/site",
                &[
                    ("page", "-1".to_string()),
                    ("size", "-1".to_string()),
                    ("reservationRoleId", "3".to_string()),
                ],
                Some(token),
                false,
            )
            .await?;

        let rows = json_array_at(&response, &["data", "content"])
            .or_else(|| json_array_at(&response, &["data", "list"]))
            .or_else(|| response.get("data").and_then(Value::as_array).cloned())
            .unwrap_or_default();

        Ok(rows.iter().filter_map(parse_venue_site).collect())
    }

    /// Lists the reservation purposes offered by the service.

    pub async fn cgyy_list_purpose_types(&self, token: &str) -> Result<Vec<(i64, String)>> {

        let response = self
            .cgyy_request("get", "/api/codes", &[], Some(token), false)
            .await?;

        let rows = json_array_at(&response, &["data", "purposeType"])
            .or_else(|| json_array_at(&response, &["data"]))
            .unwrap_or_default();

        Ok(rows
            .iter()
            .filter_map(|row| {

                let key = row.get("key").and_then(Value::as_i64)?;

                let name = row
                    .get("name")
                    .and_then(Value::as_str)
                    .unwrap_or("未命名")
                    .to_string();

                Some((key, name))
            })
            .collect())
    }

    /// Loads a room's availability for one date.

    pub async fn cgyy_day_info(
        &self,
        token: &str,
        venue_site_id: i64,
        date: &str,
    ) -> Result<DayInfo> {

        let response = self
            .cgyy_request(
                "get",
                "/api/reservation/day/info",
                &[
                    ("searchDate", date.to_string()),
                    ("venueSiteId", venue_site_id.to_string()),
                ],
                Some(token),
                false,
            )
            .await?;

        parse_day_info(&response, venue_site_id, date)
    }

    /// Lists the caller's own reservations.

    pub async fn cgyy_orders(&self, token: &str, page: i64, size: i64) -> Result<Vec<Order>> {

        let response = self
            .cgyy_request(
                "get",
                "/api/orders/mine",
                &[("page", page.to_string()), ("size", size.to_string())],
                Some(token),
                false,
            )
            .await?;

        let rows = json_array_at(&response, &["data", "content"])
            .or_else(|| json_array_at(&response, &["data", "list"]))
            .or_else(|| response.get("data").and_then(Value::as_array).cloned())
            .unwrap_or_default();

        Ok(rows.iter().filter_map(parse_order).collect())
    }

    /// Returns the door lock code for the caller's active reservation.

    pub async fn cgyy_lock_code(&self, token: &str) -> Result<String> {

        let response = self
            .cgyy_request("get", "/api/orders/lock/code", &[], Some(token), false)
            .await?;

        let data = response.get("data").cloned().unwrap_or(Value::Null);

        // The payload varies: sometimes a bare string, sometimes an object.
        let code = data
            .as_str()
            .map(str::to_string)
            .or_else(|| {

                data.get("lockCode")
                    .or_else(|| data.get("code"))
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| anyhow!("当前没有可用的门锁码"))?;

        Ok(code)
    }

    /// Reserves one room for one or more consecutive slots.
    ///
    /// Why:
    /// This is a destructive, real-world action: it claims a physical room and
    /// can only be undone by cancelling. It therefore requires an explicit
    /// confirmation flag at the CLI layer, and this function refuses to guess.
    ///
    /// How:
    /// Re-reads availability, validates the chosen slots, creates the order
    /// context, then solves the captcha and submits. The captcha is retried a
    /// few times because the solver can misjudge a low-contrast image.

    pub async fn cgyy_reserve(&self, token: &str, request: &ReservationRequest) -> Result<Order> {

        if request.time_ids.is_empty() {

            bail!("请至少选择一个时段");
        }

        let day = self
            .cgyy_day_info(token, request.venue_site_id, &request.date)
            .await
            .context("预约前读取可用时段失败")?;

        let reservation_token = day
            .reservation_token
            .clone()
            .filter(|value| !value.is_empty())
            .ok_or_else(|| anyhow!("预约上下文 token 缺失，请稍后重试"))?;

        let space = day
            .spaces
            .iter()
            .find(|space| space.space_id == request.space_id)
            .ok_or_else(|| anyhow!("所选房间不存在或已失效"))?;

        for time_id in &request.time_ids {

            let slot = space
                .slots
                .iter()
                .find(|slot| slot.time_id == *time_id)
                .ok_or_else(|| anyhow!("所选时段 {time_id} 不存在或已失效"))?;

            if !slot.reservable {

                bail!("时段 {time_id} 已不可预约，请刷新后重试");
            }
        }

        let selections: Vec<Value> = request
            .time_ids
            .iter()
            .map(|time_id| {

                json!({
                    "spaceId": request.space_id,
                    "timeId": time_id,
                })
            })
            .collect();

        let order_json = serde_json::to_string(&selections)?;

        // The service requires an order-context row before submission.
        let _ = self
            .cgyy_request(
                "post",
                "/api/reservation/order/info",
                &[
                    ("venueSiteId", request.venue_site_id.to_string()),
                    ("reservationDate", request.date.clone()),
                    ("weekStartDate", request.date.clone()),
                    ("reservationOrderJson", order_json.clone()),
                    ("token", reservation_token.clone()),
                ],
                Some(token),
                false,
            )
            .await
            .context("创建预约上下文失败")?;

        let mut last_error = None;

        for _ in 0..3 {

            match self
                .cgyy_submit_once(token, request, &order_json, &reservation_token)
                .await
            {
                Ok(order) => return Ok(order),
                Err(error) => last_error = Some(error),
            }
        }

        Err(last_error.unwrap_or_else(|| anyhow!("预约提交失败")))
    }

    async fn cgyy_submit_once(
        &self,
        token: &str,
        request: &ReservationRequest,
        order_json: &str,
        reservation_token: &str,
    ) -> Result<Order> {

        let challenge = self.cgyy_captcha(token).await?;

        let solved = tokio::task::spawn_blocking(move || solve_captcha(&challenge))
            .await
            .context("验证码求解任务失败")??;

        // The plaintext point is what the server decrypts; logging it alongside
        // the offset makes a rejected captcha diagnosable.
        if std::env::var_os("ICLASS_CGYY_CAPTCHA_DEBUG").is_some() {

            eprintln!(
                "研讨室验证码: 偏移={} 明文={}",
                solved.move_distance, solved.point_json_data
            );
        }

        let _ = self
            .cgyy_request(
                "post",
                "/api/captcha/check",
                &[
                    ("pointJson", solved.point_json.clone()),
                    ("token", solved.token.clone()),
                ],
                Some(token),
                false,
            )
            .await
            .context("验证码校验失败")?;

        let response = self
            .cgyy_request(
                "post",
                "/api/reservation/order/submit",
                &[
                    ("venueSiteId", request.venue_site_id.to_string()),
                    ("reservationDate", request.date.clone()),
                    ("reservationOrderJson", order_json.to_string()),
                    ("weekStartDate", request.date.clone()),
                    ("phone", request.phone.trim().to_string()),
                    ("theme", request.theme.trim().to_string()),
                    ("purposeType", request.purpose_type.to_string()),
                    ("joinerNum", request.joiner_num.to_string()),
                    ("activityContent", request.activity.trim().to_string()),
                    ("joiners", request.joiners.trim().to_string()),
                    ("isPhilosophySocialSciences", "0".to_string()),
                    ("isOffSchoolJoiner", "0".to_string()),
                    ("captchaVerification", solved.captcha_verification.clone()),
                    ("token", reservation_token.to_string()),
                ],
                Some(token),
                false,
            )
            .await
            .context("提交预约失败")?;

        Ok(response
            .pointer("/data/orderInfo")
            .and_then(parse_order)
            .unwrap_or_default())
    }

    async fn cgyy_captcha(&self, token: &str) -> Result<CaptchaChallenge> {

        let timestamp = chrono::Utc::now().timestamp_millis();

        let client_uid = format!("slider-{timestamp}");

        let response = self
            .cgyy_request(
                "get",
                "/api/captcha/get",
                &[
                    ("captchaType", "blockPuzzle".to_string()),
                    ("clientUid", client_uid),
                    ("ts", timestamp.to_string()),
                ],
                Some(token),
                false,
            )
            .await?;

        let data = response.get("data").cloned().unwrap_or(Value::Null);

        // `success` arrives as a string in some deliveries and a bool in others.
        let success = data
            .get("success")
            .map(|value| {

                value
                    .as_bool()
                    .or_else(|| value.as_str().map(|text| text == "true"))
                    .unwrap_or(false)
            })
            .unwrap_or(false);

        if !success {

            let message = data
                .get("repMsg")
                .and_then(Value::as_str)
                .unwrap_or("获取验证码失败");

            bail!("{message}");
        }

        let rep = data
            .get("repData")
            .ok_or_else(|| anyhow!("验证码数据缺失"))?;

        Ok(CaptchaChallenge {
            secret_key: required_string(rep, "secretKey")?,
            token:      required_string(rep, "token")?,
            original:   required_string(rep, "originalImageBase64")?,
            jigsaw:     required_string(rep, "jigsawImageBase64")?,
        })
    }

    /// Cancels one of the caller's reservations.

    pub async fn cgyy_cancel(&self, token: &str, order_id: i64) -> Result<()> {

        let _ = self
            .cgyy_request(
                "post",
                &format!("/api/orders/new/cancel/{order_id}"),
                &[],
                Some(token),
                false,
            )
            .await
            .context("取消预约失败")?;

        Ok(())
    }
}

fn cgyy_url(use_vpn: bool, raw: &str) -> String {

    if use_vpn {

        to_webvpn_url(raw)
    } else {

        raw.to_string()
    }
}

fn required_string(value: &Value, key: &str) -> Result<String> {

    value
        .get(key)
        .and_then(Value::as_str)
        .filter(|text| !text.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("响应缺少字段 {key}"))
}

fn json_array_at(root: &Value, path: &[&str]) -> Option<Vec<Value>> {

    let mut current = root;

    for key in path {

        current = current.get(*key)?;
    }

    current.as_array().cloned()
}

/// Reads an integer that may arrive as a number or a numeric string.

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

fn parse_venue_site(row: &Value) -> Option<VenueSite> {

    Some(VenueSite {
        id:          row.get("id").and_then(flexible_i64)?,
        site_name:   row
            .get("siteName")
            .and_then(Value::as_str)
            .unwrap_or("未命名场地")
            .to_string(),
        venue_name:  row
            .get("venueName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        campus_name: row
            .get("campusName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string(),
        seat_count:  row.get("seatCount").and_then(flexible_i64),
    })
}

fn parse_order(row: &Value) -> Option<Order> {

    Some(Order {
        id:               row.get("id").and_then(flexible_i64)?,
        site_name:        row.get("siteName").and_then(flexible_string),
        space_name:       row.get("venueSpaceName").and_then(flexible_string),
        reservation_date: row.get("reservationDate").and_then(flexible_string),
        start_date:       row.get("reservationStartDate").and_then(flexible_string),
        end_date:         row.get("reservationEndDate").and_then(flexible_string),
        theme:            row.get("theme").and_then(flexible_string),
        order_status:     row.get("orderStatus").and_then(flexible_i64),
        status_text:      row.get("orderStatusName").and_then(flexible_string),
    })
}

fn parse_day_info(response: &Value, venue_site_id: i64, date: &str) -> Result<DayInfo> {

    let data = response
        .get("data")
        .cloned()
        .ok_or_else(|| anyhow!("研讨室时段响应缺少 data"))?;

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

                    Some(TimeSlot {
                        id:         row.get("id").and_then(flexible_i64)?,
                        begin_time: row
                            .get("beginTime")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        end_time:   row
                            .get("endTime")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        label:      row
                            .get("label")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    let spaces = data
        .get("spaces")
        .and_then(Value::as_array)
        .map(|rows| {

            rows.iter()
                .filter_map(|row| {

                    let space_id = row.get("spaceId").and_then(flexible_i64)?;

                    let slots = row
                        .get("slots")
                        .and_then(Value::as_array)
                        .map(|slots| {

                            slots
                                .iter()
                                .filter_map(|slot| {

                                    Some(SlotStatus {
                                        time_id:    row_time_id(slot)?,
                                        reservable: slot
                                            .get("isReservable")
                                            .and_then(Value::as_bool)
                                            .unwrap_or(false),
                                        take_up:    slot
                                            .get("takeUp")
                                            .and_then(Value::as_bool)
                                            .unwrap_or(false),
                                        order_id:   slot.get("orderId").and_then(flexible_i64),
                                    })
                                })
                                .collect()
                        })
                        .unwrap_or_default();

                    Some(SpaceAvailability {
                        space_id,
                        space_name: row
                            .get("spaceName")
                            .and_then(Value::as_str)
                            .unwrap_or("未命名房间")
                            .to_string(),
                        slots,
                    })
                })
                .collect()
        })
        .unwrap_or_default();

    Ok(DayInfo {
        venue_site_id,
        date: date.to_string(),
        available_dates,
        time_slots,
        spaces,
        reservation_token: data
            .get("reservationToken")
            .and_then(Value::as_str)
            .filter(|value| !value.is_empty())
            .map(str::to_string),
    })
}

fn row_time_id(slot: &Value) -> Option<i64> {

    slot.get("timeId")
        .and_then(flexible_i64)
        .or_else(|| slot.get("id").and_then(flexible_i64))
}

#[cfg(test)]

mod tests {

    use super::{parse_day_info, parse_order, parse_venue_site};
    use serde_json::json;

    #[test]

    fn reads_a_venue_site_row() {

        let site = parse_venue_site(&json!({
            "id": 7,
            "siteName": "研讨室 A",
            "venueName": "沙河图书馆",
            "campusName": "沙河",
            "seatCount": 8
        }))
        .expect("应能解析场地");

        assert_eq!(site.id, 7);

        assert_eq!(site.site_name, "研讨室 A");

        assert_eq!(site.seat_count, Some(8));
    }

    #[test]

    fn numeric_ids_may_arrive_as_strings() {

        // The service mixes number and string encodings between endpoints.
        let site = parse_venue_site(&json!({
            "id": "42",
            "siteName": "研讨室 B",
            "venueName": "学院路",
            "campusName": "学院路"
        }))
        .expect("字符串 id 也应可解析");

        assert_eq!(site.id, 42);
    }

    #[test]

    fn reads_day_info_with_availability() {

        let response = json!({
            "code": 0,
            "data": {
                "availableDates": ["2026-03-10", "2026-03-11"],
                "reservationToken": "tok-1",
                "timeSlots": [
                    {"id": 1, "beginTime": "08:00", "endTime": "10:00", "label": "上午一"},
                    {"id": 2, "beginTime": "10:00", "endTime": "12:00", "label": "上午二"}
                ],
                "spaces": [
                    {
                        "spaceId": 100,
                        "spaceName": "A101",
                        "slots": [
                            {"timeId": 1, "isReservable": true, "takeUp": false},
                            {"timeId": 2, "isReservable": false, "takeUp": true, "orderId": 55}
                        ]
                    }
                ]
            }
        });

        let day = parse_day_info(&response, 7, "2026-03-10").expect("应能解析");

        assert_eq!(day.reservation_token.as_deref(), Some("tok-1"));

        assert_eq!(day.time_slots.len(), 2);

        assert_eq!(day.spaces.len(), 1);

        assert_eq!(day.spaces[0].space_name, "A101");

        assert!(day.spaces[0].slots[0].reservable);

        assert!(!day.spaces[0].slots[1].reservable);

        assert_eq!(day.spaces[0].slots[1].order_id, Some(55));
    }

    #[test]

    fn day_info_without_data_is_an_error() {

        // Silently returning an empty day would present "no rooms" for what is
        // really a malformed response.
        assert!(parse_day_info(&json!({"code": 0}), 1, "2026-03-10").is_err());
    }

    #[test]

    fn reads_an_order_row() {

        let order = parse_order(&json!({
            "id": 12,
            "siteName": "研讨室 A",
            "venueSpaceName": "A101",
            "reservationDate": "2026-03-10",
            "orderStatus": 1,
            "theme": "小组讨论"
        }))
        .expect("应能解析订单");

        assert_eq!(order.id, 12);

        assert_eq!(order.space_name.as_deref(), Some("A101"));

        assert_eq!(order.order_status, Some(1));
    }
}
