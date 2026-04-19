//! Internal serde models for BYKC API payloads and response envelopes.

use serde::Deserialize;

/// Generic outer response envelope used by BYKC APIs.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BykcApiResponse<T> {
    pub(super) status: String,
    #[serde(default)]
    pub(super) data: Option<T>,
    #[serde(default)]
    pub(super) errmsg: String,
}

impl<T> BykcApiResponse<T> {
    pub(super) fn is_success(&self) -> bool {
        self.status == "0"
    }
}

/// Paged payload returned by the course-list query.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BykcCoursePageResult {
    #[serde(default)]
    pub(super) content: Vec<BykcCourseRaw>,
    #[serde(default)]
    pub(super) total_pages: i32,
}

#[derive(Clone, Debug, Default, Deserialize)]
pub(super) struct BykcCourseActionResult {}

/// Global BYKC configuration containing the active semester range.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BykcAllConfig {
    #[serde(default)]
    pub(super) semester: Vec<BykcSemester>,
}

/// Semester definition extracted from the BYKC config response.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BykcSemester {
    #[serde(default)]
    pub(super) semester_start_date: Option<String>,
    #[serde(default)]
    pub(super) semester_end_date: Option<String>,
}

/// Payload wrapper around the chosen-course list.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BykcChosenCoursePayload {
    #[serde(default)]
    pub(super) course_list: Vec<BykcChosenCourseRaw>,
}

/// Raw course record returned by multiple BYKC APIs.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BykcCourseRaw {
    pub(super) id: i64,
    #[serde(default)]
    pub(super) course_name: String,
    #[serde(default)]
    pub(super) course_position: Option<String>,
    #[serde(default)]
    pub(super) course_teacher: Option<String>,
    #[serde(default)]
    pub(super) course_start_date: Option<String>,
    #[serde(default)]
    pub(super) course_end_date: Option<String>,
    #[serde(default)]
    pub(super) course_select_start_date: Option<String>,
    #[serde(default)]
    pub(super) course_select_end_date: Option<String>,
    #[serde(default)]
    pub(super) course_cancel_end_date: Option<String>,
    #[serde(default)]
    pub(super) course_max_count: i32,
    #[serde(default)]
    pub(super) course_current_count: Option<i32>,
    #[serde(default)]
    pub(super) course_new_kind1: Option<BykcCourseKind>,
    #[serde(default)]
    pub(super) course_new_kind2: Option<BykcCourseKind>,
    #[serde(default)]
    pub(super) selected: Option<bool>,
    #[serde(default)]
    pub(super) course_desc: Option<String>,
    #[serde(default)]
    pub(super) course_contact: Option<String>,
    #[serde(default)]
    pub(super) course_contact_mobile: Option<String>,
    #[serde(default)]
    pub(super) course_sign_config: Option<String>,
}

/// Raw category node embedded in a BYKC course record.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BykcCourseKind {
    #[serde(default)]
    pub(super) kind_name: String,
}

/// Raw chosen-course record returned by BYKC.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BykcChosenCourseRaw {
    pub(super) id: i64,
    #[serde(default)]
    pub(super) select_date: Option<String>,
    #[serde(default)]
    pub(super) course_info: Option<BykcCourseRaw>,
    #[serde(default)]
    pub(super) checkin: Option<i32>,
    #[serde(default)]
    pub(super) score: Option<i32>,
    #[serde(default)]
    pub(super) pass: Option<i32>,
    #[serde(default)]
    pub(super) sign_info: Option<String>,
}

/// Raw JSON structure stored inside `courseSignConfig`.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BykcSignConfigRaw {
    #[serde(default)]
    pub(super) sign_start_date: Option<String>,
    #[serde(default)]
    pub(super) sign_end_date: Option<String>,
    #[serde(default)]
    pub(super) sign_out_start_date: Option<String>,
    #[serde(default)]
    pub(super) sign_out_end_date: Option<String>,
    #[serde(default)]
    pub(super) sign_point_list: Vec<BykcSignPointRaw>,
}

/// Raw sign-point entry embedded in the sign config JSON.
#[derive(Clone, Debug, Default, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(super) struct BykcSignPointRaw {
    pub(super) lat: f64,
    pub(super) lng: f64,
    #[serde(default)]
    pub(super) radius: f64,
}
