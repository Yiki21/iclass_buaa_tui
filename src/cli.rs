use std::{
    env,
    ffi::OsString,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, anyhow, bail};
use chrono::{
    DateTime, Duration as ChronoDuration, Local, LocalResult, NaiveDate, NaiveDateTime, NaiveTime,
    TimeZone, Utc,
};
use clap::{Args, Parser, Subcommand};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::time::{Duration, sleep};

use crate::{
    api::IClassApi,
    constants::network_urls,
    model::{CourseDetailItem, LoginInput, SignOutcome},
};

const APP_CONFIG_RELATIVE_PATH: &str = "iclass-buaa/config.toml";
const DEFAULT_UNIT_PREFIX: &str = "iclass-buaa";

#[derive(Debug, Parser)]
#[command(author, version, about = "BUAA iClass TUI and automation CLI")]
pub struct Cli {
    #[command(subcommand)]
    command: CommandKind,
}

#[derive(Debug, Subcommand)]
enum CommandKind {
    /// Login and print today's filtered courses.
    ListToday(ListTodayArgs),
    /// Sign one course by course_sched_id, retrying with fresh login each attempt.
    Sign(SignArgs),
    /// Run one automation cycle: fetch today's courses and sign due ones.
    Plan(PlanArgs),
    /// Install periodic systemd user service/timer units for automation.
    InstallSystemd(InstallSystemdArgs),
    /// Disable and remove the periodic systemd user service/timer units.
    UninstallSystemd(UninstallSystemdArgs),
}

#[derive(Debug, Args)]
struct ListTodayArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Print JSON instead of tab-separated text.
    #[arg(long)]
    json: bool,
}

#[derive(Debug, Args)]
struct SignArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    config: Option<PathBuf>,
    /// The course_sched_id to sign.
    #[arg(long)]
    course_sched_id: String,
    /// Optional course name shown in logs/output.
    #[arg(long)]
    course_name: Option<String>,
    /// Override retry_count from config.
    #[arg(long)]
    retry_count: Option<u32>,
    /// Override retry_interval_seconds from config.
    #[arg(long)]
    retry_interval_seconds: Option<u64>,
    /// Print server raw response and local timing diagnostics.
    #[arg(long)]
    debug: bool,
}

#[derive(Debug, Args)]
struct PlanArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Prefix for generated systemd unit names. Kept for compatibility.
    #[arg(long)]
    unit_prefix: Option<String>,
    /// Only print today's evaluation without attempting sign.
    #[arg(long)]
    dry_run: bool,
}

#[derive(Debug, Args)]
struct InstallSystemdArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Target directory for generated systemd user unit files.
    #[arg(long)]
    output_dir: Option<PathBuf>,
    /// Prefix for generated systemd unit names.
    #[arg(long)]
    unit_prefix: Option<String>,
    /// Override planner_time from config when generating the timer unit.
    #[arg(long)]
    planner_time: Option<String>,
    /// Override planner_interval_minutes from config when generating the timer unit.
    #[arg(long)]
    planner_interval_minutes: Option<u32>,
}

#[derive(Debug, Args)]
struct UninstallSystemdArgs {
    /// Target directory containing generated systemd user unit files.
    #[arg(long)]
    output_dir: Option<PathBuf>,
    /// Prefix for generated systemd unit names.
    #[arg(long)]
    unit_prefix: Option<String>,
}

/// Automation settings loaded from the CLI config file.
#[derive(Debug, Clone, Deserialize)]
struct AutomationConfig {
    student_id: String,
    #[serde(default)]
    use_vpn: bool,
    #[serde(default)]
    vpn_username: String,
    #[serde(default)]
    vpn_password: String,
    #[serde(default = "default_advance_minutes")]
    advance_minutes: i64,
    #[serde(default = "default_retry_count")]
    retry_count: u32,
    #[serde(default = "default_retry_interval_seconds")]
    retry_interval_seconds: u64,
    #[serde(default = "default_include_courses")]
    include_courses: Vec<String>,
    #[serde(default)]
    exclude_courses: Vec<String>,
    #[serde(default = "default_planner_time")]
    planner_time: String,
    #[serde(default = "default_planner_interval_minutes")]
    planner_interval_minutes: u32,
}

/// A normalized course row used by the planner and retry logic.
#[derive(Debug, Clone)]
struct ListedCourse {
    name: String,
    course_id: String,
    course_sched_id: String,
    date: String,
    start_time: String,
    end_time: String,
    signed: bool,
}

/// Retry behavior shared by course fetch and sign operations.
#[derive(Debug, Clone)]
struct RetryPolicy {
    max_attempts: u32,
    interval_seconds: u64,
}

/// Planner state for a course in the current automation cycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PollStatusKind {
    /// The planner has not reached the configured daily start time yet.
    WaitingForDailyStart,
    /// The daily planner is active, but this course is not in its sign window yet.
    WaitingForCourse,
    /// The course should be signed immediately.
    DueNow,
    /// The course is already signed.
    Signed,
    /// The course has already ended.
    Expired,
    /// The course is missing `course_sched_id`, so sign requests cannot be sent.
    MissingCourseSchedId,
}

/// A course plus its computed planner state and first eligible sign time.
#[derive(Debug, Clone)]
struct EvaluatedCourse {
    course: ListedCourse,
    status: PollStatusKind,
    available_at: Option<DateTime<Local>>,
}

fn default_advance_minutes() -> i64 {
    5
}

fn default_retry_count() -> u32 {
    6
}

fn default_retry_interval_seconds() -> u64 {
    30
}

fn default_include_courses() -> Vec<String> {
    vec!["*".to_string()]
}

fn default_planner_time() -> String {
    "07:00:00".to_string()
}

fn default_planner_interval_minutes() -> u32 {
    10
}

pub fn should_run_cli(args: impl IntoIterator<Item = OsString>) -> bool {
    args.into_iter().nth(1).is_some()
}

/// Entry point for the non-TUI command set.
pub async fn run_cli() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        CommandKind::ListToday(args) => list_today(args).await,
        CommandKind::Sign(args) => sign_command(args).await,
        CommandKind::Plan(args) => plan_command(args).await,
        CommandKind::InstallSystemd(args) => install_systemd(args),
        CommandKind::UninstallSystemd(args) => uninstall_systemd(args),
    }
}

/// Prints today's filtered courses without attempting any sign action.
async fn list_today(args: ListTodayArgs) -> Result<()> {
    let config = load_config(args.config.as_deref())?;
    let today_courses = fetch_today_courses_with_retry(&config).await?;
    let filtered = filter_courses(today_courses, &config);

    if args.json {
        let rows: Vec<Value> = filtered
            .iter()
            .map(|course| {
                json!({
                    "name": course.name,
                    "course_id": course.course_id,
                    "course_sched_id": course.course_sched_id,
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
        println!("今日无匹配课程");
        return Ok(());
    }

    println!("name\tdate\tstart\tend\tcourse_id\tcourse_sched_id\tsigned");
    for course in filtered {
        println!(
            "{}\t{}\t{}\t{}\t{}\t{}\t{}",
            course.name,
            course.date,
            course.start_time,
            course.end_time,
            course.course_id,
            course.course_sched_id,
            if course.signed { "yes" } else { "no" }
        );
    }

    Ok(())
}

/// Signs a single course immediately and optionally emits debug diagnostics.
async fn sign_command(args: SignArgs) -> Result<()> {
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

    let display_name = args
        .course_name
        .clone()
        .unwrap_or_else(|| args.course_sched_id.clone());
    let outcome = sign_with_retry(
        &config,
        &args.course_sched_id,
        retry,
        Some(display_name.clone()),
    )
    .await?;

    let result = json!({
        "course_sched_id": args.course_sched_id,
        "course_name": display_name,
        "message": outcome.message,
        "success": outcome.success_like,
    });
    let result = if args.debug {
        enrich_sign_result_with_debug(result, &config, &args.course_sched_id, &outcome).await
    } else {
        result
    };
    println!("{}", serde_json::to_string_pretty(&result)?);

    if outcome.success_like {
        return Ok(());
    }

    bail!("签到未成功: {}", outcome.message)
}

/// Runs one planner cycle and signs every course that is currently due.
async fn plan_command(args: PlanArgs) -> Result<()> {
    let config = load_config(args.config.as_deref())?;
    let _unit_prefix = args.unit_prefix;
    let evaluated = evaluate_today_courses(&config).await?;

    if args.dry_run {
        print_evaluated_courses(&evaluated);
        return Ok(());
    }

    let due_targets: Vec<ListedCourse> = evaluated
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
        eprintln!("开始签到: {} ({})", target.name, target.course_sched_id);
        match sign_with_retry(
            &config,
            &target.course_sched_id,
            retry.clone(),
            Some(target.name.clone()),
        )
        .await
        {
            Ok(outcome) => {
                println!(
                    "{}\t{}\t{}",
                    target.name, target.course_sched_id, outcome.message
                );
            }
            Err(error) => {
                failures.push(format!(
                    "{} ({}) -> {}",
                    target.name, target.course_sched_id, error
                ));
            }
        }
    }

    if failures.is_empty() {
        return Ok(());
    }

    bail!("部分课程签到失败:\n{}", failures.join("\n"))
}

fn install_systemd(args: InstallSystemdArgs) -> Result<()> {
    let config = load_config(args.config.as_deref())?;
    let config_path = resolve_config_path(args.config.as_deref())?;
    let output_dir = args.output_dir.unwrap_or(default_systemd_user_dir()?);

    let unit_prefix = args
        .unit_prefix
        .unwrap_or_else(|| DEFAULT_UNIT_PREFIX.to_string());
    let planner_time = args.planner_time.unwrap_or(config.planner_time.clone());
    let planner_interval_minutes = args
        .planner_interval_minutes
        .unwrap_or(config.planner_interval_minutes);

    let binary_path = current_binary_path()?;
    parse_planner_time(&planner_time)?;
    validate_planner_interval_minutes(planner_interval_minutes)?;

    fs::create_dir_all(&output_dir)
        .with_context(|| format!("创建 systemd 目录失败: {}", output_dir.display()))?;

    let service_name = format!("{unit_prefix}-planner.service");
    let timer_name = format!("{unit_prefix}-planner.timer");
    let service_path = output_dir.join(&service_name);
    let timer_path = output_dir.join(&timer_name);

    let service_content = render_planner_service(&binary_path, &config_path);
    let timer_content = render_planner_timer(&service_name, planner_interval_minutes);

    fs::write(&service_path, service_content)
        .with_context(|| format!("写入失败: {}", service_path.display()))?;
    fs::write(&timer_path, timer_content)
        .with_context(|| format!("写入失败: {}", timer_path.display()))?;

    println!(
        "已生成 systemd user units:\n{}\n{}",
        service_path.display(),
        timer_path.display()
    );
    println!("自动签到将在每天 {planner_time} 后开始按 {planner_interval_minutes} 分钟周期轮询。");
    println!(
        "启用方式: systemctl --user daemon-reload && systemctl --user enable --now {timer_name}"
    );
    Ok(())
}

fn uninstall_systemd(args: UninstallSystemdArgs) -> Result<()> {
    let output_dir = args.output_dir.unwrap_or(default_systemd_user_dir()?);
    let unit_prefix = args
        .unit_prefix
        .unwrap_or_else(|| DEFAULT_UNIT_PREFIX.to_string());
    let service_name = format!("{unit_prefix}-planner.service");
    let timer_name = format!("{unit_prefix}-planner.timer");
    let service_path = output_dir.join(&service_name);
    let timer_path = output_dir.join(&timer_name);

    run_systemctl_user(["disable", "--now", &timer_name])?;

    remove_file_if_exists(&service_path)?;
    remove_file_if_exists(&timer_path)?;

    run_systemctl_user(["daemon-reload"])?;

    println!(
        "已卸载 systemd user units:\n{}\n{}",
        service_path.display(),
        timer_path.display()
    );
    Ok(())
}

fn load_config(path: Option<&Path>) -> Result<AutomationConfig> {
    let config_path = resolve_config_path(path)?;
    let raw = fs::read_to_string(&config_path)
        .with_context(|| format!("读取配置失败: {}", config_path.display()))?;
    let config: AutomationConfig = toml::from_str(&raw)
        .with_context(|| format!("解析 TOML 失败: {}", config_path.display()))?;
    ensure_config_permissions(&config_path, &config)?;
    config.validate()?;
    Ok(config)
}

fn resolve_config_path(path: Option<&Path>) -> Result<PathBuf> {
    if let Some(path) = path {
        return path
            .canonicalize()
            .with_context(|| format!("找不到配置文件: {}", path.display()));
    }

    let candidates = candidate_config_paths()?;
    if let Some(found) = candidates.iter().find(|path| path.is_file()) {
        return found
            .canonicalize()
            .with_context(|| format!("无法解析配置路径: {}", found.display()));
    }

    let searched = candidates
        .iter()
        .map(|path| path.display().to_string())
        .collect::<Vec<_>>()
        .join(", ");
    bail!("找不到配置文件，已搜索: {searched}")
}

fn candidate_config_paths() -> Result<Vec<PathBuf>> {
    let mut paths = Vec::new();

    if let Some(path) = user_xdg_config_path() {
        push_unique(&mut paths, path);
    }
    if let Some(path) = home_config_path()? {
        push_unique(&mut paths, path);
    }

    for path in system_xdg_config_paths() {
        push_unique(&mut paths, path);
    }

    push_unique(
        &mut paths,
        PathBuf::from("/etc").join(APP_CONFIG_RELATIVE_PATH),
    );

    Ok(paths)
}

fn user_xdg_config_path() -> Option<PathBuf> {
    env::var_os("XDG_CONFIG_HOME").map(|base| PathBuf::from(base).join(APP_CONFIG_RELATIVE_PATH))
}

fn home_config_path() -> Result<Option<PathBuf>> {
    let Some(home) = env::var_os("HOME") else {
        return Ok(None);
    };
    Ok(Some(
        PathBuf::from(home)
            .join(".config")
            .join(APP_CONFIG_RELATIVE_PATH),
    ))
}

fn system_xdg_config_paths() -> Vec<PathBuf> {
    let raw = env::var_os("XDG_CONFIG_DIRS")
        .map(|value| value.to_string_lossy().into_owned())
        .unwrap_or_else(|| "/etc/xdg".to_string());

    raw.split(':')
        .filter(|segment| !segment.trim().is_empty())
        .map(|segment| PathBuf::from(segment).join(APP_CONFIG_RELATIVE_PATH))
        .collect()
}

fn push_unique(paths: &mut Vec<PathBuf>, path: PathBuf) {
    if !paths.iter().any(|existing| existing == &path) {
        paths.push(path);
    }
}

fn default_systemd_user_dir() -> Result<PathBuf> {
    let home =
        env::var_os("HOME").ok_or_else(|| anyhow!("HOME 未设置，无法定位 systemd user 目录"))?;
    Ok(PathBuf::from(home).join(".config/systemd/user"))
}

fn run_systemctl_user<const N: usize>(args: [&str; N]) -> Result<()> {
    let status = Command::new("systemctl")
        .arg("--user")
        .args(args)
        .status()
        .context("执行 systemctl --user 失败")?;

    if !status.success() {
        bail!("systemctl --user 返回失败状态: {status}");
    }
    Ok(())
}

fn remove_file_if_exists(path: &Path) -> Result<()> {
    if path.exists() {
        fs::remove_file(path).with_context(|| format!("删除失败: {}", path.display()))?;
    }
    Ok(())
}

fn current_binary_path() -> Result<PathBuf> {
    env::current_exe().context("无法获取当前程序路径")
}

fn ensure_config_permissions(path: &Path, config: &AutomationConfig) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if !config.use_vpn || config.vpn_password.is_empty() {
            return Ok(());
        }

        let metadata =
            fs::metadata(path).with_context(|| format!("读取配置权限失败: {}", path.display()))?;
        let mode = metadata.permissions().mode() & 0o777;
        if mode != 0o600 {
            bail!(
                "配置文件包含 vpn_password 时权限必须是 600，当前为 {:o}: {}",
                mode,
                path.display()
            );
        }
    }
    Ok(())
}

impl AutomationConfig {
    fn validate(&self) -> Result<()> {
        if self.student_id.trim().is_empty() {
            bail!("student_id 不能为空");
        }
        if self.use_vpn && (self.vpn_username.trim().is_empty() || self.vpn_password.is_empty()) {
            bail!("use_vpn = true 时必须提供 vpn_username 和 vpn_password");
        }
        if self.retry_count == 0 {
            bail!("retry_count 必须大于 0");
        }
        if self.planner_time.trim().is_empty() {
            bail!("planner_time 不能为空");
        }

        parse_planner_time(&self.planner_time)?;
        validate_planner_interval_minutes(self.planner_interval_minutes)?;
        Ok(())
    }

    fn login_input(&self) -> LoginInput {
        LoginInput {
            student_id: self.student_id.clone(),
            use_vpn: self.use_vpn,
            vpn_username: self.vpn_username.clone(),
            vpn_password: self.vpn_password.clone(),
        }
    }
}

async fn fetch_today_courses(config: &AutomationConfig) -> Result<Vec<ListedCourse>> {
    let api = IClassApi::new(config.use_vpn)?;
    let session = api.login(&config.login_input()).await?;
    let today = Local::now().date_naive().format("%Y-%m-%d").to_string();

    let courses = api.get_merged_course_details(&session, 0).await?;
    Ok(courses
        .into_iter()
        .filter(|course| course.date == today)
        .map(map_course)
        .collect())
}

/// Fetches today's courses, retrying login and API calls on transient failures.
async fn fetch_today_courses_with_retry(config: &AutomationConfig) -> Result<Vec<ListedCourse>> {
    let retry = RetryPolicy {
        max_attempts: config.retry_count,
        interval_seconds: config.retry_interval_seconds,
    };
    let mut last_error = None;

    for attempt in 1..=retry.max_attempts {
        match fetch_today_courses(config).await {
            Ok(courses) => return Ok(courses),
            Err(error) => {
                let message = error.to_string();
                eprintln!(
                    "[attempt {attempt}/{}] 获取今日课程失败 -> {}",
                    retry.max_attempts, message
                );
                last_error = Some(error);
                if attempt < retry.max_attempts {
                    sleep(Duration::from_secs(retry.interval_seconds)).await;
                }
            }
        }
    }

    Err(last_error.unwrap().context("多次尝试后仍无法获取今日课程"))
}

/// Computes planner status for every filtered course scheduled today.
async fn evaluate_today_courses(config: &AutomationConfig) -> Result<Vec<EvaluatedCourse>> {
    let courses = filter_courses(fetch_today_courses_with_retry(config).await?, config);
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
    course: ListedCourse,
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

    if course.course_sched_id.trim().is_empty() {
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
        start_at - ChronoDuration::minutes(advance_minutes),
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

fn map_course(course: CourseDetailItem) -> ListedCourse {
    let signed = course.signed();
    ListedCourse {
        name: course.name,
        course_id: course.id,
        course_sched_id: course.course_sched_id,
        date: course.date,
        start_time: course.start_time,
        end_time: course.end_time,
        signed,
    }
}

fn filter_courses(courses: Vec<ListedCourse>, config: &AutomationConfig) -> Vec<ListedCourse> {
    let include_all = config
        .include_courses
        .iter()
        .any(|pattern| pattern.trim() == "*");
    courses
        .into_iter()
        .filter(|course| {
            let included = include_all
                || config
                    .include_courses
                    .iter()
                    .any(|pattern| course_matches_pattern(course, pattern));
            let excluded = config
                .exclude_courses
                .iter()
                .any(|pattern| course_matches_pattern(course, pattern));
            included && !excluded
        })
        .collect()
}

fn course_matches_pattern(course: &ListedCourse, pattern: &str) -> bool {
    let pattern = pattern.trim();
    if pattern.is_empty() || pattern == "*" {
        return true;
    }
    wildcard_match(pattern, &course.name)
        || wildcard_match(pattern, &course.course_id)
        || wildcard_match(pattern, &course.course_sched_id)
        || course.name.contains(pattern)
        || course.course_id.contains(pattern)
        || course.course_sched_id.contains(pattern)
}

fn wildcard_match(pattern: &str, text: &str) -> bool {
    if pattern == "*" {
        return true;
    }
    if !pattern.contains('*') {
        return text == pattern;
    }

    let parts: Vec<&str> = pattern.split('*').collect();
    let starts_with_wildcard = pattern.starts_with('*');
    let ends_with_wildcard = pattern.ends_with('*');
    let mut remaining = text;

    for (index, part) in parts.iter().enumerate() {
        if part.is_empty() {
            continue;
        }

        if index == 0 && !starts_with_wildcard {
            let Some(stripped) = remaining.strip_prefix(part) else {
                return false;
            };
            remaining = stripped;
            continue;
        }

        if index == parts.len() - 1 && !ends_with_wildcard {
            return remaining.ends_with(part);
        }

        let Some(position) = remaining.find(part) else {
            return false;
        };
        remaining = &remaining[position + part.len()..];
    }

    true
}

async fn sign_with_retry(
    config: &AutomationConfig,
    course_sched_id: &str,
    retry: RetryPolicy,
    course_name: Option<String>,
) -> Result<SignOutcome> {
    let course_sched_id = course_sched_id.trim();
    if course_sched_id.is_empty() {
        bail!("course_sched_id 不能为空");
    }

    let display_name = course_name.unwrap_or_else(|| course_sched_id.to_string());
    let mut last_outcome = SignOutcome {
        message: "未执行签到".to_string(),
        success_like: false,
        http_status: 0,
        server_status: String::new(),
        raw_response: Value::Null,
    };

    for attempt in 1..=retry.max_attempts {
        let api = IClassApi::new(config.use_vpn)?;
        let session = api.login(&config.login_input()).await?;
        let today_courses = api.get_merged_course_details(&session, 0).await?;

        if let Some(course) = today_courses
            .iter()
            .find(|course| course.course_sched_id == course_sched_id)
        {
            if course.signed() {
                return Ok(SignOutcome {
                    message: format!("{display_name} 已签到"),
                    success_like: true,
                    http_status: 200,
                    server_status: "0".to_string(),
                    raw_response: json!({
                        "STATUS": "0",
                        "ERRMSG": format!("{display_name} 已签到"),
                    }),
                });
            }
        }

        let outcome = api.sign_now(&session, course_sched_id).await?;
        eprintln!(
            "[attempt {attempt}/{}] {} -> {}",
            retry.max_attempts, display_name, outcome.message
        );

        if outcome.success_like || outcome.message.contains("已签到") {
            return Ok(SignOutcome {
                message: outcome.message,
                success_like: true,
                http_status: outcome.http_status,
                server_status: outcome.server_status,
                raw_response: outcome.raw_response,
            });
        }

        last_outcome = outcome;
        if attempt < retry.max_attempts {
            sleep(Duration::from_secs(retry.interval_seconds)).await;
        }
    }

    Ok(last_outcome)
}

async fn enrich_sign_result_with_debug(
    result: Value,
    config: &AutomationConfig,
    course_sched_id: &str,
    outcome: &SignOutcome,
) -> Value {
    let mut object = result.as_object().cloned().unwrap_or_default();
    object.insert("http_status".to_string(), json!(outcome.http_status));
    object.insert("server_status".to_string(), json!(outcome.server_status));
    object.insert("raw_response".to_string(), outcome.raw_response.clone());

    match collect_sign_debug_context(config, course_sched_id).await {
        Ok(debug) => {
            object.insert("debug".to_string(), debug);
        }
        Err(error) => {
            object.insert("debug_error".to_string(), json!(error.to_string()));
        }
    }

    Value::Object(object)
}

async fn collect_sign_debug_context(
    config: &AutomationConfig,
    course_sched_id: &str,
) -> Result<Value> {
    let api = IClassApi::new(config.use_vpn)?;
    let session = api.login(&config.login_input()).await?;
    let now = Local::now();
    let server_now_ms = session.server_now_millis();
    let today = now.format("%Y-%m-%d").to_string();
    let daily_start = daily_start_at(config, now)?;
    let endpoints = network_urls(config.use_vpn);
    let course = api
        .get_merged_course_details(&session, 0)
        .await?
        .into_iter()
        .filter(|course| course.date == today)
        .map(map_course)
        .into_iter()
        .find(|course| course.course_sched_id == course_sched_id);

    let matched_course = if let Some(course) = course {
        let evaluated = evaluate_course(course.clone(), daily_start, now, config.advance_minutes)?;
        json!({
            "name": course.name,
            "course_id": course.course_id,
            "course_sched_id": course.course_sched_id,
            "date": course.date,
            "start_time": course.start_time,
            "end_time": course.end_time,
            "signed": course.signed,
            "local_status": poll_status_label(evaluated.status),
            "available_at": evaluated.available_at.map(|value| value.to_rfc3339()),
        })
    } else {
        Value::Null
    };

    Ok(json!({
        "local_now": now.to_rfc3339(),
        "server_time_offset_ms": session.server_time_offset_ms,
        "server_now": Utc
            .timestamp_millis_opt(server_now_ms)
            .single()
            .map(|value| value.to_rfc3339()),
        "planner_time": config.planner_time,
        "advance_minutes": config.advance_minutes,
        "use_vpn": session.use_vpn,
        "session_user_id": session.user_id,
        "endpoints": {
            "user_login": endpoints.user_login,
            "course_schedule_by_date": endpoints.course_schedule_by_date,
            "course_sign_detail": endpoints.course_sign_detail,
            "scan_sign": endpoints.scan_sign,
        },
        "matched_today_course": matched_course,
    }))
}

fn build_local_time(date: &str, time: &str) -> Result<Option<DateTime<Local>>> {
    if date.trim().is_empty() || time.trim().is_empty() {
        return Ok(None);
    }

    let date = NaiveDate::parse_from_str(date, "%Y-%m-%d")
        .with_context(|| format!("无法解析日期: {date}"))?;
    let time = NaiveTime::parse_from_str(time, "%H:%M")
        .with_context(|| format!("无法解析时间: {time}"))?;
    let naive = NaiveDateTime::new(date, time);

    match Local.from_local_datetime(&naive) {
        LocalResult::Single(value) => Ok(Some(value)),
        LocalResult::Ambiguous(first, _) => Ok(Some(first)),
        LocalResult::None => bail!("本地时区无法表示时间: {naive}"),
    }
}

fn print_evaluated_courses(evaluated: &[EvaluatedCourse]) {
    println!("course\tstatus\tavailable_at\tcourse_sched_id");
    for entry in evaluated {
        let available_at = entry
            .available_at
            .map(|value| value.format("%Y-%m-%d %H:%M:%S").to_string())
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{}\t{}\t{}\t{}",
            entry.course.name,
            poll_status_label(entry.status),
            available_at,
            entry.course.course_sched_id
        );
    }
}

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

    println!(
        "今日课程汇总: due_now={due_now}, waiting={waiting}, signed={signed}, expired={expired}, missing_course_sched_id={missing}"
    );
}

fn poll_status_label(status: PollStatusKind) -> &'static str {
    match status {
        PollStatusKind::WaitingForDailyStart => "waiting-daily-start",
        PollStatusKind::WaitingForCourse => "waiting-course-window",
        PollStatusKind::DueNow => "due-now",
        PollStatusKind::Signed => "signed",
        PollStatusKind::Expired => "expired",
        PollStatusKind::MissingCourseSchedId => "missing-course-sched-id",
    }
}

fn render_planner_service(exe_path: &Path, config_path: &Path) -> String {
    let mut content = String::new();
    let _ = writeln!(content, "[Unit]");
    let _ = writeln!(content, "Description=BUAA iClass periodic sign poller");
    let _ = writeln!(content);
    let _ = writeln!(content, "[Service]");
    let _ = writeln!(content, "Type=oneshot");
    let _ = writeln!(
        content,
        "ExecStart={} plan --config {}",
        escape_exec_arg(&exe_path.display().to_string()),
        escape_exec_arg(&config_path.display().to_string()),
    );
    content
}

fn render_planner_timer(service_name: &str, planner_interval_minutes: u32) -> String {
    let mut content = String::new();
    let _ = writeln!(content, "[Unit]");
    let _ = writeln!(content, "Description=Run BUAA iClass poller periodically");
    let _ = writeln!(content);
    let _ = writeln!(content, "[Timer]");
    let _ = writeln!(content, "OnBootSec=1min");
    let _ = writeln!(content, "OnUnitActiveSec={}min", planner_interval_minutes);
    let _ = writeln!(content, "AccuracySec=1min");
    let _ = writeln!(content, "Persistent=true");
    let _ = writeln!(content, "Unit={service_name}");
    let _ = writeln!(content);
    let _ = writeln!(content, "[Install]");
    let _ = writeln!(content, "WantedBy=timers.target");
    content
}

fn escape_exec_arg(text: &str) -> String {
    if text
        .chars()
        .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '/' | '.' | '_' | '-'))
    {
        text.to_string()
    } else {
        format!("'{}'", text.replace('\'', "'\\''"))
    }
}

fn parse_planner_time(value: &str) -> Result<NaiveTime> {
    let formats = ["%H:%M", "%H:%M:%S"];
    for format in formats {
        if let Ok(parsed) = NaiveTime::parse_from_str(value, format) {
            return Ok(parsed);
        }
    }
    bail!("planner_time 格式必须是 HH:MM 或 HH:MM:SS")
}

fn validate_planner_interval_minutes(value: u32) -> Result<()> {
    if value == 0 {
        bail!("planner_interval_minutes 必须大于 0");
    }
    Ok(())
}
