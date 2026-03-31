use crate::api::IClassApi;

#[derive(Clone, Debug, Default)]
pub struct LoginInput {
    pub student_id: String,
    pub use_vpn: bool,
    pub vpn_username: String,
    pub vpn_password: String,
}

#[derive(Clone, Debug)]
pub struct Session {
    pub api: IClassApi,
    pub user_id: String,
    pub user_name: String,
    pub session_id: String,
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
