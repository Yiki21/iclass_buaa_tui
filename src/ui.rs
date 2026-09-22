//! Pure rendering code for the login screen, iClass workspace, and BYKC views.

use chrono::{Duration, Local, NaiveDate, TimeZone};
use qrcode::{EcLevel, QrCode};
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap},
};
use tui_qrcode::{QrCodeWidget, QuietZone, Scaling};

use crate::app::{App, BykcView, CourseView, EventLevel, LoginFocus, QrMode, Screen, WorkspaceTab};
use crate::bykc::can_deselect_bykc_course;
use crate::theme;

const QR_MAX_MODULE_SCALE: u16 = 1;

/// Renders the whole frame, then overlays transient popups in a fixed z-order.
///
/// Why:
/// Busy, QR, detail, and help overlays can coexist conceptually, so the top-level
/// renderer keeps their ordering explicit instead of scattering popup decisions
/// across the screen-specific renderers.

pub fn render(frame: &mut Frame, app: &App) {

    match app.screen {
        Screen::Login => render_login(frame, app),
        Screen::Workspace => render_workspace(frame, app),
    }

    if app.busy {

        render_busy_popup(frame, app);
    } else if app.show_event_log {

        render_event_log_popup(frame, app);
    } else if app.screen == Screen::Login && app.show_login_details {

        render_login_diagnostic_popup(frame, app);
    } else if app.screen == Screen::Login && app.show_doctor_details {

        render_doctor_popup(frame, app);
    } else if app.active_tab == WorkspaceTab::IClass
        && app.qr_display.is_some()
        && app.qr_mode == QrMode::Terminal
    {

        render_qr_popup(frame, app);
    } else if app.active_tab == WorkspaceTab::Bykc && app.bykc.show_detail_popup {

        render_bykc_detail_popup(frame, app);
    }

    if app.show_help {

        render_help_popup(frame, app);
    }
}

fn render_login(frame: &mut Frame, app: &App) {

    let area = frame.area();

    frame.render_widget(Clear, area);

    let outer = Block::default()
        .title("BUAA Rust TUI")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::ACCENT));

    let inner = outer.inner(area);

    frame.render_widget(outer, area);

    let mut constraints = vec![Constraint::Length(5), Constraint::Length(3)];

    constraints.push(Constraint::Length(3));

    constraints.push(Constraint::Length(3));

    if app.login.captcha_required {

        constraints.push(Constraint::Length(3));

        constraints.push(Constraint::Length(4));
    }

    constraints.push(Constraint::Length(3));

    constraints.push(Constraint::Length(6));

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints(constraints)
        .split(inner);

    let title = Paragraph::new(vec![
        Line::from(theme::gradient_text(
            "Controll Your Campus Life In Terminal",
            0.0,
        )),
        Line::from(Span::styled(app.version_text(), app.version_style())),
        Line::from(Span::styled(
            "登录后可在课表、iClass 与 BYKC 间切换",
            theme::text_style(),
        )),
        Line::from(Span::styled(
            "直连与 VPN 模式均使用统一认证；VPN 仅改变访问路径",
            theme::muted_style(),
        )),
        Line::from(Span::styled(
            "tab 切换字段，space 切换选项，enter 登录，D 自检，v 失败详情，? 帮助，q 退出",
            theme::label_style(),
        )),
    ]);

    frame.render_widget(title, chunks[0]);

    render_input(
        frame,
        chunks[1],
        "VPN 模式",
        if app.login.use_vpn {

            "开启"
        } else {

            "关闭"
        },
        app.login.current_focus() == LoginFocus::UseVpn,
        false,
    );

    let mut next_index = if app.login.use_vpn {

        render_input(
            frame,
            chunks[2],
            "统一认证账号",
            &app.login.vpn_username,
            app.login.current_focus() == LoginFocus::VpnUsername,
            false,
        );

        3
    } else {

        render_input(
            frame,
            chunks[2],
            "学号",
            &app.login.student_id,
            app.login.current_focus() == LoginFocus::StudentId,
            false,
        );

        3
    };

    render_input(
        frame,
        chunks[next_index],
        "统一认证密码",
        &mask_password(&app.login.vpn_password),
        app.login.current_focus() == LoginFocus::VpnPassword,
        true,
    );

    next_index += 1;

    if app.login.captcha_required {

        render_input(
            frame,
            chunks[next_index],
            "验证码",
            &app.login.captcha,
            app.login.current_focus() == LoginFocus::Captcha,
            false,
        );

        next_index += 1;

        let captcha_hint = app
            .pending_captcha_login
            .as_ref()
            .map(|pending| {

                format!(
                    "验证码图片: {} | 输入后按 enter 继续同一登录会话",
                    pending.challenge.captcha_path
                )
            })
            .unwrap_or_else(|| "验证码状态已失效，请重新登录".to_string());

        let hint = Paragraph::new(captcha_hint)
            .block(Block::default().title("验证码").borders(Borders::ALL))
            .wrap(Wrap { trim: true });

        frame.render_widget(hint, chunks[next_index]);

        next_index += 1;
    }

    render_input(
        frame,
        chunks[next_index],
        "记住我",
        if app.login.remember_me {

            "开启"
        } else {

            "关闭"
        },
        app.login.current_focus() == LoginFocus::RememberMe,
        false,
    );

    let status_index = next_index + 1;

    render_event_log_block(frame, chunks[status_index], app);
}

/// Renders the shared workspace shell before delegating to the active tab body.

fn render_workspace(frame: &mut Frame, app: &App) {

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(10),
        ])
        .split(frame.area());

    render_workspace_tabs(frame, chunks[0], app);

    render_workspace_hint(frame, chunks[1], app);

    match app.active_tab {
        WorkspaceTab::Schedule => render_schedule(frame, chunks[2], app),
        WorkspaceTab::IClass => render_iclass(frame, chunks[2], app),
        WorkspaceTab::Bykc => render_bykc(frame, chunks[2], app),
    }
}

fn render_workspace_tabs(frame: &mut Frame, area: Rect, app: &App) {

    let schedule_title = app.schedule.portal_label();

    let titles = [schedule_title, " iClass ", " BYKC "]
        .into_iter()
        .map(Line::from)
        .collect::<Vec<_>>();

    let selected = match app.active_tab {
        WorkspaceTab::Schedule => 0,
        WorkspaceTab::IClass => 1,
        WorkspaceTab::Bykc => 2,
    };

    // A slow-scrolling gradient title is the app's signature strip. It moves
    // gently at all times so the terminal never reads as frozen, while all
    // other motion is reserved for real activity.
    let tabs = Tabs::new(titles)
        .block(
            Block::default()
                .title(Line::from(theme::gradient_text(
                    " iClass BUAA ",
                    (app.tick as f32 / 120.0).rem_euclid(1.0),
                )))
                .title_bottom(Line::from(Span::styled(
                    " tab / shift+tab 切换 ",
                    theme::muted_style(),
                )))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER_FOCUS)),
        )
        .select(selected)
        .style(theme::muted_style())
        .highlight_style(theme::selection_style());

    frame.render_widget(tabs, area);
}

fn render_workspace_hint(frame: &mut Frame, area: Rect, app: &App) {

    let current = match app.active_tab {
        WorkspaceTab::Schedule => app.schedule.portal_label(),
        WorkspaceTab::IClass => "iClass",
        WorkspaceTab::Bykc => "BYKC",
    };

    let hint = Paragraph::new(vec![
        Line::from(format!(
            "当前页: {current} | tab: 下一个标签 | shift+tab: 上一个标签 | e: 事件日志 | ?: 帮助"
        )),
        Line::from(Span::styled(app.version_text(), app.version_style())),
    ])
    .block(Block::default().title("切换提示").borders(Borders::ALL))
    .wrap(Wrap { trim: true });

    frame.render_widget(hint, area);
}

fn render_schedule(frame: &mut Frame, area: Rect, app: &App) {

    match app.schedule.view {
        CourseView::Today => return render_today_courses(frame, area, app),
        CourseView::Exams => return render_exams(frame, area, app),
        CourseView::Grades => return render_grades(frame, area, app),
        CourseView::Classrooms => return render_classrooms(frame, area, app),
        CourseView::Tasks => return render_tasks(frame, area, app),
        CourseView::Schedule => {}
    }

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(8),
            Constraint::Length(6),
        ])
        .split(area);

    let account = app.schedule.account.as_deref().unwrap_or("未登录");

    let semester = app.schedule.current_semester();

    let (state_text, state_style) = if app.schedule.updating {

        ("正在更新", Style::default().fg(theme::WARN))
    } else if semester.is_some() {

        ("离线可读", Style::default().fg(theme::OK))
    } else {

        ("尚未导入", theme::muted_style())
    };

    let portal_style = match app.schedule.portal() {
        crate::schedule::PortalKind::Graduate => Style::default().fg(theme::INFO),
        crate::schedule::PortalKind::Undergraduate => Style::default().fg(theme::OK),
        crate::schedule::PortalKind::Unknown => theme::muted_style(),
    };

    let header = Paragraph::new(Line::from(vec![
        Span::styled("\u{8d26}\u{53f7}: ", theme::label_style()),
        Span::styled(account.to_string(), theme::text_style()),
        Span::styled("  |  \u{5b66}\u{671f}: ", theme::label_style()),
        Span::styled(
            semester
                .map(|item| item.term_code.clone())
                .unwrap_or_else(|| "\u{672a}\u{5bfc}\u{5165}".to_string()),
            Style::default().fg(theme::INFO),
        ),
        Span::styled("  |  \u{95e8}\u{6237}: ", theme::label_style()),
        Span::styled(app.schedule.portal_label(), portal_style),
        Span::raw("  |  "),
        // Spinner appears only while an import is actually in flight.
        theme::activity_badge_styled(app.tick, app.schedule.updating, state_text, state_style),
    ]))
    .block(
        Block::default()
            .title(app.schedule.portal_label())
            .borders(Borders::ALL)
            .border_style(Style::default().fg(if app.schedule.updating {

                theme::ramp((app.tick as f32 / 24.0).rem_euclid(1.0))
            } else {

                theme::BORDER_FOCUS
            }))
            .title_style(theme::title_style()),
    );

    frame.render_widget(header, vertical[0]);

    let week = app.schedule.current_week();

    let controls = Paragraph::new(format!(
        "学期: {} | 周次: {} | {} | ,/. 切学期 | [ ] 或 h/l 切周 | u 更新整学期",
        app.schedule
            .current_schedule()
            .map(|item| item.term_name.as_str())
            .or_else(|| semester.map(|item| item.term_code.as_str()))
            .unwrap_or("无缓存"),
        week.map(|item| item.name.as_str()).unwrap_or("无周次"),
        week.map(|item| format!("{} ~ {}", item.start_date, item.end_date))
            .unwrap_or_else(|| "无日期".to_string()),
    ))
    .block(
        Block::default()
            .title("课表操作")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::BORDER_IDLE))
            .title_style(theme::title_style()),
    )
    .wrap(Wrap { trim: true });

    frame.render_widget(controls, vertical[1]);

    let schedule = app.schedule.current_schedule();

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 7); 7])
        .split(vertical[2]);

    let labels = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];

    for (day, column) in columns.iter().enumerate() {

        let entries = schedule
            .map(|item| {

                item.entries
                    .iter()
                    .enumerate()
                    .filter(|(_, entry)| entry.day_of_week == Some(day + 1))
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();

        let items = if entries.is_empty() {

            vec![ListItem::new("-")]
        } else {

            entries
                .iter()
                .map(|(index, entry)| {

                    let label = format!(
                        "{}-{}\n{}\n{}",
                        entry.begin_time.as_deref().unwrap_or("--:--"),
                        entry.end_time.as_deref().unwrap_or("--:--"),
                        entry.course_name,
                        entry.place.as_deref().unwrap_or("未填写地点"),
                    );

                    let style = if Some(*index) == Some(app.schedule.selected_entry) {

                        theme::selection_style()
                    } else {

                        Style::default()
                    };

                    ListItem::new(label).style(style)
                })
                .collect()
        };

        let list = List::new(items)
            .block(Block::default().title(labels[day]).borders(Borders::ALL))
            .highlight_symbol("");

        frame.render_widget(list, *column);
    }

    let detail = if let Some(entry) = app.schedule.selected_entry() {

        Paragraph::new(vec![
            Line::from(format!(
                "课程: {} ({})",
                entry.course_name, entry.course_code
            )),
            Line::from(format!(
                "时间: {} - {} | 节次: {}-{}",
                entry.begin_time.as_deref().unwrap_or("--:--"),
                entry.end_time.as_deref().unwrap_or("--:--"),
                entry
                    .begin_section
                    .map(|value| value.to_string())
                    .as_deref()
                    .unwrap_or("-"),
                entry
                    .end_section
                    .map(|value| value.to_string())
                    .as_deref()
                    .unwrap_or("-")
            )),
            Line::from(format!(
                "地点: {} | 教师: {}",
                entry.place.as_deref().unwrap_or("未填写"),
                entry.weeks_and_teachers.as_deref().unwrap_or("未填写")
            )),
            Line::from("课表只读；按 u 手动导入整学期，更新失败不会覆盖旧缓存"),
        ])
    } else {

        Paragraph::new("没有当前周课程。首次使用请按 u 更新整学期课表")
    };

    frame.render_widget(
        detail
            .block(
                Block::default()
                    .title("课程详情")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme::BORDER_IDLE))
                    .title_style(theme::title_style()),
            )
            .wrap(Wrap { trim: true }),
        vertical[3],
    );

    render_event_log_block(frame, vertical[4], app);
}

fn render_today_courses(frame: &mut Frame, area: Rect, app: &App) {

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(8),
            Constraint::Length(6),
        ])
        .split(area);

    let today = Local::now().date_naive();

    let entries = app.schedule.today_entries();

    let title = if app.schedule.filtering {

        format!("今日课程 | 搜索: {}_", app.schedule.query)
    } else if app.schedule.query.is_empty() {

        "今日课程 | 1 今日 2 课表 3 考试 4 成绩 5 空教室 6 作业 | / 搜索".to_string()
    } else {

        format!("今日课程 | 搜索: {} | / 修改", app.schedule.query)
    };

    // Sign-in progress across today's courses, drawn as a gradient bar.
    let signed = entries
        .iter()
        .filter(|(_, entry)| {

            app.courses
                .iter()
                .find(|course| course.name == entry.course_name)
                .is_some_and(crate::model::CourseDetailItem::signed)
        })
        .count();

    let mut summary_line = vec![
        Span::styled("日期: ", theme::label_style()),
        Span::styled(today.to_string(), Style::default().fg(theme::INFO)),
        Span::styled(format!("  |  共 {} 门", entries.len()), theme::text_style()),
        Span::styled("  |  签到 ", theme::label_style()),
    ];

    summary_line.extend(theme::progress_bar(signed, entries.len(), 12));

    summary_line.push(Span::styled(
        format!(" {signed}/{}", entries.len()),
        Style::default().fg(if signed == entries.len() && !entries.is_empty() {

            theme::OK
        } else {

            theme::TEXT
        }),
    ));

    if app.schedule.updating {

        summary_line.push(Span::raw("  "));

        summary_line.push(theme::activity_badge(app.tick, true, "更新中"));
    }

    let header = Paragraph::new(vec![
        Line::from(summary_line),
        Line::from(Span::styled(
            "j/k 选择课程 | r 刷新当前数据 | u 更新整学期课表 | tab 切换签到工作区",
            theme::muted_style(),
        )),
    ])
    .block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::BORDER_FOCUS))
            .title_style(theme::title_style()),
    )
    .wrap(Wrap { trim: true });

    frame.render_widget(header, chunks[0]);

    let items = if entries.is_empty() {

        vec![ListItem::new(if app.schedule.semesters.is_empty() {

            "暂无本地课表，请按 u 导入整学期课表"
        } else {

            "今天没有匹配课程"
        })]
    } else {

        entries
            .iter()
            .map(|(index, entry)| {

                let matched = app
                    .courses
                    .iter()
                    .find(|course| course.name == entry.course_name);

                let (status_text, status_style) = matched
                    .map(today_sign_state)
                    .unwrap_or(("未同步", theme::muted_style()));

                // Color the sign state separately from the row so the state stays
                // readable even when the row itself is selected.
                let row_style = if *index == app.schedule.selected_entry {

                    theme::selection_style()
                } else {

                    Style::default()
                };

                let line = Line::from(vec![
                    Span::styled(
                        format!(
                            "{}-{}  ",
                            entry.begin_time.as_deref().unwrap_or("--:--"),
                            entry.end_time.as_deref().unwrap_or("--:--"),
                        ),
                        theme::muted_style(),
                    ),
                    Span::styled(entry.course_name.clone(), theme::text_style()),
                    Span::styled(
                        format!("  [{}]", entry.place.as_deref().unwrap_or("未填写地点")),
                        Style::default().fg(theme::INFO),
                    ),
                    Span::styled(
                        format!(
                            "  {}",
                            entry.weeks_and_teachers.as_deref().unwrap_or("未填写教师")
                        ),
                        theme::muted_style(),
                    ),
                    Span::raw("  "),
                    Span::styled(status_text, status_style),
                ]);

                ListItem::new(line).style(row_style)
            })
            .collect()
    };

    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title("课程列表")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER_FOCUS))
                .title_style(theme::title_style()),
        ),
        chunks[1],
    );

    let detail = app
        .schedule
        .selected_entry()
        .map(|entry| {

            Paragraph::new(vec![
                Line::from(format!(
                    "课程: {} ({})",
                    entry.course_name, entry.course_code
                )),
                Line::from(format!(
                    "地点: {}",
                    entry.place.as_deref().unwrap_or("未填写")
                )),
                Line::from(format!(
                    "教师: {}",
                    entry.weeks_and_teachers.as_deref().unwrap_or("未填写")
                )),
                Line::from(format!(
                    "签到状态: {}",
                    app.courses
                        .iter()
                        .find(|course| course.name == entry.course_name)
                        .map(today_sign_status)
                        .unwrap_or("未同步")
                )),
            ])
        })
        .unwrap_or_else(|| Paragraph::new("未选择课程"));

    frame.render_widget(
        detail
            .block(
                Block::default()
                    .title("下一步")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme::BORDER_IDLE))
                    .title_style(theme::title_style()),
            )
            .wrap(Wrap { trim: true }),
        chunks[2],
    );

    render_event_log_block(frame, chunks[3], app);
}

/// Sign status text plus the color that carries its meaning.
///
/// Why:
/// This is the value a user scans for on the today list. Coloring it by state
/// lets them spot "can sign now" and "already ended" without reading each row.

fn today_sign_status(course: &crate::model::CourseDetailItem) -> &'static str {

    today_sign_state(course).0
}

fn today_sign_state(course: &crate::model::CourseDetailItem) -> (&'static str, Style) {

    if course.signed() {

        return ("已签到", Style::default().fg(theme::OK));
    }

    if course.course_sched_id.trim().is_empty() {

        return ("缺少签到 ID", Style::default().fg(theme::MUTED));
    }

    let now = Local::now().format("%H:%M").to_string();

    if !course.start_time.is_empty() && now < course.start_time {

        ("未开始", Style::default().fg(theme::INFO))
    } else if !course.end_time.is_empty() && now > course.end_time {

        ("已结束", Style::default().fg(theme::MUTED))
    } else {

        (
            "可签到",
            Style::default()
                .fg(theme::WARN)
                .add_modifier(Modifier::BOLD),
        )
    }
}

fn render_exams(frame: &mut Frame, area: Rect, app: &App) {

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(6),
        ])
        .split(area);

    let header = Paragraph::new(format!(
        "考试安排 | 1 今日 2 课表 3 考试 4 成绩 5 空教室 6 作业 | r 刷新 | 当前记录 {} 条",
        app.schedule.exams.len()
    ))
    .block(
        Block::default()
            .title("考试")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::BORDER_FOCUS))
            .title_style(theme::title_style()),
    )
    .wrap(Wrap { trim: true });

    frame.render_widget(header, chunks[0]);

    let items = if app.schedule.exams.is_empty() {

        vec![ListItem::new(if app.schedule.academic_loading {

            Line::from(vec![theme::activity_badge(
                app.tick,
                true,
                "正在拉取考试安排",
            )])
        } else {

            Line::from(Span::styled(
                "暂无考试数据，按 r 加载",
                theme::muted_style(),
            ))
        })]
    } else {

        app.schedule
            .exams
            .iter()
            .map(|exam| {

                ListItem::new(Line::from(vec![
                    Span::styled(
                        exam.exam_date.as_deref().unwrap_or("未定日期").to_string(),
                        Style::default().fg(theme::INFO),
                    ),
                    Span::raw("  "),
                    Span::styled(exam.course_name.clone(), theme::text_style()),
                    Span::styled(
                        format!(
                            " {}-{}",
                            exam.start_time.as_deref().unwrap_or("--:--"),
                            exam.end_time.as_deref().unwrap_or("--:--"),
                        ),
                        theme::muted_style(),
                    ),
                    Span::styled(
                        format!("  {}", exam.place.as_deref().unwrap_or("未定地点")),
                        theme::label_style(),
                    ),
                    Span::styled(
                        format!("  座位 {}", exam.seat.as_deref().unwrap_or("-")),
                        Style::default().fg(theme::OK),
                    ),
                ]))
            })
            .collect()
    };

    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title("考试列表")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER_IDLE)),
        ),
        chunks[1],
    );

    render_event_log_block(frame, chunks[2], app);
}

fn render_grades(frame: &mut Frame, area: Rect, app: &App) {

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(6),
        ])
        .split(area);

    let header = Paragraph::new(format!(
        "成绩查询 | 1 今日 2 课表 3 考试 4 成绩 5 空教室 6 作业 | r 刷新 | 当前记录 {} 条",
        app.schedule.grades.len()
    ))
    .block(
        Block::default()
            .title("成绩")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::BORDER_FOCUS))
            .title_style(theme::title_style()),
    )
    .wrap(Wrap { trim: true });

    frame.render_widget(header, chunks[0]);

    let items = if app.schedule.grades.is_empty() {

        vec![ListItem::new(if app.schedule.academic_loading {

            Line::from(vec![theme::activity_badge(app.tick, true, "正在拉取成绩")])
        } else {

            Line::from(Span::styled(
                "暂无成绩数据，按 r 加载",
                theme::muted_style(),
            ))
        })]
    } else {

        app.schedule
            .grades
            .iter()
            .map(|grade| {

                let score = grade.score.as_deref().unwrap_or("-").trim();

                // Emphasize the score by band so a failing grade is visible at a glance.
                let score_style = match score.parse::<f64>() {
                    Ok(value) if value < 60.0 => {
                        Style::default()
                            .fg(theme::ERROR)
                            .add_modifier(Modifier::BOLD)
                    }
                    Ok(value) if value >= 85.0 => {
                        Style::default().fg(theme::OK).add_modifier(Modifier::BOLD)
                    }
                    Ok(_) => Style::default().fg(theme::WARN),
                    Err(_) => theme::muted_style(),
                };

                ListItem::new(Line::from(vec![
                    Span::styled(grade.course_name.clone(), theme::text_style()),
                    Span::styled("  成绩: ", theme::label_style()),
                    Span::styled(score.to_string(), score_style),
                    Span::styled(
                        format!(
                            "  学分: {}",
                            grade
                                .credit
                                .map(|value| value.to_string())
                                .unwrap_or_else(|| "-".to_string()),
                        ),
                        theme::muted_style(),
                    ),
                    Span::styled(
                        format!("  绩点: {}", grade.grade_point.as_deref().unwrap_or("-")),
                        Style::default().fg(theme::INFO),
                    ),
                    Span::styled(
                        format!("  {}", grade.passed.as_deref().unwrap_or("-")),
                        theme::muted_style(),
                    ),
                ]))
            })
            .collect()
    };

    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title("成绩列表")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER_IDLE)),
        ),
        chunks[1],
    );

    render_event_log_block(frame, chunks[2], app);
}

fn render_classrooms(frame: &mut Frame, area: Rect, app: &App) {

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(6),
        ])
        .split(area);

    let header = Paragraph::new(format!(
        "空教室 | 1 今日 2 课表 3 考试 4 成绩 5 空教室 6 作业 | r 刷新 | 校区 {} | 日期 {}",
        app.schedule.classroom_campus, app.schedule.classroom_date
    ))
    .block(
        Block::default()
            .title("空教室")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::BORDER_FOCUS))
            .title_style(theme::title_style()),
    )
    .wrap(Wrap { trim: true });

    frame.render_widget(header, chunks[0]);

    let items = if app.schedule.classrooms.is_empty() {

        vec![ListItem::new(if app.schedule.academic_loading {

            "加载空教室中..."
        } else {

            "暂无空教室数据，按 r 加载"
        })]
    } else {

        app.schedule
            .classrooms
            .iter()
            .map(|room| {

                ListItem::new(Line::from(vec![
                    Span::styled(room.building.clone(), theme::label_style()),
                    Span::raw(" "),
                    Span::styled(room.name.clone(), theme::text_style()),
                    Span::styled("  空闲节次: ", theme::muted_style()),
                    Span::styled(
                        room.free_sections
                            .iter()
                            .map(ToString::to_string)
                            .collect::<Vec<_>>()
                            .join(","),
                        Style::default().fg(theme::OK),
                    ),
                ]))
            })
            .collect()
    };

    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title("教室列表")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER_IDLE)),
        ),
        chunks[1],
    );

    render_event_log_block(frame, chunks[2], app);
}

fn render_tasks(frame: &mut Frame, area: Rect, app: &App) {

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(4),
            Constraint::Min(8),
            Constraint::Length(6),
        ])
        .split(area);

    let header = Paragraph::new(format!(
        "作业 | 1 今日 2 课表 3 考试 4 成绩 5 空教室 6 作业 | r 刷新 | 当前 {} 条",
        app.schedule.tasks.len()
    ))
    .block(
        Block::default()
            .title("作业")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::BORDER_FOCUS))
            .title_style(theme::title_style()),
    )
    .wrap(Wrap { trim: true });

    frame.render_widget(header, chunks[0]);

    let items = if app.schedule.tasks.is_empty() {

        vec![ListItem::new(if app.schedule.academic_loading {

            Line::from(vec![theme::activity_badge(app.tick, true, "正在拉取作业")])
        } else {

            Line::from(Span::styled(
                "暂无作业数据，按 r 加载",
                theme::muted_style(),
            ))
        })]
    } else {

        app.schedule
            .tasks
            .iter()
            .map(|task| {

                let status_style =
                    if task.status.contains("未提交") || task.status.contains("未作答") {

                        Style::default()
                            .fg(theme::ERROR)
                            .add_modifier(Modifier::BOLD)
                    } else if task.status.contains("已提交") {

                        Style::default().fg(theme::OK)
                    } else {

                        theme::muted_style()
                    };

                ListItem::new(Line::from(vec![
                    Span::styled(task.course_name.clone(), theme::label_style()),
                    Span::raw("  "),
                    Span::styled(task.title.clone(), theme::text_style()),
                    Span::styled(
                        format!("  截止 {}", task.due_time.as_deref().unwrap_or("未定")),
                        Style::default().fg(theme::INFO),
                    ),
                    Span::styled(
                        format!("  得分 {}", task.score.as_deref().unwrap_or("-")),
                        theme::muted_style(),
                    ),
                    Span::raw("  "),
                    Span::styled(task.status.clone(), status_style),
                ]))
            })
            .collect()
    };

    frame.render_widget(
        List::new(items).block(
            Block::default()
                .title("作业列表")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER_IDLE)),
        ),
        chunks[1],
    );

    render_event_log_block(frame, chunks[2], app);
}

/// Renders the iClass weekly grid plus the selected-course detail panel.
///
/// How:
/// The layout is split into session info, week info, seven day columns, one
/// detail block, and one status block. Selection highlighting is derived from
/// the absolute selected course index so horizontal and vertical navigation stay
/// aligned with the same underlying flat course list.

fn render_iclass(frame: &mut Frame, area: Rect, app: &App) {

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(8),
            Constraint::Length(6),
        ])
        .split(area);

    let header_line = if let Some(session) = &app.session {

        Line::from(vec![
            Span::styled("用户: ", theme::label_style()),
            Span::styled(session.user_name.clone(), theme::text_style()),
            Span::styled(format!(" ({})", session.user_id), theme::muted_style()),
            Span::styled("  |  模式: ", theme::label_style()),
            Span::styled(
                if session.use_vpn { "VPN" } else { "直连" },
                Style::default().fg(theme::INFO),
            ),
            Span::styled("  |  ", theme::muted_style()),
            theme::activity_badge(
                app.tick,
                app.iclass_loading,
                if app.iclass_loading {

                    "正在加载课程"
                } else {

                    "iClass 就绪"
                },
            ),
            Span::styled(
                format!("  |  共 {} 条课程", app.courses.len()),
                theme::muted_style(),
            ),
        ])
    } else {

        Line::from(Span::styled("未登录", theme::muted_style()))
    };

    let header = Paragraph::new(header_line).block(
        Block::default()
            .title("会话")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::BORDER_IDLE))
            .title_style(theme::title_style()),
    );

    frame.render_widget(header, vertical[0]);

    let week_text = if let Some(week) = app.selected_week_group() {

        format!(
            "当前周: {} | {} ~ {} | {} 条课程 | H/L 或 [ ] 切周",
            week.label,
            week.start_date,
            week.end_date,
            app.visible_courses_len()
        )
    } else {

        "当前没有可显示的周数据".to_string()
    };

    let week_bar =
        Paragraph::new(week_text).block(Block::default().title("周视图").borders(Borders::ALL));

    frame.render_widget(week_bar, vertical[1]);

    let day_columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Ratio(1, 7); 7])
        .split(vertical[2]);

    let week_start = app
        .selected_week_group()
        .and_then(|group| NaiveDate::parse_from_str(&group.start_date, "%Y-%m-%d").ok());

    let selected_absolute_index = app.selected_course_absolute_index();

    let weekday_labels = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];

    for (offset, area) in day_columns.iter().enumerate() {

        let Some(week_start) = week_start else {

            let empty = Paragraph::new("无周数据")
                .block(
                    Block::default()
                        .title(weekday_labels[offset])
                        .borders(Borders::ALL),
                )
                .wrap(Wrap { trim: true });

            frame.render_widget(empty, *area);

            continue;
        };

        let date = week_start + Duration::days(offset as i64);

        let date_key = date.format("%Y-%m-%d").to_string();

        let title = format!("{} {}", weekday_labels[offset], date.format("%m/%d"));

        let courses_in_day: Vec<usize> = app
            .visible_course_indices()
            .iter()
            .copied()
            .filter(|index| app.courses[*index].date == date_key)
            .collect();

        let items = if courses_in_day.is_empty() {

            vec![ListItem::new("  -")]
        } else {

            courses_in_day
                .iter()
                .map(|index| {

                    let course = &app.courses[*index];

                    let mut style = if course.signed() {

                        Style::default().fg(theme::OK)
                    } else {

                        Style::default()
                    };

                    if Some(*index) == selected_absolute_index {

                        style = theme::selection_style();
                    }

                    let label =
                        format!("{}-{}\n{}", course.start_time, course.end_time, course.name);

                    ListItem::new(label).style(style)
                })
                .collect()
        };

        let day_list = List::new(items)
            .block(Block::default().title(title).borders(Borders::ALL))
            .highlight_symbol("");

        let selected_in_day = courses_in_day
            .iter()
            .position(|index| Some(*index) == selected_absolute_index);

        let mut list_state = ListState::default().with_selected(selected_in_day);

        frame.render_stateful_widget(day_list, *area, &mut list_state);
    }

    let detail_lines = if let Some(course) = app.selected_course() {

        vec![
            Line::from(vec![
                Span::styled("课程: ", theme::label_style()),
                Span::raw(course.name.as_str()),
            ]),
            Line::from(vec![
                Span::styled("日期: ", theme::label_style()),
                Span::raw(course.date.as_str()),
            ]),
            Line::from(vec![
                Span::styled("时间: ", theme::label_style()),
                Span::raw(format!("{} - {}", course.start_time, course.end_time)),
            ]),
            Line::from(vec![
                Span::styled("签到: ", theme::label_style()),
                Span::raw(if course.signed() {

                    "已签到"
                } else {

                    "未签到"
                }),
            ]),
            Line::from(vec![
                Span::styled("courseSchedId: ", theme::label_style()),
                Span::raw(course.course_sched_id.as_str()),
            ]),
            Line::from(""),
            Line::from(
                "操作: r 刷新 | s 直接签到 | g 终端二维码 | G 外部二维码 | Shift+X 退出登录",
            ),
            Line::from(format!(
                "外部二维码: {} | {}",
                app.external_qr_path
                    .as_ref()
                    .map(|path| path.display().to_string())
                    .unwrap_or_else(|| "-".to_string()),
                app.external_qr_status.as_deref().unwrap_or("未启动")
            )),
        ]
    } else {

        vec![Line::from("当前没有课程")]
    };

    let detail = Paragraph::new(detail_lines)
        .block(Block::default().title("详情").borders(Borders::ALL))
        .wrap(Wrap { trim: true });

    frame.render_widget(detail, vertical[3]);

    render_event_log_block(frame, vertical[4], app);
}

/// Renders the BYKC workspace with view tabs, list content, detail, and status.

fn render_bykc(frame: &mut Frame, area: Rect, app: &App) {

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(12),
            Constraint::Length(6),
        ])
        .split(area);

    let header_text = if let Some(session) = &app.session {

        let statistics = bykc_statistics_summary(app);

        Line::from(vec![
            Span::styled("用户: ", theme::label_style()),
            Span::styled(session.user_name.clone(), theme::text_style()),
            Span::styled(format!(" ({})", session.user_id), theme::muted_style()),
            Span::styled("  |  VPN: ", theme::label_style()),
            Span::styled(
                if session.use_vpn { "开启" } else { "关闭" },
                Style::default().fg(theme::INFO),
            ),
            Span::styled("  |  ", theme::muted_style()),
            theme::activity_badge(
                app.tick,
                app.bykc.loading,
                if app.bykc.loading {

                    "正在加载 BYKC"
                } else {

                    "BYKC 就绪"
                },
            ),
            Span::styled(
                format!(
                    "  |  可选 {} 门 | 已选 {} 门 | {statistics}",
                    app.bykc.courses.len(),
                    app.bykc.chosen_courses.len()
                ),
                theme::muted_style(),
            ),
        ])
    } else {

        Line::from(Span::styled("未登录", theme::muted_style()))
    };

    let header = Paragraph::new(header_text).block(
        Block::default()
            .title("BYKC 会话")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::BORDER_FOCUS))
            .title_style(theme::title_style()),
    );

    frame.render_widget(header, vertical[0]);

    let view_titles = [" 可选课程 ", " 已选课程 "]
        .into_iter()
        .map(Line::from)
        .collect::<Vec<_>>();

    let selected_view = match app.bykc.view {
        BykcView::Courses => 0,
        BykcView::Chosen => 1,
    };

    let subtitle = format!(
        " include_all={} | 1/2 或 h/l 切换视图 | o 查看详情 ",
        if app.bykc.include_all { "on" } else { "off" }
    );

    let tabs = Tabs::new(view_titles)
        .block(Block::default().title(subtitle).borders(Borders::ALL))
        .select(selected_view)
        .style(theme::muted_style())
        .highlight_style(
            Style::default()
                .fg(theme::SELECTION_FG)
                .bg(theme::OK)
                .add_modifier(Modifier::BOLD),
        );

    frame.render_widget(tabs, vertical[1]);

    match app.bykc.view {
        BykcView::Courses => render_bykc_courses_list(frame, vertical[2], app),
        BykcView::Chosen => render_bykc_chosen_list(frame, vertical[2], app),
    }

    render_bykc_detail(frame, vertical[3], app);

    render_event_log_block(frame, vertical[4], app);
}

fn render_bykc_courses_list(frame: &mut Frame, area: Rect, app: &App) {

    let items = if app.bykc.courses.is_empty() {

        vec![ListItem::new(if app.bykc.loading {

            "加载 BYKC 可选课程中..."
        } else {

            "暂无课程"
        })]
    } else {

        app.bykc
            .courses
            .iter()
            .map(|course| {

                let selected_tag = if course.selected {

                    if let Some(chosen) = app.bykc.chosen_course_for(course.id) {

                        if chosen.can_sign {

                            "可签到"
                        } else if chosen.can_sign_out {

                            "可签退"
                        } else if can_deselect_bykc_course(&chosen.course_cancel_end_date) {

                            "已报"
                        } else {

                            "已过退选"
                        }
                    } else {

                        "已报"
                    }
                } else {

                    course.status.as_str()
                };

                let label = format!(
                    "[{}] {} | {} | {} | {} | {} | {}/{}",
                    selected_tag,
                    course.course_name,
                    empty_dash(&course.sub_category),
                    if course.course_teacher.is_empty() {

                        "未知教师"
                    } else {

                        course.course_teacher.as_str()
                    },
                    bykc_sign_type_label(course.course_sign_type),
                    bykc_self_sign_label(course.has_sign_points),
                    course.course_current_count,
                    course.course_max_count,
                );

                ListItem::new(label)
            })
            .collect()
    };

    let list = List::new(items)
        .block(
            Block::default()
                .title("课程列表")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER_FOCUS))
                .title_style(theme::title_style()),
        )
        .highlight_style(theme::selection_style());

    let mut state = ListState::default()
        .with_selected((!app.bykc.courses.is_empty()).then_some(app.bykc.selected_course));

    frame.render_stateful_widget(list, area, &mut state);
}

fn render_bykc_chosen_list(frame: &mut Frame, area: Rect, app: &App) {

    let items = if app.bykc.chosen_courses.is_empty() {

        vec![ListItem::new(if app.bykc.loading {

            "加载 BYKC 已选课程中..."
        } else {

            "暂无已选课程"
        })]
    } else {

        app.bykc
            .chosen_courses
            .iter()
            .map(|course| {

                let attendance = if course.can_sign {

                    "可签到"
                } else if course.can_sign_out {

                    "可签退"
                } else {

                    "不可操作"
                };

                let has_sign_points = course
                    .sign_config
                    .as_ref()
                    .is_some_and(|config| !config.sign_points.is_empty());

                let label = format!(
                    "[{}] {} | {} | {} | {} | checkin={} | {}",
                    attendance,
                    course.course_name,
                    empty_dash(&course.sub_category),
                    bykc_sign_type_label(course.course_sign_type),
                    bykc_self_sign_label(has_sign_points),
                    course.checkin,
                    course.sign_info
                );

                ListItem::new(label)
            })
            .collect()
    };

    let list = List::new(items)
        .block(Block::default().title("已选课程").borders(Borders::ALL))
        .highlight_style(theme::selection_style());

    let mut state = ListState::default()
        .with_selected((!app.bykc.chosen_courses.is_empty()).then_some(app.bykc.selected_chosen));

    frame.render_stateful_widget(list, area, &mut state);
}

/// Renders the inline BYKC detail panel under the current list.
///
/// Why:
/// Users should see location, windows, and basic status immediately on cursor
/// movement, even before opening the full popup. This panel therefore prefers
/// cached detail, but falls back to lighter list payloads when necessary.

fn render_bykc_detail(frame: &mut Frame, area: Rect, app: &App) {

    let lines = if let Some(detail) = app.bykc.selected_cached_detail() {

        vec![
            Line::from(vec![
                Span::styled("课程: ", theme::label_style()),
                Span::raw(detail.course_name.as_str()),
            ]),
            Line::from(vec![
                Span::styled("教师: ", Style::default().fg(theme::ACCENT_WARM)),
                Span::raw(if detail.course_teacher.is_empty() {

                    "-"
                } else {

                    detail.course_teacher.as_str()
                }),
            ]),
            Line::from(vec![
                Span::styled("地点: ", Style::default().fg(theme::ACCENT_WARM)),
                Span::raw(if detail.course_position.is_empty() {

                    "-"
                } else {

                    detail.course_position.as_str()
                }),
            ]),
            Line::from(vec![
                Span::styled("状态: ", Style::default().fg(theme::ACCENT_WARM)),
                Span::raw(detail.status.as_str()),
            ]),
            Line::from(vec![
                Span::styled("分类: ", Style::default().fg(theme::ACCENT_WARM)),
                Span::raw(bykc_category_label(&detail.category, &detail.sub_category)),
            ]),
            Line::from(vec![
                Span::styled("签到模式: ", Style::default().fg(theme::ACCENT_WARM)),
                Span::raw(bykc_sign_type_label(detail.course_sign_type)),
            ]),
            Line::from(vec![
                Span::styled("自主签到: ", Style::default().fg(theme::ACCENT_WARM)),
                Span::raw(bykc_self_sign_value(
                    detail
                        .sign_config
                        .as_ref()
                        .is_some_and(|config| !config.sign_points.is_empty()),
                )),
            ]),
            Line::from(vec![
                Span::styled("选课时间: ", Style::default().fg(theme::ACCENT_WARM)),
                Span::raw(format!(
                    "{} ~ {}",
                    empty_dash(&detail.course_select_start_date),
                    empty_dash(&detail.course_select_end_date)
                )),
            ]),
            Line::from(vec![
                Span::styled("签到窗口: ", Style::default().fg(theme::ACCENT_WARM)),
                Span::raw(
                    detail
                        .sign_config
                        .as_ref()
                        .map(|config| {

                            format!(
                                "{} ~ {}",
                                empty_dash(&config.sign_start_date),
                                empty_dash(&config.sign_end_date)
                            )
                        })
                        .unwrap_or_else(|| "-".to_string()),
                ),
            ]),
            Line::from(vec![
                Span::styled("签退窗口: ", Style::default().fg(theme::ACCENT_WARM)),
                Span::raw(
                    detail
                        .sign_config
                        .as_ref()
                        .map(|config| {

                            format!(
                                "{} ~ {}",
                                empty_dash(&config.sign_out_start_date),
                                empty_dash(&config.sign_out_end_date)
                            )
                        })
                        .unwrap_or_else(|| "-".to_string()),
                ),
            ]),
            Line::from("操作: o 打开详情浮窗 | s 报名/签到 | x 退选 | u 签退 | a 切换 include_all"),
        ]
    } else {

        let fallback = match app.bykc.view {
            BykcView::Courses => {
                app.bykc.selected_course().map(|course| {

                    vec![
                        Line::from(vec![
                            Span::styled("课程: ", theme::label_style()),
                            Span::raw(course.course_name.as_str()),
                        ]),
                        Line::from(vec![
                            Span::styled("状态: ", Style::default().fg(theme::ACCENT_WARM)),
                            Span::raw(course.status.as_str()),
                        ]),
                        Line::from(vec![
                            Span::styled("分类: ", Style::default().fg(theme::ACCENT_WARM)),
                            Span::raw(bykc_category_label(&course.category, &course.sub_category)),
                        ]),
                        Line::from(vec![
                            Span::styled("签到模式: ", Style::default().fg(theme::ACCENT_WARM)),
                            Span::raw(bykc_sign_type_label(course.course_sign_type)),
                        ]),
                        Line::from(vec![
                            Span::styled("自主签到: ", Style::default().fg(theme::ACCENT_WARM)),
                            Span::raw(bykc_self_sign_value(course.has_sign_points)),
                        ]),
                        Line::from(vec![
                            Span::styled("教师: ", Style::default().fg(theme::ACCENT_WARM)),
                            Span::raw(empty_dash(&course.course_teacher)),
                        ]),
                        Line::from(vec![
                            Span::styled("地点: ", Style::default().fg(theme::ACCENT_WARM)),
                            Span::raw(empty_dash(&course.course_position)),
                        ]),
                        Line::from(vec![
                            Span::styled("上课时间: ", Style::default().fg(theme::ACCENT_WARM)),
                            Span::raw(format!(
                                "{} ~ {}",
                                empty_dash(&course.course_start_date),
                                empty_dash(&course.course_end_date)
                            )),
                        ]),
                        Line::from(vec![
                            Span::styled("选课时间: ", Style::default().fg(theme::ACCENT_WARM)),
                            Span::raw(format!(
                                "{} ~ {}",
                                empty_dash(&course.course_select_start_date),
                                empty_dash(&course.course_select_end_date)
                            )),
                        ]),
                        Line::from(vec![
                            Span::styled("退选提示: ", Style::default().fg(theme::ACCENT_WARM)),
                            Span::raw(
                                app.bykc
                                    .chosen_course_for(course.id)
                                    .map(|chosen| {
                                        if can_deselect_bykc_course(&chosen.course_cancel_end_date)
                                        {

                                            format!(
                                                "可退选，截止 {}",
                                                empty_dash(&chosen.course_cancel_end_date)
                                            )
                                        } else {

                                            format!(
                                                "已过退选时间 {}",
                                                empty_dash(&chosen.course_cancel_end_date)
                                            )
                                        }
                                    })
                                    .unwrap_or_else(|| {
                                        if course.selected {

                                            "已报，但未找到退选记录".to_string()
                                        } else {

                                            "-".to_string()
                                        }
                                    }),
                            ),
                        ]),
                        Line::from(vec![
                            Span::styled("简介: ", Style::default().fg(theme::ACCENT_WARM)),
                            Span::raw(empty_dash(&course.course_desc)),
                        ]),
                        Line::from("按 s 报名，已报课程可按 x 退选；如需补充字段可按 o 或 enter"),
                    ]
                })
            }
            BykcView::Chosen => {
                app.bykc.selected_chosen_course().map(|course| {

                    vec![
                        Line::from(vec![
                            Span::styled("课程: ", theme::label_style()),
                            Span::raw(course.course_name.as_str()),
                        ]),
                        Line::from(vec![
                            Span::styled("签到状态: ", theme::label_style()),
                            Span::raw(course.checkin.to_string()),
                        ]),
                        Line::from(vec![
                            Span::styled("分类: ", Style::default().fg(theme::ACCENT_WARM)),
                            Span::raw(bykc_category_label(&course.category, &course.sub_category)),
                        ]),
                        Line::from(vec![
                            Span::styled("签到模式: ", Style::default().fg(theme::ACCENT_WARM)),
                            Span::raw(bykc_sign_type_label(course.course_sign_type)),
                        ]),
                        Line::from(vec![
                            Span::styled("自主签到: ", Style::default().fg(theme::ACCENT_WARM)),
                            Span::raw(bykc_self_sign_value(
                                course
                                    .sign_config
                                    .as_ref()
                                    .is_some_and(|config| !config.sign_points.is_empty()),
                            )),
                        ]),
                        Line::from(vec![
                            Span::styled("教师: ", Style::default().fg(theme::ACCENT_WARM)),
                            Span::raw(empty_dash(&course.course_teacher)),
                        ]),
                        Line::from(vec![
                            Span::styled("地点: ", Style::default().fg(theme::ACCENT_WARM)),
                            Span::raw(empty_dash(&course.course_position)),
                        ]),
                        Line::from(vec![
                            Span::styled("上课时间: ", Style::default().fg(theme::ACCENT_WARM)),
                            Span::raw(format!(
                                "{} ~ {}",
                                empty_dash(&course.course_start_date),
                                empty_dash(&course.course_end_date)
                            )),
                        ]),
                        Line::from(vec![
                            Span::styled("签到窗口: ", Style::default().fg(theme::ACCENT_WARM)),
                            Span::raw(
                                course
                                    .sign_config
                                    .as_ref()
                                    .map(|config| {

                                        format!(
                                            "{} ~ {}",
                                            empty_dash(&config.sign_start_date),
                                            empty_dash(&config.sign_end_date)
                                        )
                                    })
                                    .unwrap_or_else(|| "-".to_string()),
                            ),
                        ]),
                        Line::from(vec![
                            Span::styled("签退窗口: ", Style::default().fg(theme::ACCENT_WARM)),
                            Span::raw(
                                course
                                    .sign_config
                                    .as_ref()
                                    .map(|config| {

                                        format!(
                                            "{} ~ {}",
                                            empty_dash(&config.sign_out_start_date),
                                            empty_dash(&config.sign_out_end_date)
                                        )
                                    })
                                    .unwrap_or_else(|| "-".to_string()),
                            ),
                        ]),
                        Line::from(vec![
                            Span::styled("签到备注: ", Style::default().fg(theme::ACCENT_WARM)),
                            Span::raw(empty_dash(&course.sign_info)),
                        ]),
                        Line::from("按 s 签到，u 签退，x 退选；如需补充字段可按 o 或 enter"),
                    ]
                })
            }
        };

        fallback.unwrap_or_else(|| vec![Line::from("当前没有可显示的博雅课程")])
    };

    let detail = Paragraph::new(lines)
        .block(Block::default().title("详情").borders(Borders::ALL))
        .wrap(Wrap { trim: true });

    frame.render_widget(detail, area);
}

fn render_bykc_detail_popup(frame: &mut Frame, app: &App) {

    let Some(detail) = app.bykc.selected_cached_detail() else {

        return;
    };

    let area = centered_rect(72, 70, frame.area());

    frame.render_widget(Clear, area);

    let outer = Block::default()
        .title("BYKC 详情")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::OK));

    let inner = outer.inner(area);

    frame.render_widget(outer, area);

    let lines = vec![
        Line::from(vec![
            Span::styled("课程: ", theme::label_style()),
            Span::raw(detail.course_name.as_str()),
        ]),
        Line::from(vec![
            Span::styled("教师: ", Style::default().fg(theme::ACCENT_WARM)),
            Span::raw(empty_dash(&detail.course_teacher)),
        ]),
        Line::from(vec![
            Span::styled("地点: ", Style::default().fg(theme::ACCENT_WARM)),
            Span::raw(empty_dash(&detail.course_position)),
        ]),
        Line::from(vec![
            Span::styled("联系人: ", Style::default().fg(theme::ACCENT_WARM)),
            Span::raw(empty_dash(&detail.course_contact)),
        ]),
        Line::from(vec![
            Span::styled("联系电话: ", Style::default().fg(theme::ACCENT_WARM)),
            Span::raw(empty_dash(&detail.course_contact_mobile)),
        ]),
        Line::from(vec![
            Span::styled("上课时间: ", Style::default().fg(theme::ACCENT_WARM)),
            Span::raw(format!(
                "{} ~ {}",
                empty_dash(&detail.course_start_date),
                empty_dash(&detail.course_end_date)
            )),
        ]),
        Line::from(vec![
            Span::styled("选课时间: ", Style::default().fg(theme::ACCENT_WARM)),
            Span::raw(format!(
                "{} ~ {}",
                empty_dash(&detail.course_select_start_date),
                empty_dash(&detail.course_select_end_date)
            )),
        ]),
        Line::from(vec![
            Span::styled("退选截止: ", Style::default().fg(theme::ACCENT_WARM)),
            Span::raw(empty_dash(&detail.course_cancel_end_date)),
        ]),
        Line::from(vec![
            Span::styled("状态: ", Style::default().fg(theme::ACCENT_WARM)),
            Span::raw(detail.status.as_str()),
        ]),
        Line::from(vec![
            Span::styled("分类: ", Style::default().fg(theme::ACCENT_WARM)),
            Span::raw(bykc_category_label(&detail.category, &detail.sub_category)),
        ]),
        Line::from(vec![
            Span::styled("签到模式: ", Style::default().fg(theme::ACCENT_WARM)),
            Span::raw(bykc_sign_type_label(detail.course_sign_type)),
        ]),
        Line::from(vec![
            Span::styled("自主签到: ", Style::default().fg(theme::ACCENT_WARM)),
            Span::raw(bykc_self_sign_value(
                detail
                    .sign_config
                    .as_ref()
                    .is_some_and(|config| !config.sign_points.is_empty()),
            )),
        ]),
        Line::from(vec![
            Span::styled("签到窗口: ", Style::default().fg(theme::ACCENT_WARM)),
            Span::raw(
                detail
                    .sign_config
                    .as_ref()
                    .map(|config| {

                        format!(
                            "{} ~ {}",
                            empty_dash(&config.sign_start_date),
                            empty_dash(&config.sign_end_date)
                        )
                    })
                    .unwrap_or_else(|| "-".to_string()),
            ),
        ]),
        Line::from(vec![
            Span::styled("签退窗口: ", Style::default().fg(theme::ACCENT_WARM)),
            Span::raw(
                detail
                    .sign_config
                    .as_ref()
                    .map(|config| {

                        format!(
                            "{} ~ {}",
                            empty_dash(&config.sign_out_start_date),
                            empty_dash(&config.sign_out_end_date)
                        )
                    })
                    .unwrap_or_else(|| "-".to_string()),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("简介: ", Style::default().fg(theme::ACCENT_WARM)),
            Span::raw(empty_dash(&detail.course_desc)),
        ]),
        Line::from(""),
        Line::from("按 esc / o / enter 关闭"),
    ];

    let detail = Paragraph::new(lines)
        .block(Block::default().title("详细信息").borders(Borders::ALL))
        .wrap(Wrap { trim: true });

    frame.render_widget(detail, inner);
}

fn render_input(
    frame: &mut Frame,
    area: Rect,
    title: &str,
    value: &str,
    focused: bool,
    secret: bool,
) {

    let display = if secret {

        mask_password(value)
    } else {

        value.to_string()
    };

    let widget = Paragraph::new(Span::styled(display, theme::text_style()))
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(if focused {

                    Style::default().fg(theme::BORDER_FOCUS)
                } else {

                    Style::default().fg(theme::BORDER_IDLE)
                })
                .title_style(if focused {

                    theme::title_style()
                } else {

                    theme::subtitle_style()
                }),
        )
        .wrap(Wrap { trim: true });

    frame.render_widget(widget, area);
}

fn render_busy_popup(frame: &mut Frame, app: &App) {

    let area = centered_rect(40, 20, frame.area());

    frame.render_widget(Clear, area);

    // The single most useful place for motion: a blocking wait that otherwise
    // looks identical to a hung process.
    let popup = Paragraph::new(vec![
        Line::from(vec![theme::activity_badge(
            app.tick,
            true,
            "处理中，请稍候",
        )]),
        Line::from(""),
        Line::from(theme::gradient_text("▁▂▃▄▅▆▇█▇▆▅▄▃▂▁", 0.0)),
    ])
    .block(
        Block::default()
            .title("请稍候")
            .borders(Borders::ALL)
            .border_style(
                Style::default().fg(theme::ramp((app.tick as f32 / 30.0).rem_euclid(1.0))),
            )
            .title_style(theme::title_style()),
    )
    .wrap(Wrap { trim: true });

    frame.render_widget(popup, area);
}

fn render_qr_popup(frame: &mut Frame, app: &App) {

    let Some(qr) = &app.qr_display else {

        return;
    };

    let area = centered_rect(92, 94, frame.area());

    frame.render_widget(Clear, area);

    let outer = Block::default()
        .title("二维码签到")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::OK));

    let inner = outer.inner(area);

    frame.render_widget(outer, area);

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([Constraint::Min(12), Constraint::Length(5)])
        .split(inner);

    if let Ok(code) = QrCode::with_error_correction_level(qr.qr_url.as_bytes(), EcLevel::L) {

        let module_count = qr_module_count(&code);

        let qr_area = centered_qr_rect(module_count, sections[0]);

        let scale = qr_scale(qr_area, module_count);

        let widget = QrCodeWidget::new(code)
            .quiet_zone(QuietZone::Enabled)
            .scaling(Scaling::Exact(scale, scale))
            .style(Style::default().fg(Color::Black).bg(Color::White));

        frame.render_widget(widget, qr_area);
    } else {

        let failed = Paragraph::new("二维码生成失败")
            .block(Block::default().borders(Borders::ALL))
            .wrap(Wrap { trim: true });

        frame.render_widget(failed, sections[0]);
    }

    let generated_at = chrono::Local
        .timestamp_millis_opt(qr.timestamp)
        .single()
        .map(|time| time.format("%Y-%m-%d %H:%M:%S").to_string())
        .unwrap_or_else(|| qr.timestamp.to_string());

    let info = Paragraph::new(vec![
        Line::from(format!("courseSchedId: {}", qr.course_sched_id)),
        Line::from(format!("生成时间: {generated_at}")),
        Line::from("二维码每 2 秒刷新，按 g 关闭"),
    ])
    .block(Block::default().title("信息").borders(Borders::ALL))
    .wrap(Wrap { trim: true });

    frame.render_widget(info, sections[1]);
}

fn render_login_diagnostic_popup(frame: &mut Frame, app: &App) {

    let Some(diagnostic) = &app.login_diagnostic else {

        return;
    };

    let area = centered_rect(78, 72, frame.area());

    frame.render_widget(Clear, area);

    let lines = vec![
        Line::from(format!("阶段: {}", diagnostic.stage)),
        Line::from(format!("类型: {:?}", diagnostic.kind)),
        Line::from(format!("摘要: {}", diagnostic.summary)),
        Line::from(format!(
            "最终 URL: {}",
            diagnostic.final_url.as_deref().unwrap_or("-")
        )),
        Line::from(format!(
            "HTTP 状态: {}",
            diagnostic
                .http_status
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string())
        )),
        Line::from(format!(
            "页面线索: {}",
            diagnostic.page_hint.as_deref().unwrap_or("-")
        )),
        Line::from(""),
        Line::from("错误链:"),
        Line::from(diagnostic.error_chain.join(" | ")),
        Line::from(""),
        Line::from("建议:"),
        Line::from(diagnostic.suggestions.join("；")),
        Line::from(""),
        Line::from("按 v 或 esc 关闭"),
    ];

    let popup = Paragraph::new(lines)
        .block(Block::default().title("登录诊断").borders(Borders::ALL))
        .wrap(Wrap { trim: true });

    frame.render_widget(popup, area);
}

fn render_doctor_popup(frame: &mut Frame, app: &App) {

    let Some(report) = &app.doctor_report else {

        return;
    };

    let area = centered_rect(86, 80, frame.area());

    frame.render_widget(Clear, area);

    let mut lines = vec![
        Line::from(format!(
            "模式: {}",
            if report.use_vpn { "VPN" } else { "直连" }
        )),
        Line::from(""),
    ];

    for check in &report.checks {

        lines.push(Line::from(format!(
            "[{}] {} | {}ms | status={} | http={} ",
            if check.ok { "OK" } else { "FAIL" },
            check.name,
            check.elapsed_ms,
            check.status,
            check
                .http_status
                .map(|value| value.to_string())
                .unwrap_or_else(|| "-".to_string())
        )));

        lines.push(Line::from(format!(
            "URL: {}",
            check.final_url.as_deref().unwrap_or("-")
        )));

        lines.push(Line::from(format!(
            "DNS: {}",
            if check.resolved_addrs.is_empty() {

                "-".to_string()
            } else {

                check.resolved_addrs.join(", ")
            }
        )));

        lines.push(Line::from(format!("建议: {}", check.suggestion)));

        lines.push(Line::from(""));
    }

    lines.push(Line::from("按 D 或 esc 关闭"));

    let popup = Paragraph::new(lines)
        .block(Block::default().title("网络自检").borders(Borders::ALL))
        .wrap(Wrap { trim: true });

    frame.render_widget(popup, area);
}

fn render_event_log_block(frame: &mut Frame, area: Rect, app: &App) {

    let rows = app
        .latest_events()
        .iter()
        .rev()
        .take(4)
        .map(|entry| {

            let level = match entry.level {
                EventLevel::Info => "INFO",
                EventLevel::Success => "OK",
                EventLevel::Warn => "WARN",
                EventLevel::Error => "ERR",
            };

            let style = match entry.level {
                EventLevel::Info => Style::default().fg(theme::ACCENT),
                EventLevel::Success => Style::default().fg(theme::OK),
                EventLevel::Warn => Style::default().fg(theme::ACCENT_WARM),
                EventLevel::Error => {
                    Style::default()
                        .fg(theme::ERROR)
                        .add_modifier(Modifier::BOLD)
                }
            };

            Line::from(vec![
                Span::styled(format!("[{level}] "), style),
                Span::raw(entry.message.as_str()),
            ])
        })
        .collect::<Vec<_>>();

    let events = Paragraph::new(rows)
        .block(
            Block::default()
                .title("事件 | e 日志弹窗 | y 复制最近错误 | C 清空")
                .borders(Borders::ALL),
        )
        .wrap(Wrap { trim: true });

    frame.render_widget(events, area);
}

fn render_event_log_popup(frame: &mut Frame, app: &App) {

    let area = centered_rect(78, 65, frame.area());

    frame.render_widget(Clear, area);

    let mut lines = app
        .latest_events()
        .iter()
        .rev()
        .map(|entry| {

            let (level, style) = match entry.level {
                EventLevel::Info => ("INFO", Style::default().fg(theme::ACCENT)),
                EventLevel::Success => ("SUCCESS", Style::default().fg(theme::OK)),
                EventLevel::Warn => ("WARN", Style::default().fg(theme::ACCENT_WARM)),
                EventLevel::Error => {
                    (
                        "ERROR",
                        Style::default()
                            .fg(theme::ERROR)
                            .add_modifier(Modifier::BOLD),
                    )
                }
            };

            Line::from(vec![
                Span::styled(format!("[{level}] "), style),
                Span::raw(entry.message.as_str()),
            ])
        })
        .collect::<Vec<_>>();

    if lines.is_empty() {

        lines.push(Line::from("暂无事件"));
    }

    lines.push(Line::from(""));

    lines.push(Line::from(format!(
        "本地日志: {}",
        crate::logging::path()
            .map(|path| path.display().to_string())
            .unwrap_or_else(|| "未初始化".to_string())
    )));

    lines.push(Line::from(""));

    lines.push(Line::from("按 e / q / esc 关闭 | y 复制最近错误 | C 清空"));

    let popup = Paragraph::new(lines)
        .block(Block::default().title("事件日志").borders(Borders::ALL))
        .wrap(Wrap { trim: true });

    frame.render_widget(popup, area);
}

fn render_help_popup(frame: &mut Frame, app: &App) {

    let area = centered_rect(70, 70, frame.area());

    frame.render_widget(Clear, area);

    let lines = vec![
        Line::from(Span::styled(
            "全局",
            Style::default()
                .fg(theme::WARN)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("tab / shift+tab: 切换 iClass / BYKC"),
        Line::from("e: 打开或关闭事件日志"),
        Line::from("?: 打开或关闭帮助"),
        Line::from("Shift+X: 退出登录"),
        Line::from("q / esc: 退出程序"),
        Line::from(""),
        Line::from(Span::styled(
            "登录",
            Style::default()
                .fg(theme::WARN)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("tab / shift+tab: 切换字段"),
        Line::from("space: 切换 VPN 模式或记住我"),
        Line::from("enter: 登录"),
        Line::from("D: 执行 WebVPN / SSO / iClass / BYKC 自检"),
        Line::from("v: 查看最近一次登录失败详情"),
        Line::from("y: 复制最近错误（事件日志打开时）"),
        Line::from("C: 清空事件日志（事件日志打开时）"),
        Line::from(""),
        Line::from(Span::styled(
            "iClass",
            Style::default()
                .fg(theme::WARN)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("h/j/k/l: 周视图内移动"),
        Line::from("[ ] / H L: 切换周"),
        Line::from("r: 刷新课程"),
        Line::from("s: 直接签到"),
        Line::from("g: 打开或关闭终端二维码签到"),
        Line::from("G: 打开或关闭外部二维码签到"),
        Line::from(""),
        Line::from(Span::styled(
            "课表",
            Style::default()
                .fg(theme::WARN)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("j/k: 选择课程 | h/l 或 [ ]: 切换周"),
        Line::from(", / .: 切换已保存学期 | u: 导入整学期课表"),
        Line::from(""),
        Line::from(Span::styled(
            "BYKC",
            Style::default()
                .fg(theme::WARN)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("1 / 2 或 h / l: 切换 可选课程 / 已选课程"),
        Line::from("j / k: 移动选中项"),
        Line::from("r: 刷新博雅数据"),
        Line::from("o / enter: 加载课程详情"),
        Line::from("a: 切换 include_all（仅可选课程视图）"),
        Line::from("s: 报名课程，或在已选课程里执行签到"),
        Line::from("x: 退选当前已选课程"),
        Line::from("u: 执行签退"),
        Line::from(""),
        Line::from(format!(
            "本地日志: {}",
            crate::logging::path()
                .map(|path| path.display().to_string())
                .unwrap_or_else(|| "未初始化".to_string())
        )),
        Line::from(format!("当前标签: {:?}", app.active_tab)),
        Line::from("按 ?、q 或 esc 关闭帮助"),
    ];

    let popup = Paragraph::new(lines)
        .block(Block::default().title("帮助").borders(Borders::ALL))
        .wrap(Wrap { trim: true });

    frame.render_widget(popup, area);
}

fn centered_rect(percent_x: u16, percent_y: u16, area: Rect) -> Rect {

    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - percent_y) / 2),
            Constraint::Percentage(percent_y),
            Constraint::Percentage((100 - percent_y) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - percent_x) / 2),
            Constraint::Percentage(percent_x),
            Constraint::Percentage((100 - percent_x) / 2),
        ])
        .split(vertical[1])[1]
}

fn centered_qr_rect(module_count: u16, area: Rect) -> Rect {

    let scale = qr_scale(area, module_count);

    let width = module_count.saturating_mul(scale).min(area.width);

    let height = module_count
        .saturating_mul(scale)
        .div_ceil(2)
        .min(area.height);

    Rect {
        x: area.x + area.width.saturating_sub(width) / 2,
        y: area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    }
}

fn qr_scale(area: Rect, module_count: u16) -> u16 {

    if module_count == 0 {

        return 1;
    }

    let horizontal = area.width / module_count;

    let vertical = area.height.saturating_mul(2) / module_count;

    horizontal.min(vertical).clamp(1, QR_MAX_MODULE_SCALE)
}

fn qr_module_count(code: &QrCode) -> u16 {

    code.width() as u16 + 8
}

fn mask_password(value: &str) -> String {

    "*".repeat(value.chars().count())
}

fn bykc_sign_type_label(value: Option<i32>) -> &'static str {

    match value {
        Some(1) => "仅签到",
        Some(2) => "签到+签退",
        Some(_) => "未知签到模式",
        None => "无签到模式",
    }
}

fn bykc_self_sign_label(has_sign_points: bool) -> &'static str {

    if has_sign_points {

        "自主签到"
    } else {

        "非自主签到"
    }
}

fn bykc_self_sign_value(has_sign_points: bool) -> &'static str {

    if has_sign_points { "是" } else { "否" }
}

fn bykc_category_label(category: &str, sub_category: &str) -> String {

    match (category.trim().is_empty(), sub_category.trim().is_empty()) {
        (false, false) => format!("{category}/{sub_category}"),
        (false, true) => category.to_string(),
        (true, false) => sub_category.to_string(),
        (true, true) => "-".to_string(),
    }
}

fn bykc_statistics_summary(app: &App) -> String {

    if let Some(statistics) = &app.bykc.statistics {

        let rows = statistics
            .categories
            .iter()
            .map(|item| {

                let status = if item.is_qualified {

                    "达标"
                } else {

                    "未达标"
                };

                format!(
                    "{} {}/{} {}",
                    empty_dash(&item.sub_category),
                    item.passed_count,
                    item.required_count,
                    status
                )
            })
            .collect::<Vec<_>>();

        if rows.is_empty() {

            format!("统计: 有效 {}", statistics.total_valid_count)
        } else {

            format!(
                "统计: 有效 {} [{}]",
                statistics.total_valid_count,
                rows.join(", ")
            )
        }
    } else if let Some(error) = &app.bykc.statistics_error {

        format!("统计加载失败: {error}")
    } else {

        "统计: 未加载".to_string()
    }
}

fn empty_dash(value: &str) -> String {

    if value.trim().is_empty() {

        "-".to_string()
    } else {

        value.to_string()
    }
}
