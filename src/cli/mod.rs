//! Entry point for the non-TUI automation CLI and its internal submodules.

mod args;
mod autologin;
mod clockin;
mod config;
mod core;
mod eval;
mod planner;
mod seat;
mod venue;

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
        CommandKind::Notify(args) => planner::notify_command(args),
        CommandKind::Venues(args) => venue::venues_command(args).await,
        CommandKind::VenueSlots(args) => venue::venue_slots_command(args).await,
        CommandKind::VenueReserve(args) => venue::venue_reserve_command(args).await,
        CommandKind::VenueOrders(args) => venue::venue_orders_command(args).await,
        CommandKind::Seats(args) => seat::seats_command(args).await,
        CommandKind::SeatBook(args) => seat::seat_book_command(args).await,
        CommandKind::SeatOrders(args) => seat::seat_orders_command(args).await,
        CommandKind::Clockin(args) => clockin::clockin_command(args).await,
        CommandKind::ClockinSubmit(args) => clockin::clockin_submit_command(args).await,
        CommandKind::Eval(args) => eval::eval_command(args).await,
        CommandKind::EvalSubmit(args) => eval::eval_submit_command(args).await,
        CommandKind::Today(args) => planner::today_command(args).await,
        CommandKind::Exams(args) => planner::exams_command(args).await,
        CommandKind::Grades(args) => planner::grades_command(args).await,
        CommandKind::Classrooms(args) => planner::classrooms_command(args).await,
        CommandKind::Tasks(args) => planner::tasks_command(args).await,
        CommandKind::ScheduleExport(args) => planner::schedule_export_command(args).await,
        CommandKind::ScheduleDiff(args) => planner::schedule_diff_command(args).await,
        CommandKind::InstallAutologin(args) => autologin::install_autologin(args),
        CommandKind::AutologinStatus(args) => autologin::autologin_status(args),
        CommandKind::UninstallAutologin(args) => autologin::uninstall_autologin(args),
    }
}
