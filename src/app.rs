//! Application state, async event routing, and keyboard-driven TUI behavior.

use chrono::{Datelike, Duration as ChronoDuration, Local, NaiveDate};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use qrcode::{EcLevel, QrCode, render::svg};
use ratatui::style::{Color, Modifier, Style};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

use crate::bykc::{
    BykcApi, BykcChosenCourse, BykcCourse, BykcCourseDetail, BykcSignAction, BykcStatistics,
    can_deselect_bykc_course,
};
use crate::model::{
    CourseDetailItem, DoctorReport, LoginDiagnostic, LoginInput, Session, SignOutcome,
};

#[derive(Clone, Debug)]

pub enum AsyncEvent {
    Login(Result<LoginSuccess, LoginFailure>),
    Refresh(Result<Vec<CourseDetailItem>, String>),
    Sign(Result<SignOutcome, String>),
    BykcSync(Box<Result<BykcSyncSuccess, String>>),
    VersionCheck(Result<VersionInfo, String>),
    Doctor(Result<DoctorReport, String>),
}

#[derive(Clone, Debug)]

pub struct LoginSuccess {
    pub session: Session,
    pub courses: Vec<CourseDetailItem>,
}

#[derive(Clone, Debug)]

pub struct LoginFailure {
    pub message:    String,
    pub diagnostic: LoginDiagnostic,
}

#[derive(Clone, Debug)]

pub struct QrDisplay {
    pub course_sched_id: String,
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
    IClass,
    Bykc,
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
    RememberMe,
}

/// Login form state for the TUI login screen.
#[derive(Clone, Debug, Default)]

pub struct LoginForm {
    pub student_id:   String,
    pub use_vpn:      bool,
    pub vpn_username: String,
    pub vpn_password: String,
    pub remember_me:  bool,
    pub focus:        usize,
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
            student_id:   remembered.student_id,
            use_vpn:      remembered.use_vpn,
            vpn_username: remembered.vpn_username,
            vpn_password: remembered.vpn_password,
            remember_me:  true,
            focus:        0,
        }
    }

    /// Returns the fields currently visible to the user.

    pub fn visible_focuses(&self) -> Vec<LoginFocus> {

        let mut fields = vec![LoginFocus::UseVpn];

        if !self.use_vpn {

            fields.push(LoginFocus::StudentId);
        }

        if self.use_vpn {

            fields.push(LoginFocus::VpnUsername);

            fields.push(LoginFocus::VpnPassword);
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

#[derive(Debug)]

pub struct App {
    pub screen:              Screen,
    pub active_tab:          WorkspaceTab,
    pub login:               LoginForm,
    pub session:             Option<Session>,
    pub courses:             Vec<CourseDetailItem>,
    pub week_groups:         Vec<WeekGroup>,
    pub selected_week:       usize,
    pub selected:            usize,
    pub bykc:                BykcState,
    pub status:              String,
    pub busy:                bool,
    pub should_quit:         bool,
    pub show_help:           bool,
    pub qr_display:          Option<QrDisplay>,
    pub qr_refreshing:       bool,
    pub qr_mode:             QrMode,
    pub version_info:        Option<VersionInfo>,
    pub version_error:       Option<String>,
    pub login_diagnostic:    Option<LoginDiagnostic>,
    pub show_login_details:  bool,
    pub doctor_report:       Option<DoctorReport>,
    pub show_doctor_details: bool,
    next_qr_refresh_at:      Option<Instant>,
}

impl Default for App {
    fn default() -> Self {

        Self {
            screen:              Screen::Login,
            active_tab:          WorkspaceTab::IClass,
            login:               LoginForm::default(),
            session:             None,
            courses:             Vec::new(),
            week_groups:         Vec::new(),
            selected_week:       0,
            selected:            0,
            bykc:                BykcState::default(),
            status:              "直连模式输入学号；VPN 模式输入 VPN 账号密码后按 enter 登录"
                .to_string(),
            busy:                false,
            should_quit:         false,
            show_help:           false,
            qr_display:          None,
            qr_refreshing:       false,
            qr_mode:             QrMode::Terminal,
            version_info:        None,
            version_error:       None,
            login_diagnostic:    None,
            show_login_details:  false,
            doctor_report:       None,
            show_doctor_details: false,
            next_qr_refresh_at:  None,
        }
    }
}

impl App {
    pub fn load() -> Self {

        let mut app = Self::default();

        match load_remembered_login() {
            Ok(Some(remembered)) => {

                app.login = LoginForm::from_remembered(remembered);

                app.status = "已载入上次登录信息，按 enter 登录；space 可关闭记住我".to_string();
            }
            Ok(None) => {}
            Err(error) => {

                app.status = format!("读取记住我信息失败: {error}");
            }
        }

        app
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

        if self.screen == Screen::Workspace
            && self.active_tab == WorkspaceTab::Bykc
            && self.bykc.show_detail_popup
        {

            match key.code {
                KeyCode::Esc | KeyCode::Enter | KeyCode::Char('o') => {

                    self.bykc.show_detail_popup = false;

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

    pub fn handle_tick(&mut self) {

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

            self.status = format!("二维码刷新失败: {error}");

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

    pub fn handle_async(&mut self, event: AsyncEvent) {

        self.busy = false;

        match event {
            AsyncEvent::Login(result) => {
                match result {
                    Ok(data) => {

                        self.screen = Screen::Workspace;

                        self.active_tab = WorkspaceTab::IClass;

                        self.session = Some(data.session);

                        self.login_diagnostic = None;

                        self.show_login_details = false;

                        self.replace_courses(data.courses, None, None);

                        self.bykc = BykcState::default();

                        self.clear_qr();

                        let remember_status = self.persist_remembered_login_status();

                        self.status = format!(
                            "登录成功。tab 切换 iClass / BYKC，s 直接签到，g 终端二维码，G \
                             外部二维码，r 刷新，Shift+X 退出登录。{remember_status}"
                        );
                    }
                    Err(error) => {

                        self.login_diagnostic = Some(error.diagnostic);

                        self.show_login_details = false;

                        self.status = format!("登录失败: {}。按 v 查看详情", error.message);
                    }
                }
            }
            AsyncEvent::Refresh(result) => {
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

                        self.status = format!(
                            "课程已刷新，共 {} 条，当前周：{}",
                            self.visible_courses_len(),
                            week_label
                        );

                        if self
                            .qr_display
                            .as_ref()
                            .is_some_and(|qr| qr.course_sched_id != selected_id)
                        {

                            self.clear_qr();
                        }
                    }
                    Err(error) => {

                        self.status = format!("刷新失败: {error}");
                    }
                }
            }
            AsyncEvent::Sign(result) => {
                match result {
                    Ok(outcome) => {

                        self.status = if outcome.success_like {

                            "签到成功".to_string()
                        } else {

                            format!("签到失败: {}", outcome.message)
                        };

                        if outcome.success_like
                            && let Some(index) = self.selected_course_absolute_index()
                            && let Some(item) = self.courses.get_mut(index)
                        {

                            item.sign_status = "1".to_string();
                        }
                    }
                    Err(error) => {

                        self.status = format!("签到失败: {error}");
                    }
                }
            }
            AsyncEvent::BykcSync(result) => {
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

                        self.status = message;
                    }
                    Err(error) => {

                        self.status = format!("博雅操作失败: {error}");
                    }
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
                match result {
                    Ok(report) => {

                        let failed = report.checks.iter().filter(|check| !check.ok).count();

                        self.doctor_report = Some(report);

                        self.show_doctor_details = true;

                        self.status = if failed == 0 {

                            "网络自检完成：全部通过".to_string()
                        } else {

                            format!("网络自检完成：{failed} 项异常，按 D 查看详情")
                        };
                    }
                    Err(error) => {

                        self.status = format!("网络自检失败: {error}");
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

        let tabs = [WorkspaceTab::IClass, WorkspaceTab::Bykc];

        let current_index = tabs
            .iter()
            .position(|tab| *tab == self.active_tab)
            .unwrap_or_default();

        let next_index =
            ((current_index as isize + delta).rem_euclid(tabs.len() as isize)) as usize;

        self.active_tab = tabs[next_index];

        if self.active_tab != WorkspaceTab::IClass {

            self.clear_qr();
        }

        if self.active_tab == WorkspaceTab::Bykc && !self.bykc.loaded {

            self.refresh_bykc(tx);
        }
    }

    fn push_char(&mut self, ch: char) {

        match self.login.current_focus() {
            LoginFocus::StudentId => self.login.student_id.push(ch),
            LoginFocus::VpnUsername => self.login.vpn_username.push(ch),
            LoginFocus::VpnPassword => self.login.vpn_password.push(ch),
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
            LoginFocus::UseVpn | LoginFocus::RememberMe => {}
        }
    }

    fn submit_login(&mut self, tx: &UnboundedSender<AsyncEvent>) {

        let input = self.login.to_input();

        if !input.use_vpn && input.student_id.is_empty() {

            self.status = "直连模式需要输入学号".to_string();

            return;
        }

        if input.use_vpn && (input.vpn_username.is_empty() || input.vpn_password.is_empty()) {

            self.status = "VPN 模式需要输入账号和密码".to_string();

            return;
        }

        self.busy = true;

        self.login_diagnostic = None;

        self.show_login_details = false;

        self.status = "登录中并拉取课程...".to_string();

        spawn_login(input, tx.clone());
    }

    fn run_doctor(&mut self, tx: &UnboundedSender<AsyncEvent>) {

        self.busy = true;

        self.show_doctor_details = false;

        self.status = "执行网络自检中...".to_string();

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

            self.status = "当前未登录".to_string();

            self.screen = Screen::Login;

            return;
        };

        self.busy = true;

        self.status = "刷新课程中...".to_string();

        spawn_refresh(session, tx.clone());
    }

    fn refresh_bykc(&mut self, tx: &UnboundedSender<AsyncEvent>) {

        let Some(session) = self.session.clone() else {

            self.status = "当前未登录".to_string();

            self.screen = Screen::Login;

            return;
        };

        if session.bykc_api.is_none() {

            self.status = "博雅功能需要 VPN 模式登录".to_string();

            return;
        }

        self.busy = true;

        self.status = "加载博雅课程中...".to_string();

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

            self.status = "当前没有可查看的博雅课程".to_string();

            return;
        };

        if let Some(detail) = self.bykc.detail_cache.get(&course_id).cloned() {

            self.bykc.detail = Some(detail);

            self.bykc.detail_course_id = Some(course_id);

            self.bykc.show_detail_popup = true;

            self.status = "已打开博雅详情".to_string();

            return;
        }

        let Some(session) = self.session.clone() else {

            self.status = "当前未登录".to_string();

            self.screen = Screen::Login;

            return;
        };

        self.busy = true;

        self.status = "加载博雅课程详情...".to_string();

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

            self.status = "当前未登录".to_string();

            self.screen = Screen::Login;

            return;
        };

        let Some(course) = self.bykc.selected_course().cloned() else {

            self.status = "当前没有可报名的博雅课程".to_string();

            return;
        };

        if course.selected {

            self.status = "该课程已经报名".to_string();

            return;
        }

        self.busy = true;

        self.status = format!("报名中: {}", course.course_name);

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

            self.status = "当前未登录".to_string();

            self.screen = Screen::Login;

            return;
        };

        let Some(course) = self.bykc.selected_chosen_course().cloned() else {

            self.status = "当前没有可退选的博雅课程".to_string();

            return;
        };

        if !can_deselect_bykc_course(&course.course_cancel_end_date) {

            self.status = "当前课程已超过退选时间".to_string();

            return;
        }

        self.busy = true;

        self.status = format!("退选中: {}", course.course_name);

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

            self.status = "当前未登录".to_string();

            self.screen = Screen::Login;

            return;
        };

        let Some(course) = self.bykc.selected_course().cloned() else {

            self.status = "当前没有可退选的博雅课程".to_string();

            return;
        };

        if !course.selected {

            self.status = "当前课程尚未报名，无法退选".to_string();

            return;
        }

        let Some(chosen) = self.bykc.chosen_course_for(course.id).cloned() else {

            self.status = "当前课程已标记为已报，但未找到对应已选记录，请先刷新".to_string();

            return;
        };

        if !can_deselect_bykc_course(&chosen.course_cancel_end_date) {

            self.status = "当前课程已超过退选时间".to_string();

            return;
        }

        self.busy = true;

        self.status = format!("退选中: {}", course.course_name);

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

            self.status = "当前未登录".to_string();

            self.screen = Screen::Login;

            return;
        };

        let action_label = if action == BykcSignAction::SignIn {

            "签到"
        } else {

            "签退"
        };

        let Some(course) = self.bykc.selected_chosen_course().cloned() else {

            self.status = format!("当前没有可{action_label}的博雅课程");

            return;
        };

        let (allowed, pending_status) = match action {
            BykcSignAction::SignIn => (course.can_sign, "当前课程不在可签到状态"),
            BykcSignAction::SignOut => (course.can_sign_out, "当前课程不在可签退状态"),
        };

        if !allowed {

            self.status = pending_status.to_string();

            return;
        }

        self.busy = true;

        self.status = format!("博雅{action_label}中: {}", course.course_name);

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

            self.status = "当前未登录".to_string();

            self.screen = Screen::Login;

            return;
        };

        let Some(course) = self.selected_course().cloned() else {

            self.status = "当前没有可签到课程".to_string();

            return;
        };

        if course.signed() {

            self.status = "该课程已签到".to_string();

            return;
        }

        if course.course_sched_id.trim().is_empty() {

            self.status = "当前课程缺少 courseSchedId，无法签到".to_string();

            return;
        }

        self.busy = true;

        self.status = format!("签到中: {}", course.name);

        spawn_sign(session, course.course_sched_id, tx.clone());
    }

    fn logout(&mut self) {

        self.screen = Screen::Login;

        self.active_tab = WorkspaceTab::IClass;

        self.session = None;

        self.courses.clear();

        self.week_groups.clear();

        self.selected_week = 0;

        self.selected = 0;

        self.bykc = BykcState::default();

        self.busy = false;

        self.clear_qr();

        self.login_diagnostic = None;

        self.show_login_details = false;

        self.doctor_report = None;

        self.show_doctor_details = false;

        self.status = "已退出登录".to_string();
    }

    fn toggle_qr(&mut self) {

        if self.qr_refreshing && self.qr_mode == QrMode::Terminal {

            self.clear_qr();

            self.status = "已关闭二维码刷新".to_string();

            return;
        }

        match self.refresh_qr_inline() {
            Ok(()) => {

                self.qr_refreshing = true;

                self.qr_mode = QrMode::Terminal;

                self.next_qr_refresh_at = Some(Instant::now() + Duration::from_secs(2));

                self.status = "二维码刷新中，按 g 关闭".to_string();
            }
            Err(error) => {

                self.status = format!("二维码生成失败: {error}");

                self.clear_qr();
            }
        }
    }

    fn toggle_external_qr(&mut self) {

        if self.qr_refreshing && self.qr_mode == QrMode::External {

            self.clear_qr();

            self.status = "已关闭外部二维码刷新，浏览器页面可手动关闭".to_string();

            return;
        }

        match self.refresh_qr_inline() {
            Ok(()) => {

                self.qr_refreshing = true;

                self.qr_mode = QrMode::External;

                self.next_qr_refresh_at = Some(Instant::now() + Duration::from_secs(2));

                match self.open_external_qr_viewer() {
                    Ok(()) => {

                        self.status = "外部二维码刷新中，按 G 关闭刷新".to_string();
                    }
                    Err(error) => {

                        self.status = format!("外部二维码打开失败: {error}");

                        self.clear_qr();
                    }
                }
            }
            Err(error) => {

                self.status = format!("二维码生成失败: {error}");

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
            qr_url:          qr.qr_url,
            timestamp:       qr.timestamp,
        });

        if self.qr_mode == QrMode::External {

            self.write_external_qr_svg()?;
        }

        Ok(())
    }

    fn open_external_qr_viewer(&self) -> Result<(), String> {

        self.write_external_qr_svg()?;

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

    fn write_external_qr_html(&self) -> Result<PathBuf, String> {

        let dir = external_qr_dir()?;

        let path = dir.join("index.html");

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
    }
  </style>
</head>
<body>
  <main>
    <img id="qr" src="iclass-buaa-tui-qr.svg" alt="签到二维码">
    <div class="meta" id="meta">自动刷新中</div>
  </main>
  <script>
    const img = document.getElementById("qr");
    const meta = document.getElementById("meta");
    function refreshQr() {
      const now = new Date();
      img.src = "iclass-buaa-tui-qr.svg?t=" + now.getTime();
      meta.textContent = "刷新时间 " + now.toLocaleTimeString();
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

        self.qr_display = None;

        self.qr_refreshing = false;

        self.qr_mode = QrMode::Terminal;

        self.next_qr_refresh_at = None;
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

            self.status = format!("已切换到 {}", week.label);
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

            self.status = format!("已切换到 {}", week.label);
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

fn spawn_login(input: LoginInput, tx: UnboundedSender<AsyncEvent>) {

    tokio::spawn(async move {

        let result = match crate::iclass::IClassApi::new(input.use_vpn) {
            Ok(api) => {
                match api.login_with_diagnostic(&input).await {
                    Ok(session) => {
                        api.get_merged_course_details(&session, 7)
                            .await
                            .map(|courses| LoginSuccess { session, courses })
                            .map_err(|error| {
                                LoginFailure {
                                    message:    format_anyhow_error(error),
                                    diagnostic: LoginDiagnostic {
                                        kind:        crate::model::LoginFailureKind::IclassApi,
                                        stage:       "course_prefetch".to_string(),
                                        summary:     "登录成功，但拉取课程失败".to_string(),
                                        error_chain: vec!["登录成功，但拉取课程失败".to_string()],
                                        final_url:   None,
                                        http_status: None,
                                        page_hint:   None,
                                        suggestions: vec!["按 r 重试刷新课程".to_string()],
                                    },
                                }
                            })
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
