//! Clap argument definitions for the automation CLI surface.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use super::core::{SignAction, SignSource};

/// Top-level CLI parser for automation commands.
#[derive(Debug, Parser)]
#[command(author, version, about = "BUAA iClass TUI and automation CLI")]
pub(crate) struct Cli {
    #[command(subcommand)]
    pub(crate) command: CommandKind,
}

#[derive(Debug, Subcommand)]
pub(crate) enum CommandKind {
    /// Login and print today's filtered sign targets.
    ListToday(ListTodayArgs),
    /// Sign one iClass or BYKC target, retrying with fresh login each attempt.
    Sign(SignArgs),
    /// Run one automation cycle: fetch today's sign targets and sign due ones.
    Plan(PlanArgs),
    /// Install platform-native scheduled autologin automation.
    #[command(name = "install-autologin", alias = "install-systemd")]
    InstallAutologin(InstallAutologinArgs),
    /// Uninstall platform-native scheduled autologin automation.
    #[command(name = "uninstall-autologin", alias = "uninstall-systemd")]
    UninstallAutologin(UninstallAutologinArgs),
}

#[derive(Debug, Args)]
pub(crate) struct ListTodayArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,
    /// Print JSON instead of tab-separated text.
    #[arg(long)]
    pub(crate) json: bool,
}

#[derive(Debug, Args)]
pub(crate) struct SignArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,
    /// Sign target source.
    #[arg(long, value_enum, default_value_t = SignSourceArg::Iclass)]
    pub(crate) source: SignSourceArg,
    /// The course_sched_id to sign.
    #[arg(long, default_value = "")]
    pub(crate) course_sched_id: String,
    /// BYKC course id used for VPN-mode sign-in/sign-out.
    #[arg(long)]
    pub(crate) bykc_course_id: Option<i64>,
    /// Sign action for the selected source.
    #[arg(long, value_enum, default_value_t = SignActionArg::SignIn)]
    pub(crate) action: SignActionArg,
    /// Optional course name shown in logs/output.
    #[arg(long)]
    pub(crate) course_name: Option<String>,
    /// Override retry_count from config.
    #[arg(long)]
    pub(crate) retry_count: Option<u32>,
    /// Override retry_interval_seconds from config.
    #[arg(long)]
    pub(crate) retry_interval_seconds: Option<u64>,
    /// Print server raw response and local timing diagnostics.
    #[arg(long)]
    pub(crate) debug: bool,
}

#[derive(Debug, Args)]
pub(crate) struct PlanArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,
    /// Prefix for generated scheduler task names. Kept for compatibility.
    #[arg(long)]
    pub(crate) unit_prefix: Option<String>,
    /// Only print today's evaluation without attempting sign.
    #[arg(long)]
    pub(crate) dry_run: bool,
}

#[derive(Debug, Args)]
pub(crate) struct InstallAutologinArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,
    /// Target directory for generated scheduler files when applicable.
    #[arg(long)]
    pub(crate) output_dir: Option<PathBuf>,
    /// Prefix for generated scheduler task names.
    #[arg(long)]
    pub(crate) unit_prefix: Option<String>,
    /// Override planner_time from config when generating the scheduler entry.
    #[arg(long)]
    pub(crate) planner_time: Option<String>,
    /// Override planner_interval_minutes from config when generating the scheduler entry.
    #[arg(long)]
    pub(crate) planner_interval_minutes: Option<u32>,
}

#[derive(Debug, Args)]
pub(crate) struct UninstallAutologinArgs {
    /// Target directory containing generated scheduler files when applicable.
    #[arg(long)]
    pub(crate) output_dir: Option<PathBuf>,
    /// Prefix for generated scheduler task names.
    #[arg(long)]
    pub(crate) unit_prefix: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum SignSourceArg {
    Iclass,
    Bykc,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub(crate) enum SignActionArg {
    SignIn,
    SignOut,
}

impl From<SignSourceArg> for SignSource {
    fn from(value: SignSourceArg) -> Self {
        match value {
            SignSourceArg::Iclass => Self::IClass,
            SignSourceArg::Bykc => Self::Bykc,
        }
    }
}

impl From<SignActionArg> for SignAction {
    fn from(value: SignActionArg) -> Self {
        match value {
            SignActionArg::SignIn => Self::SignIn,
            SignActionArg::SignOut => Self::SignOut,
        }
    }
}
