//! CLI commands for sunshine clock-in (阳光打卡).
//!
//! Why:
//! Reading progress is harmless, but submitting a clock-in writes a real
//! sports record under the user's name and requires a photo. The two are
//! therefore separate commands, and submission previews by default.

use anyhow::{Context, Result, bail};
use chrono::TimeZone;

use crate::iclass::IClassApi;
use crate::ygdk::YgdkSession;

use super::args::{ClockinArgs, ClockinSubmitArgs};
use super::config::load_config;
use super::venue::authenticated_api;

/// Adds the advice that applies when the YGDK session is missing.

fn clockin_error(error: anyhow::Error) -> anyhow::Error {

    let text = error.to_string();

    if text.contains("code") || text.contains("登录") {

        return anyhow::anyhow!(
            "{text}\n阳光打卡需要先登录 app.buaa.edu.cn，请运行 list-today 建立会话后重试。"
        );
    }

    error
}

async fn clockin_session(api: &IClassApi) -> Result<YgdkSession> {

    api.ygdk_login().await.map_err(clockin_error)
}

pub(crate) async fn clockin_command(args: ClockinArgs) -> Result<()> {

    let config = load_config(args.config.as_deref())?;

    let api = authenticated_api(&config, args.debug_login).await?;

    let session = clockin_session(&api).await?;

    let classifies = api.ygdk_classifies(&session).await.map_err(clockin_error)?;

    if classifies.is_empty() {

        println!("没有查询到阳光打卡类别");

        return Ok(());
    }

    let selected = match args.classify {
        Some(id) => {
            classifies
                .iter()
                .find(|item| item.id == id)
                .ok_or_else(|| {

                    let available = classifies
                        .iter()
                        .map(|item| format!("{}={}", item.id, item.name))
                        .collect::<Vec<_>>()
                        .join("  ");

                    anyhow::anyhow!("没有类别 {id}。可用类别: {available}")
                })?
        }
        None => &classifies[0],
    };

    let count = api
        .ygdk_count(&session, selected.id)
        .await
        .map_err(clockin_error)?;

    let items = api
        .ygdk_items(&session, selected.id)
        .await
        .map_err(clockin_error)?;

    if args.json {

        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "classifies": classifies,
                "selected": selected.id,
                "count": count,
                "items": items,
            }))?
        );

        return Ok(());
    }

    println!("类别\t{}", selected.name);

    println!();

    // Progress is the number a student actually wants, so it leads.
    if count.term_num > 0 {

        let state = if count.satisfied() {

            "已达标"
        } else {

            "未达标"
        };

        println!(
            "达标情况\t{}/{}\t{}\t还差 {} 次",
            count.term_count,
            count.term_num,
            state,
            count.remaining()
        );
    } else {

        println!("本学期次数\t{}", count.term_count);
    }

    println!(
        "周\t{}/{}\t本月\t{}/{}\t今日\t{}",
        count.week_count, count.week_num, count.month_count, count.month_num, count.day_count
    );

    if !items.is_empty() {

        println!();

        println!(
            "可打卡项目\t{}",
            items
                .iter()
                .map(|item| format!("{}={}", item.id, item.name))
                .collect::<Vec<_>>()
                .join("  ")
        );
    }

    if classifies.len() > 1 {

        println!();

        println!(
            "其他类别\t{}",
            classifies
                .iter()
                .filter(|item| item.id != selected.id)
                .map(|item| format!("{}={}", item.id, item.name))
                .collect::<Vec<_>>()
                .join("  ")
        );
    }

    if args.records {

        println!();

        let records = api
            .ygdk_records(&session, selected.id, 1, 20)
            .await
            .map_err(clockin_error)?;

        if records.is_empty() {

            println!("没有打卡记录");
        } else {

            println!("记录");

            for record in &records {

                println!(
                    "\t{}\t{}\t{}\t{} ~ {}",
                    record.id,
                    record.item_name,
                    dash(&record.place),
                    format_stamp(record.start),
                    format_stamp(record.end),
                );
            }
        }
    }

    Ok(())
}

pub(crate) async fn clockin_submit_command(args: ClockinSubmitArgs) -> Result<()> {

    if !args.photo.exists() {

        bail!("找不到照片文件: {}", args.photo.display());
    }

    let config = load_config(args.config.as_deref())?;

    let api = authenticated_api(&config, args.debug_login).await?;

    let session = clockin_session(&api).await?;

    let items = api
        .ygdk_items(&session, args.classify)
        .await
        .map_err(clockin_error)?;

    let item = items
        .iter()
        .find(|item| item.id == args.item)
        .ok_or_else(|| {

            let available = items
                .iter()
                .map(|item| format!("{}={}", item.id, item.name))
                .collect::<Vec<_>>()
                .join("  ");

            anyhow::anyhow!(
                "类别 {} 没有项目 {}。可用项目: {available}",
                args.classify,
                args.item
            )
        })?;

    let (start, end) = resolve_span(args.start.as_deref(), args.end.as_deref())?;

    println!("类别\t{}", args.classify);

    println!("项目\t{}\t{}", item.id, item.name);

    println!("地点\t{}", args.place);

    println!(
        "时间\t{} ~ {}",
        start.format("%Y-%m-%d %H:%M"),
        end.format("%Y-%m-%d %H:%M")
    );

    println!("照片\t{}", args.photo.display());

    if !args.yes {

        println!();

        println!("这是预览。确认无误后加上 --yes 才会真正提交打卡。");

        return Ok(());
    }

    let result = api
        .ygdk_clockin(
            &session,
            args.classify,
            item,
            start,
            end,
            &args.place,
            &args.photo,
        )
        .await
        .map_err(clockin_error)
        .context("提交打卡失败")?;

    println!();

    println!(
        "打卡成功\t{}\t本学期 {} 次",
        result.message.trim(),
        result.term_count
    );

    if let Some(record_id) = result.record_id {

        println!("记录号\t{record_id}");
    }

    Ok(())
}

/// Resolves the clock-in time window.
///
/// Why:
/// The service records a start and end. Defaulting to a plausible forty-minute
/// window that ends now means the common case needs no arguments, while an
/// explicit range is still possible for a session that already happened.

fn resolve_span(
    start: Option<&str>,
    end: Option<&str>,
) -> Result<(
    chrono::DateTime<chrono::FixedOffset>,
    chrono::DateTime<chrono::FixedOffset>,
)> {

    let zone = chrono::FixedOffset::east_opt(8 * 3600).expect("东八区偏移有效");

    let now = chrono::Utc::now().with_timezone(&zone);

    let end_at = match end {
        Some(value) => parse_local(value, zone)?,

        None => now,
    };

    let start_at = match start {
        Some(value) => parse_local(value, zone)?,

        None => end_at - chrono::Duration::minutes(40),
    };

    if start_at >= end_at {

        bail!(
            "开始时间必须早于结束时间：{} ~ {}",
            start_at.format("%Y-%m-%d %H:%M"),
            end_at.format("%Y-%m-%d %H:%M")
        );
    }

    Ok((start_at, end_at))
}

/// Parses `YYYY-MM-DD HH:MM` in Beijing time.

fn parse_local(
    value: &str,
    zone: chrono::FixedOffset,
) -> Result<chrono::DateTime<chrono::FixedOffset>> {

    let naive = chrono::NaiveDateTime::parse_from_str(value.trim(), "%Y-%m-%d %H:%M")
        .with_context(|| format!("时间格式必须是 YYYY-MM-DD HH:MM: {value}"))?;

    zone.from_local_datetime(&naive)
        .single()
        .ok_or_else(|| anyhow::anyhow!("无法解析本地时间: {value}"))
}

/// Formats a unix timestamp in Beijing time.

fn format_stamp(value: Option<i64>) -> String {

    let Some(seconds) = value else {

        return "-".to_string();
    };

    let zone = chrono::FixedOffset::east_opt(8 * 3600).expect("东八区偏移有效");

    chrono::DateTime::from_timestamp(seconds, 0)
        .map(|time| time.with_timezone(&zone).format("%m-%d %H:%M").to_string())
        .unwrap_or_else(|| "-".to_string())
}

fn dash(value: &str) -> &str {

    if value.trim().is_empty() { "-" } else { value }
}

#[cfg(test)]

mod tests {

    use super::{format_stamp, resolve_span};
    use chrono::TimeZone;

    #[test]

    fn span_defaults_to_a_plausible_window_ending_now() {

        let (start, end) = resolve_span(None, None).expect("应有默认区间");

        assert_eq!((end - start).num_minutes(), 40);
    }

    #[test]

    fn explicit_span_is_parsed_in_beijing_time() {

        let (start, end) =
            resolve_span(Some("2026-03-10 06:00"), Some("2026-03-10 06:40")).expect("应能解析");

        assert_eq!(start.format("%H:%M").to_string(), "06:00");

        assert_eq!(end.format("%H:%M").to_string(), "06:40");
    }

    #[test]

    fn reversed_or_equal_span_is_rejected() {

        // The service records a duration; a non-positive one is meaningless and
        // would be submitted as-is.
        assert!(resolve_span(Some("2026-03-10 07:00"), Some("2026-03-10 06:00")).is_err());

        assert!(resolve_span(Some("2026-03-10 06:00"), Some("2026-03-10 06:00")).is_err());
    }

    #[test]

    fn bad_time_format_is_rejected() {

        assert!(resolve_span(Some("2026/03/10 06:00"), None).is_err());

        assert!(resolve_span(Some("昨天"), None).is_err());
    }

    #[test]

    fn timestamps_render_in_beijing_time() {

        // Built from the same zone the formatter uses, so the expectation
        // cannot drift with a mistyped epoch.
        let zone = chrono::FixedOffset::east_opt(8 * 3600).expect("东八区偏移有效");

        let naive = chrono::NaiveDate::from_ymd_opt(2026, 3, 10)
            .and_then(|date| date.and_hms_opt(6, 0, 0))
            .expect("应能构造时间");

        let moment = zone
            .from_local_datetime(&naive)
            .single()
            .expect("应能定位时间");

        assert_eq!(format_stamp(Some(moment.timestamp())), "03-10 06:00");

        assert_eq!(format_stamp(None), "-");
    }
}
