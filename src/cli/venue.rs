//! CLI commands for seminar-room (研讨室) reservation.
//!
//! Why:
//! Room booking claims a physical space, so the CLI is deliberately split into
//! a preview step and a write step. `venue-reserve` without `--yes` prints
//! exactly what would be booked and stops, which makes a mistyped command
//! harmless.

use std::io::Write;

use anyhow::{Context, Result};

use crate::cgyy::{DayInfo, Order, ReservationRequest};
use crate::iclass::IClassApi;

use super::args::{VenueArgs, VenueOrdersArgs, VenueReserveArgs, VenueSlotsArgs};
use super::config::{AutomationConfig, load_config};

/// How many of the caller's most recent orders are read, for the list and for
/// finding the order a `--cancel` names.

const ORDER_PAGE_SIZE: i64 = 20;

/// Logs in and returns an authenticated API client.
///
/// Why:
/// The venue commands only need a session, not the schedule or course state
/// that the planner's shared helper also prepares. Logging in directly keeps
/// the dependency surface small and mirrors how the diagnostic is reported.

pub(crate) async fn authenticated_api(
    config: &AutomationConfig,
    debug_login: bool,
) -> Result<IClassApi> {

    let api = IClassApi::new(config.use_vpn)?;

    if let Err(diagnostic) = api.login_with_diagnostic(&config.login_input()).await {

        if debug_login {

            eprintln!("{}", serde_json::to_string_pretty(&diagnostic)?);
        }

        let mut error = anyhow::anyhow!(diagnostic.summary);

        for cause in diagnostic.error_chain.into_iter().rev() {

            error = error.context(cause);
        }

        return Err(error);
    }

    Ok(api)
}

async fn venue_api(config: &AutomationConfig, debug_login: bool) -> Result<IClassApi> {

    match IClassApi::for_venue(&config.login_input()).await {
        Ok(api) => Ok(api),
        Err(error) => {

            if debug_login {

                eprintln!("研讨室 SSO 登录失败: {error:#}");
            }

            Err(error)
        }
    }
}

/// Reports a seminar-room failure with the advice that actually applies.
///
/// Why:
/// The CGYY service needs its own session derived from the shared SSO login.
/// When that is missing the raw error is an opaque auth failure, and the fix
/// (log in again) is not obvious from it.

fn venue_error(error: anyhow::Error) -> anyhow::Error {

    let text = error.to_string();

    if text.contains("SSO Token") || text.contains("登录状态已失效") {

        return anyhow::anyhow!(
            "{text}\n研讨室需要有效统一认证会话，\
             请检查当前命令的统一认证配置后重试（进程间不共享会话）。"
        );
    }

    error
}

async fn venue_token(api: &IClassApi) -> Result<String> {

    api.cgyy_login().await.map_err(venue_error)
}

pub(crate) async fn venues_command(args: VenueArgs) -> Result<()> {

    let config = load_config(args.config.as_deref())?;

    let api = venue_api(&config, args.debug_login).await?;

    let token = venue_token(&api).await?;

    let mut sites = api.cgyy_list_sites(&token).await.map_err(venue_error)?;

    if let Some(query) = args.query.as_deref() {

        let query = query.trim();

        sites.retain(|site| {

            site.site_name.contains(query)
                || site.venue_name.contains(query)
                || site.campus_name.contains(query)
        });
    }

    if args.json {

        println!("{}", serde_json::to_string_pretty(&sites)?);

        return Ok(());
    }

    if sites.is_empty() {

        println!("没有找到可预约的研讨室");

        return Ok(());
    }

    // Purpose ids are opaque numbers; showing the names is what makes
    // --purpose usable.
    if let Ok(purposes) = api.cgyy_list_purpose_types(&token).await
        && !purposes.is_empty()
    {

        println!(
            "用途: {}",
            purposes
                .iter()
                .map(|(key, name)| format!("{key}={name}"))
                .collect::<Vec<_>>()
                .join("  ")
        );

        println!();
    }

    println!("id\t校区\t场馆\t房间\t座位");

    for site in &sites {

        println!(
            "{}\t{}\t{}\t{}\t{}",
            site.id,
            dash(&site.campus_name),
            dash(&site.venue_name),
            site.site_name,
            site.seat_count
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
        );
    }

    Ok(())
}

pub(crate) async fn venue_slots_command(args: VenueSlotsArgs) -> Result<()> {

    let config = load_config(args.config.as_deref())?;

    let api = venue_api(&config, args.debug_login).await?;

    let token = venue_token(&api).await?;

    let date = resolve_date(args.date)?;

    let day = api
        .cgyy_day_info(&token, args.site, &date)
        .await
        .map_err(venue_error)?;

    if args.json {

        println!("{}", serde_json::to_string_pretty(&day)?);

        return Ok(());
    }

    print_day(&day);

    Ok(())
}

pub(crate) async fn venue_reserve_command(args: VenueReserveArgs) -> Result<()> {

    let config = load_config(args.config.as_deref())?;

    let api = venue_api(&config, args.debug_login).await?;

    let token = venue_token(&api).await?;

    // Read availability first so the preview below is truthful, and so the
    // room and slot names can be shown before anything is submitted.
    let day = api
        .cgyy_day_info(&token, args.site, &args.date)
        .await
        .map_err(venue_error)?;

    let space = day
        .spaces
        .first()
        .ok_or_else(|| anyhow::anyhow!("该场地在 {date} 没有可预约的房间", date = args.date))?;

    let request = ReservationRequest {
        venue_site_id: args.site,
        date:          args.date.clone(),
        space_id:      space.space_id,
        time_ids:      args.slots.clone(),
        phone:         args.phone.clone(),
        theme:         args.theme.clone(),
        purpose_type:  args.purpose,
        joiner_num:    args.joiners,
        activity:      args.activity.clone(),
        joiners:       args.joiner_names.clone(),
    };

    if args.json && !args.yes {

        // A dry run still reports what would happen, so an agent can plan
        // without claiming a room.
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "action": "venue-reserve",
                "submitted": false,
                "would_reserve": {
                    "venue_site_id": request.venue_site_id,
                    "space_id": request.space_id,
                    "date": request.date,
                    "slots": request.time_ids,
                },
                "hint": "加上 --yes 才会真正提交预约",
            }))?
        );

        return Ok(());
    }

    describe_reservation(&day, &request);

    if !args.yes {

        println!();

        println!("这是预览。确认无误后加上 --yes 才会真正提交预约。");

        return Ok(());
    }

    let order = api
        .cgyy_reserve(&token, &request)
        .await
        .map_err(venue_error)
        .context("研讨室预约失败")?;

    if args.json {

        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "action": "venue-reserve",
                "submitted": true,
                "order_id": order.id,
                "room": order.space_name,
                "date": order.reservation_date.clone().unwrap_or_else(|| args.date.clone()),
                "slots": args.slots,
                "confirmed": args.yes,
            }))?
        );

        return Ok(());
    }

    println!();

    if order.id > 0 {

        println!(
            "预约成功\t订单 {}\t{}\t{}",
            order.id,
            order.space_name.as_deref().unwrap_or("-"),
            order.reservation_date.as_deref().unwrap_or(&args.date),
        );
    } else {

        println!("预约已提交（服务未返回订单详情，请用 venue-orders 确认）");
    }

    Ok(())
}

pub(crate) async fn venue_orders_command(args: VenueOrdersArgs) -> Result<()> {

    let config = load_config(args.config.as_deref())?;

    let api = venue_api(&config, args.debug_login).await?;

    let token = venue_token(&api).await?;

    if let Some(order_id) = args.cancel {

        // Look the order up in the list just read, so the preview can name the
        // room and state whether the cancellation is possible at all — the
        // same contract `seat-orders --cancel` offers. A missing id is a
        // blocked cancel, not a request to send blind.
        let orders = api
            .cgyy_orders(&token, 1, ORDER_PAGE_SIZE)
            .await
            .map_err(venue_error)?;

        let state = venue_cancel_state(order_id, &orders);

        if args.yes {

            if let Some(reason) = state["blocked_reason"].as_str() {

                anyhow::bail!("未发送取消请求：{reason}");
            }

            api.cgyy_cancel(&token, order_id)
                .await
                .map_err(venue_error)
                .with_context(|| format!("取消订单 {order_id} 失败"))?;
        }

        return write_cancel_outcome(
            &mut std::io::stdout().lock(),
            &mut std::io::stderr().lock(),
            order_id,
            &state,
            args.yes,
            args.json,
        );
    }

    let orders = api
        .cgyy_orders(&token, 1, ORDER_PAGE_SIZE)
        .await
        .map_err(venue_error)?;

    if args.json {

        println!("{}", serde_json::to_string_pretty(&orders)?);

        return Ok(());
    }

    if orders.is_empty() {

        println!("没有预约记录");

        return Ok(());
    }

    if let Ok(code) = api.cgyy_lock_code(&token).await {

        println!("门锁码\t{code}");

        println!();
    }

    println!("id\t日期\t房间\t时段\t主题\t状态");

    for order in &orders {

        let span = match (order.start_date.as_deref(), order.end_date.as_deref()) {
            (Some(start), Some(end)) => format!("{start} ~ {end}"),

            _ => "-".to_string(),
        };

        println!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            order.id,
            order.reservation_date.as_deref().unwrap_or("-"),
            order.space_name.as_deref().unwrap_or("-"),
            span,
            order.theme.as_deref().unwrap_or("-"),
            order
                .status_text
                .as_deref()
                .map(str::to_string)
                .unwrap_or_else(|| {

                    order
                        .order_status
                        .map(|status| status.to_string())
                        .unwrap_or_else(|| "-".to_string())
                }),
        );
    }

    Ok(())
}

/// Prints the outcome of `venue-orders --cancel`, preview or confirmed.
///
/// Why:
/// `--json` callers parse stdout as one document, so under `--json` nothing but
/// that document may reach stdout; the human line still has to be visible, so
/// it moves to stderr. Taking both writers as parameters lets a test hold the
/// command to that split without a live session.

fn write_cancel_outcome(
    out: &mut dyn Write,
    err: &mut dyn Write,
    order_id: i64,
    state: &serde_json::Value,
    submitted: bool,
    json: bool,
) -> Result<()> {

    let human = human_cancel_line(order_id, state, submitted);

    if !json {

        writeln!(out, "{human}")?;

        return Ok(());
    }

    // stdout carries exactly one JSON document; the prose goes to stderr.
    writeln!(err, "{human}")?;

    let document = if submitted {

        serde_json::json!({
            "action": "venue-orders --cancel",
            "submitted": true,
            "cancelled": state["would_cancel"].clone(),
        })
    } else {

        state.clone()
    };

    writeln!(out, "{}", serde_json::to_string_pretty(&document)?)?;

    Ok(())
}

/// The one-line human summary shared by the preview and the result.

fn human_cancel_line(order_id: i64, state: &serde_json::Value, submitted: bool) -> String {

    if submitted {

        return format!("已取消订单 {order_id}");
    }

    match state["blocked_reason"].as_str() {
        Some(reason) => format!("不能取消 {order_id}：{reason}"),

        None => format!("将取消订单 {order_id}。确认后加上 --yes 才会真正取消。"),
    }
}

/// Builds the `--json` document for a `venue-orders --cancel` attempt.
///
/// Why:
/// The sibling library path (`seat-orders --cancel`) reports whether the
/// cancellation can go ahead before `--yes` is passed, and an automated caller
/// relies on that. Splitting it into a pure function keeps the shape testable
/// without a live session.
///
/// How:
/// The order is looked up in the page that was just read; an id that is not
/// there is reported as `blocked_reason` and never sent. CGYY's status codes
/// for a finished or cancelled order are not known from a live sample, so an
/// order that is listed is not second-guessed here: the service decides, and
/// its refusal reaches the caller as an error.

fn venue_cancel_state(order_id: i64, orders: &[Order]) -> serde_json::Value {

    let order = orders.iter().find(|order| order.id == order_id);

    let blocked = order.is_none().then(|| {

        format!(
            "预约 id 无效：最近 {ORDER_PAGE_SIZE} 条预约里没有 {order_id}，请先用 venue-orders \
             查看"
        )
    });

    serde_json::json!({
        "action": "venue-orders --cancel",
        "submitted": false,
        "would_cancel": order,
        "cancellable": blocked.is_none(),
        "blocked_reason": blocked,
        // `--yes` would also be refused, so a blocked preview must not
        // suggest it.
        "hint": if blocked.is_none() {
            "加上 --yes 才会真正取消"
        } else {
            "这条预约不能取消，加 --yes 也不会发送请求"
        },
    })
}

/// Prints what a reservation would book, without submitting it.

fn describe_reservation(day: &DayInfo, request: &ReservationRequest) {

    let space_name = day
        .spaces
        .iter()
        .find(|space| space.space_id == request.space_id)
        .map(|space| space.space_name.as_str())
        .unwrap_or("未知房间");

    println!("场地\t{}\t{}", request.venue_site_id, space_name);

    println!("日期\t{}", request.date);

    println!("主题\t{}", request.theme);

    println!("人数\t{}", request.joiner_num);

    println!("电话\t{}", request.phone);

    println!("事由\t{}", request.activity);

    println!("时段");

    for time_id in &request.time_ids {

        let slot = day.time_slots.iter().find(|slot| slot.id == *time_id);

        let availability = day
            .spaces
            .iter()
            .find(|space| space.space_id == request.space_id)
            .and_then(|space| space.slots.iter().find(|status| status.time_id == *time_id));

        let label = slot
            .map(|slot| format!("{}-{} {}", slot.begin_time, slot.end_time, slot.label))
            .unwrap_or_else(|| format!("时段 {time_id}"));

        let state = match availability {
            Some(status) if status.reservable => "可预约",

            Some(_) => "已不可预约",

            None => "状态未知",
        };

        println!("\t{time_id}\t{label}\t{state}");
    }
}

fn print_day(day: &DayInfo) {

    if !day.available_dates.is_empty() {

        println!("可预约日期\t{}", day.available_dates.join(", "));

        println!();
    }

    if day.spaces.is_empty() {

        println!("该日期没有可预约的房间");

        return;
    }

    for space in &day.spaces {

        println!("{} ({})", space.space_name, space.space_id);

        if space.slots.is_empty() {

            println!("\t没有时段信息");

            continue;
        }

        for status in &space.slots {

            let slot = day.time_slots.iter().find(|slot| slot.id == status.time_id);

            let state = if status.reservable {

                "可预约"
            } else if status.take_up {

                "已被占用"
            } else {

                "不可预约"
            };

            println!(
                "\t{}\t{}-{}\t{}",
                status.time_id,
                slot.map(|s| s.begin_time.as_str()).unwrap_or("--:--"),
                slot.map(|s| s.end_time.as_str()).unwrap_or("--:--"),
                state
            );
        }
    }
}

fn resolve_date(value: Option<String>) -> Result<String> {

    let date = value.unwrap_or_else(|| chrono::Local::now().date_naive().to_string());

    chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d")
        .with_context(|| format!("日期格式必须是 YYYY-MM-DD: {date}"))?;

    Ok(date)
}

fn dash(value: &str) -> &str {

    if value.trim().is_empty() { "-" } else { value }
}

#[cfg(test)]

mod tests {

    use super::{Order, resolve_date, venue_cancel_state, write_cancel_outcome};

    fn order(id: i64, status_text: &str) -> Order {

        Order {
            id,
            status_text: Some(status_text.to_string()),
            space_name: Some("四层中文借阅室东区".to_string()),
            reservation_date: Some("2026-10-04".to_string()),
            ..Default::default()
        }
    }

    #[test]

    fn resolve_date_accepts_iso_and_rejects_noise() {

        assert_eq!(
            resolve_date(Some("2026-03-10".to_string())).unwrap(),
            "2026-03-10"
        );

        assert!(resolve_date(Some("2026/03/10".to_string())).is_err());

        assert!(resolve_date(Some("明天".to_string())).is_err());
    }

    #[test]

    fn a_cancellable_order_is_previewed_as_json() {

        // The documented contract: a preview is `submitted: false` with the
        // order it would act on, and `--yes` is what would actually cancel it.
        let state = venue_cancel_state(12, &[order(12, "预约成功")]);

        assert_eq!(state["action"], "venue-orders --cancel");

        assert_eq!(state["submitted"], false);

        assert_eq!(state["cancellable"], true);

        assert!(state["blocked_reason"].is_null());

        assert_eq!(state["would_cancel"]["id"], 12);
    }

    #[test]

    fn an_order_not_in_the_list_is_refused_before_yes() {

        let missing = venue_cancel_state(999, &[order(12, "预约成功")]);

        assert_eq!(missing["cancellable"], false);

        assert!(missing["blocked_reason"].is_string());

        assert!(missing["would_cancel"].is_null());
    }

    /// Runs the cancel output path and checks the stdout/stderr split.

    fn json_stdout(submitted: bool) -> (serde_json::Value, String) {

        let state = venue_cancel_state(12, &[order(12, "预约成功")]);

        let (mut out, mut err) = (Vec::new(), Vec::new());

        write_cancel_outcome(&mut out, &mut err, 12, &state, submitted, true).expect("写入输出");

        let stderr = String::from_utf8(err).expect("UTF-8");

        // AGENTS.md: with --json, stdout is the document a caller parses and
        // nothing else; the prose a human reads goes to stderr.
        let parsed = serde_json::from_slice(&out).unwrap_or_else(|error| {

            panic!(
                "--json 的 stdout 必须是 JSON（{error}）：{}",
                String::from_utf8_lossy(&out)
            )
        });

        (parsed, stderr)
    }

    #[test]

    fn cancel_preview_with_json_prints_only_json() {

        let (preview, stderr) = json_stdout(false);

        assert_eq!(preview["submitted"], false);

        assert_eq!(preview["cancellable"], true);

        assert_eq!(preview["would_cancel"]["id"], 12);

        assert!(
            stderr.contains("将取消订单 12"),
            "人读提示应到 stderr：{stderr}"
        );
    }

    #[test]

    fn confirmed_cancel_with_json_prints_only_json() {

        let (result, stderr) = json_stdout(true);

        assert_eq!(result["action"], "venue-orders --cancel");

        assert_eq!(result["submitted"], true);

        assert_eq!(result["cancelled"]["id"], 12);

        assert!(
            stderr.contains("已取消订单 12"),
            "人读提示应到 stderr：{stderr}"
        );
    }
}
