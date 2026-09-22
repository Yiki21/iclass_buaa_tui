//! Clap argument definitions for the automation CLI surface.

use std::path::PathBuf;

use clap::{Args, Parser, Subcommand};

use super::core::{SignAction, SignSource};

/// Top-level CLI parser for automation commands.
#[derive(Debug, Parser)]
#[command(author, version, about = "BUAA iClass TUI and automation CLI")]

pub(crate) struct Cli {
    /// Minimum structured log level: error, warn, info, or debug.
    #[arg(long, global = true, default_value = "info")]
    pub(crate) log_level: String,
    /// Structured JSONL log file path. Defaults to the user state directory.
    #[arg(long, global = true)]
    pub(crate) log_file:  Option<PathBuf>,
    #[command(subcommand)]
    pub(crate) command:   CommandKind,
}

#[derive(Debug, Subcommand)]

pub(crate) enum CommandKind {
    /// Login and print today's filtered sign targets.
    ListToday(ListTodayArgs),
    /// Sign one iClass or BYKC target, retrying with fresh login each attempt.
    Sign(SignArgs),
    /// Run one automation cycle: fetch today's sign targets and sign due ones.
    Plan(PlanArgs),
    /// Check WebVPN, SSO, iClass, and BYKC connectivity before login.
    Doctor(DoctorArgs),
    /// Send a test desktop notification to verify notifications work here.
    Notify(NotifyArgs),
    /// List seminar rooms (研讨室) available for booking.
    Venues(VenueArgs),
    /// Show one room's bookable time slots on a date.
    VenueSlots(VenueSlotsArgs),
    /// Reserve a seminar room. Requires --yes; this claims a real room.
    VenueReserve(VenueReserveArgs),
    /// List and optionally cancel your seminar-room reservations.
    VenueOrders(VenueOrdersArgs),
    /// Show today's cached academic courses and next class.
    Today(TodayArgs),
    /// Show exam arrangements for one academic term.
    Exams(AcademicListArgs),
    /// Show grades for one academic term.
    Grades(AcademicListArgs),
    /// Query available classrooms by campus and date.
    Classrooms(ClassroomArgs),
    /// Show read-only assignment summaries from supported course systems.
    Tasks(TaskArgs),
    /// Export a cached semester schedule.
    ScheduleExport(ScheduleExportArgs),
    /// Compare two cached semester snapshots.
    ScheduleDiff(ScheduleDiffArgs),
    /// Install platform-native scheduled autologin automation.
    #[command(name = "install-autologin", alias = "install-systemd")]
    InstallAutologin(InstallAutologinArgs),
    /// Show platform scheduler health for autologin automation.
    #[command(name = "autologin-status")]
    AutologinStatus(AutologinStatusArgs),
    /// Uninstall platform-native scheduled autologin automation.
    #[command(name = "uninstall-autologin", alias = "uninstall-systemd")]
    UninstallAutologin(UninstallAutologinArgs),
}

#[derive(Debug, Args)]

pub(crate) struct ListTodayArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    pub(crate) config:      Option<PathBuf>,
    /// Print JSON instead of tab-separated text.
    #[arg(long)]
    pub(crate) json:        bool,
    /// Print structured login diagnostics on login failure.
    #[arg(long)]
    pub(crate) debug_login: bool,
}

#[derive(Debug, Args)]

pub(crate) struct SignArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    pub(crate) config:                 Option<PathBuf>,
    /// Sign target source.
    #[arg(long, value_enum, default_value_t = SignSourceArg::Iclass)]
    pub(crate) source:                 SignSourceArg,
    /// The course_sched_id to sign.
    #[arg(long, default_value = "")]
    pub(crate) course_sched_id:        String,
    /// BYKC course id used for VPN-mode sign-in/sign-out.
    #[arg(long)]
    pub(crate) bykc_course_id:         Option<i64>,
    /// Sign action for the selected source.
    #[arg(long, value_enum, default_value_t = SignActionArg::SignIn)]
    pub(crate) action:                 SignActionArg,
    /// Optional course name shown in logs/output.
    #[arg(long)]
    pub(crate) course_name:            Option<String>,
    /// Override retry_count from config.
    #[arg(long)]
    pub(crate) retry_count:            Option<u32>,
    /// Override retry_interval_seconds from config.
    #[arg(long)]
    pub(crate) retry_interval_seconds: Option<u64>,
    /// Print server raw response and local timing diagnostics.
    #[arg(long)]
    pub(crate) debug:                  bool,
    /// Print structured login diagnostics on login failure.
    #[arg(long)]
    pub(crate) debug_login:            bool,
}

#[derive(Debug, Args)]

pub(crate) struct PlanArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    pub(crate) config:      Option<PathBuf>,
    /// Prefix for generated scheduler task names. Kept for compatibility.
    #[arg(long)]
    pub(crate) unit_prefix: Option<String>,
    /// Only print today's evaluation without attempting sign.
    #[arg(long)]
    pub(crate) dry_run:     bool,
    /// Print structured login diagnostics on login failure.
    #[arg(long)]
    pub(crate) debug_login: bool,
}

#[derive(Debug, Args)]

pub(crate) struct VenueArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    pub(crate) config:      Option<PathBuf>,
    /// Filter to rooms whose name contains this text.
    #[arg(long)]
    pub(crate) query:       Option<String>,
    /// Print JSON instead of a human-readable table.
    #[arg(long)]
    pub(crate) json:        bool,
    /// Print structured login diagnostics on login failure.
    #[arg(long)]
    pub(crate) debug_login: bool,
}

#[derive(Debug, Args)]

pub(crate) struct VenueSlotsArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    pub(crate) config:      Option<PathBuf>,
    /// Room id from `venues`.
    #[arg(long)]
    pub(crate) site:        i64,
    /// Date in YYYY-MM-DD format. Defaults to today.
    #[arg(long)]
    pub(crate) date:        Option<String>,
    /// Print JSON instead of a human-readable table.
    #[arg(long)]
    pub(crate) json:        bool,
    /// Print structured login diagnostics on login failure.
    #[arg(long)]
    pub(crate) debug_login: bool,
}

#[derive(Debug, Args)]

pub(crate) struct VenueReserveArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    pub(crate) config:       Option<PathBuf>,
    /// Room id from `venues`.
    #[arg(long)]
    pub(crate) site:         i64,
    /// Date in YYYY-MM-DD format.
    #[arg(long)]
    pub(crate) date:         String,
    /// Time-slot ids from `venue-slots`, comma separated or repeated.
    #[arg(long, value_delimiter = ',', required = true)]
    pub(crate) slots:        Vec<i64>,
    /// Contact phone number required by the service.
    #[arg(long)]
    pub(crate) phone:        String,
    /// Purpose type id from `venue-slots`.
    #[arg(long, default_value_t = 1)]
    pub(crate) purpose:      i64,
    /// Short title for the reservation.
    #[arg(long)]
    pub(crate) theme:        String,
    /// Number of attendees.
    #[arg(long, default_value_t = 1)]
    pub(crate) joiners:      i64,
    /// Activity description.
    #[arg(long, default_value = "小组讨论")]
    pub(crate) activity:     String,
    /// Names of the other attendees, comma separated.
    #[arg(long, default_value = "")]
    pub(crate) joiner_names: String,
    /// Confirm the reservation. Without this the command only previews.
    ///
    /// Why:
    /// Reserving claims a physical room that other students cannot then use.
    /// Requiring an explicit flag means a mistyped command cannot take one.
    #[arg(long)]
    pub(crate) yes:          bool,
    /// Print structured login diagnostics on login failure.
    #[arg(long)]
    pub(crate) debug_login:  bool,
}

#[derive(Debug, Args)]

pub(crate) struct VenueOrdersArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    pub(crate) config:      Option<PathBuf>,
    /// Cancel this order id. Requires --yes.
    #[arg(long)]
    pub(crate) cancel:      Option<i64>,
    /// Confirm the cancellation.
    #[arg(long)]
    pub(crate) yes:         bool,
    /// Print JSON instead of a human-readable table.
    #[arg(long)]
    pub(crate) json:        bool,
    /// Print structured login diagnostics on login failure.
    #[arg(long)]
    pub(crate) debug_login: bool,
}

#[derive(Debug, Args)]

pub(crate) struct NotifyArgs {
    /// Message body. Defaults to a fixed sample.
    #[arg(long)]
    pub(crate) message: Option<String>,
}

#[derive(Debug, Args)]

pub(crate) struct DoctorArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,
    /// Print JSON instead of human-readable text.
    #[arg(long)]
    pub(crate) json:   bool,
}

#[derive(Debug, Args)]

pub(crate) struct TodayArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,
    /// Print JSON instead of a human-readable table.
    #[arg(long)]
    pub(crate) json:   bool,
}

#[derive(Debug, Args)]

pub(crate) struct AcademicListArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    pub(crate) config:      Option<PathBuf>,
    /// Academic term code, such as 2025-2026-1.
    #[arg(long)]
    pub(crate) term:        Option<String>,
    /// Load every term the portal lists instead of one, and print a
    /// credit-weighted GPA summary. Only meaningful for `grades`.
    #[arg(long, conflicts_with = "term")]
    pub(crate) all:         bool,
    /// Print JSON instead of a human-readable table.
    #[arg(long)]
    pub(crate) json:        bool,
    /// Print structured login diagnostics on login failure.
    #[arg(long)]
    pub(crate) debug_login: bool,
}

#[derive(Debug, Args)]

pub(crate) struct ClassroomArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    pub(crate) config:      Option<PathBuf>,
    /// Campus id: 1=学院路, 2=沙河, 3=杭州.
    #[arg(long)]
    pub(crate) campus:      i64,
    /// Date in YYYY-MM-DD format. Defaults to today.
    #[arg(long)]
    pub(crate) date:        Option<String>,
    /// Only show rooms free for this section.
    #[arg(long)]
    pub(crate) section:     Option<usize>,
    /// Print JSON instead of a human-readable table.
    #[arg(long)]
    pub(crate) json:        bool,
    /// Print structured login diagnostics on login failure.
    #[arg(long)]
    pub(crate) debug_login: bool,
}

#[derive(Debug, Args)]

pub(crate) struct TaskArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    pub(crate) config:      Option<PathBuf>,
    /// Print JSON instead of a human-readable table.
    #[arg(long)]
    pub(crate) json:        bool,
    /// Print structured login diagnostics on login failure.
    #[arg(long)]
    pub(crate) debug_login: bool,
}

#[derive(Debug, Args)]

pub(crate) struct ScheduleExportArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,
    /// Cached academic term code. Defaults to the current cached term.
    #[arg(long)]
    pub(crate) term:   Option<String>,
    /// Output format: markdown, csv, json, or ics.
    #[arg(long, value_parser = ["markdown", "csv", "json", "ics"], default_value = "markdown")]
    pub(crate) format: String,
    /// Write to a file instead of stdout.
    #[arg(long)]
    pub(crate) output: Option<PathBuf>,
}

#[derive(Debug, Args)]

pub(crate) struct ScheduleDiffArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    pub(crate) config: Option<PathBuf>,
    /// First cached term code. Defaults to the oldest cached term.
    #[arg(long)]
    pub(crate) from:   Option<String>,
    /// Second cached term code. Defaults to the newest cached term.
    #[arg(long)]
    pub(crate) to:     Option<String>,
    /// Print JSON instead of a human-readable list.
    #[arg(long)]
    pub(crate) json:   bool,
}

#[derive(Debug, Args)]

pub(crate) struct InstallAutologinArgs {
    /// Explicit config file path. Overrides XDG config lookup.
    #[arg(long)]
    pub(crate) config:                   Option<PathBuf>,
    /// Target directory for generated scheduler files when applicable.
    #[arg(long)]
    pub(crate) output_dir:               Option<PathBuf>,
    /// Prefix for generated scheduler task names.
    #[arg(long)]
    pub(crate) unit_prefix:              Option<String>,
    /// Override planner_time from config when generating the scheduler entry.
    #[arg(long)]
    pub(crate) planner_time:             Option<String>,
    /// Override planner_interval_minutes from config when generating the scheduler entry.
    #[arg(long)]
    pub(crate) planner_interval_minutes: Option<u32>,
}

#[derive(Debug, Args)]

pub(crate) struct AutologinStatusArgs {
    /// Target directory containing generated scheduler files when applicable.
    #[arg(long)]
    pub(crate) output_dir:  Option<PathBuf>,
    /// Prefix for generated scheduler task names.
    #[arg(long)]
    pub(crate) unit_prefix: Option<String>,
}

#[derive(Debug, Args)]

pub(crate) struct UninstallAutologinArgs {
    /// Target directory containing generated scheduler files when applicable.
    #[arg(long)]
    pub(crate) output_dir:  Option<PathBuf>,
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
