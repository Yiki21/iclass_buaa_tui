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
    TimeZone,
};
use clap::{Args, Parser, Subcommand};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::time::{Duration, sleep};

use crate::{
    api::IClassApi,
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
    /// Plan today's sign jobs and optionally create transient systemd user timers.
    Plan(PlanArgs),
    /// Install the daily planner systemd user service/timer units.
    InstallSystemd(InstallSystemdArgs),
    /// Disable and remove the daily planner systemd user service/timer units.
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
}

#[derive(Debug, Args)]
struct PlanArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    config: Option<PathBuf>,
    /// Prefix for generated systemd unit names.
    #[arg(long)]
    unit_prefix: Option<String>,
    /// Only print the plan without creating timers.
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
}

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

#[derive(Debug, Clone)]
struct RetryPolicy {
    max_attempts: u32,
    interval_seconds: u64,
}

#[derive(Debug)]
struct PlannedUnit {
    unit_name: String,
    scheduled_at: DateTime<Local>,
    course: ListedCourse,
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

pub fn should_run_cli(args: impl IntoIterator<Item = OsString>) -> bool {
    args.into_iter().nth(1).is_some()
}

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

async fn list_today(args: ListTodayArgs) -> Result<()> {
    let config = load_config(args.config.as_deref())?;
    let today_courses = fetch_today_courses(&config).await?;
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
    println!("{}", serde_json::to_string_pretty(&result)?);

    if outcome.success_like {
        return Ok(());
    }

    bail!("签到未成功: {}", outcome.message)
}

async fn plan_command(args: PlanArgs) -> Result<()> {
    let config = load_config(args.config.as_deref())?;
    let config_path = resolve_config_path(args.config.as_deref())?;
    let unit_prefix = args
        .unit_prefix
        .unwrap_or_else(|| DEFAULT_UNIT_PREFIX.to_string());
    let binary_path = current_binary_path()?;
    let planned = build_plan(&config, &unit_prefix).await?;

    if planned.is_empty() {
        println!("今日无需要调度的课程");
        return Ok(());
    }

    if args.dry_run {
        print_plan(&planned);
        return Ok(());
    }

    let mut scheduled = 0usize;
    for entry in &planned {
        schedule_with_systemd(&binary_path, &config_path, entry)?;
        scheduled += 1;
    }

    print_plan(&planned);
    eprintln!("已创建 {scheduled} 个一次性 systemd user timer");
    Ok(())
}

fn install_systemd(args: InstallSystemdArgs) -> Result<()> {
    let config = load_config(args.config.as_deref())?;
    let config_path = resolve_config_path(args.config.as_deref())?;
    let output_dir = args.output_dir.unwrap_or(default_systemd_user_dir()?);
    let unit_prefix = args
        .unit_prefix
        .unwrap_or_else(|| DEFAULT_UNIT_PREFIX.to_string());
    let planner_time = args.planner_time.unwrap_or(config.planner_time.clone());
    let binary_path = current_binary_path()?;
    validate_planner_time(&planner_time)?;

    fs::create_dir_all(&output_dir)
        .with_context(|| format!("创建 systemd 目录失败: {}", output_dir.display()))?;

    let service_name = format!("{unit_prefix}-planner.service");
    let timer_name = format!("{unit_prefix}-planner.timer");
    let service_path = output_dir.join(&service_name);
    let timer_path = output_dir.join(&timer_name);

    let service_content = render_planner_service(&binary_path, &config_path, &unit_prefix);
    let timer_content = render_planner_timer(&service_name, &planner_time);

    fs::write(&service_path, service_content)
        .with_context(|| format!("写入失败: {}", service_path.display()))?;
    fs::write(&timer_path, timer_content)
        .with_context(|| format!("写入失败: {}", timer_path.display()))?;

    println!(
        "已生成 systemd user units:\n{}\n{}",
        service_path.display(),
        timer_path.display()
    );
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
        validate_planner_time(&self.planner_time)?;
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
            });
        }

        last_outcome = outcome;
        if attempt < retry.max_attempts {
            sleep(Duration::from_secs(retry.interval_seconds)).await;
        }
    }

    Ok(last_outcome)
}

async fn build_plan(config: &AutomationConfig, unit_prefix: &str) -> Result<Vec<PlannedUnit>> {
    let courses = filter_courses(fetch_today_courses(config).await?, config);
    let now = Local::now();
    let mut planned = Vec::new();

    for course in courses {
        if course.signed || course.course_sched_id.trim().is_empty() {
            continue;
        }

        let Some(start_at) = course_start_at(&course)? else {
            continue;
        };
        let Some(end_at) = course_end_at(&course)? else {
            continue;
        };

        let mut scheduled_at = start_at - ChronoDuration::minutes(config.advance_minutes);
        if scheduled_at <= now {
            if end_at <= now {
                continue;
            }
            scheduled_at = now + ChronoDuration::seconds(5);
        }

        planned.push(PlannedUnit {
            unit_name: build_unit_name(unit_prefix, &course),
            scheduled_at,
            course,
        });
    }

    planned.sort_by_key(|entry| entry.scheduled_at);
    Ok(planned)
}

fn course_start_at(course: &ListedCourse) -> Result<Option<DateTime<Local>>> {
    build_local_time(&course.date, &course.start_time)
}

fn course_end_at(course: &ListedCourse) -> Result<Option<DateTime<Local>>> {
    build_local_time(&course.date, &course.end_time)
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

fn build_unit_name(unit_prefix: &str, course: &ListedCourse) -> String {
    let mut suffix = sanitize_unit_component(&course.name);
    if suffix.is_empty() {
        suffix = "course".to_string();
    }
    format!(
        "{unit_prefix}-sign-{}-{}-{}",
        course.date.replace('-', ""),
        sanitize_unit_component(&course.start_time),
        sanitize_unit_component(&format!("{}-{}", suffix, course.course_sched_id)),
    )
}

fn sanitize_unit_component(value: &str) -> String {
    value
        .chars()
        .map(|ch| match ch {
            'a'..='z' | 'A'..='Z' | '0'..='9' => ch,
            _ => '-',
        })
        .collect::<String>()
        .trim_matches('-')
        .to_lowercase()
}

fn print_plan(planned: &[PlannedUnit]) {
    println!("course\tscheduled_at\tcourse_sched_id");
    for entry in planned {
        println!(
            "{}\t{}\t{}",
            entry.course.name,
            entry.scheduled_at.format("%Y-%m-%d %H:%M:%S"),
            entry.course.course_sched_id
        );
    }
}

fn schedule_with_systemd(exe_path: &Path, config_path: &Path, entry: &PlannedUnit) -> Result<()> {
    let timestamp = entry.scheduled_at.format("%Y-%m-%d %H:%M:%S").to_string();
    let status = Command::new("systemd-run")
        .arg("--user")
        .arg("--collect")
        .arg("--unit")
        .arg(&entry.unit_name)
        .arg("--on-calendar")
        .arg(timestamp)
        .arg(exe_path)
        .arg("sign")
        .arg("--config")
        .arg(config_path)
        .arg("--course-sched-id")
        .arg(&entry.course.course_sched_id)
        .arg("--course-name")
        .arg(&entry.course.name)
        .status()
        .with_context(|| format!("执行 systemd-run 失败: {}", entry.course.name))?;

    if !status.success() {
        bail!(
            "systemd-run 返回失败状态: {} ({})",
            status,
            entry.course.name
        );
    }
    Ok(())
}

fn render_planner_service(exe_path: &Path, config_path: &Path, unit_prefix: &str) -> String {
    let mut content = String::new();
    let _ = writeln!(content, "[Unit]");
    let _ = writeln!(content, "Description=BUAA iClass daily sign planner");
    let _ = writeln!(content);
    let _ = writeln!(content, "[Service]");
    let _ = writeln!(content, "Type=oneshot");
    let _ = writeln!(
        content,
        "ExecStart={} plan --config {} --unit-prefix {}",
        escape_exec_arg(&exe_path.display().to_string()),
        escape_exec_arg(&config_path.display().to_string()),
        escape_exec_arg(unit_prefix),
    );
    content
}

fn render_planner_timer(service_name: &str, planner_time: &str) -> String {
    let mut content = String::new();
    let _ = writeln!(content, "[Unit]");
    let _ = writeln!(content, "Description=Run BUAA iClass planner daily");
    let _ = writeln!(content);
    let _ = writeln!(content, "[Timer]");
    let _ = writeln!(content, "OnCalendar=*-*-* {planner_time}");
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

fn validate_planner_time(value: &str) -> Result<()> {
    let formats = ["%H:%M", "%H:%M:%S"];
    if formats
        .iter()
        .any(|format| NaiveTime::parse_from_str(value, format).is_ok())
    {
        return Ok(());
    }
    bail!("planner_time 格式必须是 HH:MM 或 HH:MM:SS")
}
