//! Application state, async event routing, and keyboard-driven TUI behavior.

use chrono::{Datelike, Duration as ChronoDuration, Local, NaiveDate, TimeZone};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use qrcode::{EcLevel, QrCode, render::svg};
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};
use std::cell::RefCell;
use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

use crate::academic::{ClassroomRoom, ExamItem, GradeItem};
use crate::bykc::{
    BykcApi, BykcChosenCourse, BykcCourse, BykcCourseDetail, BykcSignAction, BykcStatistics,
    can_deselect_bykc_course,
};
use crate::iclass::IClassApi;
use crate::logging::{self, LogLevel};
use crate::model::{
    CourseDetailItem, DoctorReport, LoginCaptchaChallenge, LoginDiagnostic, LoginInput, LoginStart,
    Session, SignOutcome,
};
use crate::schedule::{
    ScheduleEntry, SemesterSchedule, Week, load_cached_schedules, save_cached_schedule,
};
use crate::tasks::AssignmentItem;

const MAX_EVENT_LOG_ENTRIES: usize = 8;

#[derive(Clone, Debug)]

pub enum AsyncEvent {
    Login(Result<LoginSuccess, LoginFailure>),
    LoginCaptcha(Result<PendingCaptchaLogin, String>),
    Refresh(Result<Vec<CourseDetailItem>, String>),
    Sign(Result<SignOutcome, String>),
    BykcSync(Box<Result<BykcSyncSuccess, String>>),
    ScheduleImport(Result<SemesterSchedule, String>),
    Exams(Result<Vec<ExamItem>, String>),
    Grades(Result<Vec<GradeItem>, String>),
    AllGrades(Result<crate::academic::GradesForTerms, String>),
    Classrooms(Result<Vec<ClassroomRoom>, String>),
    Tasks(Result<Vec<AssignmentItem>, String>),
    VersionCheck(Result<VersionInfo, String>),
    Doctor(Result<DoctorReport, String>),
}

#[derive(Clone, Debug)]

pub struct LoginSuccess {
    pub session:               Session,
    pub courses:               Vec<CourseDetailItem>,
    pub course_prefetch_error: Option<String>,
    pub schedule_account:      String,
}

#[derive(Clone, Debug)]

pub struct LoginFailure {
    pub message:    String,
    pub diagnostic: LoginDiagnostic,
}

#[derive(Clone, Debug)]

pub struct PendingCaptchaLogin {
    pub api:       IClassApi,
    pub input:     LoginInput,
    pub challenge: LoginCaptchaChallenge,
}

#[derive(Clone, Debug)]

pub struct QrDisplay {
    pub course_sched_id: String,
    pub course_name:     String,
    pub qr_url:          String,
    pub timestamp:       i64,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]

pub enum QrMode {
    #[default]
    Terminal,
    External,
}

#[derive(Clone, Debug)]

pub struct VersionInfo {
    pub current:    String,
    pub latest:     String,
    pub latest_url: String,
    pub is_latest:  bool,
}

#[derive(Clone, Debug)]

pub struct WeekGroup {
    pub key:            String,
    pub label:          String,
    pub start_date:     String,
    pub end_date:       String,
    pub course_indices: Vec<usize>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]

pub enum Screen {
    Login,
    Workspace,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]

pub enum WorkspaceTab {
    Schedule,
    IClass,
    Bykc,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]

pub enum CourseView {
    #[default]
    Today,
    Schedule,
    Exams,
    Grades,
    Classrooms,
    Tasks,
}

#[derive(Clone, Debug, Default)]

pub struct ScheduleState {
    pub account:           Option<String>,
    pub semesters:         Vec<SemesterSchedule>,
    pub selected_semester: usize,
    pub selected_week:     usize,
    pub selected_entry:    usize,
    pub loaded:            bool,
    pub updating:          bool,
    pub view:              CourseView,
    pub query:             String,
    pub filtering:         bool,
    pub exams:             Vec<ExamItem>,
    pub grades:            Vec<GradeItem>,
    /// Terms already merged into `grades` by the all-terms load.
    ///
    /// Why:
    /// Once several terms are loaded, "the current term's grades" is no longer
    /// what the list shows. Recording the count lets the header say so and lets
    /// a single-term refresh know it would be replacing a wider view.
    pub grades_all_terms:  bool,

    pub classrooms:       Vec<ClassroomRoom>,
    pub tasks:            Vec<AssignmentItem>,
    pub academic_loading: bool,
    pub classroom_campus: i64,
    pub classroom_date:   String,
}

impl ScheduleState {
    pub fn from_cache(account: String, mut semesters: Vec<SemesterSchedule>) -> Self {

        let today = Local::now().date_naive();

        for semester in &mut semesters {

            for week in &mut semester.weeks {

                week.current = date_in_range(today, &week.start_date, &week.end_date);
            }
        }

        semesters.sort_by(|left, right| right.term_code.cmp(&left.term_code));

        let mut state = Self {
            account: Some(account),
            semesters,
            loaded: true,
            classroom_campus: 1,
            classroom_date: Local::now().date_naive().to_string(),
            ..Self::default()
        };

        state.select_current_semester();

        state
    }

    pub fn current_semester(&self) -> Option<&SemesterSchedule> {

        self.semesters.get(self.selected_semester)
    }

    pub fn current_week(&self) -> Option<&Week> {

        self.current_semester()?.weeks.get(self.selected_week)
    }

    pub fn current_schedule(&self) -> Option<&crate::schedule::WeeklySchedule> {

        let number = self.current_week()?.number;

        self.current_semester()?.schedule_for(number)
    }

    pub fn selected_entry(&self) -> Option<&ScheduleEntry> {

        self.current_schedule()?.entries.get(self.selected_entry)
    }

    /// Portal label for the schedule tab, based on the cached snapshot.
    ///
    /// Why:
    /// Undergraduate BYXT and graduate GSMIS are different systems. Until a
    /// snapshot exists the portal is unknown, so the tab stays neutral instead
    /// of claiming to be the graduate timetable.

    pub fn portal_label(&self) -> &'static str {

        self.semesters
            .first()
            .map(|semester| semester.portal.label())
            .unwrap_or("课表")
    }

    pub fn visible_entries(&self) -> Vec<(usize, &ScheduleEntry)> {

        let query = self.query.trim();

        self.current_schedule()
            .map(|schedule| {

                schedule
                    .entries
                    .iter()
                    .enumerate()
                    .filter(|(_, entry)| {

                        query.is_empty()
                            || entry.course_name.contains(query)
                            || entry.course_code.contains(query)
                            || entry
                                .place
                                .as_deref()
                                .is_some_and(|value| value.contains(query))
                            || entry
                                .weeks_and_teachers
                                .as_deref()
                                .is_some_and(|value| value.contains(query))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    pub fn today_entries(&self) -> Vec<(usize, &ScheduleEntry)> {

        let weekday = Local::now().weekday().number_from_monday() as usize;

        self.visible_entries()
            .into_iter()
            .filter(|(_, entry)| entry.day_of_week == Some(weekday))
            .collect()
    }

    pub fn switch_view(&mut self, view: CourseView) {

        self.view = view;

        self.selected_entry = 0;

        self.filtering = false;
    }

    pub fn select_current_semester(&mut self) {

        self.selected_semester = self
            .semesters
            .iter()
            .position(|semester| semester.weeks.iter().any(|week| week.current))
            .unwrap_or(0);

        self.reset_week_selection();
    }

    pub fn select_semester(&mut self, index: usize) {

        if index < self.semesters.len() {

            self.selected_semester = index;

            self.reset_week_selection();
        }
    }

    pub fn move_week(&mut self, delta: isize) {

        let len = self
            .current_semester()
            .map(|item| item.weeks.len())
            .unwrap_or(0);

        self.selected_week = clamp_step(self.selected_week, len, delta);

        self.selected_entry = 0;
    }

    pub fn move_entry(&mut self, delta: isize) {

        let visible = match self.view {
            CourseView::Today => self.today_entries(),
            CourseView::Schedule => self.visible_entries(),
            _ => Vec::new(),
        };

        let position = visible
            .iter()
            .position(|(index, _)| *index == self.selected_entry)
            .unwrap_or(0);

        let next = clamp_step(position, visible.len(), delta);

        self.selected_entry = visible
            .get(next)
            .map(|(index, _)| *index)
            .unwrap_or_default();
    }

    fn reset_week_selection(&mut self) {

        self.selected_week = self
            .current_semester()
            .and_then(|semester| semester.weeks.iter().position(|week| week.current))
            .unwrap_or(0);

        self.selected_entry = 0;
    }

    fn replace_semester(&mut self, schedule: SemesterSchedule) {

        let code = schedule.term_code.clone();

        self.semesters.retain(|item| item.term_code != code);

        self.semesters.push(schedule);

        self.semesters
            .sort_by(|left, right| right.term_code.cmp(&left.term_code));

        self.selected_semester = self
            .semesters
            .iter()
            .position(|item| item.term_code == code)
            .unwrap_or(0);

        self.reset_week_selection();

        self.loaded = true;
    }
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]

pub enum BykcView {
    #[default]
    Courses,
    Chosen,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]

pub enum LoginFocus {
    StudentId,
    UseVpn,
    VpnUsername,
    VpnPassword,
    Captcha,
    RememberMe,
}

/// Login form state for the TUI login screen.
#[derive(Clone, Debug, Default)]

pub struct LoginForm {
    pub student_id:       String,
    pub use_vpn:          bool,
    pub vpn_username:     String,
    pub vpn_password:     String,
    pub captcha:          String,
    pub captcha_required: bool,
    pub remember_me:      bool,
    pub focus:            usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]

struct RememberedLogin {
    student_id:   String,
    use_vpn:      bool,
    vpn_username: String,
    vpn_password: String,
}

impl LoginForm {
    fn from_remembered(remembered: RememberedLogin) -> Self {

        Self {
            student_id:       remembered.student_id,
            use_vpn:          remembered.use_vpn,
            vpn_username:     remembered.vpn_username,
            vpn_password:     remembered.vpn_password,
            captcha:          String::new(),
            captcha_required: false,
            remember_me:      true,
            focus:            0,
        }
    }

    /// Returns the fields currently visible to the user.

    pub fn visible_focuses(&self) -> Vec<LoginFocus> {

        let mut fields = vec![LoginFocus::UseVpn];

        if self.use_vpn {

            fields.push(LoginFocus::VpnUsername);
        } else {

            fields.push(LoginFocus::StudentId);
        }

        // Both connection modes now establish the same SSO session.  The
        // historical VpnPassword field is retained in the model and config so
        // existing remembered credentials remain readable.
        fields.push(LoginFocus::VpnPassword);

        if self.captcha_required {

            fields.push(LoginFocus::Captcha);
        }

        fields.push(LoginFocus::RememberMe);

        fields
    }

    pub fn current_focus(&self) -> LoginFocus {

        let visible = self.visible_focuses();

        let idx = self.focus.min(visible.len().saturating_sub(1));

        visible[idx]
    }

    pub fn next_focus(&mut self) {

        let len = self.visible_focuses().len();

        self.focus = (self.focus + 1) % len.max(1);
    }

    pub fn prev_focus(&mut self) {

        let len = self.visible_focuses().len();

        if len == 0 {

            return;
        }

        self.focus = (self.focus + len - 1) % len;
    }

    pub fn reset_focus_bounds(&mut self) {

        let len = self.visible_focuses().len();

        if len == 0 {

            self.focus = 0;
        } else if self.focus >= len {

            self.focus = len - 1;
        }
    }

    /// Builds the login payload expected by the network layer.
    ///
    /// Why:
    /// Direct mode and VPN mode shape credentials differently. Centralizing that
    /// rule here keeps the input handlers focused on editing state instead of
    /// duplicating login normalization logic.

    pub fn to_input(&self) -> LoginInput {

        let student_id = self.student_id.trim();

        let vpn_username = self.vpn_username.trim();

        LoginInput {
            student_id:   if self.use_vpn && student_id.is_empty() {

                vpn_username.to_string()
            } else {

                student_id.to_string()
            },
            use_vpn:      self.use_vpn,
            vpn_username: vpn_username.to_string(),
            vpn_password: self.vpn_password.clone(),
        }
    }
}

impl From<&LoginForm> for RememberedLogin {
    fn from(login: &LoginForm) -> Self {

        let input = login.to_input();

        Self {
            student_id:   input.student_id,
            use_vpn:      input.use_vpn,
            vpn_username: input.vpn_username,
            vpn_password: input.vpn_password,
        }
    }
}

#[derive(Clone, Debug, Default)]

pub struct BykcState {
    pub view:              BykcView,
    pub include_all:       bool,
    pub loading:           bool,
    pub loaded:            bool,
    pub courses:           Vec<BykcCourse>,
    pub selected_course:   usize,
    pub chosen_courses:    Vec<BykcChosenCourse>,
    pub selected_chosen:   usize,
    pub statistics:        Option<BykcStatistics>,
    pub statistics_error:  Option<String>,
    pub detail:            Option<BykcCourseDetail>,
    pub detail_cache:      HashMap<i64, BykcCourseDetail>,
    pub detail_course_id:  Option<i64>,
    pub show_detail_popup: bool,
    /// Vertical scroll offset inside the detail popup.
    ///
    /// Why:
    /// Course descriptions run to many lines and used to be cut off silently
    /// once the popup filled. Scrolling makes the whole description reachable.
    pub detail_scroll:     u16,
}

impl BykcState {
    pub fn selected_course(&self) -> Option<&BykcCourse> {

        self.courses.get(self.selected_course)
    }

    pub fn selected_chosen_course(&self) -> Option<&BykcChosenCourse> {

        self.chosen_courses.get(self.selected_chosen)
    }

    pub fn chosen_course_for(&self, course_id: i64) -> Option<&BykcChosenCourse> {

        self.chosen_courses
            .iter()
            .find(|course| course.course_id == course_id)
    }

    pub fn selected_detail_target(&self) -> Option<i64> {

        match self.view {
            BykcView::Courses => self.selected_course().map(|course| course.id),
            BykcView::Chosen => self.selected_chosen_course().map(|course| course.course_id),
        }
    }

    pub fn move_selection(&mut self, delta: isize) {

        match self.view {
            BykcView::Courses => {

                self.selected_course = clamp_step(self.selected_course, self.courses.len(), delta);
            }
            BykcView::Chosen => {

                self.selected_chosen =
                    clamp_step(self.selected_chosen, self.chosen_courses.len(), delta);
            }
        }

        self.show_detail_popup = false;

        self.sync_detail_from_cache();
    }

    pub fn set_view(&mut self, view: BykcView) {

        if self.view == view {

            return;
        }

        self.view = view;

        self.show_detail_popup = false;

        self.sync_detail_from_cache();
    }

    /// Replaces BYKC list data while preserving selection and cached detail when possible.
    ///
    /// How:
    /// Resolve the previous selected ids before replacing the vectors, then map
    /// those ids back onto the fresh lists. This avoids cursor jumps after a
    /// refresh and keeps the detail panel anchored to the same logical course.

    pub fn replace_data(
        &mut self,
        courses: Vec<BykcCourse>,
        chosen_courses: Vec<BykcChosenCourse>,
        statistics: Option<BykcStatistics>,
        statistics_error: Option<String>,
        detail: Option<BykcCourseDetail>,
    ) {

        let previous_course_id = self.selected_course().map(|course| course.id);

        let previous_chosen_id = self.selected_chosen_course().map(|course| course.id);

        self.courses = courses;

        self.selected_course = previous_course_id
            .and_then(|id| self.courses.iter().position(|course| course.id == id))
            .unwrap_or(0);

        if self.selected_course >= self.courses.len() {

            self.selected_course = 0;
        }

        self.chosen_courses = chosen_courses;

        self.selected_chosen = previous_chosen_id
            .and_then(|id| {

                self.chosen_courses
                    .iter()
                    .position(|course| course.id == id)
            })
            .unwrap_or(0);

        if self.selected_chosen >= self.chosen_courses.len() {

            self.selected_chosen = 0;
        }

        self.loaded = true;

        self.statistics = statistics;

        self.statistics_error = statistics_error;

        if let Some(detail) = detail {

            self.detail_course_id = Some(detail.id);

            self.detail_cache.insert(detail.id, detail.clone());

            self.detail = Some(detail);
        } else {

            self.sync_detail_from_cache();
        }
    }

    /// Rebinds the inline detail panel to the current selection using cached detail first.

    pub fn sync_detail_from_cache(&mut self) {

        let Some(course_id) = self.selected_detail_target() else {

            self.detail = None;

            self.detail_course_id = None;

            return;
        };

        self.detail = self.detail_cache.get(&course_id).cloned();

        self.detail_course_id = self.detail.as_ref().map(|item| item.id);
    }

    pub fn selected_cached_detail(&self) -> Option<&BykcCourseDetail> {

        let course_id = self.selected_detail_target()?;

        self.detail_cache
            .get(&course_id)
            .or_else(|| self.detail.as_ref().filter(|detail| detail.id == course_id))
    }
}

#[derive(Clone, Debug)]

pub struct BykcSyncSuccess {
    pub courses:           Vec<BykcCourse>,
    pub chosen_courses:    Vec<BykcChosenCourse>,
    pub statistics:        Option<BykcStatistics>,
    pub statistics_error:  Option<String>,
    pub detail:            Option<BykcCourseDetail>,
    pub message:           Option<String>,
    pub open_detail_popup: bool,
}

#[derive(Clone, Copy, Debug)]

enum BykcDetailTarget {
    Auto,
    CourseFirst(i64),
    ChosenFirst(i64),
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]

pub enum EventLevel {
    Info,
    Success,
    Warn,
    Error,
}

#[derive(Clone, Debug)]

pub struct EventEntry {
    pub level:   EventLevel,
    pub message: String,
}

/// A clickable region reported by the renderer.
///
/// Why:
/// Mouse clicks arrive as absolute screen coordinates, but only the renderer
/// knows where things ended up. Recording regions as they are drawn means the
/// layout is computed exactly once, in the drawing code, and hit testing can
/// never drift out of sync with what is on screen.
#[derive(Clone, Debug, PartialEq)]

pub struct Hotspot {
    pub area:   Rect,
    pub action: HotAction,
}

/// What a click inside a [`Hotspot`] should do.

#[derive(Clone, Debug, PartialEq)]

pub enum HotAction {
    /// Switch the workspace tab.
    WorkspaceTab(WorkspaceTab),
    /// Switch the course view inside the schedule tab.
    CourseView(CourseView),
    /// Switch the BYKC view.
    BykcView(BykcView),
    /// Select a row of the active list.
    ///
    /// How:
    /// `index` is absolute: a position in the underlying data the tab selects
    /// from. For the iClass grid that is a course index across the whole week;
    /// for a flat list it is a row index. Both name a real item, so a click can
    /// never select something that is not there.
    ListRow { index: usize },
}

#[derive(Debug)]

pub struct App {
    pub screen:                Screen,
    pub active_tab:            WorkspaceTab,
    /// Clickable regions recorded during the last render.
    ///
    /// Why:
    /// Stored on `App` because the renderer borrows it immutably while the
    /// event loop needs the regions to route a click. Interior mutability keeps
    /// that one-way flow intact instead of threading a collector through every
    /// render function.
    pub hotspots:              RefCell<Vec<Hotspot>>,
    /// Frame counter advanced on every tick.
    ///
    /// Why:
    /// Animated indicators need a clock. Deriving them from this counter means
    /// no widget keeps its own timer, and animation stays in step with the
    /// redraw the event loop already performs.
    pub tick:                  u64,
    pub login:                 LoginForm,
    pub session:               Option<Session>,
    pub courses:               Vec<CourseDetailItem>,
    pub week_groups:           Vec<WeekGroup>,
    pub selected_week:         usize,
    pub selected:              usize,
    pub bykc:                  BykcState,
    pub schedule:              ScheduleState,
    pub event_log:             Vec<EventEntry>,
    pub status:                String,
    pub busy:                  bool,
    pub iclass_loading:        bool,
    pub should_quit:           bool,
    pub show_help:             bool,
    pub qr_display:            Option<QrDisplay>,
    pub qr_refreshing:         bool,
    pub qr_mode:               QrMode,
    pub external_qr_path:      Option<PathBuf>,
    pub external_qr_status:    Option<String>,
    pub version_info:          Option<VersionInfo>,
    pub version_error:         Option<String>,
    pub login_diagnostic:      Option<LoginDiagnostic>,
    pub pending_captcha_login: Option<PendingCaptchaLogin>,
    pub show_login_details:    bool,
    pub doctor_report:         Option<DoctorReport>,
    pub show_doctor_details:   bool,
    pub show_event_log:        bool,
    /// First visible line of the event log popup.
    pub event_log_offset:      usize,
    next_qr_refresh_at:        Option<Instant>,
}

impl Default for App {
    fn default() -> Self {

        Self {
            screen:                Screen::Login,
            active_tab:            WorkspaceTab::IClass,
            tick:                  0,
            hotspots:              RefCell::new(Vec::new()),
            login:                 LoginForm::default(),
            session:               None,
            courses:               Vec::new(),
            week_groups:           Vec::new(),
            selected_week:         0,
            selected:              0,
            bykc:                  BykcState::default(),
            schedule:              ScheduleState::default(),
            event_log:             vec![EventEntry {
                level:   EventLevel::Info,
                message: "输入统一认证账号和密码，选择访问模式后按 enter 登录".to_string(),
            }],
            status:                "输入统一认证账号和密码，选择访问模式后按 enter 登录"
                .to_string(),
            busy:                  false,
            iclass_loading:        false,
            should_quit:           false,
            show_help:             false,
            qr_display:            None,
            qr_refreshing:         false,
            qr_mode:               QrMode::Terminal,
            external_qr_path:      None,
            external_qr_status:    None,
            version_info:          None,
            version_error:         None,
            login_diagnostic:      None,
            pending_captcha_login: None,
            show_login_details:    false,
            doctor_report:         None,
            show_doctor_details:   false,
            show_event_log:        false,
            event_log_offset:      0,
            next_qr_refresh_at:    None,
        }
    }
}

impl App {
    pub fn load() -> Self {

        let mut app = Self::default();

        match load_remembered_login() {
            Ok(Some(remembered)) => {

                app.login = LoginForm::from_remembered(remembered);

                app.info("已载入上次登录信息，按 enter 登录；space 可关闭记住我");
            }
            Ok(None) => {}
            Err(error) => {

                app.error(format!("读取记住我信息失败: {error}"));
            }
        }

        app
    }

    fn load_schedule_cache(&mut self, account: String) {

        match load_cached_schedules(&account) {
            Ok(semesters) if semesters.is_empty() => {

                self.schedule = ScheduleState {
                    account: Some(account),
                    loaded: true,
                    classroom_campus: 1,
                    classroom_date: Local::now().date_naive().to_string(),
                    ..ScheduleState::default()
                };
            }
            Ok(semesters) => {

                self.schedule = ScheduleState::from_cache(account, semesters);
            }
            Err(error) => {

                self.schedule = ScheduleState {
                    account: Some(account),
                    loaded: true,
                    classroom_campus: 1,
                    classroom_date: Local::now().date_naive().to_string(),
                    ..ScheduleState::default()
                };

                self.warn(format!("读取离线课表失败: {error}"));
            }
        }
    }

    pub fn visible_course_indices(&self) -> &[usize] {

        self.week_groups
            .get(self.selected_week)
            .map(|group| group.course_indices.as_slice())
            .unwrap_or(&[])
    }

    pub fn selected_week_group(&self) -> Option<&WeekGroup> {

        self.week_groups.get(self.selected_week)
    }

    pub fn visible_courses_len(&self) -> usize {

        self.visible_course_indices().len()
    }

    pub fn selected_course_absolute_index(&self) -> Option<usize> {

        self.visible_course_indices().get(self.selected).copied()
    }

    pub fn selected_course(&self) -> Option<&CourseDetailItem> {

        let index = *self.visible_course_indices().get(self.selected)?;

        self.courses.get(index)
    }

    /// Compact version label for the one-row top bar.
    ///
    /// Why:
    /// The full string ("版本: v0.6.0 | 已是最新") was written for a dedicated
    /// hint box. On a shared top bar only the number and fault state matter;
    /// the rest lives in the version check's own output.

    pub fn version_short(&self) -> String {

        let current = env!("CARGO_PKG_VERSION");

        if self.version_error.is_some() {

            return format!("v{current} 检查失败");
        }

        match &self.version_info {
            Some(info) if info.is_latest => format!("v{current}"),
            Some(info) => format!("v{current} → v{}", info.latest),
            None => format!("v{current}"),
        }
    }

    pub fn version_text(&self) -> String {

        if let Some(info) = &self.version_info {

            if info.is_latest {

                return format!("版本: v{} | 已是最新", info.current);
            }

            return format!(
                "版本: v{} | 最新: v{} | {}",
                info.current, info.latest, info.latest_url
            );
        }

        if self.version_error.is_some() {

            return format!(
                "版本: v{} | 更新检查失败: {}",
                env!("CARGO_PKG_VERSION"),
                self.version_error.as_deref().unwrap_or("未知错误")
            );
        }

        format!("版本: v{} | 正在检查更新...", env!("CARGO_PKG_VERSION"))
    }

    pub fn version_style(&self) -> Style {

        if self.version_error.is_some() {

            return Style::default().fg(Color::Red).add_modifier(Modifier::BOLD);
        }

        if self
            .version_info
            .as_ref()
            .is_some_and(|info| !info.is_latest)
        {

            return Style::default()
                .fg(Color::LightYellow)
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD);
        }

        if self.version_info.is_some() {

            return Style::default()
                .fg(Color::LightGreen)
                .add_modifier(Modifier::BOLD);
        }

        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD)
    }

    pub fn latest_events(&self) -> &[EventEntry] {

        &self.event_log
    }

    pub fn clear_event_log(&mut self) {

        self.event_log.clear();

        self.info("已清空事件日志");
    }

    /// Reports whether a text field currently owns the keystroke.
    ///
    /// Why:
    /// Global single-letter shortcuts must not steal characters from the login
    /// form or the course search box, or typing a course name would trigger them.

    pub fn is_text_input_active(&self) -> bool {

        (self.screen == Screen::Login && !self.busy) || self.schedule.filtering
    }

    pub fn copy_latest_error_to_clipboard(&mut self) {

        let Some(entry) = self
            .event_log
            .iter()
            .rev()
            .find(|entry| entry.level == EventLevel::Error)
            .cloned()
        else {

            self.warn("最近没有可复制的错误事件");

            return;
        };

        match copy_to_clipboard(&entry.message) {
            Ok(()) => self.success("已复制最近错误到剪贴板"),
            Err(error) => self.error(format!("复制最近错误失败: {error}")),
        }
    }

    fn set_status_with_level(&mut self, level: EventLevel, message: impl Into<String>) {

        let message = message.into();

        self.status = message.clone();

        self.event_log.push(EventEntry { level, message });

        logging::event(
            log_level_from_event(level),
            "tui.event",
            &self.status,
            serde_json::json!({ "screen": format!("{:?}", self.screen) }),
        );

        if self.event_log.len() > MAX_EVENT_LOG_ENTRIES {

            let overflow = self.event_log.len() - MAX_EVENT_LOG_ENTRIES;

            self.event_log.drain(0..overflow);
        }
    }

    fn info(&mut self, message: impl Into<String>) {

        self.set_status_with_level(EventLevel::Info, message);
    }

    fn success(&mut self, message: impl Into<String>) {

        self.set_status_with_level(EventLevel::Success, message);
    }

    fn warn(&mut self, message: impl Into<String>) {

        self.set_status_with_level(EventLevel::Warn, message);
    }

    fn error(&mut self, message: impl Into<String>) {

        self.set_status_with_level(EventLevel::Error, message);
    }

    /// Applies one key press to the current screen state.
    ///
    /// Why:
    /// Global shortcuts, popups, and screen-specific handlers must share one
    /// gateway so they do not conflict with each other as more features are added.

    pub fn handle_key(&mut self, key: KeyEvent, tx: &UnboundedSender<AsyncEvent>) {

        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {

            self.should_quit = true;

            return;
        }

        if self.show_help {

            match key.code {
                KeyCode::Char('?') | KeyCode::Char('q') | KeyCode::Esc => {

                    self.show_help = false;
                }
                _ => {}
            }

            return;
        }

        if self.show_event_log {

            match key.code {
                KeyCode::Char('e') | KeyCode::Esc | KeyCode::Char('q') => {

                    self.show_event_log = false;
                }
                KeyCode::Char('y') => self.copy_latest_error_to_clipboard(),
                KeyCode::Char('C') => self.clear_event_log(),
                _ => {}
            }

            return;
        }

        if self.screen == Screen::Login && (self.show_login_details || self.show_doctor_details) {

            match key.code {
                KeyCode::Esc | KeyCode::Char('v') | KeyCode::Char('D') => {

                    self.show_login_details = false;

                    self.show_doctor_details = false;
                }
                _ => {}
            }

            return;
        }

        if key.code == KeyCode::Char('?') {

            self.show_help = true;

            return;
        }

        if key.code == KeyCode::Char('e') {

            self.show_event_log = true;

            return;
        }

        // The event block on every screen advertises `C` and `y`. They used to
        // work only once the popup was open, which contradicted that hint.
        if key.code == KeyCode::Char('C') && !self.is_text_input_active() {

            self.clear_event_log();

            return;
        }

        if key.code == KeyCode::Char('y') && !self.is_text_input_active() {

            self.copy_latest_error_to_clipboard();

            return;
        }

        if self.screen == Screen::Workspace
            && self.active_tab == WorkspaceTab::Bykc
            && self.bykc.show_detail_popup
        {

            match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('o') => {

                    self.bykc.show_detail_popup = false;

                    return;
                }
                // Scroll rather than move the list selection, since the popup
                // owns the keyboard while it is open.
                KeyCode::Up | KeyCode::Char('k') => {

                    self.bykc.detail_scroll = self.bykc.detail_scroll.saturating_sub(1);

                    return;
                }
                KeyCode::Down | KeyCode::Char('j') => {

                    self.bykc.detail_scroll = self.bykc.detail_scroll.saturating_add(1);

                    return;
                }
                KeyCode::PageUp => {

                    self.bykc.detail_scroll = self.bykc.detail_scroll.saturating_sub(8);

                    return;
                }
                KeyCode::PageDown => {

                    self.bykc.detail_scroll = self.bykc.detail_scroll.saturating_add(8);

                    return;
                }
                KeyCode::Home | KeyCode::Char('g') => {

                    self.bykc.detail_scroll = 0;

                    return;
                }
                _ => {}
            }
        }

        match self.screen {
            Screen::Login => self.handle_login_key(key, tx),
            Screen::Workspace => self.handle_workspace_key(key, tx),
        }
    }

    /// Records a clickable region for the frame being drawn.
    ///
    /// Why:
    /// Called from the renderer, which knows the real geometry. Clearing happens
    /// once per frame.

    pub fn record_hotspot(&self, area: Rect, action: HotAction) {

        self.hotspots.borrow_mut().push(Hotspot { area, action });
    }

    /// Forgets every region recorded for the previous frame.

    pub fn clear_hotspots(&self) {

        self.hotspots.borrow_mut().clear();
    }

    /// Resolves a screen position to the action it lands on.

    fn hotspot_at(&self, column: u16, row: u16) -> Option<HotAction> {

        let hotspots = self.hotspots.borrow();

        hotspots
            .iter()
            .rev()
            .find(|hotspot| {

                column >= hotspot.area.x
                    && column < hotspot.area.x + hotspot.area.width
                    && row >= hotspot.area.y
                    && row < hotspot.area.y + hotspot.area.height
            })
            .map(|hotspot| hotspot.action.clone())
    }

    /// Number of selectable rows in the list the active tab is showing.

    fn active_list_len(&self) -> usize {

        match self.active_tab {
            WorkspaceTab::Schedule => {
                match self.schedule.view {
                    CourseView::Today => self.schedule.today_entries().len(),
                    CourseView::Schedule => self.schedule.visible_entries().len(),
                    CourseView::Exams => self.schedule.exams.len(),
                    CourseView::Grades => self.schedule.grades.len(),
                    CourseView::Classrooms => self.schedule.classrooms.len(),
                    CourseView::Tasks => self.schedule.tasks.len(),
                }
            }
            WorkspaceTab::IClass => self.visible_courses_len(),
            WorkspaceTab::Bykc => {
                match self.bykc.view {
                    BykcView::Courses => self.bykc.courses.len(),
                    BykcView::Chosen => self.bykc.chosen_courses.len(),
                }
            }
        }
    }

    /// Selects a row by position in the active list.

    fn select_list_row(&mut self, index: usize) {

        if index >= self.active_list_len() {

            return;
        }

        match self.active_tab {
            WorkspaceTab::Schedule => self.schedule.selected_entry = index,
            WorkspaceTab::IClass => self.selected = index,
            WorkspaceTab::Bykc => {
                match self.bykc.view {
                    BykcView::Courses => self.bykc.selected_course = index,
                    BykcView::Chosen => self.bykc.selected_chosen = index,
                }
            }
        }
    }

    /// Routes a mouse event.
    ///
    /// How:
    /// Clicks resolve through the regions the renderer recorded, so hit testing
    /// and drawing can never disagree. The wheel falls back to the same
    /// movement the arrow keys perform, which keeps every list scrollable
    /// without per-widget handling.

    pub fn handle_mouse(&mut self, event: MouseEvent, tx: &UnboundedSender<AsyncEvent>) {

        if self.busy {

            return;
        }

        // A popup owns the mouse while it is open.
        if self.show_event_log {

            match event.kind {
                MouseEventKind::ScrollDown => self.scroll_event_log(3),
                MouseEventKind::ScrollUp => self.scroll_event_log(-3),
                MouseEventKind::Down(MouseButton::Left) => self.show_event_log = false,
                _ => {}
            }

            return;
        }

        if self.active_tab == WorkspaceTab::Bykc && self.bykc.show_detail_popup {

            match event.kind {
                MouseEventKind::ScrollDown => {

                    self.bykc.detail_scroll = self.bykc.detail_scroll.saturating_add(3);
                }
                MouseEventKind::ScrollUp => {

                    self.bykc.detail_scroll = self.bykc.detail_scroll.saturating_sub(3);
                }
                MouseEventKind::Down(MouseButton::Left) => self.bykc.show_detail_popup = false,
                _ => {}
            }

            return;
        }

        if self.show_help {

            if matches!(event.kind, MouseEventKind::Down(MouseButton::Left)) {

                self.show_help = false;
            }

            return;
        }

        match event.kind {
            MouseEventKind::ScrollDown => self.scroll_active(-1, tx),
            MouseEventKind::ScrollUp => self.scroll_active(1, tx),
            MouseEventKind::Down(MouseButton::Left) => {

                let Some(action) = self.hotspot_at(event.column, event.row) else {

                    return;
                };

                self.apply_hot_action(action, tx);
            }
            _ => {}
        }
    }

    fn apply_hot_action(&mut self, action: HotAction, tx: &UnboundedSender<AsyncEvent>) {

        match action {
            HotAction::WorkspaceTab(tab) => {
                if tab != self.active_tab {

                    self.set_workspace_tab(tab, tx);
                }
            }
            HotAction::CourseView(view) => {
                if view != self.schedule.view {

                    self.schedule.switch_view(view);

                    if matches!(
                        view,
                        CourseView::Exams
                            | CourseView::Grades
                            | CourseView::Classrooms
                            | CourseView::Tasks
                    ) {

                        self.refresh_academic_view(tx);
                    }
                }
            }
            HotAction::BykcView(view) => self.bykc.set_view(view),
            HotAction::ListRow { index } => {

                // The renderer already resolved the screen row to a real item,
                // including any list scroll offset, so this is a plain lookup.
                self.select_list_row(index);
            }
        }
    }

    /// Moves the active list, or scrolls the BYKC detail when it is showing.

    fn scroll_active(&mut self, delta: isize, _tx: &UnboundedSender<AsyncEvent>) {

        match self.active_tab {
            WorkspaceTab::Schedule => self.schedule.move_entry(delta),
            WorkspaceTab::IClass => {

                let len = self.visible_courses_len();

                self.selected = clamp_step(self.selected, len, delta);
            }
            WorkspaceTab::Bykc => self.bykc.move_selection(delta),
        }
    }

    fn scroll_event_log(&mut self, delta: isize) {

        self.event_log_offset = clamp_step(self.event_log_offset, self.event_log.len(), delta);
    }

    pub fn handle_tick(&mut self) {

        self.tick = self.tick.wrapping_add(1);

        if !self.qr_refreshing || self.active_tab != WorkspaceTab::IClass {

            return;
        }

        let Some(next_at) = self.next_qr_refresh_at else {

            self.next_qr_refresh_at = Some(Instant::now() + Duration::from_secs(2));

            return;
        };

        if Instant::now() < next_at {

            return;
        }

        if let Err(error) = self.refresh_qr_inline() {

            self.error(format!("二维码刷新失败: {error}"));

            if self.qr_mode == QrMode::External {

                self.mark_external_qr_error(&error);
            }

            self.clear_qr();

            return;
        }

        self.next_qr_refresh_at = Some(Instant::now() + Duration::from_secs(2));
    }

    /// Incorporates one completed background task back into foreground UI state.
    ///
    /// How:
    /// Worker tasks only return immutable payloads. All UI mutation in response
    /// to those payloads flows through this single function, which makes async
    /// state transitions much easier to reason about.

    pub fn handle_async(&mut self, event: AsyncEvent, tx: &UnboundedSender<AsyncEvent>) {

        match event {
            AsyncEvent::Login(result) => {

                self.busy = false;

                match result {
                    Ok(data) => {

                        self.screen = Screen::Workspace;

                        self.active_tab = WorkspaceTab::Schedule;

                        let should_prewarm_bykc =
                            data.session.use_vpn && data.session.bykc_api.is_some();

                        self.session = Some(data.session.clone());

                        self.login_diagnostic = None;

                        self.pending_captcha_login = None;

                        self.login.captcha_required = false;

                        self.login.captcha.clear();

                        self.show_login_details = false;

                        self.replace_courses(data.courses, None, None);

                        self.load_schedule_cache(data.schedule_account);

                        self.bykc = BykcState::default();

                        self.clear_qr();

                        let remember_status = self.persist_remembered_login_status();

                        if should_prewarm_bykc {

                            self.prewarm_bykc(data.session, tx);
                        }

                        if let Some(error) = data.course_prefetch_error {

                            self.warn(format!(
                                "登录成功，但拉取课程失败: {error}。按 r \
                                 重试刷新课程。{remember_status}"
                            ));
                        } else {

                            self.success(format!(
                                "登录成功。tab 切换 iClass / BYKC，s 直接签到，g 终端二维码，G \
                                 外部二维码，r 刷新，Shift+X 退出登录。{remember_status}"
                            ));
                        }
                    }
                    Err(error) => {

                        self.login_diagnostic = Some(error.diagnostic);

                        self.pending_captcha_login = None;

                        self.login.captcha_required = false;

                        self.login.captcha.clear();

                        self.show_login_details = false;

                        self.error(format!("登录失败: {}。按 v 查看详情", error.message));
                    }
                }
            }
            AsyncEvent::LoginCaptcha(result) => {

                self.busy = false;

                match result {
                    Ok(pending) => {

                        self.pending_captcha_login = Some(pending.clone());

                        self.login.captcha_required = true;

                        self.login.captcha.clear();

                        self.login.reset_focus_bounds();

                        self.warn(format!(
                            "检测到验证码，请查看 {} 并输入后按 enter 继续",
                            pending.challenge.captcha_path
                        ));
                    }
                    Err(error) => {

                        self.error(format!("验证码准备失败: {error}"));
                    }
                }
            }
            AsyncEvent::Refresh(result) => {

                self.iclass_loading = false;

                match result {
                    Ok(courses) => {

                        let selected_id = self
                            .selected_course()
                            .map(|item| item.course_sched_id.clone());

                        let week_key = self.selected_week_group().map(|item| item.key.clone());

                        self.replace_courses(courses, week_key, selected_id.clone());

                        let selected_id = self
                            .selected_course()
                            .map(|item| item.course_sched_id.clone())
                            .unwrap_or_default();

                        let week_label = self
                            .selected_week_group()
                            .map(|item| item.label.as_str())
                            .unwrap_or("未分组");

                        self.success(format!(
                            "课程已刷新，共 {} 条，当前周：{}",
                            self.visible_courses_len(),
                            week_label
                        ));

                        if self
                            .qr_display
                            .as_ref()
                            .is_some_and(|qr| qr.course_sched_id != selected_id)
                        {

                            self.mark_external_qr_inactive("课程已切换，旧二维码已失效");

                            self.clear_qr();
                        }
                    }
                    Err(error) => {

                        self.error(format!("刷新失败: {error}"));
                    }
                }
            }
            AsyncEvent::Sign(result) => {

                self.iclass_loading = false;

                match result {
                    Ok(outcome) => {

                        if outcome.success_like {

                            self.success("签到成功");
                        } else {

                            self.error(format!("签到失败: {}", outcome.message));
                        }

                        if outcome.success_like
                            && let Some(index) = self.selected_course_absolute_index()
                            && let Some(item) = self.courses.get_mut(index)
                        {

                            item.sign_status = "1".to_string();
                        }
                    }
                    Err(error) => {

                        self.error(format!("签到失败: {error}"));
                    }
                }
            }
            AsyncEvent::BykcSync(result) => {

                self.bykc.loading = false;

                match *result {
                    Ok(data) => {

                        let course_count = data.courses.len();

                        let chosen_count = data.chosen_courses.len();

                        let message = data.message.unwrap_or_else(|| {

                            format!(
                                "博雅数据已加载，可选 {} 门，已选 {} 门",
                                course_count, chosen_count
                            )
                        });

                        self.bykc.replace_data(
                            data.courses,
                            data.chosen_courses,
                            data.statistics,
                            data.statistics_error,
                            data.detail,
                        );

                        self.bykc.show_detail_popup = data.open_detail_popup;

                        self.success(message);
                    }
                    Err(error) => {

                        self.error(format!("博雅操作失败: {error}"));
                    }
                }
            }
            AsyncEvent::ScheduleImport(result) => {

                self.schedule.updating = false;

                match result {
                    Ok(schedule) => {

                        let Some(account) = self.schedule.account.clone() else {

                            self.warn("课表账号已失效，请重新登录");

                            return;
                        };

                        match save_cached_schedule(&account, schedule.clone()) {
                            Ok(()) => {

                                self.schedule.replace_semester(schedule);

                                self.success("整学期课表已更新，旧课表仍保留在本地缓存中");
                            }
                            Err(error) => {

                                self.error(format!("课表已获取，但保存离线课表失败: {error}"));
                            }
                        }
                    }
                    Err(error) => {

                        self.error(format!("课表更新失败，已保存课表未修改: {error}"));
                    }
                }
            }
            AsyncEvent::Exams(result) => {

                self.schedule.academic_loading = false;

                match result {
                    Ok(exams) => {

                        self.schedule.exams = exams;

                        self.success(format!(
                            "考试安排已加载，共 {} 条",
                            self.schedule.exams.len()
                        ));
                    }
                    Err(error) => self.error(format!("考试安排加载失败: {error}")),
                }
            }
            AsyncEvent::Grades(result) => {

                self.schedule.academic_loading = false;

                match result {
                    Ok(grades) => {

                        self.schedule.grades = grades;

                        self.schedule.grades_all_terms = false;

                        self.success(format!("成绩已加载，共 {} 条", self.schedule.grades.len()));
                    }
                    Err(error) => self.error(format!("成绩加载失败: {error}")),
                }
            }
            AsyncEvent::AllGrades(result) => {

                self.schedule.academic_loading = false;

                match result {
                    Ok(loaded) => {

                        let count = loaded.grades.len();

                        self.schedule.grades = loaded.grades;

                        self.schedule.grades_all_terms = true;

                        if loaded.failed.is_empty() {

                            self.success(format!("全部学期成绩已加载，共 {count} 条"));
                        } else {

                            // Partial success is the common case when a term
                            // predates the grade system or is not yet released.
                            self.warn(format!(
                                "已加载 {count} 条；{} 个学期失败: {}",
                                loaded.failed.len(),
                                loaded
                                    .failed
                                    .iter()
                                    .map(|(term, _)| term.as_str())
                                    .collect::<Vec<_>>()
                                    .join(", ")
                            ));

                            for (term, error) in &loaded.failed {

                                self.error(format!("{term} 学期成绩加载失败: {error}"));
                            }
                        }
                    }
                    Err(error) => self.error(format!("全部学期成绩加载失败: {error}")),
                }
            }
            AsyncEvent::Classrooms(result) => {

                self.schedule.academic_loading = false;

                match result {
                    Ok(rooms) => {

                        self.schedule.classrooms = rooms;

                        self.success(format!(
                            "空教室已加载，共 {} 间",
                            self.schedule.classrooms.len()
                        ));
                    }
                    Err(error) => self.error(format!("空教室加载失败: {error}")),
                }
            }
            AsyncEvent::Tasks(result) => {

                self.schedule.academic_loading = false;

                match result {
                    Ok(tasks) => {

                        self.schedule.tasks = tasks;

                        self.success(format!("作业已加载，共 {} 条", self.schedule.tasks.len()));
                    }
                    Err(error) => self.error(format!("作业加载失败: {error}")),
                }
            }
            AsyncEvent::VersionCheck(result) => {
                match result {
                    Ok(info) => {

                        self.version_error = None;

                        self.version_info = Some(info);
                    }
                    Err(error) => {

                        self.version_info = None;

                        self.version_error = Some(error);
                    }
                }
            }
            AsyncEvent::Doctor(result) => {

                self.busy = false;

                match result {
                    Ok(report) => {

                        let failed = report.checks.iter().filter(|check| !check.ok).count();

                        self.doctor_report = Some(report);

                        self.show_doctor_details = true;

                        if failed == 0 {

                            self.success("网络自检完成：全部通过");
                        } else {

                            self.warn(format!("网络自检完成：{failed} 项异常，按 D 查看详情"));
                        }
                    }
                    Err(error) => {

                        self.error(format!("网络自检失败: {error}"));
                    }
                }
            }
        }
    }

    fn handle_login_key(&mut self, key: KeyEvent, tx: &UnboundedSender<AsyncEvent>) {

        if self.busy {

            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {

                self.should_quit = true;
            }

            return;
        }

        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.should_quit = true,
            KeyCode::Tab | KeyCode::Down => self.login.next_focus(),
            KeyCode::BackTab | KeyCode::Up => self.login.prev_focus(),
            KeyCode::Enter => self.submit_login(tx),
            KeyCode::Char('v') => {
                if self.login_diagnostic.is_some() {

                    self.show_login_details = true;
                }
            }
            KeyCode::Char('D') => self.run_doctor(tx),
            KeyCode::Char(' ') if self.login.current_focus() == LoginFocus::UseVpn => {

                self.login.use_vpn = !self.login.use_vpn;

                self.login.reset_focus_bounds();
            }
            KeyCode::Char(' ') if self.login.current_focus() == LoginFocus::RememberMe => {

                self.login.remember_me = !self.login.remember_me;
            }
            KeyCode::Char(ch) => self.push_char(ch),
            KeyCode::Backspace => self.pop_char(),
            _ => {}
        }
    }

    fn handle_workspace_key(&mut self, key: KeyEvent, tx: &UnboundedSender<AsyncEvent>) {

        if self.busy {

            if matches!(key.code, KeyCode::Esc | KeyCode::Char('q')) {

                self.should_quit = true;
            }

            return;
        }

        match key.code {
            KeyCode::Tab => {

                self.switch_workspace_tab(1, tx);

                return;
            }
            KeyCode::BackTab => {

                self.switch_workspace_tab(-1, tx);

                return;
            }
            _ => {}
        }

        match self.active_tab {
            WorkspaceTab::Schedule => self.handle_schedule_key(key, tx),
            WorkspaceTab::IClass => self.handle_iclass_key(key, tx),
            WorkspaceTab::Bykc => self.handle_bykc_key(key, tx),
        }
    }

    fn handle_iclass_key(&mut self, key: KeyEvent, tx: &UnboundedSender<AsyncEvent>) {

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('[') | KeyCode::Char('H') => self.select_prev_week(),
            KeyCode::Char(']') | KeyCode::Char('L') => self.select_next_week(),
            KeyCode::Left | KeyCode::Char('h') => self.move_horizontal(-1),
            KeyCode::Right | KeyCode::Char('l') => self.move_horizontal(1),
            KeyCode::Up | KeyCode::Char('k') => self.move_vertical(-1),
            KeyCode::Down | KeyCode::Char('j') => self.move_vertical(1),
            KeyCode::Char('r') => self.refresh_courses(tx),
            KeyCode::Char('s') => self.sign_selected(tx),
            KeyCode::Char('g') => self.toggle_qr(),
            KeyCode::Char('G') => self.toggle_external_qr(),
            KeyCode::Char('X') => self.logout(),
            _ => {}
        }
    }

    fn handle_bykc_key(&mut self, key: KeyEvent, tx: &UnboundedSender<AsyncEvent>) {

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('1') => {
                self.bykc.set_view(BykcView::Courses)
            }
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char('2') => {
                self.bykc.set_view(BykcView::Chosen)
            }
            KeyCode::Up | KeyCode::Char('k') => self.bykc.move_selection(-1),
            KeyCode::Down | KeyCode::Char('j') => self.bykc.move_selection(1),
            KeyCode::Char('r') => self.refresh_bykc(tx),
            KeyCode::Char('a') if self.bykc.view == BykcView::Courses => {

                self.bykc.include_all = !self.bykc.include_all;

                self.refresh_bykc(tx);
            }
            KeyCode::Enter | KeyCode::Char('o') => self.load_bykc_detail(tx),
            KeyCode::Char('s') if self.bykc.view == BykcView::Courses => {
                self.select_bykc_course(tx)
            }
            KeyCode::Char('x') if self.bykc.view == BykcView::Courses => {
                self.deselect_selected_bykc_course(tx)
            }
            KeyCode::Char('x') if self.bykc.view == BykcView::Chosen => {
                self.deselect_bykc_course(tx)
            }
            KeyCode::Char('s') if self.bykc.view == BykcView::Chosen => {
                self.sign_selected_bykc_course(BykcSignAction::SignIn, tx)
            }
            KeyCode::Char('u') if self.bykc.view == BykcView::Chosen => {
                self.sign_selected_bykc_course(BykcSignAction::SignOut, tx)
            }
            KeyCode::Char('X') => self.logout(),
            _ => {}
        }
    }

    fn switch_workspace_tab(&mut self, delta: isize, tx: &UnboundedSender<AsyncEvent>) {

        let tabs = [
            WorkspaceTab::Schedule,
            WorkspaceTab::IClass,
            WorkspaceTab::Bykc,
        ];

        let current_index = tabs
            .iter()
            .position(|tab| *tab == self.active_tab)
            .unwrap_or_default();

        let next_index =
            ((current_index as isize + delta).rem_euclid(tabs.len() as isize)) as usize;

        self.set_workspace_tab(tabs[next_index], tx);
    }

    /// Switches to a specific tab and runs the side effects that switch implies.
    ///
    /// Why:
    /// The tab keys and a mouse click on a tab must leave the app in exactly the
    /// same state; keeping the effects here means neither path can drift.

    fn set_workspace_tab(&mut self, tab: WorkspaceTab, tx: &UnboundedSender<AsyncEvent>) {

        self.active_tab = tab;

        if self.active_tab != WorkspaceTab::IClass {

            self.clear_qr();
        }

        if self.active_tab == WorkspaceTab::Bykc && !self.bykc.loaded && !self.bykc.loading {

            self.refresh_bykc(tx);
        }
    }

    fn handle_schedule_key(&mut self, key: KeyEvent, tx: &UnboundedSender<AsyncEvent>) {

        if self.schedule.filtering {

            match key.code {
                KeyCode::Esc | KeyCode::Enter => self.schedule.filtering = false,
                KeyCode::Backspace => {

                    self.schedule.query.pop();

                    self.schedule.selected_entry = 0;
                }
                KeyCode::Char(ch) => {

                    self.schedule.query.push(ch);

                    self.schedule.selected_entry = 0;
                }
                _ => {}
            }

            return;
        }

        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Char('/') => self.schedule.filtering = true,
            KeyCode::Char('1') => self.schedule.switch_view(CourseView::Today),
            KeyCode::Char('2') => self.schedule.switch_view(CourseView::Schedule),
            KeyCode::Char('3') => {

                self.schedule.switch_view(CourseView::Exams);

                self.refresh_academic_view(tx);
            }
            KeyCode::Char('4') => {

                self.schedule.switch_view(CourseView::Grades);

                self.refresh_academic_view(tx);
            }
            KeyCode::Char('5') => {

                self.schedule.switch_view(CourseView::Classrooms);

                self.refresh_academic_view(tx);
            }
            KeyCode::Char('6') => {

                self.schedule.switch_view(CourseView::Tasks);

                self.refresh_academic_view(tx);
            }
            KeyCode::Left | KeyCode::Char('h') | KeyCode::Char('[') => self.schedule.move_week(-1),
            KeyCode::Right | KeyCode::Char('l') | KeyCode::Char(']') => self.schedule.move_week(1),
            KeyCode::Up | KeyCode::Char('k') => self.schedule.move_entry(-1),
            KeyCode::Down | KeyCode::Char('j') => self.schedule.move_entry(1),
            KeyCode::Char(',') => {

                let index = self.schedule.selected_semester.saturating_sub(1);

                self.schedule.select_semester(index);
            }
            KeyCode::Char('.') => {

                let index = self.schedule.selected_semester.saturating_add(1);

                self.schedule.select_semester(index);
            }
            KeyCode::Char('u') => self.update_schedule(tx),
            KeyCode::Char('r') => self.refresh_academic_view(tx),
            KeyCode::Char('A') if self.schedule.view == CourseView::Grades => {

                self.load_all_grades(tx);
            }
            KeyCode::Char('X') => self.logout(),
            _ => {}
        }
    }

    fn push_char(&mut self, ch: char) {

        match self.login.current_focus() {
            LoginFocus::StudentId => self.login.student_id.push(ch),
            LoginFocus::VpnUsername => self.login.vpn_username.push(ch),
            LoginFocus::VpnPassword => self.login.vpn_password.push(ch),
            LoginFocus::Captcha => self.login.captcha.push(ch),
            LoginFocus::UseVpn => {
                if ch == ' ' {

                    self.login.use_vpn = !self.login.use_vpn;

                    self.login.reset_focus_bounds();
                }
            }
            LoginFocus::RememberMe => {
                if ch == ' ' {

                    self.login.remember_me = !self.login.remember_me;
                }
            }
        }
    }

    fn pop_char(&mut self) {

        match self.login.current_focus() {
            LoginFocus::StudentId => {

                self.login.student_id.pop();
            }
            LoginFocus::VpnUsername => {

                self.login.vpn_username.pop();
            }
            LoginFocus::VpnPassword => {

                self.login.vpn_password.pop();
            }
            LoginFocus::Captcha => {

                self.login.captcha.pop();
            }
            LoginFocus::UseVpn | LoginFocus::RememberMe => {}
        }
    }

    fn submit_login(&mut self, tx: &UnboundedSender<AsyncEvent>) {

        let input = self.login.to_input();

        if !input.use_vpn && input.student_id.is_empty() {

            self.warn("直连模式需要输入学号");

            return;
        }

        if input.vpn_password.is_empty() {

            self.warn("统一认证密码不能为空");

            return;
        }

        if input.use_vpn && input.vpn_username.is_empty() {

            self.warn("VPN 模式需要输入统一认证账号");

            return;
        }

        if self.login.captcha_required {

            let Some(pending) = self.pending_captcha_login.clone() else {

                self.warn("验证码状态已失效，请重新登录");

                self.login.captcha_required = false;

                self.login.captcha.clear();

                return;
            };

            if self.login.captcha.trim().is_empty() {

                self.warn("请输入验证码后按 enter 继续");

                return;
            }

            self.busy = true;

            self.login_diagnostic = None;

            self.show_login_details = false;

            self.info("提交验证码并继续登录...");

            spawn_continue_captcha_login(pending, self.login.captcha.clone(), tx.clone());

            return;
        }

        self.busy = true;

        self.login_diagnostic = None;

        self.pending_captcha_login = None;

        self.show_login_details = false;

        self.info("登录中并拉取课程...");

        spawn_login(input, tx.clone());
    }

    fn run_doctor(&mut self, tx: &UnboundedSender<AsyncEvent>) {

        self.busy = true;

        self.show_doctor_details = false;

        self.info("执行网络自检中...");

        spawn_doctor(self.login.use_vpn, tx.clone());
    }

    fn persist_remembered_login_status(&self) -> String {

        if self.login.remember_me {

            match save_remembered_login(&RememberedLogin::from(&self.login)) {
                Ok(()) => "已记住登录信息。".to_string(),
                Err(error) => format!("记住登录信息失败: {error}"),
            }
        } else {

            match delete_remembered_login() {
                Ok(()) => "未启用记住我。".to_string(),
                Err(error) => format!("清理记住我信息失败: {error}"),
            }
        }
    }

    fn refresh_courses(&mut self, tx: &UnboundedSender<AsyncEvent>) {

        let Some(session) = self.session.clone() else {

            self.warn("当前未登录");

            self.screen = Screen::Login;

            return;
        };

        self.iclass_loading = true;

        self.info("刷新课程中...");

        spawn_refresh(session, tx.clone());
    }

    fn update_schedule(&mut self, tx: &UnboundedSender<AsyncEvent>) {

        let Some(session) = self.session.clone() else {

            self.warn("当前未登录，无法更新课表");

            self.screen = Screen::Login;

            return;
        };

        if self.schedule.updating {

            return;
        }

        let requested_term = self
            .schedule
            .current_semester()
            .map(|semester| semester.term_code.clone());

        self.schedule.updating = true;

        self.info("正在导入整学期课表...");

        spawn_schedule_import(session, requested_term, tx.clone());
    }

    /// Loads grades for every term the portal lists, concurrently.
    ///
    /// Why:
    /// The term list comes from the cached schedule, so this never needs a
    /// separate request to discover terms and works offline for the list even
    /// if the grade fetches themselves must go online.

    fn load_all_grades(&mut self, tx: &UnboundedSender<AsyncEvent>) {

        let Some(session) = self.session.clone() else {

            self.warn("当前未登录");

            self.screen = Screen::Login;

            return;
        };

        // Terms are listed in every cached semester; take the union so a stale
        // snapshot from an older term does not shrink the set.
        let mut term_codes: Vec<String> = self
            .schedule
            .semesters
            .iter()
            .flat_map(|semester| semester.terms.iter().map(|term| term.code.clone()))
            .filter(|code| crate::academic::looks_like_score_term(code))
            .collect();

        term_codes.sort();

        term_codes.dedup();

        if term_codes.is_empty() {

            self.warn("请先按 u 导入课表，系统才能确定学期列表");

            return;
        }

        if self.schedule.academic_loading {

            return;
        }

        self.schedule.academic_loading = true;

        self.info(format!("正在加载 {} 个学期的成绩", term_codes.len()));

        spawn_all_grades(session, term_codes, tx.clone());
    }

    fn refresh_academic_view(&mut self, tx: &UnboundedSender<AsyncEvent>) {

        let view = self.schedule.view;

        if matches!(view, CourseView::Today | CourseView::Schedule) {

            return;
        }

        let Some(session) = self.session.clone() else {

            self.warn("当前未登录");

            self.screen = Screen::Login;

            return;
        };

        let Some(term_code) = self
            .schedule
            .current_semester()
            .map(|semester| semester.term_code.clone())
        else {

            self.warn("请先按 u 导入课表，系统才能确定当前学期");

            return;
        };

        if self.schedule.academic_loading {

            return;
        }

        self.schedule.academic_loading = true;

        match view {
            CourseView::Exams => spawn_exams(session, term_code, tx.clone()),
            CourseView::Grades => spawn_grades(session, term_code, tx.clone()),
            CourseView::Classrooms => {
                spawn_classrooms(
                    session,
                    self.schedule.classroom_campus,
                    self.schedule.classroom_date.clone(),
                    tx.clone(),
                )
            }
            CourseView::Tasks => spawn_tasks(session, tx.clone()),
            CourseView::Today | CourseView::Schedule => {}
        }
    }

    fn prewarm_bykc(&mut self, session: Session, tx: &UnboundedSender<AsyncEvent>) {

        if self.bykc.loading || self.bykc.loaded {

            return;
        }

        self.bykc.loading = true;

        self.info("登录成功，后台预热 BYKC 数据...");

        spawn_bykc_sync(
            session,
            self.bykc.include_all,
            self.bykc.detail_course_id,
            Some("BYKC 数据已预热".to_string()),
            false,
            tx.clone(),
        );
    }

    fn refresh_bykc(&mut self, tx: &UnboundedSender<AsyncEvent>) {

        let Some(session) = self.session.clone() else {

            self.warn("当前未登录");

            self.screen = Screen::Login;

            return;
        };

        if session.bykc_api.is_none() {

            self.warn("博雅功能需要 VPN 模式登录");

            return;
        }

        self.bykc.loading = true;

        self.info("加载博雅课程中...");

        spawn_bykc_sync(
            session,
            self.bykc.include_all,
            self.bykc.detail_course_id,
            None,
            false,
            tx.clone(),
        );
    }

    fn load_bykc_detail(&mut self, tx: &UnboundedSender<AsyncEvent>) {

        let Some(course_id) = self.bykc.selected_detail_target() else {

            self.warn("当前没有可查看的博雅课程");

            return;
        };

        if let Some(detail) = self.bykc.detail_cache.get(&course_id).cloned() {

            self.bykc.detail = Some(detail);

            self.bykc.detail_course_id = Some(course_id);

            // A new course starts at the top; a stale offset would open it
            // mid-description.
            self.bykc.detail_scroll = 0;

            self.bykc.show_detail_popup = true;

            self.info("已打开博雅详情");

            return;
        }

        let Some(session) = self.session.clone() else {

            self.warn("当前未登录");

            self.screen = Screen::Login;

            return;
        };

        self.bykc.loading = true;

        self.info("加载博雅课程详情...");

        spawn_bykc_sync(
            session,
            self.bykc.include_all,
            Some(course_id),
            None,
            true,
            tx.clone(),
        );
    }

    fn select_bykc_course(&mut self, tx: &UnboundedSender<AsyncEvent>) {

        let Some(session) = self.session.clone() else {

            self.warn("当前未登录");

            self.screen = Screen::Login;

            return;
        };

        let Some(course) = self.bykc.selected_course().cloned() else {

            self.warn("当前没有可报名的博雅课程");

            return;
        };

        if course.selected {

            self.warn("该课程已经报名");

            return;
        }

        self.bykc.loading = true;

        self.info(format!("报名中: {}", course.course_name));

        spawn_bykc_task(
            session,
            self.bykc.include_all,
            BykcDetailTarget::CourseFirst(course.id),
            false,
            false,
            tx.clone(),
            move |api| async move { api.select_course(course.id).await.map(Some) },
        );
    }

    fn deselect_bykc_course(&mut self, tx: &UnboundedSender<AsyncEvent>) {

        let Some(session) = self.session.clone() else {

            self.warn("当前未登录");

            self.screen = Screen::Login;

            return;
        };

        let Some(course) = self.bykc.selected_chosen_course().cloned() else {

            self.warn("当前没有可退选的博雅课程");

            return;
        };

        if !can_deselect_bykc_course(&course.course_cancel_end_date) {

            self.warn("当前课程已超过退选时间");

            return;
        }

        self.bykc.loading = true;

        self.info(format!("退选中: {}", course.course_name));

        let course_id = course.course_id;

        spawn_bykc_task(
            session,
            self.bykc.include_all,
            BykcDetailTarget::ChosenFirst(course_id),
            false,
            false,
            tx.clone(),
            move |api| async move { api.deselect_course(course_id).await.map(Some) },
        );
    }

    fn deselect_selected_bykc_course(&mut self, tx: &UnboundedSender<AsyncEvent>) {

        let Some(session) = self.session.clone() else {

            self.warn("当前未登录");

            self.screen = Screen::Login;

            return;
        };

        let Some(course) = self.bykc.selected_course().cloned() else {

            self.warn("当前没有可退选的博雅课程");

            return;
        };

        if !course.selected {

            self.warn("当前课程尚未报名，无法退选");

            return;
        }

        let Some(chosen) = self.bykc.chosen_course_for(course.id).cloned() else {

            self.warn("当前课程已标记为已报，但未找到对应已选记录，请先刷新");

            return;
        };

        if !can_deselect_bykc_course(&chosen.course_cancel_end_date) {

            self.warn("当前课程已超过退选时间");

            return;
        }

        self.bykc.loading = true;

        self.info(format!("退选中: {}", course.course_name));

        let course_id = course.id;

        spawn_bykc_task(
            session,
            self.bykc.include_all,
            BykcDetailTarget::ChosenFirst(course_id),
            false,
            false,
            tx.clone(),
            move |api| async move { api.deselect_course(course_id).await.map(Some) },
        );
    }

    fn sign_selected_bykc_course(
        &mut self,
        action: BykcSignAction,
        tx: &UnboundedSender<AsyncEvent>,
    ) {

        let Some(session) = self.session.clone() else {

            self.warn("当前未登录");

            self.screen = Screen::Login;

            return;
        };

        let action_label = if action == BykcSignAction::SignIn {

            "签到"
        } else {

            "签退"
        };

        let Some(course) = self.bykc.selected_chosen_course().cloned() else {

            self.warn(format!("当前没有可{action_label}的博雅课程"));

            return;
        };

        let (allowed, pending_status) = match action {
            BykcSignAction::SignIn => (course.can_sign, "当前课程不在可签到状态"),
            BykcSignAction::SignOut => (course.can_sign_out, "当前课程不在可签退状态"),
        };

        if !allowed {

            self.warn(pending_status);

            return;
        }

        self.bykc.loading = true;

        self.info(format!("博雅{action_label}中: {}", course.course_name));

        spawn_bykc_task(
            session,
            self.bykc.include_all,
            BykcDetailTarget::ChosenFirst(course.course_id),
            false,
            false,
            tx.clone(),
            move |api| async move { api.sign_course(course.course_id, action).await.map(Some) },
        );
    }

    fn sign_selected(&mut self, tx: &UnboundedSender<AsyncEvent>) {

        let Some(session) = self.session.clone() else {

            self.warn("当前未登录");

            self.screen = Screen::Login;

            return;
        };

        let Some(course) = self.selected_course().cloned() else {

            self.warn("当前没有可签到课程");

            return;
        };

        if course.signed() {

            self.warn("该课程已签到");

            return;
        }

        if course.course_sched_id.trim().is_empty() {

            self.warn("当前课程缺少 courseSchedId，无法签到");

            return;
        }

        self.iclass_loading = true;

        self.info(format!("签到中: {}", course.name));

        spawn_sign(session, course.course_sched_id, tx.clone());
    }

    fn logout(&mut self) {

        self.screen = Screen::Login;

        self.active_tab = WorkspaceTab::Schedule;

        self.session = None;

        self.courses.clear();

        self.week_groups.clear();

        self.selected_week = 0;

        self.selected = 0;

        self.bykc = BykcState::default();

        self.busy = false;

        self.iclass_loading = false;

        self.clear_qr();

        self.login_diagnostic = None;

        self.show_login_details = false;

        self.doctor_report = None;

        self.show_doctor_details = false;

        self.show_event_log = false;

        self.info("已退出登录");
    }

    fn toggle_qr(&mut self) {

        if self.qr_refreshing && self.qr_mode == QrMode::Terminal {

            self.clear_qr();

            self.info("已关闭二维码刷新");

            return;
        }

        match self.refresh_qr_inline() {
            Ok(()) => {

                self.qr_refreshing = true;

                self.qr_mode = QrMode::Terminal;

                self.next_qr_refresh_at = Some(Instant::now() + Duration::from_secs(2));

                self.info("二维码刷新中，按 g 关闭");
            }
            Err(error) => {

                self.error(format!("二维码生成失败: {error}"));

                self.clear_qr();
            }
        }
    }

    fn toggle_external_qr(&mut self) {

        if self.qr_refreshing && self.qr_mode == QrMode::External {

            self.clear_qr();

            self.info("已关闭外部二维码刷新，浏览器页面可手动关闭");

            return;
        }

        match self.refresh_qr_inline() {
            Ok(()) => {

                self.qr_refreshing = true;

                self.qr_mode = QrMode::External;

                self.next_qr_refresh_at = Some(Instant::now() + Duration::from_secs(2));

                match self.open_external_qr_viewer() {
                    Ok(()) => {

                        let path = self
                            .external_qr_path
                            .as_ref()
                            .map(|path| path.display().to_string())
                            .unwrap_or_else(|| "未知路径".to_string());

                        self.external_qr_status = Some("刷新中".to_string());

                        self.info(format!("外部二维码刷新中，按 G 关闭刷新，页面: {path}"));
                    }
                    Err(error) => {

                        self.error(format!("外部二维码打开失败: {error}"));

                        self.clear_qr();
                    }
                }
            }
            Err(error) => {

                self.error(format!("二维码生成失败: {error}"));

                self.mark_external_qr_error(&error);

                self.clear_qr();
            }
        }
    }

    fn refresh_qr_inline(&mut self) -> Result<(), String> {

        let Some(session) = self.session.clone() else {

            self.screen = Screen::Login;

            return Err("当前未登录".to_string());
        };

        let Some(course) = self.selected_course().cloned() else {

            return Err("当前没有可生成二维码的课程".to_string());
        };

        if course.course_sched_id.trim().is_empty() {

            return Err("当前课程缺少 courseSchedId".to_string());
        }

        let qr = session
            .api
            .generate_sign_qr(&course.course_sched_id, session.server_now_millis())
            .map_err(|error| error.to_string())?;

        self.qr_display = Some(QrDisplay {
            course_sched_id: qr.course_sched_id,
            course_name:     course.name,
            qr_url:          qr.qr_url,
            timestamp:       qr.timestamp,
        });

        if self.qr_mode == QrMode::External {

            self.write_external_qr_svg()?;

            self.write_external_qr_status("refreshing", None)?;
        }

        Ok(())
    }

    fn open_external_qr_viewer(&mut self) -> Result<(), String> {

        self.write_external_qr_svg()?;

        self.write_external_qr_status("refreshing", None)?;

        let html_path = self.write_external_qr_html()?;

        open_path_with_system(&html_path)
    }

    fn write_external_qr_svg(&self) -> Result<(), String> {

        let Some(qr) = &self.qr_display else {

            return Err("当前没有二维码".to_string());
        };

        let code = QrCode::with_error_correction_level(qr.qr_url.as_bytes(), EcLevel::L)
            .map_err(|error| error.to_string())?;

        let image = code
            .render::<svg::Color>()
            .min_dimensions(360, 360)
            .dark_color(svg::Color("#000000"))
            .light_color(svg::Color("#ffffff"))
            .build();

        let path = external_qr_svg_path()?;

        fs::write(&path, image).map_err(|error| format!("写入 {} 失败: {error}", path.display()))
    }

    fn write_external_qr_status(&self, state: &str, error: Option<&str>) -> Result<(), String> {

        let Some(qr) = &self.qr_display else {

            return Err("当前没有二维码".to_string());
        };

        let path = external_qr_status_path()?;

        let payload = serde_json::json!({
            "state": state,
            "course_name": qr.course_name,
            "course_sched_id": qr.course_sched_id,
            "generated_at": qr.timestamp,
            "generated_at_text": format_qr_timestamp(qr.timestamp),
            "error": error,
        });

        let body = serde_json::to_string_pretty(&payload).map_err(|error| error.to_string())?;

        fs::write(&path, body).map_err(|error| format!("写入 {} 失败: {error}", path.display()))
    }

    fn write_external_qr_html(&mut self) -> Result<PathBuf, String> {

        let dir = external_qr_dir()?;

        let path = dir.join("index.html");

        self.external_qr_path = Some(path.clone());

        let html = r#"<!doctype html>
<html lang="zh-CN">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <title>iClass 签到二维码</title>
  <style>
    html, body {
      height: 100%;
      margin: 0;
      background: #f3f4f6;
      color: #111827;
      font-family: -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
    }
    body {
      display: grid;
      place-items: center;
    }
    main {
      display: grid;
      gap: 14px;
      justify-items: center;
      width: min(92vw, 560px);
    }
    h1 {
      margin: 0;
      font-size: 20px;
      font-weight: 700;
      text-align: center;
    }
    img {
      width: min(76vw, 420px);
      height: min(76vw, 420px);
      background: #ffffff;
      border: 18px solid #ffffff;
      box-shadow: 0 12px 30px rgba(17, 24, 39, 0.16);
      image-rendering: pixelated;
    }
    .meta {
      font-size: 14px;
      color: #4b5563;
      text-align: center;
      line-height: 1.6;
    }
    .status-ok { color: #047857; }
    .status-error { color: #b91c1c; }
    .status-inactive { color: #92400e; }
    .hidden { display: none; }
    .error-box {
      padding: 10px 12px;
      border: 1px solid #fecaca;
      background: #fef2f2;
      color: #991b1b;
      border-radius: 6px;
      max-width: 100%;
      overflow-wrap: anywhere;
    }
  </style>
</head>
<body>
  <main>
    <h1 id="course">iClass 签到二维码</h1>
    <img id="qr" src="iclass-buaa-tui-qr.svg" alt="签到二维码">
    <div class="meta" id="meta">等待 TUI 写入刷新状态</div>
    <div class="error-box hidden" id="error"></div>
  </main>
  <script>
    const img = document.getElementById("qr");
    const meta = document.getElementById("meta");
    const course = document.getElementById("course");
    const errorBox = document.getElementById("error");
    async function refreshQr() {
      const now = Date.now();
      try {
        const response = await fetch("iclass-buaa-tui-qr-status.json?t=" + now, { cache: "no-store" });
        if (!response.ok) throw new Error("状态文件读取失败: HTTP " + response.status);
        const status = await response.json();
        course.textContent = status.course_name || "iClass 签到二维码";
        img.src = "iclass-buaa-tui-qr.svg?t=" + now;
        meta.className = "meta " + (
          status.state === "refreshing" ? "status-ok" :
          status.state === "error" ? "status-error" :
          "status-inactive"
        );
        meta.textContent = "状态: " + status.state
          + " | 最后更新: " + (status.generated_at_text || "-")
          + " | courseSchedId: " + (status.course_sched_id || "-");
        if (status.error) {
          errorBox.textContent = status.error;
          errorBox.classList.remove("hidden");
        } else {
          errorBox.textContent = "";
          errorBox.classList.add("hidden");
        }
      } catch (error) {
        meta.className = "meta status-error";
        meta.textContent = "状态刷新失败: " + error.message;
        errorBox.textContent = "请确认 TUI 仍在运行并保持外部二维码刷新开启。";
        errorBox.classList.remove("hidden");
      }
    }
    refreshQr();
    setInterval(refreshQr, 1000);
  </script>
</body>
</html>
"#;

        fs::write(&path, html).map_err(|error| format!("写入 {} 失败: {error}", path.display()))?;

        Ok(path)
    }

    fn clear_qr(&mut self) {

        if self.qr_mode == QrMode::External
            && self.qr_display.is_some()
            && !self
                .external_qr_status
                .as_deref()
                .is_some_and(|status| status.starts_with("刷新失败"))
        {

            self.mark_external_qr_inactive("TUI 已停止外部二维码刷新");
        }

        self.qr_display = None;

        self.qr_refreshing = false;

        self.qr_mode = QrMode::Terminal;

        self.next_qr_refresh_at = None;
    }

    fn mark_external_qr_error(&mut self, error: &str) {

        self.external_qr_status = Some(format!("刷新失败: {error}"));

        let _ = self.write_external_qr_status("error", Some(error));
    }

    fn mark_external_qr_inactive(&mut self, reason: &str) {

        self.external_qr_status = Some(reason.to_string());

        let _ = self.write_external_qr_status("inactive", Some(reason));
    }

    fn move_vertical(&mut self, delta: isize) {

        let Some(current_abs) = self.selected_course_absolute_index() else {

            return;
        };

        let current_date = self.courses[current_abs].date.clone();

        let day_courses = self.day_course_absolute_indices(&current_date);

        if day_courses.is_empty() {

            return;
        }

        let Some(day_pos) = day_courses.iter().position(|index| *index == current_abs) else {

            return;
        };

        let next_pos = clamp_step(day_pos, day_courses.len(), delta);

        self.set_selected_absolute(day_courses[next_pos]);
    }

    fn move_horizontal(&mut self, delta_days: i64) {

        let Some(current_abs) = self.selected_course_absolute_index() else {

            return;
        };

        let Some(current_date) = parse_course_date(&self.courses[current_abs].date) else {

            return;
        };

        let Some(week) = self.selected_week_group() else {

            return;
        };

        let Some(week_start) = parse_course_date(&week.start_date) else {

            return;
        };

        let current_day_courses = self.day_course_absolute_indices(&self.courses[current_abs].date);

        let current_row = current_day_courses
            .iter()
            .position(|index| *index == current_abs)
            .unwrap_or(0);

        let step = delta_days.signum();

        if step == 0 {

            return;
        }

        let week_end = week_start + ChronoDuration::days(6);

        let mut target_date = current_date + ChronoDuration::days(step);

        while target_date >= week_start && target_date <= week_end {

            let target_key = target_date.format("%Y-%m-%d").to_string();

            let target_courses = self.day_course_absolute_indices(&target_key);

            if !target_courses.is_empty() {

                let target_row = current_row.min(target_courses.len().saturating_sub(1));

                self.set_selected_absolute(target_courses[target_row]);

                return;
            }

            target_date += ChronoDuration::days(step);
        }
    }

    fn day_course_absolute_indices(&self, date: &str) -> Vec<usize> {

        self.visible_course_indices()
            .iter()
            .copied()
            .filter(|index| self.courses[*index].date == date)
            .collect()
    }

    fn set_selected_absolute(&mut self, absolute_index: usize) {

        if let Some(position) = self
            .visible_course_indices()
            .iter()
            .position(|index| *index == absolute_index)
        {

            self.selected = position;

            self.clear_qr();
        }
    }

    fn select_prev_week(&mut self) {

        if self.week_groups.is_empty() || self.selected_week == 0 {

            return;
        }

        self.selected_week -= 1;

        self.selected = 0;

        self.clear_qr();

        if let Some(week) = self.selected_week_group() {

            self.info(format!("已切换到 {}", week.label));
        }
    }

    fn select_next_week(&mut self) {

        if self.week_groups.is_empty() || self.selected_week + 1 >= self.week_groups.len() {

            return;
        }

        self.selected_week += 1;

        self.selected = 0;

        self.clear_qr();

        if let Some(week) = self.selected_week_group() {

            self.info(format!("已切换到 {}", week.label));
        }
    }

    fn replace_courses(
        &mut self,
        courses: Vec<CourseDetailItem>,
        preferred_week_key: Option<String>,
        preferred_course_id: Option<String>,
    ) {

        self.courses = courses;

        self.week_groups = build_week_groups(&self.courses);

        self.selected_week = preferred_week_key
            .as_deref()
            .and_then(|key| self.week_groups.iter().position(|group| group.key == key))
            .or_else(|| {

                self.week_groups
                    .iter()
                    .position(|group| group.key == current_week_key())
            })
            .unwrap_or(0);

        self.selected = preferred_course_id
            .as_deref()
            .and_then(|course_sched_id| {

                self.visible_course_indices()
                    .iter()
                    .position(|index| self.courses[*index].course_sched_id == course_sched_id)
            })
            .unwrap_or(0);

        if self.selected >= self.visible_courses_len() {

            self.selected = 0;
        }
    }
}

fn log_level_from_event(level: EventLevel) -> LogLevel {

    match level {
        EventLevel::Info | EventLevel::Success => LogLevel::Info,
        EventLevel::Warn => LogLevel::Warn,
        EventLevel::Error => LogLevel::Error,
    }
}

fn spawn_login(input: LoginInput, tx: UnboundedSender<AsyncEvent>) {

    tokio::spawn(async move {

        let result = match crate::iclass::IClassApi::new(input.use_vpn) {
            Ok(api) => {
                match api.start_login(&input).await {
                    Ok(LoginStart::Complete(session)) => {

                        let courses = api.get_merged_course_details(&session, 7).await;

                        Ok(login_success_with_prefetch_result(
                            session,
                            courses,
                            input.student_id.clone(),
                        ))
                    }
                    Ok(LoginStart::Captcha(challenge)) => {

                        let pending = PendingCaptchaLogin {
                            api,
                            input,
                            challenge,
                        };

                        let _ = tx.send(AsyncEvent::LoginCaptcha(Ok(pending)));

                        return;
                    }
                    Err(diagnostic) => {
                        Err(LoginFailure {
                            message: diagnostic.summary.clone(),
                            diagnostic,
                        })
                    }
                }
            }
            Err(error) => {
                Err(LoginFailure {
                    message:    format_anyhow_error(error),
                    diagnostic: LoginDiagnostic {
                        kind:        crate::model::LoginFailureKind::Unknown,
                        stage:       "client_init".to_string(),
                        summary:     "初始化 HTTP 客户端失败".to_string(),
                        error_chain: vec!["初始化 HTTP 客户端失败".to_string()],
                        final_url:   None,
                        http_status: None,
                        page_hint:   None,
                        suggestions: vec!["检查本机 TLS/证书环境".to_string()],
                    },
                })
            }
        };

        let _ = tx.send(AsyncEvent::Login(result));
    });
}

fn spawn_continue_captcha_login(
    pending: PendingCaptchaLogin,
    captcha: String,
    tx: UnboundedSender<AsyncEvent>,
) {

    tokio::spawn(async move {

        let result = match pending
            .api
            .continue_captcha_login(&pending.input, &pending.challenge, &captcha)
            .await
        {
            Ok(session) => {

                let courses = pending.api.get_merged_course_details(&session, 7).await;

                Ok(login_success_with_prefetch_result(
                    session,
                    courses,
                    pending.input.student_id.clone(),
                ))
            }
            Err(diagnostic) => {
                Err(LoginFailure {
                    message: diagnostic.summary.clone(),
                    diagnostic,
                })
            }
        };

        let _ = tx.send(AsyncEvent::Login(result));
    });
}

fn login_success_with_prefetch_result(
    session: Session,
    courses: Result<Vec<CourseDetailItem>, anyhow::Error>,
    schedule_account: String,
) -> LoginSuccess {

    match courses {
        Ok(courses) => {
            LoginSuccess {
                session,
                courses,
                course_prefetch_error: None,
                schedule_account,
            }
        }
        Err(error) => {
            LoginSuccess {
                session,
                courses: Vec::new(),
                course_prefetch_error: Some(format_anyhow_error(error)),
                schedule_account,
            }
        }
    }
}

fn spawn_doctor(use_vpn: bool, tx: UnboundedSender<AsyncEvent>) {

    tokio::spawn(async move {

        let result = match crate::iclass::IClassApi::new(use_vpn) {
            Ok(api) => Ok(api.doctor().await),
            Err(error) => Err(format_anyhow_error(error)),
        };

        let _ = tx.send(AsyncEvent::Doctor(result));
    });
}

pub fn spawn_version_check(tx: UnboundedSender<AsyncEvent>) {

    tokio::spawn(async move {

        let result = fetch_latest_version_info()
            .await
            .map_err(|error| error.to_string());

        let _ = tx.send(AsyncEvent::VersionCheck(result));
    });
}

fn spawn_refresh(session: Session, tx: UnboundedSender<AsyncEvent>) {

    tokio::spawn(async move {

        let result = session
            .api
            .get_merged_course_details(&session, 7)
            .await
            .map_err(format_anyhow_error);

        let _ = tx.send(AsyncEvent::Refresh(result));
    });
}

fn spawn_schedule_import(
    session: Session,
    requested_term: Option<String>,
    tx: UnboundedSender<AsyncEvent>,
) {

    tokio::spawn(async move {

        let result = session
            .api
            .import_semester_schedule(&session, requested_term.as_deref())
            .await
            .map_err(format_anyhow_error);

        let _ = tx.send(AsyncEvent::ScheduleImport(result));
    });
}

fn spawn_exams(session: Session, term_code: String, tx: UnboundedSender<AsyncEvent>) {

    tokio::spawn(async move {

        let result = session
            .api
            .get_exams(&term_code)
            .await
            .map_err(format_anyhow_error);

        let _ = tx.send(AsyncEvent::Exams(result));
    });
}

fn spawn_grades(session: Session, term_code: String, tx: UnboundedSender<AsyncEvent>) {

    tokio::spawn(async move {

        let result = session
            .api
            .get_grades(&term_code)
            .await
            .map_err(format_anyhow_error);

        let _ = tx.send(AsyncEvent::Grades(result));
    });
}

fn spawn_all_grades(session: Session, term_codes: Vec<String>, tx: UnboundedSender<AsyncEvent>) {

    tokio::spawn(async move {

        let result = Ok(session.api.get_grades_for_terms(&term_codes).await);

        let _ = tx.send(AsyncEvent::AllGrades(result));
    });
}

fn spawn_classrooms(session: Session, campus: i64, date: String, tx: UnboundedSender<AsyncEvent>) {

    tokio::spawn(async move {

        let result = session
            .api
            .query_classrooms(campus, &date)
            .await
            .map_err(format_anyhow_error);

        let _ = tx.send(AsyncEvent::Classrooms(result));
    });
}

fn spawn_tasks(session: Session, tx: UnboundedSender<AsyncEvent>) {

    tokio::spawn(async move {

        let result = session
            .api
            .get_assignments()
            .await
            .map_err(format_anyhow_error);

        let _ = tx.send(AsyncEvent::Tasks(result));
    });
}

fn spawn_sign(session: Session, course_sched_id: String, tx: UnboundedSender<AsyncEvent>) {

    tokio::spawn(async move {

        let result = session
            .api
            .sign_now(&session, &course_sched_id)
            .await
            .map_err(format_anyhow_error);

        let _ = tx.send(AsyncEvent::Sign(result));
    });
}

/// Spawns one BYKC refresh task that can update lists and optionally load a detail record.
///
/// Why:
/// BYKC screens often need a list refresh and a focused detail fetch to land
/// together. Running them in one worker avoids racing updates from multiple
/// overlapping tasks.

fn spawn_bykc_sync(
    session: Session,
    include_all: bool,
    detail_course_id: Option<i64>,
    message: Option<String>,
    open_detail_popup: bool,
    tx: UnboundedSender<AsyncEvent>,
) {

    let detail_target =
        detail_course_id.map_or(BykcDetailTarget::Auto, BykcDetailTarget::CourseFirst);

    spawn_bykc_task(
        session,
        include_all,
        detail_target,
        open_detail_popup,
        true,
        tx,
        |_| async move { Ok(message) },
    );
}

fn spawn_bykc_task<F, Fut>(
    session: Session,
    include_all: bool,
    detail_target: BykcDetailTarget,
    open_detail_popup: bool,
    require_detail: bool,
    tx: UnboundedSender<AsyncEvent>,
    action: F,
) where
    F: FnOnce(BykcApi) -> Fut + Send + 'static,
    Fut: Future<Output = anyhow::Result<Option<String>>> + Send + 'static,
{

    tokio::spawn(async move {

        let result = async {

            let api = session
                .bykc_api
                .clone()
                .ok_or_else(|| anyhow::anyhow!("博雅功能需要 VPN 模式登录"))?;

            let message = action(api.clone()).await?;

            build_bykc_sync_success(
                &api,
                include_all,
                detail_target,
                message,
                open_detail_popup,
                require_detail,
            )
            .await
        }
        .await
        .map_err(format_anyhow_error);

        let _ = tx.send(AsyncEvent::BykcSync(Box::new(result)));
    });
}

fn format_anyhow_error(error: anyhow::Error) -> String {

    let mut parts = error.chain().map(ToString::to_string);

    let Some(first) = parts.next() else {

        return "未知错误".to_string();
    };

    parts.fold(first, |mut output, cause| {

        if !output.contains(&cause) {

            output.push_str(": ");

            output.push_str(&cause);
        }

        output
    })
}

async fn build_bykc_sync_success(
    api: &BykcApi,
    include_all: bool,
    detail_target: BykcDetailTarget,
    message: Option<String>,
    open_detail_popup: bool,
    require_detail: bool,
) -> anyhow::Result<BykcSyncSuccess> {

    let courses = api.get_courses(include_all).await?;

    let chosen_courses = api.get_chosen_courses().await?;

    let (statistics, statistics_error) = match api.get_statistics().await {
        Ok(statistics) => (Some(statistics), None),
        Err(error) => (None, Some(error.to_string())),
    };

    let detail_target = resolve_bykc_detail_target(detail_target, &courses, &chosen_courses);

    let detail = if let Some(course_id) = detail_target {

        if require_detail {

            Some(api.get_course_detail(course_id).await?)
        } else {

            api.get_course_detail(course_id).await.ok()
        }
    } else {

        None
    };

    Ok(BykcSyncSuccess {
        courses,
        chosen_courses,
        statistics,
        statistics_error,
        detail,
        message,
        open_detail_popup,
    })
}

fn resolve_bykc_detail_target(
    target: BykcDetailTarget,
    courses: &[BykcCourse],
    chosen_courses: &[BykcChosenCourse],
) -> Option<i64> {

    match target {
        BykcDetailTarget::Auto => None,
        BykcDetailTarget::CourseFirst(course_id) => {
            courses
                .iter()
                .find(|course| course.id == course_id)
                .map(|course| course.id)
                .or_else(|| {

                    chosen_courses
                        .iter()
                        .find(|course| course.course_id == course_id)
                        .map(|course| course.course_id)
                })
        }
        BykcDetailTarget::ChosenFirst(course_id) => {
            chosen_courses
                .iter()
                .find(|course| course.course_id == course_id)
                .map(|course| course.course_id)
        }
    }
    .or_else(|| courses.first().map(|course| course.id))
    .or_else(|| chosen_courses.first().map(|course| course.course_id))
}

/// Builds Monday-based week buckets from the flat iClass course list.
///
/// Why:
/// The backend returns rows, but the UI is a weekly grid. Precomputing week
/// groups once keeps navigation and rendering simple and stable.

fn build_week_groups(courses: &[CourseDetailItem]) -> Vec<WeekGroup> {

    let mut groups: Vec<WeekGroup> = Vec::new();

    let mut current_group_key = String::new();

    for (index, course) in courses.iter().enumerate() {

        let Some(date) = parse_course_date(&course.date) else {

            let key = "unknown".to_string();

            if current_group_key != key {

                current_group_key = key.clone();

                groups.push(WeekGroup {
                    key,
                    label: "未识别周".to_string(),
                    start_date: String::new(),
                    end_date: String::new(),
                    course_indices: Vec::new(),
                });
            }

            if let Some(group) = groups.last_mut() {

                group.course_indices.push(index);
            }

            continue;
        };

        let week_start = monday_of(date);

        let week_end = week_start + ChronoDuration::days(6);

        let key = week_start.format("%Y-%m-%d").to_string();

        if current_group_key != key {

            current_group_key = key.clone();

            groups.push(WeekGroup {
                key,
                label: format!(
                    "{} - {}{}",
                    week_start.format("%m/%d"),
                    week_end.format("%m/%d"),
                    if week_start == monday_of(Local::now().date_naive()) {

                        " (本周)"
                    } else {

                        ""
                    }
                ),
                start_date: week_start.format("%Y-%m-%d").to_string(),
                end_date: week_end.format("%Y-%m-%d").to_string(),
                course_indices: Vec::new(),
            });
        }

        if let Some(group) = groups.last_mut() {

            group.course_indices.push(index);
        }
    }

    groups
}

fn parse_course_date(value: &str) -> Option<NaiveDate> {

    NaiveDate::parse_from_str(value.trim(), "%Y-%m-%d").ok()
}

fn monday_of(date: NaiveDate) -> NaiveDate {

    let delta = i64::from(date.weekday().num_days_from_monday());

    date - ChronoDuration::days(delta)
}

fn current_week_key() -> String {

    monday_of(Local::now().date_naive())
        .format("%Y-%m-%d")
        .to_string()
}

fn date_in_range(today: NaiveDate, start: &str, end: &str) -> bool {

    NaiveDate::parse_from_str(start, "%Y-%m-%d")
        .ok()
        .zip(NaiveDate::parse_from_str(end, "%Y-%m-%d").ok())
        .is_some_and(|(start, end)| today >= start && today <= end)
}

async fn fetch_latest_version_info() -> anyhow::Result<VersionInfo> {

    let current = env!("CARGO_PKG_VERSION").to_string();

    let (latest, latest_url) = fetch_tag(&current).await?;

    Ok(make_version_info(&current, &latest, &latest_url))
}

fn compare_version(current: &str, latest: &str) -> i32 {

    let current_parts = parse_version_parts(normalize_version(current));

    let latest_parts = parse_version_parts(normalize_version(latest));

    let max_len = current_parts.len().max(latest_parts.len());

    for index in 0..max_len {

        let left = current_parts.get(index).copied().unwrap_or(0);

        let right = latest_parts.get(index).copied().unwrap_or(0);

        if left > right {

            return 1;
        }

        if left < right {

            return -1;
        }
    }

    0
}

fn normalize_version(version: &str) -> &str {

    version.trim().trim_start_matches(['v', 'V'])
}

fn parse_version_parts(version: &str) -> Vec<u32> {

    version
        .split('.')
        .map(|part| {

            part.chars()
                .take_while(|ch| ch.is_ascii_digit())
                .collect::<String>()
                .parse::<u32>()
                .unwrap_or(0)
        })
        .collect()
}

fn make_version_info(current: &str, latest: &str, latest_url: &str) -> VersionInfo {

    VersionInfo {
        current:    current.to_string(),
        latest:     normalize_version(latest).to_string(),
        latest_url: latest_url.to_string(),
        is_latest:  compare_version(current, latest) >= 0,
    }
}

async fn fetch_tag(current: &str) -> anyhow::Result<(String, String)> {

    let response = reqwest::Client::builder()
        .build()?
        .get("https://github.com/Yiki21/iclass_buaa_tui/releases/latest")
        .header("User-Agent", format!("iclass_buaa_tui/{current}"))
        .send()
        .await?
        .error_for_status()?;

    let latest_url = response.url().to_string();

    let latest = latest_url
        .rsplit("/tag/")
        .next()
        .filter(|value| !value.is_empty() && *value != latest_url)
        .ok_or_else(|| anyhow::anyhow!("GitHub releases/latest 未跳转到 tag 页面: {latest_url}"))?
        .to_string();

    Ok((latest, latest_url))
}

fn external_qr_dir() -> Result<PathBuf, String> {

    let dir = std::env::temp_dir().join("iclass-buaa-tui-qr");

    fs::create_dir_all(&dir).map_err(|error| format!("创建 {} 失败: {error}", dir.display()))?;

    Ok(dir)
}

fn external_qr_svg_path() -> Result<PathBuf, String> {

    Ok(external_qr_dir()?.join("iclass-buaa-tui-qr.svg"))
}

fn external_qr_status_path() -> Result<PathBuf, String> {

    Ok(external_qr_dir()?.join("iclass-buaa-tui-qr-status.json"))
}

fn format_qr_timestamp(timestamp: i64) -> String {

    chrono::Local
        .timestamp_millis_opt(timestamp)
        .single()
        .map(|time| time.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| timestamp.to_string())
}

fn open_path_with_system(path: &Path) -> Result<(), String> {

    let result = if cfg!(target_os = "macos") {

        Command::new("open").arg(path).spawn()
    } else if cfg!(target_os = "windows") {

        Command::new("cmd")
            .args(["/C", "start", ""])
            .arg(path)
            .spawn()
    } else {

        Command::new("xdg-open").arg(path).spawn()
    };

    result
        .map(|_| ())
        .map_err(|error| format!("启动外部查看器失败: {error}"))
}

fn copy_to_clipboard(text: &str) -> Result<(), String> {

    if cfg!(target_os = "macos") {

        return run_clipboard_command("pbcopy", &[], text);
    }

    if cfg!(target_os = "windows") {

        return run_clipboard_command("clip", &[], text);
    }

    run_clipboard_command("wl-copy", &[], text)
        .or_else(|_| run_clipboard_command("xclip", &["-selection", "clipboard"], text))
        .or_else(|_| run_clipboard_command("xsel", &["--clipboard", "--input"], text))
}

fn run_clipboard_command(command: &str, args: &[&str], text: &str) -> Result<(), String> {

    let mut child = Command::new(command)
        .args(args)
        .stdin(std::process::Stdio::piped())
        .spawn()
        .map_err(|error| format!("启动 {command} 失败: {error}"))?;

    let Some(mut stdin) = child.stdin.take() else {

        return Err(format!("{command} 未提供 stdin"));
    };

    use std::io::Write;

    stdin
        .write_all(text.as_bytes())
        .map_err(|error| format!("写入 {command} stdin 失败: {error}"))?;

    drop(stdin);

    let status = child
        .wait()
        .map_err(|error| format!("等待 {command} 退出失败: {error}"))?;

    if status.success() {

        Ok(())
    } else {

        Err(format!("{command} 返回退出码 {status}"))
    }
}

fn remembered_login_path() -> Result<PathBuf, String> {

    Ok(user_config_dir()?
        .join("iclass-buaa")
        .join("tui-login.toml"))
}

fn user_config_dir() -> Result<PathBuf, String> {

    if let Some(base) = std::env::var_os("XDG_CONFIG_HOME") {

        return Ok(PathBuf::from(base));
    }

    #[cfg(windows)]
    {

        if let Some(base) = std::env::var_os("APPDATA") {

            return Ok(PathBuf::from(base));
        }
    }

    let home = std::env::var_os("HOME").ok_or_else(|| "找不到 HOME 目录".to_string())?;

    Ok(PathBuf::from(home).join(".config"))
}

fn load_remembered_login() -> Result<Option<RememberedLogin>, String> {

    let path = remembered_login_path()?;

    if !path.is_file() {

        return Ok(None);
    }

    let raw = fs::read_to_string(&path)
        .map_err(|error| format!("读取 {} 失败: {error}", path.display()))?;

    let remembered =
        toml::from_str(&raw).map_err(|error| format!("解析 {} 失败: {error}", path.display()))?;

    Ok(Some(remembered))
}

fn save_remembered_login(remembered: &RememberedLogin) -> Result<(), String> {

    let path = remembered_login_path()?;

    let Some(dir) = path.parent() else {

        return Err("无法解析记住我配置目录".to_string());
    };

    fs::create_dir_all(dir).map_err(|error| format!("创建 {} 失败: {error}", dir.display()))?;

    let raw =
        toml::to_string(remembered).map_err(|error| format!("序列化记住我信息失败: {error}"))?;

    fs::write(&path, raw).map_err(|error| format!("写入 {} 失败: {error}", path.display()))?;

    restrict_owner_only(&path)?;

    Ok(())
}

fn delete_remembered_login() -> Result<(), String> {

    let path = remembered_login_path()?;

    if !path.exists() {

        return Ok(());
    }

    fs::remove_file(&path).map_err(|error| format!("删除 {} 失败: {error}", path.display()))
}

fn restrict_owner_only(path: &Path) -> Result<(), String> {

    #[cfg(unix)]
    {

        use std::os::unix::fs::PermissionsExt;

        fs::set_permissions(path, fs::Permissions::from_mode(0o600))
            .map_err(|error| format!("设置 {} 权限失败: {error}", path.display()))?;
    }

    Ok(())
}

fn clamp_step(current: usize, len: usize, delta: isize) -> usize {

    if len == 0 {

        return 0;
    }

    let next = current as isize + delta;

    next.clamp(0, len.saturating_sub(1) as isize) as usize
}

#[cfg(test)]

mod tests {

    use super::{App, AsyncEvent, BykcSyncSuccess, EventLevel, MAX_EVENT_LOG_ENTRIES};
    use crate::bykc::BykcCourse;

    #[test]

    fn event_log_keeps_only_recent_entries() {

        let mut app = App::default();

        for index in 0..(MAX_EVENT_LOG_ENTRIES + 3) {

            app.info(format!("event-{index}"));
        }

        assert_eq!(app.event_log.len(), MAX_EVENT_LOG_ENTRIES);

        assert_eq!(
            app.event_log.first().map(|entry| entry.message.as_str()),
            Some("event-3")
        );

        assert_eq!(
            app.event_log.last().map(|entry| entry.message.as_str()),
            Some("event-10")
        );
    }

    #[test]

    fn clear_event_log_replaces_entries_with_clear_notice() {

        let mut app = App::default();

        app.error("boom");

        app.clear_event_log();

        assert_eq!(app.event_log.len(), 1);

        assert_eq!(app.event_log[0].level, EventLevel::Info);

        assert_eq!(app.event_log[0].message, "已清空事件日志");

        assert_eq!(app.status, "已清空事件日志");
    }

    #[test]

    fn async_refresh_events_clear_only_their_module_loading_state() {

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        let mut app = App {
            busy: true,
            iclass_loading: true,
            ..App::default()
        };

        app.bykc.loading = true;

        app.handle_async(AsyncEvent::Refresh(Ok(Vec::new())), &tx);

        assert!(app.busy);

        assert!(!app.iclass_loading);

        assert!(app.bykc.loading);

        app.handle_async(
            AsyncEvent::BykcSync(Box::new(Ok(BykcSyncSuccess {
                courses:           vec![BykcCourse {
                    id: 42,
                    course_name: "博雅课程".to_string(),
                    ..BykcCourse::default()
                }],
                chosen_courses:    Vec::new(),
                statistics:        None,
                statistics_error:  None,
                detail:            None,
                message:           Some("预热完成".to_string()),
                open_detail_popup: false,
            }))),
            &tx,
        );

        assert!(app.busy);

        assert!(!app.bykc.loading);

        assert!(app.bykc.loaded);
    }

    #[test]

    fn external_qr_status_json_contains_course_and_error_state() {

        let app = App {
            qr_display: Some(super::QrDisplay {
                course_sched_id: "sched-42".to_string(),
                course_name:     "测试课程".to_string(),
                qr_url:          "https://example.invalid/qr".to_string(),
                timestamp:       1_779_811_200_000,
            }),
            ..App::default()
        };

        app.write_external_qr_status("error", Some("刷新失败"))
            .unwrap();

        let status_path = super::external_qr_status_path().unwrap();

        let raw = std::fs::read_to_string(status_path).unwrap();

        let value: serde_json::Value = serde_json::from_str(&raw).unwrap();

        assert_eq!(value["state"], "error");

        assert_eq!(value["course_name"], "测试课程");

        assert_eq!(value["course_sched_id"], "sched-42");

        assert_eq!(value["error"], "刷新失败");
    }
}
