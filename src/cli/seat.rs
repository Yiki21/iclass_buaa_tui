//! CLI commands for library seat reservation (图书馆座位).
//!
//! Why:
//! Reserving a seat occupies a real place other students cannot then use, so
//! the flow is split the same way as seminar rooms: a preview by default, and a
//! write only with `--yes`.

use anyhow::{Context, Result, bail};

use crate::iclass::IClassApi;

use super::args::{SeatArgs, SeatBookArgs, SeatOrdersArgs};
use super::config::load_config;
use super::venue::authenticated_api;

/// Adds the advice that applies when the library session is missing.
///
/// Why:
/// The booking service needs its own token derived from the shared login.
/// Without it the raw error is an opaque auth failure whose fix is not obvious.

fn seat_error(error: anyhow::Error) -> anyhow::Error {

    let text = error.to_string();

    if text.contains("ticket") || text.contains("登录状态已失效") {

        return anyhow::anyhow!(
            "{text}\n图书馆座位需要有效统一认证会话，请先运行 list-today 完成一次登录。"
        );
    }

    error
}

async fn seat_token(api: &IClassApi) -> Result<String> {

    api.libbook_login().await.map_err(seat_error)
}

pub(crate) async fn seats_command(args: SeatArgs) -> Result<()> {

    let config = load_config(args.config.as_deref())?;

    let api = authenticated_api(&config, args.debug_login).await?;

    let token = seat_token(&api).await?;

    let date = resolve_date(args.date)?;

    let libraries = api
        .libbook_libraries(&token, &date)
        .await
        .map_err(seat_error)?;

    if args.json {

        println!("{}", serde_json::to_string_pretty(&libraries)?);

        return Ok(());
    }

    if libraries.is_empty() {

        println!("没有查询到图书馆信息");

        return Ok(());
    }

    println!("id\t图书馆\t空闲/总数");

    for library in &libraries {

        println!(
            "{}\t{}\t{}/{}",
            library.id, library.name, library.free_num, library.total_num
        );
    }

    if let Some(library_id) = args.library.as_deref() {

        println!();

        let areas = api
            .libbook_areas(&token, library_id, &date)
            .await
            .map_err(seat_error)?;

        if areas.is_empty() {

            println!("该图书馆在 {date} 没有可预约的阅览区");

            return Ok(());
        }

        println!("阅览区 (--area 参数用)");

        for area in &areas {

            println!(
                "\t{}\t{}\t{}/{}",
                area.id, area.name, area.free_num, area.total_num
            );
        }
    }

    Ok(())
}

pub(crate) async fn seat_book_command(args: SeatBookArgs) -> Result<()> {

    let config = load_config(args.config.as_deref())?;

    let api = authenticated_api(&config, args.debug_login).await?;

    let token = seat_token(&api).await?;

    let detail = api
        .libbook_area_detail(&token, &args.area)
        .await
        .map_err(seat_error)?;

    let segment = resolve_segment(&detail.time_slots, args.segment.as_deref())?;

    let seats = api
        .libbook_seats(&token, &args.area, &args.date, &segment.start, &segment.end)
        .await
        .map_err(seat_error)?;

    let seat = seats
        .iter()
        .find(|seat| seat.id == args.seat)
        .ok_or_else(|| {

            anyhow::anyhow!(
                "在 {} 的 {}-{} 没有找到座位 {}，可先不带 --seat 运行以查看可用座位",
                args.area,
                segment.start,
                segment.end,
                args.seat
            )
        })?;

    println!("阅览区\t{}", detail.name);

    println!("座位\t{}\t{}", seat.no, seat.name);

    println!("日期\t{}", args.date);

    println!("时段\t{}\t{}-{}", segment.id, segment.start, segment.end);

    println!(
        "状态\t{}",
        if seat.is_available {

            "可预约"
        } else {

            "不可预约"
        }
    );

    if !seat.is_available {

        bail!("该座位当前不可预约：{}", seat.status_name);
    }

    if !args.yes {

        if args.json {

            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "action": "seat-book",
                    "submitted": false,
                    "would_reserve": {
                        "area_id": args.area,
                        "seat_id": seat.id,
                        "seat_no": seat.no,
                        "date": args.date,
                        "segment": segment.id,
                    },
                    "hint": "加上 --yes 才会真正提交预约",
                }))?
            );

            return Ok(());
        }

        println!();

        println!("这是预览。确认无误后加上 --yes 才会真正提交预约。");

        return Ok(());
    }

    let booking = api
        .libbook_reserve(&token, &seat.id, &segment.id, &args.date)
        .await
        .map_err(seat_error)
        .context("预约座位失败")?;

    if args.json {

        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "action": "seat-book",
                "submitted": true,
                "booking_id": booking.id,
                "seat_no": booking.seat_no,
                "area": booking.area_name,
                "date": if booking.day.is_empty() { args.date.clone() } else { booking.day.clone() },
                "begin": booking.begin_time,
                "end": booking.end_time,
            }))?
        );

        return Ok(());
    }

    println!();

    if booking.id.is_empty() {

        println!("预约已提交（服务未返回预约详情，请用 seat-orders 确认）");
    } else {

        println!(
            "预约成功\t{}\t{}\t{} {}-{}",
            booking.seat_no, booking.area_name, booking.day, booking.begin_time, booking.end_time
        );
    }

    Ok(())
}

pub(crate) async fn seat_orders_command(args: SeatOrdersArgs) -> Result<()> {

    let config = load_config(args.config.as_deref())?;

    let api = authenticated_api(&config, args.debug_login).await?;

    let token = seat_token(&api).await?;

    if let Some(booking_id) = args.cancel.as_deref() {

        if !args.yes {

            println!("将取消预约 {booking_id}。确认后加上 --yes 才会真正取消。");

            return Ok(());
        }

        api.libbook_cancel(&token, booking_id)
            .await
            .map_err(seat_error)
            .with_context(|| format!("取消预约 {booking_id} 失败"))?;

        println!("已取消预约 {booking_id}");

        return Ok(());
    }

    let bookings = api
        .libbook_bookings(&token, 1, 20)
        .await
        .map_err(seat_error)?;

    if args.json {

        println!("{}", serde_json::to_string_pretty(&bookings)?);

        return Ok(());
    }

    if bookings.is_empty() {

        println!("没有座位预约记录");

        return Ok(());
    }

    println!("id\t日期\t阅览区\t座位\t时段\t状态");

    for booking in &bookings {

        println!(
            "{}\t{}\t{}\t{}\t{}-{}\t{}",
            booking.id,
            booking.day,
            booking.area_name,
            booking.seat_no,
            booking.begin_time,
            booking.end_time,
            booking.status_name,
        );
    }

    Ok(())
}

/// Chooses a time segment, defaulting to the first offered.
///
/// Why:
/// Segment ids are opaque strings the service defines; the user should be able
/// to omit one and get a working reservation rather than guess.

fn resolve_segment<'a>(
    slots: &'a [crate::libbook::TimeSlot],
    requested: Option<&str>,
) -> Result<&'a crate::libbook::TimeSlot> {

    if slots.is_empty() {

        bail!("该阅览区没有可预约时段");
    }

    match requested {
        Some(requested) => {
            slots
                .iter()
                .find(|slot| slot.id == requested)
                .ok_or_else(|| {

                    let available = slots
                        .iter()
                        .map(|slot| format!("{}={}", slot.id, slot.label))
                        .collect::<Vec<_>>()
                        .join("  ");

                    anyhow::anyhow!("没有时段 {requested}。可用时段: {available}")
                })
        }
        None => Ok(&slots[0]),
    }
}

fn resolve_date(value: Option<String>) -> Result<String> {

    let date = value.unwrap_or_else(|| chrono::Local::now().date_naive().to_string());

    chrono::NaiveDate::parse_from_str(&date, "%Y-%m-%d")
        .with_context(|| format!("日期格式必须是 YYYY-MM-DD: {date}"))?;

    Ok(date)
}

#[cfg(test)]

mod tests {

    use super::{resolve_date, resolve_segment};
    use crate::libbook::TimeSlot;

    fn slot(id: &str, label: &str) -> TimeSlot {

        TimeSlot {
            id:    id.to_string(),
            start: "08:00".to_string(),
            end:   "12:00".to_string(),
            label: label.to_string(),
        }
    }

    #[test]

    fn segment_defaults_to_the_first_offered() {

        let slots = vec![slot("1", "上午"), slot("2", "下午")];

        assert_eq!(resolve_segment(&slots, None).unwrap().id, "1");
    }

    #[test]

    fn unknown_segment_lists_the_available_ones() {

        // The ids are opaque, so the error has to show the choices.
        let slots = vec![slot("1", "上午"), slot("2", "下午")];

        let error = resolve_segment(&slots, Some("99")).unwrap_err().to_string();

        assert!(error.contains("1=上午"), "应列出可用时段: {error}");

        assert!(error.contains("2=下午"));
    }

    #[test]

    fn no_segments_is_an_error() {

        assert!(resolve_segment(&[], None).is_err());
    }

    #[test]

    fn resolve_date_validates_format() {

        assert_eq!(
            resolve_date(Some("2026-03-10".to_string())).unwrap(),
            "2026-03-10"
        );

        assert!(resolve_date(Some("03/10/2026".to_string())).is_err());
    }
}
