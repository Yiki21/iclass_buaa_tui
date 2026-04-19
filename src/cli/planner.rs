//! Automation command handlers, sign target evaluation, and retry logic.

use anyhow::{Context, Result, anyhow, bail};
use chrono::{
    DateTime, Duration as ChronoDuration, Local, LocalResult, NaiveDate, NaiveDateTime, TimeZone,
    Utc,
};
use serde_json::{Value, json};
use tokio::time::{Duration, sleep};

use crate::{
    bykc::BykcChosenCourse,
    constants::network_urls,
    iclass::IClassApi,
    model::{CourseDetailItem, SignOutcome},
};

use super::args::{ListTodayArgs, PlanArgs, SignArgs};
use super::config::{AutomationConfig, load_config, parse_planner_time};
use super::core::{
    EvaluatedCourse, ListedTarget, PollStatusKind, RetryPolicy, SignAction, SignSource,
};

// Command entry points

/// Prints today's filtered sign targets without attempting any sign action.
pub(crate) async fn list_today(args: ListTodayArgs) -> Result<()> {
    let config = load_config(args.config.as_deref())?;
    let today_courses = fetch_today_targets_with_retry(&config).await?;
    let filtered = filter_targets(today_courses, &config);

    if args.json {
        let rows: Vec<Value> = filtered
            .iter()
            .map(|course| {
                json!({
                    "source": course.source.label(),
                    "action": course.action.label(),
                    "name": course.name,
                    "course_id": course.course_id,
                    "target_id": course.target_id,
                    "date": course.date,
                    "start_time": course.start_time,
                    "end_time": course.end_time,
                    "signed": course.signed,
                })
            })
            .collect();
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }

    if filtered.is_empty() {
        println!("今日无匹配签到目标");
        return Ok(());
    }

    println!("source\taction\tname\tdate\tstart\tend\tcourse_id\ttarget_id\tsigned");
    for course in filtered {
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            course.source.label(),
            course.action.label(),
            course.name,
            course.date,
            course.start_time,
            course.end_time,
            course.course_id,
            course.target_id,
            if course.signed { "yes" } else { "no" }
        );
    }

    Ok(())
}

/// Signs a single course immediately and optionally emits debug diagnostics.
///
/// Why:
/// CLI users often need one-shot manual signing that reuses the same retry and
/// source-selection rules as the planner, but without waiting for a scheduled run.
///
/// How:
/// Normalize the CLI arguments into one internal source/action pair, run the
/// corresponding sign path, then optionally attach extra iClass timing context
/// when `--debug` is requested.
pub(crate) async fn sign_command(args: SignArgs) -> Result<()> {
    let config = load_config(args.config.as_deref())?;
    let retry = RetryPolicy {
        max_attempts: args.retry_count.unwrap_or(config.retry_count),
        interval_seconds: args
            .retry_interval_seconds
            .unwrap_or(config.retry_interval_seconds),
    };
    if retry.max_attempts == 0 {
        bail!("retry_count 必须大于 0");
    }

    let source: SignSource = args.source.into();
    let action: SignAction = args.action.into();

    let (target_id, outcome, result) = match source {
        SignSource::IClass => {
            if action != SignAction::SignIn {
                bail!("iClass 仅支持 sign-in");
            }
            let target_id = args.course_sched_id.trim().to_string();
            let display_name = args
                .course_name
                .clone()
                .unwrap_or_else(|| target_id.clone());
            let outcome =
                sign_iclass_with_retry(&config, &target_id, retry, Some(display_name.clone()))
                    .await?;
            let result = json!({
                "source": source.label(),
                "action": action.label(),
                "target_id": target_id,
                "course_name": display_name,
                "message": outcome.message,
                "success": outcome.success_like,
                "raw_response": outcome.raw_response,
            });
            (target_id, outcome, result)
        }
        SignSource::Bykc => {
            let bykc_course_id = args
                .bykc_course_id
                .ok_or_else(|| anyhow!("BYKC 签到需要 --bykc-course-id"))?;
            let display_name = args
                .course_name
                .clone()
                .unwrap_or_else(|| bykc_course_id.to_string());
            let outcome = sign_bykc_with_retry(
                &config,
                bykc_course_id,
                action,
                retry,
                Some(display_name.clone()),
            )
            .await?;
            let result = json!({
                "source": source.label(),
                "action": action.label(),
                "target_id": bykc_course_id,
                "course_name": display_name,
                "message": outcome.message,
                "success": outcome.success_like,
                "raw_response": outcome.raw_response,
            });
            (bykc_course_id.to_string(), outcome, result)
        }
    };
    let result = if args.debug && source == SignSource::IClass {
        enrich_sign_result_with_debug(result, &config, &target_id, &outcome).await
    } else {
        result
    };
    println!("{}", serde_json::to_string_pretty(&result)?);

    if outcome.success_like {
        return Ok(());
    }

    bail!("签到未成功: {}", outcome.message)
}

/// Runs one planner cycle and signs every target that is currently due.
///
/// Why:
/// The scheduler invokes a single idempotent command repeatedly. Keeping the
/// planner as one cycle makes the platform integration simple on Linux, macOS,
/// and Windows, and avoids shipping our own long-running daemon.
///
/// How:
/// Evaluate all today's filtered targets first, print the same summary used by
/// dry runs, then only execute entries whose state is already `DueNow`.
pub(crate) async fn plan_command(args: PlanArgs) -> Result<()> {
    let config = load_config(args.config.as_deref())?;
    let _unit_prefix = args.unit_prefix;
    let evaluated = evaluate_today_courses(&config).await?;

    if args.dry_run {
        print_evaluated_courses(&evaluated);
        return Ok(());
    }

    let due_targets: Vec<ListedTarget> = evaluated
        .iter()
        .filter(|entry| entry.status == PollStatusKind::DueNow)
        .map(|entry| entry.course.clone())
        .collect();

    if due_targets.is_empty() {
        print_evaluated_summary(&evaluated);
        return Ok(());
    }

    print_evaluated_summary(&evaluated);

    let retry = RetryPolicy {
        max_attempts: config.retry_count,
        interval_seconds: config.retry_interval_seconds,
    };
    let mut failures = Vec::new();

    for target in &due_targets {
        eprintln!(
            "开始签到: [{}:{}] {} ({})",
            target.source.label(),
            target.action.label(),
            target.name,
            target.target_id
        );
        let result = match target.source {
            SignSource::IClass => {
                sign_iclass_with_retry(
                    &config,
                    &target.target_id,
                    retry.clone(),
                    Some(target.name.clone()),
                )
                .await
            }
            SignSource::Bykc => {
                let course_id = target
                    .target_id
                    .parse::<i64>()
                    .with_context(|| format!("无效的 BYKC 课程 id: {}", target.target_id));
                match course_id {
                    Ok(course_id) => {
                        sign_bykc_with_retry(
                            &config,
                            course_id,
                            target.action,
                            retry.clone(),
                            Some(target.name.clone()),
                        )
                        .await
                    }
                    Err(error) => Err(error),
                }
            }
        };
        match result {
            Ok(outcome) => {
                println!(
                    "{}\t{}\t{}\t{}",
                    target.source.label(),
                    target.action.label(),
                    target.name,
                    outcome.message
                );
            }
            Err(error) => {
                failures.push(format!(
                    "[{}:{}] {} ({}) -> {}",
                    target.source.label(),
                    target.action.label(),
                    target.name,
                    target.target_id,
                    error
                ));
            }
        }
    }

    if failures.is_empty() {
        return Ok(());
    }

    bail!("部分课程签到失败:\n{}", failures.join("\n"))
}

// Target loading and planner evaluation

async fn fetch_today_iclass_targets(config: &AutomationConfig) -> Result<Vec<ListedTarget>> {
    if !config.enable_iclass {
        return Ok(Vec::new());
    }

    let api = IClassApi::new(config.use_vpn)?;
    let session = api.login(&config.login_input()).await?;
    let today = Local::now().date_naive().format("%Y-%m-%d").to_string();

    let courses = api.get_merged_course_details(&session, 0).await?;
    Ok(courses
        .into_iter()
        .filter(|course| course.date == today)
        .map(map_iclass_course)
        .collect())
}

/// Loads today's actionable BYKC sign-in/sign-out windows from chosen courses.
async fn fetch_today_bykc_targets(config: &AutomationConfig) -> Result<Vec<ListedTarget>> {
    if !config.enable_bykc {
        return Ok(Vec::new());
    }

    let api = IClassApi::new(config.use_vpn)?;
    let session = api.login(&config.login_input()).await?;
    let bykc_api = session
        .bykc_api
        .ok_or_else(|| anyhow!("BYKC 自动签到需要 VPN 模式登录"))?;
    let today = Local::now().date_naive();
    let chosen_courses = bykc_api.get_chosen_courses().await?;

    let mut targets = Vec::new();
    for course in chosen_courses {
        targets.extend(map_bykc_targets(course, today));
    }
    Ok(targets)
}

/// Fetches today's sign targets, retrying login and API calls on transient failures.
async fn fetch_today_targets_with_retry(config: &AutomationConfig) -> Result<Vec<ListedTarget>> {
    let retry = RetryPolicy {
        max_attempts: config.retry_count,
        interval_seconds: config.retry_interval_seconds,
    };
    let mut last_error = None;

    for attempt in 1..=retry.max_attempts {
        match fetch_today_targets(config).await {
            Ok(courses) => return Ok(courses),
            Err(error) => {
                let message = error.to_string();
                eprintln!(
                    "[attempt {attempt}/{}] 获取今日签到目标失败 -> {}",
                    retry.max_attempts, message
                );
                last_error = Some(error);
                if attempt < retry.max_attempts {
                    sleep(Duration::from_secs(retry.interval_seconds)).await;
                }
            }
        }
    }

    Err(last_error
        .unwrap()
        .context("多次尝试后仍无法获取今日签到目标"))
}

async fn fetch_today_targets(config: &AutomationConfig) -> Result<Vec<ListedTarget>> {
    let mut targets = fetch_today_iclass_targets(config).await?;
    targets.extend(fetch_today_bykc_targets(config).await?);
    Ok(targets)
}

/// Computes planner status for every filtered sign target scheduled today.
async fn evaluate_today_courses(config: &AutomationConfig) -> Result<Vec<EvaluatedCourse>> {
    let courses = filter_targets(fetch_today_targets_with_retry(config).await?, config);
    let now = Local::now();
    let daily_start_at = daily_start_at(config, now)?;
    let mut evaluated = Vec::with_capacity(courses.len());

    for course in courses {
        evaluated.push(evaluate_course(
            course,
            daily_start_at,
            now,
            config.advance_minutes,
        )?);
    }

    Ok(evaluated)
}

/// Classifies one course into the current planner state.
fn evaluate_course(
    course: ListedTarget,
    daily_start_at: DateTime<Local>,
    now: DateTime<Local>,
    advance_minutes: i64,
) -> Result<EvaluatedCourse> {
    if course.signed {
        return Ok(EvaluatedCourse {
            course,
            status: PollStatusKind::Signed,
            available_at: None,
        });
    }

    if course.target_id.trim().is_empty() {
        return Ok(EvaluatedCourse {
            course,
            status: PollStatusKind::MissingCourseSchedId,
            available_at: None,
        });
    }

    let Some(start_at) = build_local_time(&course.date, &course.start_time)? else {
        return Ok(EvaluatedCourse {
            course,
            status: PollStatusKind::MissingCourseSchedId,
            available_at: None,
        });
    };
    let Some(end_at) = build_local_time(&course.date, &course.end_time)? else {
        return Ok(EvaluatedCourse {
            course,
            status: PollStatusKind::Expired,
            available_at: None,
        });
    };

    if end_at <= now {
        return Ok(EvaluatedCourse {
            course,
            status: PollStatusKind::Expired,
            available_at: None,
        });
    }

    let available_at = std::cmp::max(
        daily_start_at,
        if course.source == SignSource::IClass {
            start_at - ChronoDuration::minutes(advance_minutes)
        } else {
            start_at
        },
    );
    let status = if now < daily_start_at {
        PollStatusKind::WaitingForDailyStart
    } else if now < available_at {
        PollStatusKind::WaitingForCourse
    } else {
        PollStatusKind::DueNow
    };

    Ok(EvaluatedCourse {
        course,
        status,
        available_at: Some(available_at),
    })
}

/// Resolves today's planner start time from `planner_time`.
fn daily_start_at(config: &AutomationConfig, now: DateTime<Local>) -> Result<DateTime<Local>> {
    let date = now.date_naive();
    let daily_time = parse_planner_time(&config.planner_time)?;
    let naive = NaiveDateTime::new(date, daily_time);

    match Local.from_local_datetime(&naive) {
        LocalResult::Single(value) => Ok(value),
        LocalResult::Ambiguous(first, _) => Ok(first),
        LocalResult::None => bail!("本地时区无法表示时间: {naive}"),
    }
}

fn map_iclass_course(course: CourseDetailItem) -> ListedTarget {
    let signed = course.signed();
    ListedTarget {
        source: SignSource::IClass,
        action: SignAction::SignIn,
        name: course.name,
        course_id: course.id,
        target_id: course.course_sched_id,
        date: course.date,
        start_time: course.start_time,
        end_time: course.end_time,
        signed,
    }
}

/// Maps one chosen BYKC course into zero, one, or two planner targets.
fn map_bykc_targets(course: BykcChosenCourse, today: NaiveDate) -> Vec<ListedTarget> {
    let mut targets = Vec::new();
    let Some(sign_config) = course.sign_config.as_ref() else {
        return targets;
    };

    if course.checkin == 0
        && let (Some(start_at), Some(end_at)) = (
            parse_cli_local_time(&sign_config.sign_start_date),
            parse_cli_local_time(&sign_config.sign_end_date),
        )
        && end_at.date_naive() >= today
        && start_at.date_naive() <= today
    {
        targets.push(ListedTarget {
            source: SignSource::Bykc,
            action: SignAction::SignIn,
            name: course.course_name.clone(),
            course_id: course.course_id.to_string(),
            target_id: course.course_id.to_string(),
            date: start_at.date_naive().format("%Y-%m-%d").to_string(),
            start_time: start_at.format("%H:%M").to_string(),
            end_time: end_at.format("%H:%M").to_string(),
            signed: course.pass == Some(1),
        });
    }

    if matches!(course.checkin, 5 | 6)
        && let (Some(start_at), Some(end_at)) = (
            parse_cli_local_time(&sign_config.sign_out_start_date),
            parse_cli_local_time(&sign_config.sign_out_end_date),
        )
        && end_at.date_naive() >= today
        && start_at.date_naive() <= today
    {
        targets.push(ListedTarget {
            source: SignSource::Bykc,
            action: SignAction::SignOut,
            name: course.course_name,
            course_id: course.course_id.to_string(),
            target_id: course.course_id.to_string(),
            date: start_at.date_naive().format("%Y-%m-%d").to_string(),
            start_time: start_at.format("%H:%M").to_string(),
            end_time: end_at.format("%H:%M").to_string(),
            signed: course.pass == Some(1),
        });
    }

    targets
}

fn filter_targets(courses: Vec<ListedTarget>, config: &AutomationConfig) -> Vec<ListedTarget> {
    courses
        .into_iter()
        .filter(|course| {
            let include_patterns = config.source_include_patterns(course.source);
            let exclude_patterns = config.source_exclude_patterns(course.source);
            let include_all = include_patterns.iter().any(|pattern| pattern.trim() == "*");
            let included = include_all
                || include_patterns
                    .iter()
                    .any(|pattern| course_matches_pattern(course, pattern));
            let excluded = exclude_patterns
                .iter()
                .any(|pattern| course_matches_pattern(course, pattern));
            included && !excluded
        })
        .collect()
}

/// Matches one normalized target against the configured include/exclude pattern.
fn course_matches_pattern(course: &ListedTarget, pattern: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() {
        return false;
    }

    wildcard_match(pattern, &course.name)
        || wildcard_match(pattern, &course.course_id)
        || wildcard_match(pattern, &course.target_id)
}

/// Applies `*` wildcard matching used by CLI course filters.
fn wildcard_match(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }

    let parts = pattern.split('*').collect::<Vec<_>>();
    if parts.len() == 1 {
        return pattern == text;
    }

    let anchored_start = !pattern.starts_with('*');
    let anchored_end = !pattern.ends_with('*');
    let mut cursor = 0usize;

    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }
        if index == 0 && anchored_start {
            if !text[cursor..].starts_with(part) {
                return false;
            }
            cursor += part.len();
            continue;
        }

        if let Some(found) = text[cursor..].find(part) {
            cursor += found + part.len();
        } else {
            return false;
        }
    }

    if anchored_end && let Some(last) = parts.last() {
        return text.ends_with(last);
    }
    true
}

// Sign execution and diagnostics

async fn sign_iclass_with_retry(
    config: &AutomationConfig,
    course_sched_id: &str,
    retry: RetryPolicy,
    display_name: Option<String>,
) -> Result<SignOutcome> {
    let mut last_error = None;

    for attempt in 1..=retry.max_attempts {
        let api = IClassApi::new(config.use_vpn)?;
        match api.login(&config.login_input()).await {
            Ok(session) => match api.sign_now(&session, course_sched_id).await {
                Ok(outcome) => return Ok(outcome),
                Err(error) => {
                    let name = display_name.as_deref().unwrap_or(course_sched_id);
                    eprintln!(
                        "[attempt {attempt}/{}] iClass 签到失败 -> {} ({}) -> {}",
                        retry.max_attempts, name, course_sched_id, error
                    );
                    last_error = Some(error);
                }
            },
            Err(error) => {
                let name = display_name.as_deref().unwrap_or(course_sched_id);
                eprintln!(
                    "[attempt {attempt}/{}] iClass 登录失败 -> {} ({}) -> {}",
                    retry.max_attempts, name, course_sched_id, error
                );
                last_error = Some(error);
            }
        }

        if attempt < retry.max_attempts {
            sleep(Duration::from_secs(retry.interval_seconds)).await;
        }
    }

    Err(last_error
        .unwrap()
        .context("多次尝试后仍无法完成 iClass 签到"))
}

/// Retries one BYKC sign-in or sign-out operation with fresh login each attempt.
async fn sign_bykc_with_retry(
    config: &AutomationConfig,
    course_id: i64,
    action: SignAction,
    retry: RetryPolicy,
    display_name: Option<String>,
) -> Result<SignOutcome> {
    let mut last_error = None;

    for attempt in 1..=retry.max_attempts {
        let api = IClassApi::new(config.use_vpn)?;
        match api.login(&config.login_input()).await {
            Ok(session) => {
                let bykc_api = session
                    .bykc_api
                    .ok_or_else(|| anyhow!("BYKC 自动签到需要 VPN 模式登录"))?;
                let result = match action {
                    SignAction::SignIn => bykc_api.sign_in(course_id).await,
                    SignAction::SignOut => bykc_api.sign_out(course_id).await,
                };

                match result {
                    Ok(message) => {
                        return Ok(SignOutcome {
                            message,
                            success_like: true,
                            http_status: 200,
                            server_status: "0".to_string(),
                            raw_response: json!({ "course_id": course_id }),
                        });
                    }
                    Err(error) => {
                        let name = display_name
                            .as_deref()
                            .map(str::to_string)
                            .unwrap_or_else(|| course_id.to_string());
                        eprintln!(
                            "[attempt {attempt}/{}] BYKC {} 失败 -> {} ({}) -> {}",
                            retry.max_attempts,
                            action.label(),
                            name,
                            course_id,
                            error
                        );
                        last_error = Some(error);
                    }
                }
            }
            Err(error) => {
                let name = display_name
                    .as_deref()
                    .map(str::to_string)
                    .unwrap_or_else(|| course_id.to_string());
                eprintln!(
                    "[attempt {attempt}/{}] BYKC 登录失败 -> {} ({}) -> {}",
                    retry.max_attempts, name, course_id, error
                );
                last_error = Some(error);
            }
        }

        if attempt < retry.max_attempts {
            sleep(Duration::from_secs(retry.interval_seconds)).await;
        }
    }

    Err(last_error
        .unwrap()
        .context("多次尝试后仍无法完成 BYKC 签到"))
}

async fn enrich_sign_result_with_debug(
    mut result: Value,
    config: &AutomationConfig,
    course_sched_id: &str,
    outcome: &SignOutcome,
) -> Value {
    let debug = collect_sign_debug_context(config, course_sched_id, outcome).await;
    result["debug"] = debug;
    result
}

/// Collects iClass timing and target context for `sign --debug`.
async fn collect_sign_debug_context(
    config: &AutomationConfig,
    course_sched_id: &str,
    outcome: &SignOutcome,
) -> Value {
    let local_now = Utc::now().timestamp_millis();
    let mut data = json!({
        "local_now_utc_millis": local_now,
        "course_sched_id": course_sched_id,
        "http_status": outcome.http_status,
        "server_status": outcome.server_status,
        "message": outcome.message,
        "request_base": network_urls(config.use_vpn).scan_sign,
    });

    let api = match IClassApi::new(config.use_vpn) {
        Ok(api) => api,
        Err(error) => {
            data["debug_error"] = json!(error.to_string());
            return data;
        }
    };

    let session = match api.login(&config.login_input()).await {
        Ok(session) => session,
        Err(error) => {
            data["debug_error"] = json!(format!("登录失败: {error}"));
            return data;
        }
    };

    data["server_time_offset_ms"] = json!(session.server_time_offset_ms);
    data["server_now_millis"] = json!(session.server_now_millis());

    match api.get_merged_course_details(&session, 0).await {
        Ok(details) => {
            let sign_detail = details
                .into_iter()
                .find(|item| item.course_sched_id == course_sched_id);
            data["sign_detail"] = match sign_detail {
                Some(item) => json!({
                    "name": item.name,
                    "id": item.id,
                    "course_sched_id": item.course_sched_id,
                    "date": item.date,
                    "start_time": item.start_time,
                    "end_time": item.end_time,
                    "sign_status": item.sign_status,
                }),
                None => Value::Null,
            };
        }
        Err(error) => {
            data["sign_detail_error"] = json!(error.to_string());
        }
    }

    data
}

// Output formatting and local parsing helpers

/// Builds one local datetime from separate date and time display fields.
fn build_local_time(date: &str, time: &str) -> Result<Option<DateTime<Local>>> {
    let date = date.trim();
    let time = time.trim();
    if date.is_empty() || time.is_empty() {
        return Ok(None);
    }

    let naive = NaiveDateTime::parse_from_str(&format!("{date} {time}:00"), "%Y-%m-%d %H:%M:%S")
        .or_else(|_| {
            NaiveDateTime::parse_from_str(&format!("{date} {time}"), "%Y-%m-%d %H:%M:%S")
        })?;

    match Local.from_local_datetime(&naive) {
        LocalResult::Single(value) => Ok(Some(value)),
        LocalResult::Ambiguous(first, _) => Ok(Some(first)),
        LocalResult::None => bail!("本地时区无法表示时间: {naive}"),
    }
}

/// Prints the full planner table used by `plan --dry-run`.
fn print_evaluated_courses(evaluated: &[EvaluatedCourse]) {
    if evaluated.is_empty() {
        println!("今日无匹配签到目标");
        return;
    }

    println!(
        "source\taction\tstatus\tavailable_at\tname\tdate\tstart\tend\tcourse_id\ttarget_id\tsigned"
    );
    for entry in evaluated {
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            entry.course.source.label(),
            entry.course.action.label(),
            poll_status_label(entry.status),
            entry
                .available_at
                .map(|value| value.to_rfc3339())
                .unwrap_or_default(),
            entry.course.name,
            entry.course.date,
            entry.course.start_time,
            entry.course.end_time,
            entry.course.course_id,
            entry.course.target_id,
            if entry.course.signed { "yes" } else { "no" }
        );
    }
}

/// Prints the compact planner summary used by normal `plan` runs.
fn print_evaluated_summary(evaluated: &[EvaluatedCourse]) {
    let due_now = evaluated
        .iter()
        .filter(|entry| entry.status == PollStatusKind::DueNow)
        .count();
    let waiting = evaluated
        .iter()
        .filter(|entry| {
            matches!(
                entry.status,
                PollStatusKind::WaitingForDailyStart | PollStatusKind::WaitingForCourse
            )
        })
        .count();
    let signed = evaluated
        .iter()
        .filter(|entry| entry.status == PollStatusKind::Signed)
        .count();
    let expired = evaluated
        .iter()
        .filter(|entry| entry.status == PollStatusKind::Expired)
        .count();
    let missing = evaluated
        .iter()
        .filter(|entry| entry.status == PollStatusKind::MissingCourseSchedId)
        .count();

    let iclass = evaluated
        .iter()
        .filter(|entry| entry.course.source == SignSource::IClass)
        .count();
    let bykc_sign_in = evaluated
        .iter()
        .filter(|entry| {
            entry.course.source == SignSource::Bykc && entry.course.action == SignAction::SignIn
        })
        .count();
    let bykc_sign_out = evaluated
        .iter()
        .filter(|entry| {
            entry.course.source == SignSource::Bykc && entry.course.action == SignAction::SignOut
        })
        .count();

    println!(
        "今日签到汇总: iclass={iclass}, bykc_sign_in={bykc_sign_in}, bykc_sign_out={bykc_sign_out}, due_now={due_now}, waiting={waiting}, signed={signed}, expired={expired}, missing_target_id={missing}"
    );
}

fn poll_status_label(status: PollStatusKind) -> &'static str {
    match status {
        PollStatusKind::WaitingForDailyStart => "waiting-daily-start",
        PollStatusKind::WaitingForCourse => "waiting-course-window",
        PollStatusKind::DueNow => "due-now",
        PollStatusKind::Signed => "signed",
        PollStatusKind::Expired => "expired",
        PollStatusKind::MissingCourseSchedId => "missing-target-id",
    }
}

fn parse_cli_local_time(value: &str) -> Option<DateTime<Local>> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }

    let naive = NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S").ok()?;
    match Local.from_local_datetime(&naive) {
        LocalResult::Single(value) => Some(value),
        LocalResult::Ambiguous(first, _) => Some(first),
        LocalResult::None => None,
    }
}
