//! CLI commands for seminar-room (研讨室) reservation.
//!
//! Why:
//! Room booking claims a physical space, so the CLI is deliberately split into
//! a preview step and a write step. `venue-reserve` without `--yes` prints
//! exactly what would be booked and stops, which makes a mistyped command
//! harmless.

use anyhow::{Context, Result, bail};

use crate::cgyy::{DayInfo, ReservationRequest};
use crate::iclass::IClassApi;

use super::args::{VenueArgs, VenueOrdersArgs, VenueReserveArgs, VenueSlotsArgs};
use super::config::{AutomationConfig, load_config};

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

    match api.login_with_diagnostic(&config.login_input()).await {
        Ok(_session) => Ok(api),

        Err(diagnostic) => {

            if debug_login {

                eprintln!("{}", serde_json::to_string_pretty(&diagnostic)?);
            }

            bail!(diagnostic.summary);
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
            "{text}\n研讨室需要有效统一认证会话，请先运行 list-today 完成一次登录。"
        );
    }

    error
}

async fn venue_token(api: &IClassApi) -> Result<String> {

    api.cgyy_login().await.map_err(venue_error)
}

pub(crate) async fn venues_command(args: VenueArgs) -> Result<()> {

    let config = load_config(args.config.as_deref())?;

    let api = authenticated_api(&config, args.debug_login).await?;

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

    let api = authenticated_api(&config, args.debug_login).await?;

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

    let api = authenticated_api(&config, args.debug_login).await?;

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

    let api = authenticated_api(&config, args.debug_login).await?;

    let token = venue_token(&api).await?;

    if let Some(order_id) = args.cancel {

        if !args.yes {

            println!("将取消订单 {order_id}。确认后加上 --yes 才会真正取消。");

            return Ok(());
        }

        api.cgyy_cancel(&token, order_id)
            .await
            .map_err(venue_error)
            .with_context(|| format!("取消订单 {order_id} 失败"))?;

        println!("已取消订单 {order_id}");

        return Ok(());
    }

    let orders = api.cgyy_orders(&token, 1, 20).await.map_err(venue_error)?;

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

            let label = slot
                .map(|slot| format!("{}-{} {}", slot.begin_time, slot.end_time, slot.label))
                .unwrap_or_else(|| format!("时段 {}", status.time_id));

            let state = if status.reservable {

                "可预约"
            } else if status.take_up {

                "已被占用"
            } else {

                "不可预约"
            };

            println!("\t{}\t{}\t{}", status.time_id, label, state);
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

    use super::resolve_date;

    #[test]

    fn resolve_date_accepts_iso_and_rejects_noise() {

        assert_eq!(
            resolve_date(Some("2026-03-10".to_string())).unwrap(),
            "2026-03-10"
        );

        assert!(resolve_date(Some("2026/03/10".to_string())).is_err());

        assert!(resolve_date(Some("明天".to_string())).is_err());
    }
}
