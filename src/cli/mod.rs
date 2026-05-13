//! Entry point for the non-TUI automation CLI and its internal submodules.

mod args;
mod autologin;
mod config;
mod core;
mod planner;

use std::ffi::OsString;

use anyhow::Result;
use clap::Parser;

use crate::logging;

use self::args::{Cli, CommandKind};

pub fn should_run_cli(args: impl IntoIterator<Item = OsString>) -> bool {

    args.into_iter().nth(1).is_some()
}

/// Entry point for the non-TUI command set.

pub async fn run_cli() -> Result<()> {

    let cli = Cli::parse();

    let log_level = logging::parse_level(&cli.log_level)?;

    let log_path = logging::init(log_level, cli.log_file.clone())?;

    logging::event(
        logging::LogLevel::Info,
        "cli",
        "CLI started",
        serde_json::json!({ "log_file": log_path }),
    );

    match cli.command {
        CommandKind::ListToday(args) => planner::list_today(args).await,
        CommandKind::Sign(args) => planner::sign_command(args).await,
        CommandKind::Plan(args) => planner::plan_command(args).await,
        CommandKind::Doctor(args) => planner::doctor_command(args).await,
        CommandKind::InstallAutologin(args) => autologin::install_autologin(args),
        CommandKind::AutologinStatus(args) => autologin::autologin_status(args),
        CommandKind::UninstallAutologin(args) => autologin::uninstall_autologin(args),
    }
}
