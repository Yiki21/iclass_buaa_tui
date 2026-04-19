//! Entry point for the non-TUI automation CLI and its internal submodules.

mod args;
mod autologin;
mod config;
mod core;
mod planner;

use std::ffi::OsString;

use anyhow::Result;
use clap::Parser;

use self::args::{Cli, CommandKind};

pub fn should_run_cli(args: impl IntoIterator<Item = OsString>) -> bool {
    args.into_iter().nth(1).is_some()
}

/// Entry point for the non-TUI command set.
pub async fn run_cli() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        CommandKind::ListToday(args) => planner::list_today(args).await,
        CommandKind::Sign(args) => planner::sign_command(args).await,
        CommandKind::Plan(args) => planner::plan_command(args).await,
        CommandKind::InstallAutologin(args) => autologin::install_autologin(args),
        CommandKind::UninstallAutologin(args) => autologin::uninstall_autologin(args),
    }
}
