//! Shared data models exchanged between the TUI, CLI, and the iClass/BYKC clients.

use crate::bykc::BykcApi;
use crate::iclass::IClassApi;

/// Login credentials normalized from either the TUI form or the CLI config file.
#[derive(Clone, Debug, Default)]
pub struct LoginInput {
    pub student_id: String,
    pub use_vpn: bool,
    pub vpn_username: String,
    pub vpn_password: String,
}

/// Login session shared across TUI and CLI operations.
#[derive(Clone, Debug)]
pub struct Session {
    pub api: IClassApi,
    pub bykc_api: Option<BykcApi>,
    pub user_id: String,
    pub user_name: String,
    pub session_id: String,
    // iclass need this
    pub server_time_offset_ms: i64,
    pub use_vpn: bool,
}

#[derive(Clone, Debug)]
pub struct CourseItem {
    pub name: String,
    pub id: String,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct CourseDetailItem {
    pub name: String,
    pub id: String,
    pub course_sched_id: String,
    pub date: String,
    pub start_time: String,
    pub end_time: String,
    pub sign_status: String,
}

#[derive(Clone, Debug)]
pub struct SignOutcome {
    pub message: String,
    pub success_like: bool,
    pub http_status: u16,
    pub server_status: String,
    /// Raw server payload retained for CLI debug output and future diagnostics.
    pub raw_response: serde_json::value::Value,
}

#[derive(Clone, Debug)]
pub struct SignQrData {
    pub qr_url: String,
    pub course_sched_id: String,
    pub timestamp: i64,
}

impl CourseDetailItem {
    /// Returns whether iClass already marks this row as signed.
    pub fn signed(&self) -> bool {
        self.sign_status == "1"
    }
}

impl Session {
    /// Returns the current server-aligned timestamp in milliseconds.
    pub fn server_now_millis(&self) -> i64 {
        chrono::Utc::now()
            .timestamp_millis()
            .saturating_add(self.server_time_offset_ms)
    }
}
