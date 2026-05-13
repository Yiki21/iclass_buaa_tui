//! Platform-specific scheduled automation installers for Linux, macOS, and Windows.

use std::{
    env,
    fmt::Write as _,
    fs,
    path::{Path, PathBuf},
    process::Command,
};

use anyhow::{Context, Result, anyhow, bail};

use super::args::{AutologinStatusArgs, InstallAutologinArgs, UninstallAutologinArgs};
use super::config::{
    load_config, parse_planner_time, resolve_config_path, validate_planner_interval_minutes,
};

const DEFAULT_AUTOLOGIN_PREFIX: &str = "iclass-buaa";

pub(crate) fn install_autologin(args: InstallAutologinArgs) -> Result<()> {

    #[cfg(target_os = "linux")]
    {

        install_autologin_linux(args)
    }

    #[cfg(target_os = "macos")]
    {

        install_autologin_macos(args)
    }

    #[cfg(target_os = "windows")]
    {

        install_autologin_windows(args)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {

        let _ = args;

        bail!("当前平台暂不支持 install-autologin");
    }
}

pub(crate) fn uninstall_autologin(args: UninstallAutologinArgs) -> Result<()> {

    #[cfg(target_os = "linux")]
    {

        uninstall_autologin_linux(args)
    }

    #[cfg(target_os = "macos")]
    {

        uninstall_autologin_macos(args)
    }

    #[cfg(target_os = "windows")]
    {

        uninstall_autologin_windows(args)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {

        let _ = args;

        bail!("当前平台暂不支持 uninstall-autologin");
    }
}

pub(crate) fn autologin_status(args: AutologinStatusArgs) -> Result<()> {

    #[cfg(target_os = "linux")]
    {

        autologin_status_linux(args)
    }

    #[cfg(target_os = "macos")]
    {

        autologin_status_macos(args)
    }

    #[cfg(target_os = "windows")]
    {

        autologin_status_windows(args)
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos", target_os = "windows")))]
    {

        let _ = args;

        bail!("当前平台暂不支持 autologin-status");
    }
}

#[cfg(target_os = "linux")]

fn install_autologin_linux(args: InstallAutologinArgs) -> Result<()> {

    let config = load_config(args.config.as_deref())?;

    let config_path = resolve_config_path(args.config.as_deref())?;

    let output_dir = args.output_dir.unwrap_or(default_linux_autologin_dir()?);

    let unit_prefix = args
        .unit_prefix
        .unwrap_or_else(|| DEFAULT_AUTOLOGIN_PREFIX.to_string());

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
        "已生成 Linux systemd user units:\n{}\n{}",
        service_path.display(),
        timer_path.display()
    );

    println!("自动签到将在每天 {planner_time} 后开始按 {planner_interval_minutes} 分钟周期轮询。");

    println!(
        "启用方式: systemctl --user daemon-reload && systemctl --user enable --now {timer_name}"
    );

    print_status_hint(&unit_prefix);

    println!("安装后状态检查:");

    print_linux_autologin_status(&output_dir, &unit_prefix)?;

    Ok(())
}

#[cfg(target_os = "linux")]

fn uninstall_autologin_linux(args: UninstallAutologinArgs) -> Result<()> {

    let output_dir = args.output_dir.unwrap_or(default_linux_autologin_dir()?);

    let unit_prefix = args
        .unit_prefix
        .unwrap_or_else(|| DEFAULT_AUTOLOGIN_PREFIX.to_string());

    let service_name = format!("{unit_prefix}-planner.service");

    let timer_name = format!("{unit_prefix}-planner.timer");

    let service_path = output_dir.join(&service_name);

    let timer_path = output_dir.join(&timer_name);

    run_systemctl_user(["disable", "--now", &timer_name])?;

    remove_file_if_exists(&service_path)?;

    remove_file_if_exists(&timer_path)?;

    run_systemctl_user(["daemon-reload"])?;

    println!(
        "已卸载 Linux systemd user units:\n{}\n{}",
        service_path.display(),
        timer_path.display()
    );

    Ok(())
}

#[cfg(target_os = "linux")]

fn autologin_status_linux(args: AutologinStatusArgs) -> Result<()> {

    let output_dir = args.output_dir.unwrap_or(default_linux_autologin_dir()?);

    let unit_prefix = args
        .unit_prefix
        .unwrap_or_else(|| DEFAULT_AUTOLOGIN_PREFIX.to_string());

    print_linux_autologin_status(&output_dir, &unit_prefix)
}

#[cfg(target_os = "linux")]

fn print_linux_autologin_status(output_dir: &Path, unit_prefix: &str) -> Result<()> {

    let service_name = format!("{unit_prefix}-planner.service");

    let timer_name = format!("{unit_prefix}-planner.timer");

    let service_path = output_dir.join(&service_name);

    let timer_path = output_dir.join(&timer_name);

    println!("autologin-status | platform=linux | scheduler=systemd --user");

    println!("service_file={}", status_file_state(&service_path));

    println!("timer_file={}", status_file_state(&timer_path));

    print_command_status(
        "timer",
        "systemctl --user status",
        Command::new("systemctl")
            .arg("--user")
            .arg("status")
            .arg(&timer_name),
    )?;

    print_command_status(
        "timer-next-run",
        "systemctl --user list-timers",
        Command::new("systemctl")
            .arg("--user")
            .arg("list-timers")
            .arg("--all")
            .arg(&timer_name),
    )?;

    print_command_status(
        "recent-logs",
        "journalctl --user",
        Command::new("journalctl")
            .arg("--user")
            .arg("-u")
            .arg(&service_name)
            .arg("-n")
            .arg("20")
            .arg("--no-pager"),
    )?;

    Ok(())
}

#[cfg(target_os = "macos")]

fn install_autologin_macos(args: InstallAutologinArgs) -> Result<()> {

    let config = load_config(args.config.as_deref())?;

    let config_path = resolve_config_path(args.config.as_deref())?;

    let output_dir = args
        .output_dir
        .unwrap_or(default_macos_launch_agents_dir()?);

    let unit_prefix = args
        .unit_prefix
        .unwrap_or_else(|| DEFAULT_AUTOLOGIN_PREFIX.to_string());

    let planner_time = args.planner_time.unwrap_or(config.planner_time.clone());

    let planner_interval_minutes = args
        .planner_interval_minutes
        .unwrap_or(config.planner_interval_minutes);

    let binary_path = current_binary_path()?;

    parse_planner_time(&planner_time)?;

    validate_planner_interval_minutes(planner_interval_minutes)?;

    fs::create_dir_all(&output_dir)
        .with_context(|| format!("创建 launchd 目录失败: {}", output_dir.display()))?;

    let label = format!("{unit_prefix}.planner");

    let plist_path = output_dir.join(format!("{label}.plist"));

    let plist_content =
        render_launchd_plist(&label, &binary_path, &config_path, planner_interval_minutes);

    fs::write(&plist_path, plist_content)
        .with_context(|| format!("写入失败: {}", plist_path.display()))?;

    let plist_arg = plist_path.display().to_string();

    let _ = run_launchctl(["unload", &plist_arg]);

    run_launchctl(["load", &plist_arg])?;

    println!("已生成并加载 macOS launchd 配置:\n{}", plist_path.display());

    println!("自动签到将在每天 {planner_time} 后开始按 {planner_interval_minutes} 分钟周期轮询。");

    print_status_hint(&unit_prefix);

    println!("安装后状态检查:");

    print_macos_autologin_status(&output_dir, &unit_prefix)?;

    Ok(())
}

#[cfg(target_os = "macos")]

fn uninstall_autologin_macos(args: UninstallAutologinArgs) -> Result<()> {

    let output_dir = args
        .output_dir
        .unwrap_or(default_macos_launch_agents_dir()?);

    let unit_prefix = args
        .unit_prefix
        .unwrap_or_else(|| DEFAULT_AUTOLOGIN_PREFIX.to_string());

    let label = format!("{unit_prefix}.planner");

    let plist_path = output_dir.join(format!("{label}.plist"));

    let plist_arg = plist_path.display().to_string();

    let _ = run_launchctl(["unload", &plist_arg]);

    remove_file_if_exists(&plist_path)?;

    println!("已卸载 macOS launchd 配置:\n{}", plist_path.display());

    Ok(())
}

#[cfg(target_os = "macos")]

fn autologin_status_macos(args: AutologinStatusArgs) -> Result<()> {

    let output_dir = args
        .output_dir
        .unwrap_or(default_macos_launch_agents_dir()?);

    let unit_prefix = args
        .unit_prefix
        .unwrap_or_else(|| DEFAULT_AUTOLOGIN_PREFIX.to_string());

    print_macos_autologin_status(&output_dir, &unit_prefix)
}

#[cfg(target_os = "macos")]

fn print_macos_autologin_status(output_dir: &Path, unit_prefix: &str) -> Result<()> {

    let label = format!("{unit_prefix}.planner");

    let plist_path = output_dir.join(format!("{label}.plist"));

    println!("autologin-status | platform=macos | scheduler=launchd");

    println!("plist_file={}", status_file_state(&plist_path));

    print_command_status(
        "launchd-service",
        "launchctl print",
        Command::new("launchctl")
            .arg("print")
            .arg(format!("gui/{}/{}", current_uid(), label)),
    )?;

    println!(
        "recent_logs=macOS launchd does not keep one fixed log file; inspect Console.app or run: \
         log show --predicate 'process == \"{}\"' --last 1h",
        label
    );

    Ok(())
}

#[cfg(target_os = "windows")]

fn install_autologin_windows(args: InstallAutologinArgs) -> Result<()> {

    let config = load_config(args.config.as_deref())?;

    let config_path = resolve_config_path(args.config.as_deref())?;

    let output_dir = args.output_dir.unwrap_or(default_windows_autologin_dir()?);

    let unit_prefix = args
        .unit_prefix
        .unwrap_or_else(|| DEFAULT_AUTOLOGIN_PREFIX.to_string());

    let planner_time = args.planner_time.unwrap_or(config.planner_time.clone());

    let planner_interval_minutes = args
        .planner_interval_minutes
        .unwrap_or(config.planner_interval_minutes);

    let binary_path = current_binary_path()?;

    parse_planner_time(&planner_time)?;

    validate_planner_interval_minutes(planner_interval_minutes)?;

    fs::create_dir_all(&output_dir)
        .with_context(|| format!("创建 Windows 自动签到目录失败: {}", output_dir.display()))?;

    let task_name = format!("{unit_prefix}-planner");

    let script_path = output_dir.join(format!("{task_name}.cmd"));

    let script_content = render_windows_wrapper(&binary_path, &config_path);

    fs::write(&script_path, script_content)
        .with_context(|| format!("写入失败: {}", script_path.display()))?;

    let task_command = format!("\"{}\"", script_path.display());

    let interval = planner_interval_minutes.to_string();

    run_schtasks([
        "/Create",
        "/F",
        "/SC",
        "MINUTE",
        "/MO",
        &interval,
        "/TN",
        &task_name,
        "/TR",
        &task_command,
        "/IT",
    ])?;

    println!(
        "已生成并注册 Windows 计划任务:\n{}\n任务名: {}",
        script_path.display(),
        task_name
    );

    println!(
        "Windows 计划任务将每 {planner_interval_minutes} 分钟触发一次，程序会在每天 \
         {planner_time} 前自行跳过。"
    );

    print_status_hint(&unit_prefix);

    println!("安装后状态检查:");

    print_windows_autologin_status(&output_dir, &unit_prefix)?;

    Ok(())
}

#[cfg(target_os = "windows")]

fn uninstall_autologin_windows(args: UninstallAutologinArgs) -> Result<()> {

    let output_dir = args.output_dir.unwrap_or(default_windows_autologin_dir()?);

    let unit_prefix = args
        .unit_prefix
        .unwrap_or_else(|| DEFAULT_AUTOLOGIN_PREFIX.to_string());

    let task_name = format!("{unit_prefix}-planner");

    let script_path = output_dir.join(format!("{task_name}.cmd"));

    let _ = run_schtasks(["/Delete", "/TN", &task_name, "/F"]);

    remove_file_if_exists(&script_path)?;

    println!(
        "已卸载 Windows 计划任务:\n{}\n任务名: {}",
        script_path.display(),
        task_name
    );

    Ok(())
}

#[cfg(target_os = "windows")]

fn autologin_status_windows(args: AutologinStatusArgs) -> Result<()> {

    let output_dir = args.output_dir.unwrap_or(default_windows_autologin_dir()?);

    let unit_prefix = args
        .unit_prefix
        .unwrap_or_else(|| DEFAULT_AUTOLOGIN_PREFIX.to_string());

    print_windows_autologin_status(&output_dir, &unit_prefix)
}

#[cfg(target_os = "windows")]

fn print_windows_autologin_status(output_dir: &Path, unit_prefix: &str) -> Result<()> {

    let task_name = format!("{unit_prefix}-planner");

    let script_path = output_dir.join(format!("{task_name}.cmd"));

    println!("autologin-status | platform=windows | scheduler=schtasks");

    println!("wrapper_file={}", status_file_state(&script_path));

    print_command_status(
        "scheduled-task",
        "schtasks /Query",
        Command::new("schtasks")
            .arg("/Query")
            .arg("/TN")
            .arg(&task_name)
            .arg("/V")
            .arg("/FO")
            .arg("LIST"),
    )?;

    println!("recent_logs=Windows Task Scheduler History for task '{task_name}'");

    Ok(())
}

#[cfg(target_os = "linux")]

fn default_linux_autologin_dir() -> Result<PathBuf> {

    let home =
        env::var_os("HOME").ok_or_else(|| anyhow!("HOME 未设置，无法定位 systemd user 目录"))?;

    Ok(PathBuf::from(home).join(".config/systemd/user"))
}

#[cfg(target_os = "macos")]

fn default_macos_launch_agents_dir() -> Result<PathBuf> {

    let home = env::var_os("HOME")
        .ok_or_else(|| anyhow!("HOME 未设置，无法定位 launchd LaunchAgents 目录"))?;

    Ok(PathBuf::from(home).join("Library/LaunchAgents"))
}

#[cfg(target_os = "windows")]

fn default_windows_autologin_dir() -> Result<PathBuf> {

    if let Some(appdata) = env::var_os("APPDATA") {

        return Ok(PathBuf::from(appdata).join("iclass-buaa"));
    }

    let profile = env::var_os("USERPROFILE")
        .ok_or_else(|| anyhow!("APPDATA 和 USERPROFILE 都未设置，无法定位计划任务目录"))?;

    Ok(PathBuf::from(profile).join("AppData/Roaming/iclass-buaa"))
}

#[cfg(target_os = "linux")]

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

#[cfg(target_os = "macos")]

fn run_launchctl<const N: usize>(args: [&str; N]) -> Result<()> {

    let status = Command::new("launchctl")
        .args(args)
        .status()
        .context("执行 launchctl 失败")?;

    if !status.success() {

        bail!("launchctl 返回失败状态: {status}");
    }

    Ok(())
}

#[cfg(target_os = "windows")]

fn run_schtasks<const N: usize>(args: [&str; N]) -> Result<()> {

    let status = Command::new("schtasks")
        .args(args)
        .status()
        .context("执行 schtasks 失败")?;

    if !status.success() {

        bail!("schtasks 返回失败状态: {status}");
    }

    Ok(())
}

fn status_file_state(path: &Path) -> String {

    if path.is_file() {

        format!("present ({})", path.display())
    } else {

        format!("missing ({})", path.display())
    }
}

fn print_command_status(label: &str, command_label: &str, command: &mut Command) -> Result<()> {

    println!("{label}: command={command_label}");

    let output = match command.output() {
        Ok(output) => output,
        Err(error) => {

            println!("{label}: ok=no");

            println!("{label}: error={error}");

            return Ok(());
        }
    };

    println!(
        "{label}: ok={}",
        if output.status.success() { "yes" } else { "no" }
    );

    println!("{label}: exit_status={}", output.status);

    let stdout = String::from_utf8_lossy(&output.stdout);

    let stderr = String::from_utf8_lossy(&output.stderr);

    print_trimmed_output(label, "stdout", stdout.trim());

    print_trimmed_output(label, "stderr", stderr.trim());

    Ok(())
}

fn print_trimmed_output(label: &str, stream: &str, text: &str) {

    if text.is_empty() {

        println!("{label}: {stream}=<empty>");

        return;
    }

    println!("{label}: {stream}:");

    for line in text.lines().take(80) {

        println!("  {line}");
    }

    if text.lines().count() > 80 {

        println!("  ...");
    }
}

fn print_status_hint(unit_prefix: &str) {

    println!("健康检查: iclass_buaa_tui autologin-status --unit-prefix {unit_prefix}");
}

#[cfg(target_os = "macos")]

fn current_uid() -> String {

    Command::new("id")
        .arg("-u")
        .output()
        .ok()
        .filter(|output| output.status.success())
        .map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| env::var("UID").unwrap_or_else(|_| "unknown".to_string()))
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

fn render_planner_service(exe_path: &Path, config_path: &Path) -> String {

    let mut content = String::new();

    let _ = writeln!(content, "[Unit]");

    let _ = writeln!(content, "Description=BUAA iClass/BYKC periodic sign poller");

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

    let _ = writeln!(
        content,
        "Description=Run BUAA iClass/BYKC poller periodically"
    );

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

#[cfg(target_os = "macos")]

fn render_launchd_plist(
    label: &str,
    exe_path: &Path,
    config_path: &Path,
    planner_interval_minutes: u32,
) -> String {

    let interval_seconds = planner_interval_minutes.saturating_mul(60);

    format!(
        concat!(
            r#"<?xml version="1.0" encoding="UTF-8"?>"#,
            "\n",
            r#"<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">"#,
            "\n",
            r#"<plist version="1.0">"#,
            "\n",
            r#"<dict>"#,
            "\n  <key>Label</key>\n  <string>{}</string>",
            "\n  <key>ProgramArguments</key>",
            "\n  <array>",
            "\n    <string>{}</string>",
            "\n    <string>plan</string>",
            "\n    <string>--config</string>",
            "\n    <string>{}</string>",
            "\n  </array>",
            "\n  <key>RunAtLoad</key>\n  <true/>",
            "\n  <key>StartInterval</key>\n  <integer>{}</integer>",
            "\n</dict>",
            "\n</plist>\n"
        ),
        xml_escape(label),
        xml_escape(&exe_path.display().to_string()),
        xml_escape(&config_path.display().to_string()),
        interval_seconds,
    )
}

#[cfg(target_os = "windows")]

fn render_windows_wrapper(exe_path: &Path, config_path: &Path) -> String {

    format!(
        "@echo off\r\n\"{}\" plan --config \"{}\"\r\n",
        escape_cmd_arg(&exe_path.display().to_string()),
        escape_cmd_arg(&config_path.display().to_string()),
    )
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

#[cfg(target_os = "macos")]

fn xml_escape(text: &str) -> String {

    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}

#[cfg(target_os = "windows")]

fn escape_cmd_arg(text: &str) -> String {

    text.replace('"', "\"\"")
}
