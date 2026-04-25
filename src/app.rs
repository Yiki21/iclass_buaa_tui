//! Application state, async event routing, and keyboard-driven TUI behavior.

use chrono::{Datelike, Duration as ChronoDuration, Local, NaiveDate};
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::style::{Color, Modifier, Style};
use std::collections::HashMap;
use std::future::Future;
use std::time::{Duration, Instant};
use tokio::sync::mpsc::UnboundedSender;

use crate::bykc::{
    BykcApi, BykcChosenCourse, BykcCourse, BykcCourseDetail, can_deselect_bykc_course,
};
use crate::model::{CourseDetailItem, LoginInput, Session, SignOutcome};

#[derive(Clone, Debug)]
pub enum AsyncEvent {
    Login(Result<LoginSuccess, String>),
    Refresh(Result<Vec<CourseDetailItem>, String>),
    Sign(Result<SignOutcome, String>),
    BykcSync(Box<Result<BykcSyncSuccess, String>>),
    VersionCheck(Result<VersionInfo, String>),
}

#[derive(Clone, Debug)]
pub struct LoginSuccess {
    pub session: Session,
    pub courses: Vec<CourseDetailItem>,
}

#[derive(Clone, Debug)]
pub struct QrDisplay {
    pub course_sched_id: String,
    pub qr_url: String,
    pub timestamp: i64,
}

#[derive(Clone, Debug)]
pub struct VersionInfo {
    pub current: String,
    pub latest: String,
    pub latest_url: String,
    pub is_latest: bool,
}

#[derive(Clone, Debug)]
pub struct WeekGroup {
    pub key: String,
    pub label: String,
    pub start_date: String,
    pub end_date: String,
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
}

/// Login form state for the TUI login screen.
#[derive(Clone, Debug, Default)]
pub struct LoginForm {
    pub student_id: String,
    pub use_vpn: bool,
    pub vpn_username: String,
    pub vpn_password: String,
    pub focus: usize,
}

impl LoginForm {
    /// Returns the fields currently visible to the user.
    pub fn visible_focuses(&self) -> Vec<LoginFocus> {
        let mut fields = vec![LoginFocus::UseVpn];
        if !self.use_vpn {
            fields.insert(0, LoginFocus::StudentId);
        }
        if self.use_vpn {
            fields.push(LoginFocus::VpnUsername);
            fields.push(LoginFocus::VpnPassword);
        }
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
            student_id: if self.use_vpn && student_id.is_empty() {
                vpn_username.to_string()
            } else {
                student_id.to_string()
            },
            use_vpn: self.use_vpn,
            vpn_username: vpn_username.to_string(),
            vpn_password: self.vpn_password.clone(),
        }
    }
}

#[derive(Clone, Debug, Default)]
pub struct BykcState {
    pub view: BykcView,
    pub include_all: bool,
    pub loaded: bool,
    pub courses: Vec<BykcCourse>,
    pub selected_course: usize,
    pub chosen_courses: Vec<BykcChosenCourse>,
    pub selected_chosen: usize,
    pub detail: Option<BykcCourseDetail>,
    pub detail_cache: HashMap<i64, BykcCourseDetail>,
    pub detail_course_id: Option<i64>,
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
    pub courses: Vec<BykcCourse>,
    pub chosen_courses: Vec<BykcChosenCourse>,
    pub detail: Option<BykcCourseDetail>,
    pub message: Option<String>,
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
    pub screen: Screen,
    pub active_tab: WorkspaceTab,
    pub login: LoginForm,
    pub session: Option<Session>,
    pub courses: Vec<CourseDetailItem>,
    pub week_groups: Vec<WeekGroup>,
    pub selected_week: usize,
    pub selected: usize,
    pub bykc: BykcState,
    pub status: String,
    pub busy: bool,
    pub should_quit: bool,
    pub show_help: bool,
    pub qr_display: Option<QrDisplay>,
    pub qr_refreshing: bool,
    pub version_info: Option<VersionInfo>,
    pub version_error: Option<String>,
    next_qr_refresh_at: Option<Instant>,
}

impl Default for App {
    fn default() -> Self {
        Self {
            screen: Screen::Login,
            active_tab: WorkspaceTab::IClass,
            login: LoginForm::default(),
            session: None,
            courses: Vec::new(),
            week_groups: Vec::new(),
            selected_week: 0,
            selected: 0,
            bykc: BykcState::default(),
            status: "直连模式输入学号；VPN 模式输入 VPN 账号密码后按 enter 登录".to_string(),
            busy: false,
            should_quit: false,
            show_help: false,
            qr_display: None,
            qr_refreshing: false,
            version_info: None,
            version_error: None,
            next_qr_refresh_at: None,
        }
    }
}

impl App {
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
            AsyncEvent::Login(result) => match result {
                Ok(data) => {
                    self.screen = Screen::Workspace;
                    self.active_tab = WorkspaceTab::IClass;
                    self.session = Some(data.session);
                    self.replace_courses(data.courses, None, None);
                    self.bykc = BykcState::default();
                    self.clear_qr();
                    self.status = "登录成功。tab 切换 iClass / BYKC，iClass 内 h/j/k/l 移动，s 直接签到，g 二维码签到，r 刷新，Shift+X 退出登录".to_string();
                }
                Err(error) => {
                    self.status = format!("登录失败: {error}");
                }
            },
            AsyncEvent::Refresh(result) => match result {
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
            },
            AsyncEvent::Sign(result) => match result {
                Ok(outcome) => {
                    self.status = if outcome.success_like {
                        "签到成功".to_string()
                    } else {
                        format!("签到结果: {}", outcome.message)
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
            },
            AsyncEvent::BykcSync(result) => match *result {
                Ok(data) => {
                    let course_count = data.courses.len();
                    let chosen_count = data.chosen_courses.len();
                    let message = data.message.unwrap_or_else(|| {
                        format!(
                            "博雅数据已加载，可选 {} 门，已选 {} 门",
                            course_count, chosen_count
                        )
                    });
                    self.bykc
                        .replace_data(data.courses, data.chosen_courses, data.detail);
                    self.bykc.show_detail_popup = data.open_detail_popup;
                    self.status = message;
                }
                Err(error) => {
                    self.status = format!("博雅操作失败: {error}");
                }
            },
            AsyncEvent::VersionCheck(result) => match result {
                Ok(info) => {
                    self.version_error = None;
                    self.version_info = Some(info);
                }
                Err(error) => {
                    self.version_info = None;
                    self.version_error = Some(error);
                }
            },
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
            KeyCode::Char(' ') if self.login.current_focus() == LoginFocus::UseVpn => {
                self.login.use_vpn = !self.login.use_vpn;
                self.login.reset_focus_bounds();
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
                self.sign_in_bykc_course(tx)
            }
            KeyCode::Char('u') if self.bykc.view == BykcView::Chosen => {
                self.sign_out_bykc_course(tx)
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
            LoginFocus::UseVpn => {}
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
        self.status = "登录中并拉取课程...".to_string();
        spawn_login(input, tx.clone());
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
        spawn_bykc_select(session, self.bykc.include_all, course.id, tx.clone());
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
        spawn_bykc_deselect(session, self.bykc.include_all, course.course_id, tx.clone());
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
        spawn_bykc_deselect(session, self.bykc.include_all, course.id, tx.clone());
    }

    fn sign_in_bykc_course(&mut self, tx: &UnboundedSender<AsyncEvent>) {
        let Some(session) = self.session.clone() else {
            self.status = "当前未登录".to_string();
            self.screen = Screen::Login;
            return;
        };
        let Some(course) = self.bykc.selected_chosen_course().cloned() else {
            self.status = "当前没有可签到的博雅课程".to_string();
            return;
        };
        if !course.can_sign {
            self.status = "当前课程不在可签到状态".to_string();
            return;
        }

        self.busy = true;
        self.status = format!("博雅签到中: {}", course.course_name);
        spawn_bykc_sign(
            session,
            self.bykc.include_all,
            course.course_id,
            1,
            tx.clone(),
        );
    }

    fn sign_out_bykc_course(&mut self, tx: &UnboundedSender<AsyncEvent>) {
        let Some(session) = self.session.clone() else {
            self.status = "当前未登录".to_string();
            self.screen = Screen::Login;
            return;
        };
        let Some(course) = self.bykc.selected_chosen_course().cloned() else {
            self.status = "当前没有可签退的博雅课程".to_string();
            return;
        };
        if !course.can_sign_out {
            self.status = "当前课程不在可签退状态".to_string();
            return;
        }

        self.busy = true;
        self.status = format!("博雅签退中: {}", course.course_name);
        spawn_bykc_sign(
            session,
            self.bykc.include_all,
            course.course_id,
            2,
            tx.clone(),
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
        self.status = "已退出登录".to_string();
    }

    fn toggle_qr(&mut self) {
        if self.qr_refreshing {
            self.clear_qr();
            self.status = "已关闭二维码刷新".to_string();
            return;
        }

        match self.refresh_qr_inline() {
            Ok(()) => {
                self.qr_refreshing = true;
                self.next_qr_refresh_at = Some(Instant::now() + Duration::from_secs(2));
                self.status = "二维码刷新中，按 g 关闭".to_string();
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
            qr_url: qr.qr_url,
            timestamp: qr.timestamp,
        });

        Ok(())
    }

    fn clear_qr(&mut self) {
        self.qr_display = None;
        self.qr_refreshing = false;
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

        let target_date = current_date + ChronoDuration::days(delta_days);
        if target_date < week_start || target_date > week_start + ChronoDuration::days(6) {
            return;
        }

        let target_key = target_date.format("%Y-%m-%d").to_string();
        let target_courses = self.day_course_absolute_indices(&target_key);
        if target_courses.is_empty() {
            return;
        }

        let target_row = current_row.min(target_courses.len().saturating_sub(1));
        self.set_selected_absolute(target_courses[target_row]);
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
        let result = async {
            let api = crate::iclass::IClassApi::new(input.use_vpn)?;
            let session = api.login(&input).await?;
            let courses = api.get_merged_course_details(&session, 7).await?;
            Ok::<LoginSuccess, anyhow::Error>(LoginSuccess { session, courses })
        }
        .await
        .map_err(|error| error.to_string());

        let _ = tx.send(AsyncEvent::Login(result));
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
            .map_err(|error| error.to_string());
        let _ = tx.send(AsyncEvent::Refresh(result));
    });
}

fn spawn_sign(session: Session, course_sched_id: String, tx: UnboundedSender<AsyncEvent>) {
    tokio::spawn(async move {
        let result = session
            .api
            .sign_now(&session, &course_sched_id)
            .await
            .map_err(|error| error.to_string());
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

fn spawn_bykc_select(
    session: Session,
    include_all: bool,
    course_id: i64,
    tx: UnboundedSender<AsyncEvent>,
) {
    spawn_bykc_task(
        session,
        include_all,
        BykcDetailTarget::CourseFirst(course_id),
        false,
        false,
        tx,
        move |api| async move { api.select_course(course_id).await.map(Some) },
    );
}

fn spawn_bykc_deselect(
    session: Session,
    include_all: bool,
    course_id: i64,
    tx: UnboundedSender<AsyncEvent>,
) {
    spawn_bykc_task(
        session,
        include_all,
        BykcDetailTarget::ChosenFirst(course_id),
        false,
        false,
        tx,
        move |api| async move { api.deselect_course(course_id).await.map(Some) },
    );
}

fn spawn_bykc_sign(
    session: Session,
    include_all: bool,
    course_id: i64,
    sign_type: i32,
    tx: UnboundedSender<AsyncEvent>,
) {
    spawn_bykc_task(
        session,
        include_all,
        BykcDetailTarget::ChosenFirst(course_id),
        false,
        false,
        tx,
        move |api| async move {
            if sign_type == 1 {
                api.sign_in(course_id).await.map(Some)
            } else {
                api.sign_out(course_id).await.map(Some)
            }
        },
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
        .map_err(|error| error.to_string());

        let _ = tx.send(AsyncEvent::BykcSync(Box::new(result)));
    });
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
        BykcDetailTarget::CourseFirst(course_id) => courses
            .iter()
            .find(|course| course.id == course_id)
            .map(|course| course.id)
            .or_else(|| {
                chosen_courses
                    .iter()
                    .find(|course| course.course_id == course_id)
                    .map(|course| course.course_id)
            }),
        BykcDetailTarget::ChosenFirst(course_id) => chosen_courses
            .iter()
            .find(|course| course.course_id == course_id)
            .map(|course| course.course_id),
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
        current: current.to_string(),
        latest: normalize_version(latest).to_string(),
        latest_url: latest_url.to_string(),
        is_latest: compare_version(current, latest) >= 0,
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

fn clamp_step(current: usize, len: usize, delta: isize) -> usize {
    if len == 0 {
        return 0;
    }

    let next = current as isize + delta;
    next.clamp(0, len.saturating_sub(1) as isize) as usize
}
