//! Public BYKC data structures shared by the TUI, CLI, and API layer.

/// Lightweight course record shown in the BYKC course list.
#[allow(dead_code)]
#[derive(Clone, Debug, Default)]
pub struct BykcCourse {
    pub id: i64,
    pub course_name: String,
    pub course_position: String,
    pub course_teacher: String,
    pub course_start_date: String,
    pub course_end_date: String,
    pub course_select_start_date: String,
    pub course_select_end_date: String,
    pub course_max_count: i32,
    pub course_current_count: i32,
    pub category: String,
    pub sub_category: String,
    pub has_sign_points: bool,
    pub status: String,
    pub selected: bool,
    pub course_desc: String,
}

/// Full course detail used by the bottom detail panel and sign actions.
#[allow(dead_code)]
#[derive(Clone, Debug, Default)]
pub struct BykcCourseDetail {
    pub id: i64,
    pub course_name: String,
    pub course_position: String,
    pub course_contact: String,
    pub course_contact_mobile: String,
    pub course_teacher: String,
    pub course_start_date: String,
    pub course_end_date: String,
    pub course_select_start_date: String,
    pub course_select_end_date: String,
    pub course_cancel_end_date: String,
    pub course_max_count: i32,
    pub course_current_count: i32,
    pub category: String,
    pub sub_category: String,
    pub status: String,
    pub selected: bool,
    pub course_desc: String,
    pub sign_config: Option<BykcSignConfig>,
    pub checkin: Option<i32>,
    pub pass: Option<i32>,
    pub can_sign: bool,
    pub can_sign_out: bool,
}

/// Chosen-course record shown in the "已选课程" view.
#[allow(dead_code)]
#[derive(Clone, Debug, Default)]
pub struct BykcChosenCourse {
    pub id: i64,
    pub course_id: i64,
    pub course_name: String,
    pub course_position: String,
    pub course_teacher: String,
    pub course_start_date: String,
    pub course_end_date: String,
    pub select_date: String,
    pub course_cancel_end_date: String,
    pub category: String,
    pub sub_category: String,
    pub checkin: i32,
    pub score: Option<i32>,
    pub pass: Option<i32>,
    pub can_sign: bool,
    pub can_sign_out: bool,
    pub sign_config: Option<BykcSignConfig>,
    pub sign_info: String,
}

/// Attendance time window and allowed sign points from BYKC.
#[derive(Clone, Debug, Default)]
pub struct BykcSignConfig {
    pub sign_start_date: String,
    pub sign_end_date: String,
    pub sign_out_start_date: String,
    pub sign_out_end_date: String,
    pub sign_points: Vec<BykcSignPoint>,
}

/// BYKC sign-point definition used to construct a valid check-in location.
#[derive(Clone, Debug, Default)]
pub struct BykcSignPoint {
    pub lat: f64,
    pub lng: f64,
    pub radius: f64,
}
