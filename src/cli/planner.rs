//! Automation command handlers, sign target evaluation, and retry logic.

use anyhow::{Context, Result, anyhow, bail};
use chrono::{
    DateTime, Datelike, Duration as ChronoDuration, Local, LocalResult, NaiveDate, NaiveDateTime,
    TimeZone, Utc,
};
use serde_json::{Value, json};
use sha1::{Digest, Sha1};
use std::{
    fs,
    path::PathBuf,
    time::{Duration as StdDuration, SystemTime},
};
use tokio::time::{Duration, sleep};

use crate::{
    academic::{ClassroomRoom, ExamItem, GradeItem},
    bykc::{BykcChosenCourse, BykcSignAction},
    constants::network_urls,
    iclass::IClassApi,
    model::{CourseDetailItem, LoginFailureKind, Session, SignOutcome},
    schedule::{
        SemesterSchedule, cached_semester, diff_semesters, load_cached_schedules,
        render_schedule_export, save_cached_schedule,
    },
};

use super::args::{
    AcademicListArgs, ClassroomArgs, DoctorArgs, ListTodayArgs, NotifyArgs, PlanArgs,
    ScheduleDiffArgs, ScheduleExportArgs, SignArgs, TaskArgs, TodayArgs,
};
use super::config::{AutomationConfig, load_config, parse_planner_time};
use super::core::{
    EvaluatedCourse, ListedTarget, PollStatusKind, RetryPolicy, SignAction, SignSource,
};

#[derive(Debug, Clone)]

struct FilterDecision {
    include_patterns: Vec<String>,
    exclude_patterns: Vec<String>,
    include_matches:  Vec<String>,
    exclude_matches:  Vec<String>,
    matched_include:  bool,
    matched_exclude:  bool,
    included:         bool,
}

#[derive(Debug, Clone)]

struct DryRunCourse {
    course:     ListedTarget,
    filter:     FilterDecision,
    evaluation: Option<EvaluatedCourse>,
}

#[derive(Debug)]

struct ClassifiedError {
    error:       anyhow::Error,
    retryable:   bool,
    reason:      &'static str,
    description: String,
}

/// One authenticated planner cycle.  iClass and BYKC share this session so a
/// single scheduled run does not perform two identical SSO logins.

struct PlannerSession {
    api:     IClassApi,
    session: Session,
}

struct PlannerLock {
    path: PathBuf,
}

impl PlannerLock {
    fn acquire(account: &str) -> Result<Self> {

        let base = if let Some(path) = std::env::var_os("XDG_STATE_HOME") {

            PathBuf::from(path)
        } else if let Some(path) = std::env::var_os("HOME") {

            PathBuf::from(path).join(".local/state")
        } else {

            bail!("找不到 planner 状态目录");
        };

        let digest = Sha1::digest(account.as_bytes());

        let key = digest
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();

        let path = base.join("iclass-buaa").join(format!("planner-{key}.lock"));

        fs::create_dir_all(path.parent().unwrap_or(&base))?;

        if path.exists() {

            let stale = fs::metadata(&path)
                .and_then(|metadata| metadata.modified())
                .ok()
                .and_then(|modified| SystemTime::now().duration_since(modified).ok())
                .is_some_and(|age| age > StdDuration::from_secs(2 * 60 * 60));

            if stale {

                let _ = fs::remove_dir(&path);
            }
        }

        fs::create_dir(&path)
            .with_context(|| format!("planner 已在运行，锁目录存在: {}", path.display()))?;

        fs::write(path.join("pid"), std::process::id().to_string())?;

        Ok(Self { path })
    }
}

impl Drop for PlannerLock {
    fn drop(&mut self) {

        let _ = fs::remove_dir_all(&self.path);
    }
}

impl PlannerSession {
    async fn establish(config: &AutomationConfig, debug_login: bool) -> Result<Self> {

        let api = IClassApi::new(config.use_vpn)?;

        let session = login_session(&api, &config.login_input(), debug_login).await?;

        Ok(Self { api, session })
    }

    async fn fetch_iclass_targets(&self) -> Result<Vec<ListedTarget>> {

        if self.session.api.use_vpn != self.api.use_vpn {

            bail!("planner 会话连接模式不一致");
        }

        let today = Local::now().date_naive().format("%Y-%m-%d").to_string();

        let courses = self.api.get_merged_course_details(&self.session, 0).await?;

        Ok(courses
            .into_iter()
            .filter(|course| course.date == today)
            .map(map_iclass_course)
            .collect())
    }

    async fn fetch_bykc_targets(&self) -> Result<Vec<ListedTarget>> {

        let bykc_api = self
            .session
            .bykc_api
            .as_ref()
            .ok_or_else(|| anyhow!("BYKC 自动签到需要 VPN 模式登录"))?;

        let today = Local::now().date_naive();

        let chosen_courses = bykc_api.get_chosen_courses().await?;

        let mut targets = Vec::new();

        for course in chosen_courses {

            targets.extend(map_bykc_targets(course, today));
        }

        Ok(targets)
    }
}

impl From<SignAction> for BykcSignAction {
    fn from(value: SignAction) -> Self {

        match value {
            SignAction::SignIn => Self::SignIn,
            SignAction::SignOut => Self::SignOut,
        }
    }
}

// Command entry points

/// Prints today's filtered sign targets without attempting any sign action.

pub(crate) async fn list_today(args: ListTodayArgs) -> Result<()> {

    let config = load_config(args.config.as_deref())?;

    let today_courses = fetch_today_targets_with_retry(&config, args.debug_login).await?;

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

    // Signing changes real attendance state, so it needs the same explicit
    // confirmation the booking commands require.
    if !args.yes {

        let source: SignSource = args.source.into();

        let action: SignAction = args.action.into();

        if args.json {

            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "action": "sign",
                    "submitted": false,
                    "target": {
                        "source": source.label(),
                        "action": action.label(),
                        "course_sched_id": args.course_sched_id,
                        "bykc_course_id": args.bykc_course_id,
                        "course_name": args.course_name,
                    },
                    "hint": "加上 --yes 才会真正签到",
                }))?
            );
        } else {

            println!(
                "将签到\t{}\t{}\t{}",
                source.label(),
                action.label(),
                args.course_name.as_deref().unwrap_or_else(|| {

                    if args.bykc_course_id.is_some() {

                        "BYKC 课程"
                    } else {

                        "(未命名)"
                    }
                }),
            );

            println!();

            println!("这是预览。确认无误后加上 --yes 才会真正签到。");
        }

        return Ok(());
    }

    let retry = RetryPolicy {
        max_attempts:      args.retry_count.unwrap_or(config.retry_count),
        interval_seconds:  args
            .retry_interval_seconds
            .unwrap_or(config.retry_interval_seconds),
        max_delay_seconds: 60,
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

            let outcome = sign_iclass_with_retry(
                &config,
                &target_id,
                retry,
                Some(display_name.clone()),
                args.debug_login,
                None,
            )
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
                args.debug_login,
                None,
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

    // JSON by request; a one-line summary otherwise, since this is usually run
    // by hand or by a scheduler that reads the exit status.
    if args.json {

        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {

        println!(
            "{}\t{}\t{}\t{}",
            source.label(),
            action.label(),
            result
                .get("course_name")
                .and_then(|value| value.as_str())
                .unwrap_or("-"),
            outcome.message,
        );
    }

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

    let _planner_lock = PlannerLock::acquire(&config.student_id)?;

    let run_id = planner_run_id();

    let _unit_prefix = args.unit_prefix;

    if args.dry_run {

        let evaluated = evaluate_today_courses_for_dry_run(&config, args.debug_login).await?;

        if args.json {

            let only_evaluated: Vec<EvaluatedCourse> = evaluated
                .iter()
                .filter_map(|e| e.evaluation.clone())
                .collect();

            println!(
                "{}",
                serde_json::to_string_pretty(&evaluated_json(&only_evaluated))?
            );
        } else {

            print_dry_run_courses(&evaluated);
        }

        return Ok(());
    }

    // A planner cycle signs real attendance state. Without confirmation it
    // reports what it would have done and stops, so a mistyped invocation
    // cannot sign anything.
    if !args.yes {

        let evaluated = evaluate_today_courses_for_dry_run(&config, args.debug_login).await?;

        if args.json {

            println!(
                "{}",
                serde_json::to_string_pretty(&json!({
                    "action": "plan",
                    "submitted": false,
                    "evaluated": evaluated_json(
                        &evaluated
                            .iter()
                            .filter_map(|entry| entry.evaluation.clone())
                            .collect::<Vec<_>>()
                    ),
                    "hint": "加上 --yes 才会真正签到；--dry-run 只评估不登录签到",
                }))?
            );
        } else {

            print_dry_run_courses(&evaluated);

            println!();

            println!("这是预览。确认无误后加上 --yes 才会真正签到。");
        }

        return Ok(());
    }

    let evaluated = evaluate_today_courses(&config, args.debug_login).await?;

    let due_targets: Vec<ListedTarget> = evaluated
        .iter()
        .filter(|entry| entry.status == PollStatusKind::DueNow)
        .map(|entry| entry.course.clone())
        .collect();

    let mut due_targets = due_targets;

    due_targets.sort_by_key(|target| target_deadline(target));

    if due_targets.is_empty() {

        print_evaluated_summary(&evaluated);

        log_planner_completed(&run_id, &evaluated, 0, 0);

        return Ok(());
    }

    print_evaluated_summary(&evaluated);

    let retry = RetryPolicy {
        max_attempts:      config.retry_count,
        interval_seconds:  config.retry_interval_seconds,
        max_delay_seconds: 60,
    };

    let mut failures = Vec::new();

    let mut succeeded = 0_u32;

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
                    args.debug_login,
                    target_deadline(target),
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
                            args.debug_login,
                            target_deadline(target),
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

                if !outcome.success_like {

                    notify_sign_failure(&config, target, &outcome.message);

                    failures.push(format!(
                        "[{}:{}] {} ({}) -> {}",
                        target.source.label(),
                        target.action.label(),
                        target.name,
                        target.target_id,
                        outcome.message
                    ));
                } else {

                    notify_sign_success(&config, target, &outcome.message);

                    succeeded += 1;
                }
            }
            Err(error) => {

                notify_sign_failure(&config, target, &error.to_string());

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

        log_planner_completed(&run_id, &evaluated, succeeded, 0);

        return Ok(());
    }

    log_planner_completed(&run_id, &evaluated, succeeded, failures.len() as u32);

    bail!("部分课程签到失败:\n{}", failures.join("\n"))
}

/// Notifies the user that one sign attempt failed.
///
/// Why:
/// A locked account or an expired session is exactly the case where the run
/// continues but nothing gets signed. Without a notification the only signal is
/// a non-zero exit status in a scheduler the user never reads.
///
/// How:
/// Best effort. Notification tooling is frequently missing, and failing to
/// notify must not change the outcome of the sign attempt itself.

/// Sends a test notification.
///
/// Why:
/// Notification tooling is platform-specific and frequently absent. A user who
/// never receives a failure alert needs a way to learn that notifications do
/// not work on this machine, rather than assuming there were no failures.

pub(crate) fn notify_command(args: NotifyArgs) -> Result<()> {

    let body = args
        .message
        .unwrap_or_else(|| "如果你看到这条通知，说明签到失败的桌面提醒可以正常工作。".to_string());

    match crate::notify::notify(
        "iClass BUAA 通知测试",
        &body,
        crate::notify::Urgency::Normal,
    ) {
        Ok(()) => {

            println!("测试通知已发送");

            Ok(())
        }
        Err(error) => {

            eprintln!("测试通知发送失败: {error}");

            bail!(
                "无法发送桌面通知: {error}\n常见的可能原因：\n  - 未安装 \
                 notify-send（Debian/Ubuntu 上属于 libnotify-bin 包）\n  - \
                 当前没有图形会话（SSH、容器或 systemd \
                 服务里常见）\n如果这台机器不需要提醒，可在配置里设置 notify_on_failure = false"
            );
        }
    }
}

fn notify_sign_failure(config: &AutomationConfig, target: &ListedTarget, reason: &str) {

    if !config.notify_on_failure {

        return;
    }

    let summary = format!("签到失败：{}", target.name.trim());

    let body = format!(
        "[{}:{}] {}\n{}",
        target.source.label(),
        target.action.label(),
        target.target_id,
        reason
    );

    if let Err(error) = crate::notify::notify(&summary, &body, crate::notify::Urgency::Critical) {

        // Reported, not fatal: the run's exit status still reflects the
        // sign-in outcome, which is what the scheduler acts on.
        eprintln!("桌面通知发送失败: {error}");
    }
}

/// Notifies the user that a run signed something.
///
/// Why:
/// Success is quieter than failure but still worth one line: it is how the
/// user learns the automation is alive without opening anything.

fn notify_sign_success(config: &AutomationConfig, target: &ListedTarget, message: &str) {

    if !config.notify_on_success {

        return;
    }

    let summary = format!("已签到：{}", target.name.trim());

    let body = format!(
        "[{}:{}] {}",
        target.source.label(),
        target.action.label(),
        message.trim()
    );

    if let Err(error) = crate::notify::notify(&summary, &body, crate::notify::Urgency::Normal) {

        eprintln!("桌面通知发送失败: {error}");
    }
}

/// Serializes an evaluation run for machine-readable output.

fn evaluated_json(evaluated: &[EvaluatedCourse]) -> serde_json::Value {

    serde_json::Value::Array(
        evaluated
            .iter()
            .map(|entry| {

                json!({
                    "source": entry.course.source.label(),
                    "action": entry.course.action.label(),
                    "course_name": entry.course.name,
                    "target_id": entry.course.target_id,
                    "date": entry.course.date,
                    "start_time": entry.course.start_time,
                    "end_time": entry.course.end_time,
                    "already_signed": entry.course.signed,
                    "status": format!("{:?}", entry.status),
                })
            })
            .collect(),
    )
}

fn planner_run_id() -> String {

    format!(
        "{}-{}",
        Utc::now().format("%Y%m%dT%H%M%SZ"),
        std::process::id()
    )
}

fn log_planner_completed(run_id: &str, evaluated: &[EvaluatedCourse], succeeded: u32, failed: u32) {

    let due = evaluated
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

    crate::logging::event(
        crate::logging::LogLevel::Info,
        "planner.completed",
        format!("planner run completed: {run_id}"),
        json!({
            "run_id": run_id,
            "targets": evaluated.len(),
            "due": due,
            "waiting": waiting,
            "signed": signed,
            "expired": expired,
            "missing_target_id": missing,
            "succeeded": succeeded,
            "failed": failed,
        }),
    );

    println!(
        "planner-run\trun_id={}\ttargets={}\tdue={}\twaiting={}\tsigned={}\texpired={}\\
         tmissing={}\tsucceeded={}\tfailed={}",
        run_id,
        evaluated.len(),
        due,
        waiting,
        signed,
        expired,
        missing,
        succeeded,
        failed,
    );
}

pub(crate) async fn doctor_command(args: DoctorArgs) -> Result<()> {

    let config = load_config(args.config.as_deref())?;

    let api = IClassApi::new(config.use_vpn)?;

    let report = api.doctor().await;

    if args.json {

        println!("{}", serde_json::to_string_pretty(&report)?);

        return Ok(());
    }

    println!(
        "doctor | mode={}",
        if report.use_vpn { "vpn" } else { "direct" }
    );

    for check in report.checks {

        println!(
            "{}\tok={}\t{}ms\tstatus={}\thttp={}\tfinal_url={}\taddrs={}\tsuggestion={}",
            check.name,
            if check.ok { "yes" } else { "no" },
            check.elapsed_ms,
            check.status,
            check
                .http_status
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            check.final_url.unwrap_or_else(|| "-".to_string()),
            if check.resolved_addrs.is_empty() {

                "-".to_string()
            } else {

                check.resolved_addrs.join(",")
            },
            check.suggestion
        );
    }

    Ok(())
}

pub(crate) async fn today_command(args: TodayArgs) -> Result<()> {

    let config = load_config(args.config.as_deref())?;

    let (api, session, account) = academic_session(&config, false).await?;

    let semesters = ensure_cached_semesters(&api, &session, &account, None).await?;

    let semester = cached_semester(&semesters, None)?;

    let today = Local::now().date_naive();

    let week = semester
        .weeks
        .iter()
        .find(|week| date_in_range(today, &week.start_date, &week.end_date));

    let entries = week
        .and_then(|week| semester.schedules.get(&week.number))
        .map(|schedule| {

            schedule
                .entries
                .iter()
                .filter(|entry| {

                    entry.day_of_week == Some(today.weekday().number_from_monday() as usize)
                })
                .collect::<Vec<_>>()
        })
        .unwrap_or_default();

    if args.json {

        println!("{}", serde_json::to_string_pretty(&entries)?);

        return Ok(());
    }

    if entries.is_empty() {

        println!("今天没有课程");

        return Ok(());
    }

    println!("time\tcourse\tplace\tteacher");

    for entry in entries {

        println!(
            "{}-{}\t{}\t{}\t{}",
            entry.begin_time.as_deref().unwrap_or("--:--"),
            entry.end_time.as_deref().unwrap_or("--:--"),
            entry.course_name,
            entry.place.as_deref().unwrap_or("-"),
            entry.weeks_and_teachers.as_deref().unwrap_or("-")
        );
    }

    Ok(())
}

pub(crate) async fn exams_command(args: AcademicListArgs) -> Result<()> {

    let config = load_config(args.config.as_deref())?;

    let (api, session, account) = academic_session(&config, args.debug_login).await?;

    let semesters = ensure_cached_semesters(&api, &session, &account, args.term.as_deref()).await?;

    let term = cached_semester(&semesters, args.term.as_deref())?;

    let exams = api.get_exams(&term.term_code).await?;

    if args.json {

        println!("{}", serde_json::to_string_pretty(&exams)?);
    } else {

        print_exams(&exams);
    }

    Ok(())
}

pub(crate) async fn grades_command(args: AcademicListArgs) -> Result<()> {

    let config = load_config(args.config.as_deref())?;

    let (api, session, account) = academic_session(&config, args.debug_login).await?;

    let semesters = ensure_cached_semesters(&api, &session, &account, args.term.as_deref()).await?;

    if args.all {

        let mut term_codes: Vec<String> = semesters
            .iter()
            .flat_map(|semester| semester.terms.iter().map(|term| term.code.clone()))
            .filter(|code| crate::academic::looks_like_score_term(code))
            .collect();

        term_codes.sort();

        term_codes.dedup();

        if term_codes.is_empty() {

            bail!("课表缓存里没有可用于成绩查询的学期，请先运行 schedule-import");
        }

        let loaded = api.get_grades_for_terms(&term_codes).await;

        let summary = crate::academic::summarize_grades(&loaded.grades);

        if args.json {

            println!(
                "{}",
                serde_json::to_string_pretty(&serde_json::json!({
                    "grades": loaded.grades,
                    "failed_terms": loaded.failed,
                    "summary": summary,
                }))?
            );
        } else {

            print_grades(&loaded.grades);

            print_grade_summary(&summary);

            for (term, error) in &loaded.failed {

                eprintln!("学期 {term} 加载失败: {error}");
            }
        }

        return Ok(());
    }

    let term = cached_semester(&semesters, args.term.as_deref())?;

    let grades = api.get_grades(&term.term_code).await?;

    if args.json {

        println!("{}", serde_json::to_string_pretty(&grades)?);
    } else {

        print_grades(&grades);
    }

    Ok(())
}

fn print_grade_summary(summary: &crate::academic::GradeSummary) {

    println!();

    match summary.weighted_gpa {
        Some(gpa) => {

            println!(
                "GPA {gpa:.3}  学分 {:.1}  计入 {} 门  跳过 {} 门",
                summary.total_credits, summary.counted, summary.skipped
            )
        }
        None => println!("没有可计入 GPA 的成绩"),
    }

    if let Some(score) = summary.weighted_score {

        println!("学分加权平均分 {score:.2}");
    }

    if summary.failed > 0 {

        println!("不及格 {} 门", summary.failed);
    }
}

pub(crate) async fn classrooms_command(args: ClassroomArgs) -> Result<()> {

    let config = load_config(args.config.as_deref())?;

    let (api, _session, _account) = academic_session(&config, args.debug_login).await?;

    let date = args
        .date
        .unwrap_or_else(|| Local::now().date_naive().to_string());

    NaiveDate::parse_from_str(&date, "%Y-%m-%d")
        .with_context(|| format!("日期格式必须是 YYYY-MM-DD: {date}"))?;

    let rooms = api.query_classrooms(args.campus, &date).await?;

    let rooms = if let Some(section) = args.section {

        rooms
            .into_iter()
            .filter(|room| room.free_sections.contains(&section))
            .collect::<Vec<_>>()
    } else {

        rooms
    };

    if args.json {

        println!("{}", serde_json::to_string_pretty(&rooms)?);
    } else {

        print_classrooms(&rooms, args.section);
    }

    Ok(())
}

pub(crate) async fn tasks_command(args: TaskArgs) -> Result<()> {

    let config = load_config(args.config.as_deref())?;

    let api = IClassApi::new(config.use_vpn)?;

    let _session = login_session(&api, &config.login_input(), args.debug_login).await?;

    let tasks = api.get_assignments().await?;

    if args.json {

        println!("{}", serde_json::to_string_pretty(&tasks)?);
    } else if tasks.is_empty() {

        println!("没有读取到希冀作业");
    } else {

        println!("source\tcourse\ttitle\tstart\tdue\tscore\tproblems\tstatus");

        for task in tasks {

            // "2/3" is the difference between done and not done; the status
            // word alone cannot express it.
            let problems = if task.total > 0 {

                format!("{}/{}", task.submitted, task.total)
            } else {

                "-".to_string()
            };

            let score = match (task.score.as_deref(), task.max_score.as_deref()) {
                (Some(score), Some(max)) => format!("{score}/{max}"),
                (Some(score), None) => score.to_string(),
                (None, Some(max)) => format!("-/{max}"),
                (None, None) => "-".to_string(),
            };

            println!(
                "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
                task.source,
                task.course_name,
                task.title,
                task.start_time.as_deref().unwrap_or("-"),
                task.due_time.as_deref().unwrap_or("-"),
                score,
                problems,
                task.status,
            );
        }
    }

    Ok(())
}

pub(crate) async fn schedule_export_command(args: ScheduleExportArgs) -> Result<()> {

    let config = load_config(args.config.as_deref())?;

    let semesters = load_cached_schedules(&config.student_id)?;

    let semester = cached_semester(&semesters, args.term.as_deref())?;

    let body = render_schedule_export(semester, &args.format)?;

    if let Some(path) = args.output {

        fs::write(&path, body).with_context(|| format!("写入导出文件失败: {}", path.display()))?;

        println!("已导出: {}", path.display());
    } else {

        print!("{body}");
    }

    Ok(())
}

pub(crate) async fn schedule_diff_command(args: ScheduleDiffArgs) -> Result<()> {

    let config = load_config(args.config.as_deref())?;

    let semesters = load_cached_schedules(&config.student_id)?;

    if semesters.len() < 2 {

        bail!("至少需要两个已缓存学期才能比较");
    }

    let from = args
        .from
        .as_deref()
        .and_then(|code| semesters.iter().find(|item| item.term_code == code))
        .unwrap_or_else(|| semesters.last().unwrap());

    let to = args
        .to
        .as_deref()
        .and_then(|code| semesters.iter().find(|item| item.term_code == code))
        .unwrap_or(&semesters[0]);

    let changes = diff_semesters(from, to);

    if args.json {

        println!("{}", serde_json::to_string_pretty(&changes)?);
    } else if changes.is_empty() {

        println!("{} 与 {} 没有课表差异", from.term_code, to.term_code);
    } else {

        for change in changes {

            println!(
                "{}\t第{}周\t周{}\t{}\t{}",
                change.kind,
                change.week,
                change
                    .day
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "-".to_string()),
                change.course_name,
                change.detail
            );
        }
    }

    Ok(())
}

async fn academic_session(
    config: &AutomationConfig,
    debug_login: bool,
) -> Result<(IClassApi, crate::model::Session, String)> {

    let api = IClassApi::new(config.use_vpn)?;

    let session = login_session(&api, &config.login_input(), debug_login).await?;

    Ok((api, session, config.student_id.clone()))
}

async fn ensure_cached_semesters(
    api: &IClassApi,
    session: &crate::model::Session,
    account: &str,
    requested_term: Option<&str>,
) -> Result<Vec<SemesterSchedule>> {

    let mut semesters = load_cached_schedules(account)?;

    let has_requested = requested_term
        .map(|term| semesters.iter().any(|item| item.term_code == term))
        .unwrap_or_else(|| !semesters.is_empty());

    if !has_requested {

        let imported = api
            .import_semester_schedule(session, requested_term)
            .await?;

        save_cached_schedule(account, imported)?;

        semesters = load_cached_schedules(account)?;
    }

    Ok(semesters)
}

fn print_exams(exams: &[ExamItem]) {

    if exams.is_empty() {

        println!("没有考试安排");

        return;
    }

    println!("course\tdate\ttime\tplace\tseat\ttype");

    for exam in exams {

        println!(
            "{}\t{}\t{}-{}\t{}\t{}\t{}",
            exam.course_name,
            exam.exam_date.as_deref().unwrap_or("-"),
            exam.start_time.as_deref().unwrap_or("--:--"),
            exam.end_time.as_deref().unwrap_or("--:--"),
            exam.place.as_deref().unwrap_or("-"),
            exam.seat.as_deref().unwrap_or("-"),
            exam.exam_type.as_deref().unwrap_or("-")
        );
    }
}

fn print_grades(grades: &[GradeItem]) {

    if grades.is_empty() {

        println!("没有成绩记录");

        return;
    }

    println!("course\tcode\tcredit\tscore\tpoint\tstatus");

    for grade in grades {

        println!(
            "{}\t{}\t{}\t{}\t{}\t{}",
            grade.course_name,
            grade.course_code.as_deref().unwrap_or("-"),
            grade
                .credit
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string()),
            grade.score.as_deref().unwrap_or("-"),
            grade.grade_point.as_deref().unwrap_or("-"),
            grade.passed.as_deref().unwrap_or("-")
        );
    }
}

fn print_classrooms(rooms: &[ClassroomRoom], section: Option<usize>) {

    if rooms.is_empty() {

        println!("没有符合条件的空教室");

        return;
    }

    println!("building\troom\tfree_sections");

    for room in rooms {

        println!(
            "{}\t{}\t{}",
            room.building,
            room.name,
            room.free_sections
                .iter()
                .map(ToString::to_string)
                .collect::<Vec<_>>()
                .join(",")
        );
    }

    if let Some(section) = section {

        println!("仅显示第 {section} 节空闲教室");
    }
}

fn date_in_range(today: NaiveDate, start: &str, end: &str) -> bool {

    NaiveDate::parse_from_str(start, "%Y-%m-%d")
        .ok()
        .zip(NaiveDate::parse_from_str(end, "%Y-%m-%d").ok())
        .is_some_and(|(start, end)| today >= start && today <= end)
}

// Target loading and planner evaluation

/// Fetches today's sign targets, retrying login and API calls on transient failures.

async fn fetch_today_targets_with_retry(
    config: &AutomationConfig,
    debug_login: bool,
) -> Result<Vec<ListedTarget>> {

    let retry = RetryPolicy {
        max_attempts:      config.retry_count,
        interval_seconds:  config.retry_interval_seconds,
        max_delay_seconds: 60,
    };

    let mut last_error = None;

    for attempt in 1..=retry.max_attempts {

        let result = async {

            let planner = PlannerSession::establish(config, debug_login).await?;

            let mut targets = Vec::new();

            let mut failures = Vec::new();

            let mut successful_sources = 0_u8;

            if config.enable_iclass {

                match planner.fetch_iclass_targets().await {
                    Ok(items) => {

                        successful_sources += 1;

                        targets.extend(items);
                    }
                    Err(error) => failures.push(format!("iClass: {error}")),
                }
            }

            if config.enable_bykc {

                match planner.fetch_bykc_targets().await {
                    Ok(items) => {

                        successful_sources += 1;

                        targets.extend(items);
                    }
                    Err(error) => failures.push(format!("BYKC: {error}")),
                }
            }

            if successful_sources > 0 {

                for failure in failures {

                    eprintln!("目标来源暂时不可用，继续处理其他来源: {failure}");
                }

                Ok::<Vec<ListedTarget>, anyhow::Error>(targets)
            } else {

                bail!("所有签到目标来源均失败: {}", failures.join("; "))
            }
        }
        .await;

        match result {
            Ok(courses) => return Ok(courses),
            Err(error) => {

                let classified = classify_anyhow_error(error);

                print_retry_decision("获取今日签到目标", attempt, &retry, &classified);

                let should_retry = classified.retryable && attempt < retry.max_attempts;

                let delay_seconds = retry.delay_seconds(attempt);

                last_error = Some(classified.error);

                if should_retry {

                    sleep(Duration::from_secs(delay_seconds)).await;
                } else {

                    break;
                }
            }
        }
    }

    Err(last_error
        .unwrap()
        .context("多次尝试后仍无法获取今日签到目标"))
}

/// Computes planner status for every filtered sign target scheduled today.

async fn evaluate_today_courses(
    config: &AutomationConfig,
    debug_login: bool,
) -> Result<Vec<EvaluatedCourse>> {

    let courses = filter_targets(
        fetch_today_targets_with_retry(config, debug_login).await?,
        config,
    );

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

/// Computes dry-run rows for all loaded targets, including filtered-out courses.

async fn evaluate_today_courses_for_dry_run(
    config: &AutomationConfig,
    debug_login: bool,
) -> Result<Vec<DryRunCourse>> {

    let courses = fetch_today_targets_with_retry(config, debug_login).await?;

    let now = Local::now();

    let daily_start_at = daily_start_at(config, now)?;

    let mut evaluated = Vec::with_capacity(courses.len());

    for course in courses {

        let filter = explain_filter_decision(&course, config);

        let evaluation = if filter.included {

            Some(evaluate_course(
                course.clone(),
                daily_start_at,
                now,
                config.advance_minutes,
            )?)
        } else {

            None
        };

        evaluated.push(DryRunCourse {
            course,
            filter,
            evaluation,
        });
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
            source:     SignSource::Bykc,
            action:     SignAction::SignIn,
            name:       course.course_name.clone(),
            course_id:  course.course_id.to_string(),
            target_id:  course.course_id.to_string(),
            date:       start_at.date_naive().format("%Y-%m-%d").to_string(),
            start_time: start_at.format("%H:%M").to_string(),
            end_time:   end_at.format("%H:%M").to_string(),
            signed:     course.pass == Some(1),
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
            source:     SignSource::Bykc,
            action:     SignAction::SignOut,
            name:       course.course_name,
            course_id:  course.course_id.to_string(),
            target_id:  course.course_id.to_string(),
            date:       start_at.date_naive().format("%Y-%m-%d").to_string(),
            start_time: start_at.format("%H:%M").to_string(),
            end_time:   end_at.format("%H:%M").to_string(),
            signed:     course.pass == Some(1),
        });
    }

    targets
}

fn filter_targets(courses: Vec<ListedTarget>, config: &AutomationConfig) -> Vec<ListedTarget> {

    courses
        .into_iter()
        .filter(|course| explain_filter_decision(course, config).included)
        .collect()
}

fn explain_filter_decision(course: &ListedTarget, config: &AutomationConfig) -> FilterDecision {

    let include_patterns = config.source_include_patterns(course.source);

    let exclude_patterns = config.source_exclude_patterns(course.source);

    let include_all = include_patterns.iter().any(|pattern| pattern.trim() == "*");

    let include_matches = matching_patterns(course, include_patterns);

    let exclude_matches = matching_patterns(course, exclude_patterns);

    let matched_include = include_all || !include_matches.is_empty();

    let matched_exclude = !exclude_matches.is_empty();

    FilterDecision {
        include_patterns: include_patterns.to_vec(),
        exclude_patterns: exclude_patterns.to_vec(),
        include_matches,
        exclude_matches,
        matched_include,
        matched_exclude,
        included: matched_include && !matched_exclude,
    }
}

fn matching_patterns(course: &ListedTarget, patterns: &[String]) -> Vec<String> {

    patterns
        .iter()
        .filter(|pattern| course_matches_pattern(course, pattern))
        .cloned()
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

async fn login_session(
    api: &IClassApi,
    input: &crate::model::LoginInput,
    debug_login: bool,
) -> Result<crate::model::Session> {

    match api.login_with_diagnostic(input).await {
        Ok(session) => Ok(session),
        Err(diagnostic) => {

            if debug_login {

                eprintln!("{}", serde_json::to_string_pretty(&diagnostic)?);
            }

            Err(anyhow!(diagnostic.summary))
        }
    }
}

async fn login_session_classified(
    api: &IClassApi,
    input: &crate::model::LoginInput,
    debug_login: bool,
) -> std::result::Result<crate::model::Session, ClassifiedError> {

    match api.login_with_diagnostic(input).await {
        Ok(session) => Ok(session),
        Err(diagnostic) => {

            if debug_login {

                match serde_json::to_string_pretty(&diagnostic) {
                    Ok(text) => eprintln!("{text}"),
                    Err(error) => eprintln!("登录诊断序列化失败: {error}"),
                }
            }

            let (retryable, reason) = classify_login_failure_kind(&diagnostic.kind);

            Err(ClassifiedError {
                error: anyhow!(diagnostic.summary.clone()),
                retryable,
                reason,
                description: diagnostic.summary,
            })
        }
    }
}

fn classify_login_failure_kind(kind: &LoginFailureKind) -> (bool, &'static str) {

    match kind {
        LoginFailureKind::Network | LoginFailureKind::Timeout | LoginFailureKind::Dns => {
            (true, "transient-login-network")
        }
        LoginFailureKind::Http | LoginFailureKind::IclassApi | LoginFailureKind::Unknown => {
            (true, "transient-login-upstream")
        }
        LoginFailureKind::Captcha => (false, "captcha-required"),
        LoginFailureKind::Credentials => (false, "bad-credentials"),
        LoginFailureKind::Validation => (false, "configuration-error"),
        LoginFailureKind::SsoChanged => (false, "sso-page-changed"),
    }
}

fn classify_anyhow_error(error: anyhow::Error) -> ClassifiedError {

    let description = error.to_string();

    let normalized = error
        .chain()
        .map(|cause| cause.to_string().to_lowercase())
        .collect::<Vec<_>>()
        .join(" | ");

    let (retryable, reason) = if normalized.contains("course_sched_id 不能为空")
        || normalized.contains("courseschedid 不能为空")
        || normalized.contains("配置")
        || normalized.contains("不能为空")
        || normalized.contains("需要 vpn 模式")
        || normalized.contains("验证码")
        || normalized.contains("账号或密码")
        || normalized.contains("423")
        || normalized.contains("locked")
        || normalized.contains("access denied")
    {

        (false, "non-retryable-input")
    } else if normalized.contains("5xx")
        || normalized.contains("500")
        || normalized.contains("502")
        || normalized.contains("503")
        || normalized.contains("504")
        || normalized.contains("timeout")
        || normalized.contains("timed out")
        || normalized.contains("超时")
        || normalized.contains("dns")
        || normalized.contains("connect")
        || normalized.contains("connection")
        || normalized.contains("网络")
        || normalized.contains("请求失败")
    {

        (true, "transient-network")
    } else {

        (false, "non-retryable-upstream")
    };

    ClassifiedError {
        error,
        retryable,
        reason,
        description,
    }
}

fn print_retry_decision(
    operation: &str,
    attempt: u32,
    retry: &RetryPolicy,
    classified: &ClassifiedError,
) {

    crate::logging::event(
        if classified.retryable {

            crate::logging::LogLevel::Warn
        } else {

            crate::logging::LogLevel::Error
        },
        "cli.retry",
        format!("{operation} failed"),
        json!({
            "attempt": attempt,
            "max_attempts": retry.max_attempts,
            "retryable": classified.retryable,
            "reason": classified.reason,
            "next_delay_seconds": if classified.retryable && attempt < retry.max_attempts {
                Some(retry.delay_seconds(attempt))
            } else {
                None
            },
            "error": classified.description,
        }),
    );

    if classified.retryable && attempt < retry.max_attempts {

        let delay = retry.delay_seconds(attempt);

        eprintln!(
            "[attempt {attempt}/{}] {operation} 失败 -> reason={} -> {}；{} 秒后重试",
            retry.max_attempts, classified.reason, classified.description, delay
        );
    } else if classified.retryable {

        eprintln!(
            "[attempt {attempt}/{}] {operation} 失败 -> reason={} -> {}；已达到最大重试次数",
            retry.max_attempts, classified.reason, classified.description
        );
    } else {

        eprintln!(
            "[attempt {attempt}/{}] {operation} 失败 -> reason={} -> {}；不可重试，停止",
            retry.max_attempts, classified.reason, classified.description
        );
    }
}

// Sign execution and diagnostics

fn target_deadline(target: &ListedTarget) -> Option<DateTime<Local>> {

    build_local_time(&target.date, &target.end_time)
        .ok()
        .flatten()
}

fn bounded_retry_delay(
    retry: &RetryPolicy,
    attempt: u32,
    deadline: Option<DateTime<Local>>,
) -> u64 {

    let requested = retry.delay_seconds(attempt);

    let Some(deadline) = deadline else {

        return requested;
    };

    let remaining = deadline.signed_duration_since(Local::now()).num_seconds();

    if remaining <= 0 {

        return 0;
    }

    requested.min(remaining as u64)
}

async fn sign_iclass_with_retry(
    config: &AutomationConfig,
    course_sched_id: &str,
    retry: RetryPolicy,
    display_name: Option<String>,
    debug_login: bool,
    deadline: Option<DateTime<Local>>,
) -> Result<SignOutcome> {

    let mut last_error = None;

    for attempt in 1..=retry.max_attempts {

        let api = IClassApi::new(config.use_vpn)?;

        match login_session_classified(&api, &config.login_input(), debug_login).await {
            Ok(session) => {
                match api.sign_now(&session, course_sched_id).await {
                    Ok(outcome) => return Ok(outcome),
                    Err(error) => {

                        let name = display_name.as_deref().unwrap_or(course_sched_id);

                        let classified = classify_anyhow_error(error);

                        print_retry_decision(
                            &format!("iClass 签到 {name} ({course_sched_id})"),
                            attempt,
                            &retry,
                            &classified,
                        );

                        let should_retry = classified.retryable && attempt < retry.max_attempts;

                        let delay_seconds = bounded_retry_delay(&retry, attempt, deadline);

                        last_error = Some(classified.error);

                        if should_retry && delay_seconds > 0 {

                            sleep(Duration::from_secs(delay_seconds)).await;
                        } else {

                            break;
                        }
                    }
                }
            }
            Err(classified) => {

                let name = display_name.as_deref().unwrap_or(course_sched_id);

                print_retry_decision(
                    &format!("iClass 登录 {name} ({course_sched_id})"),
                    attempt,
                    &retry,
                    &classified,
                );

                let should_retry = classified.retryable && attempt < retry.max_attempts;

                let delay_seconds = bounded_retry_delay(&retry, attempt, deadline);

                last_error = Some(classified.error);

                if should_retry && delay_seconds > 0 {

                    sleep(Duration::from_secs(delay_seconds)).await;
                } else {

                    break;
                }
            }
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
    debug_login: bool,
    deadline: Option<DateTime<Local>>,
) -> Result<SignOutcome> {

    let mut last_error = None;

    for attempt in 1..=retry.max_attempts {

        let api = IClassApi::new(config.use_vpn)?;

        match login_session_classified(&api, &config.login_input(), debug_login).await {
            Ok(session) => {

                let bykc_api = session
                    .bykc_api
                    .ok_or_else(|| anyhow!("BYKC 自动签到需要 VPN 模式登录"))?;

                let result = bykc_api
                    .sign_course(course_id, BykcSignAction::from(action))
                    .await;

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

                        let classified = classify_anyhow_error(error);

                        print_retry_decision(
                            &format!("BYKC {} {name} ({course_id})", action.label()),
                            attempt,
                            &retry,
                            &classified,
                        );

                        let should_retry = classified.retryable && attempt < retry.max_attempts;

                        let delay_seconds = bounded_retry_delay(&retry, attempt, deadline);

                        last_error = Some(classified.error);

                        if should_retry && delay_seconds > 0 {

                            sleep(Duration::from_secs(delay_seconds)).await;
                        } else {

                            break;
                        }
                    }
                }
            }
            Err(classified) => {

                let name = display_name
                    .as_deref()
                    .map(str::to_string)
                    .unwrap_or_else(|| course_id.to_string());

                print_retry_decision(
                    &format!("BYKC 登录 {name} ({course_id})"),
                    attempt,
                    &retry,
                    &classified,
                );

                let should_retry = classified.retryable && attempt < retry.max_attempts;

                let delay_seconds = bounded_retry_delay(&retry, attempt, deadline);

                last_error = Some(classified.error);

                if should_retry && delay_seconds > 0 {

                    sleep(Duration::from_secs(delay_seconds)).await;
                } else {

                    break;
                }
            }
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
                Some(item) => {

                    json!({
                        "name": item.name,
                        "id": item.id,
                        "course_sched_id": item.course_sched_id,
                        "date": item.date,
                        "start_time": item.start_time,
                        "end_time": item.end_time,
                        "sign_status": item.sign_status,
                    })
                }
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

    let datetime_with_seconds = format!("{date} {time}:00");

    let datetime_without_seconds = format!("{date} {time}");

    let naive = match NaiveDateTime::parse_from_str(&datetime_with_seconds, "%Y-%m-%d %H:%M:%S") {
        Ok(value) => value,
        Err(_) => NaiveDateTime::parse_from_str(&datetime_without_seconds, "%Y-%m-%d %H:%M:%S")?,
    };

    match Local.from_local_datetime(&naive) {
        LocalResult::Single(value) => Ok(Some(value)),
        LocalResult::Ambiguous(first, _) => Ok(Some(first)),
        LocalResult::None => bail!("本地时区无法表示时间: {naive}"),
    }
}

/// Prints the full planner table used by `plan --dry-run`.

fn print_dry_run_courses(evaluated: &[DryRunCourse]) {

    if evaluated.is_empty() {

        println!("今日无签到目标");

        return;
    }

    println!(
        "source\taction\tincluded\tinclude_rules\texclude_rules\tinclude_matches\\
         texclude_matches\tstatus\tskip_reason\tavailable_at\twindow\tname\tcourse_id\\
         ttarget_id\tsigned"
    );

    for entry in evaluated {

        let (status, skip_reason, available_at) = match entry.evaluation.as_ref() {
            Some(evaluation) => {
                (
                    poll_status_label(evaluation.status),
                    dry_run_skip_reason(evaluation),
                    evaluation
                        .available_at
                        .map(|value| value.to_rfc3339())
                        .unwrap_or_else(|| "-".to_string()),
                )
            }
            None => {
                (
                    "filtered-out",
                    filter_skip_reason(&entry.filter),
                    "-".to_string(),
                )
            }
        };

        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t{}",
            entry.course.source.label(),
            entry.course.action.label(),
            if entry.filter.included { "yes" } else { "no" },
            format_patterns(&entry.filter.include_patterns),
            format_patterns(&entry.filter.exclude_patterns),
            format_patterns(&entry.filter.include_matches),
            format_patterns(&entry.filter.exclude_matches),
            status,
            skip_reason,
            available_at,
            format!(
                "{} {}-{}",
                entry.course.date, entry.course.start_time, entry.course.end_time
            ),
            entry.course.name,
            entry.course.course_id,
            entry.course.target_id,
            if entry.course.signed { "yes" } else { "no" }
        );
    }
}

fn dry_run_skip_reason(entry: &EvaluatedCourse) -> &'static str {

    match entry.status {
        PollStatusKind::DueNow => "will-sign-now",
        PollStatusKind::WaitingForDailyStart => "planner-time-not-reached",
        PollStatusKind::WaitingForCourse => "course-window-not-reached",
        PollStatusKind::Signed => "already-signed",
        PollStatusKind::Expired => "expired",
        PollStatusKind::MissingCourseSchedId => "missing-target-id",
    }
}

fn filter_skip_reason(filter: &FilterDecision) -> &'static str {

    if filter.matched_exclude {

        return "excluded-by-rule";
    }

    if !filter.matched_include {

        return "no-include-rule-matched";
    }

    "filtered-out"
}

fn format_patterns(patterns: &[String]) -> String {

    if patterns.is_empty() {

        return "-".to_string();
    }

    patterns.join(",")
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
        "今日签到汇总: iclass={iclass}, bykc_sign_in={bykc_sign_in}, \
         bykc_sign_out={bykc_sign_out}, due_now={due_now}, waiting={waiting}, signed={signed}, \
         expired={expired}, missing_target_id={missing}"
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

#[cfg(test)]

mod tests {

    use chrono::{Duration as ChronoDuration, Local, NaiveDate, NaiveTime, TimeZone};

    use super::*;

    fn test_config() -> AutomationConfig {

        AutomationConfig {
            student_id:               "23370000".to_string(),
            use_vpn:                  false,
            vpn_username:             String::new(),
            vpn_password:             String::new(),
            enable_iclass:            true,
            enable_bykc:              false,
            advance_minutes:          5,
            retry_count:              1,
            retry_interval_seconds:   1,
            include_courses:          vec!["*".to_string()],
            exclude_courses:          Vec::new(),
            iclass_include_courses:   Vec::new(),
            iclass_exclude_courses:   Vec::new(),
            bykc_include_courses:     Vec::new(),
            bykc_exclude_courses:     Vec::new(),
            planner_time:             "07:00:00".to_string(),
            planner_interval_minutes: 10,
            notify_on_failure:        false,
            notify_on_success:        false,
        }
    }

    fn target(name: &str, target_id: &str, start_time: &str, end_time: &str) -> ListedTarget {

        ListedTarget {
            source:     SignSource::IClass,
            action:     SignAction::SignIn,
            name:       name.to_string(),
            course_id:  format!("{name}-id"),
            target_id:  target_id.to_string(),
            date:       "2026-05-13".to_string(),
            start_time: start_time.to_string(),
            end_time:   end_time.to_string(),
            signed:     false,
        }
    }

    fn local_datetime(hour: u32, minute: u32) -> DateTime<Local> {

        let date = NaiveDate::from_ymd_opt(2026, 5, 13).unwrap();

        let time = NaiveTime::from_hms_opt(hour, minute, 0).unwrap();

        Local
            .from_local_datetime(&NaiveDateTime::new(date, time))
            .single()
            .unwrap()
    }

    #[test]

    fn dry_run_filter_explains_include_and_exclude_rules() {

        let mut config = test_config();

        config.include_courses = vec!["*数学*".to_string()];

        config.exclude_courses = vec!["*实验*".to_string()];

        let included =
            explain_filter_decision(&target("高等数学", "sched-1", "09:00", "10:00"), &config);

        assert!(included.included);

        assert_eq!(included.include_matches, vec!["*数学*"]);

        assert!(included.exclude_matches.is_empty());

        let excluded =
            explain_filter_decision(&target("数学实验", "sched-2", "09:00", "10:00"), &config);

        assert!(!excluded.included);

        assert_eq!(excluded.include_matches, vec!["*数学*"]);

        assert_eq!(excluded.exclude_matches, vec!["*实验*"]);

        assert_eq!(filter_skip_reason(&excluded), "excluded-by-rule");

        let unmatched =
            explain_filter_decision(&target("大学英语", "sched-3", "09:00", "10:00"), &config);

        assert!(!unmatched.included);

        assert!(unmatched.include_matches.is_empty());

        assert_eq!(filter_skip_reason(&unmatched), "no-include-rule-matched");
    }

    #[test]

    fn dry_run_skip_reason_covers_planner_states() -> Result<()> {

        let daily_start_at = local_datetime(7, 0);

        let waiting_daily = evaluate_course(
            target("早课", "sched-1", "09:00", "10:00"),
            daily_start_at,
            local_datetime(6, 30),
            5,
        )?;

        assert_eq!(waiting_daily.status, PollStatusKind::WaitingForDailyStart);

        assert_eq!(
            dry_run_skip_reason(&waiting_daily),
            "planner-time-not-reached"
        );

        let waiting_course = evaluate_course(
            target("上午课", "sched-2", "10:00", "11:00"),
            daily_start_at,
            local_datetime(9, 30),
            5,
        )?;

        assert_eq!(waiting_course.status, PollStatusKind::WaitingForCourse);

        assert_eq!(
            dry_run_skip_reason(&waiting_course),
            "course-window-not-reached"
        );

        let due = evaluate_course(
            target("当前课", "sched-3", "10:00", "11:00"),
            daily_start_at,
            local_datetime(9, 55),
            5,
        )?;

        assert_eq!(due.status, PollStatusKind::DueNow);

        assert_eq!(dry_run_skip_reason(&due), "will-sign-now");

        let expired = evaluate_course(
            target("过期课", "sched-4", "08:00", "09:00"),
            daily_start_at,
            local_datetime(9, 30),
            5,
        )?;

        assert_eq!(expired.status, PollStatusKind::Expired);

        assert_eq!(dry_run_skip_reason(&expired), "expired");

        let missing = evaluate_course(
            target("缺少 ID", "", "09:00", "10:00"),
            daily_start_at,
            local_datetime(9, 0),
            5,
        )?;

        assert_eq!(missing.status, PollStatusKind::MissingCourseSchedId);

        assert_eq!(dry_run_skip_reason(&missing), "missing-target-id");

        let mut signed_target = target("已签课", "sched-5", "09:00", "10:00");

        signed_target.signed = true;

        let signed = evaluate_course(
            signed_target,
            daily_start_at,
            daily_start_at + ChronoDuration::minutes(10),
            5,
        )?;

        assert_eq!(signed.status, PollStatusKind::Signed);

        assert_eq!(dry_run_skip_reason(&signed), "already-signed");

        Ok(())
    }

    #[test]

    fn retry_policy_uses_exponential_backoff_with_cap() {

        let retry = RetryPolicy {
            max_attempts:      10,
            interval_seconds:  3,
            max_delay_seconds: 60,
        };

        assert_eq!(retry.delay_seconds(1), 3);

        assert_eq!(retry.delay_seconds(2), 6);

        assert_eq!(retry.delay_seconds(4), 24);

        assert_eq!(retry.delay_seconds(20), 60);
    }

    #[test]

    fn lock_response_is_not_retried() {

        let locked = classify_anyhow_error(anyhow!(
            "博雅登录被学校网关暂时锁定（HTTP 423 Locked），Access Denied"
        ));

        assert!(!locked.retryable);

        assert_eq!(locked.reason, "non-retryable-input");
    }

    #[test]

    fn retry_classification_stops_on_credentials_and_retries_network() {

        let (retryable, reason) = classify_login_failure_kind(&LoginFailureKind::Credentials);

        assert!(!retryable);

        assert_eq!(reason, "bad-credentials");

        let (retryable, reason) = classify_login_failure_kind(&LoginFailureKind::Timeout);

        assert!(retryable);

        assert_eq!(reason, "transient-login-network");

        let network = classify_anyhow_error(anyhow!("请求失败: timed out while connecting"));

        assert!(network.retryable);

        assert_eq!(network.reason, "transient-network");

        let input = classify_anyhow_error(anyhow!("courseSchedId 不能为空"));

        assert!(!input.retryable);

        assert_eq!(input.reason, "non-retryable-input");
    }
}
