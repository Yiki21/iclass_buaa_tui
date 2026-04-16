use crate::api::IClassApi;
use serde_json::Value;

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
    pub user_id: String,
    pub user_name: String,
    pub session_id: String,
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
    pub raw_response: Value,
}

#[derive(Clone, Debug)]
pub struct SignQrData {
    pub qr_url: String,
    pub course_sched_id: String,
    pub timestamp: i64,
}

impl CourseDetailItem {
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
