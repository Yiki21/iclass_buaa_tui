//! Pure rendering code for the login screen, iClass workspace, and BYKC views.

use chrono::{Datelike, Duration, Local, NaiveDate, TimeZone};
use qrcode::{EcLevel, QrCode};
use ratatui::{
    Frame,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use tui_qrcode::{QrCodeWidget, QuietZone, Scaling};

use crate::app::{
    App, BykcView, CourseView, EventLevel, HotAction, LoginFocus, QrMode, Screen, WorkspaceTab,
};
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

    // Regions are recomputed from scratch each frame; a stale one would route a
    // click to whatever used to be there.
    app.clear_hotspots();

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

/// Renders the login screen as a centered card.
///
/// Why:
/// The old layout stretched six bordered fields across the whole terminal
/// inside another full-screen frame, so on a wide window each field was a
/// hundred columns of border around four characters and the eye had to travel
/// the full width to read a two-word value.
///
/// How:
/// A fixed-width card, centered, with the version and the latest event beneath
/// it as quiet context. The card is sized to its content rather than to the
/// window, and the outer frame is dropped since the card is the only subject.

fn render_login(frame: &mut Frame, app: &App) {

    frame.render_widget(Clear, frame.area());

    let area = frame.area();

    let card_width = 66.min(area.width);

    // Inner rows: title 1 + mode 1 + fields (3 each) + tail 2, plus 2 border rows.
    let field_count: u16 = if app.login.captcha_required { 4 } else { 3 };

    let card_height = (1 + 1 + field_count * 3 + 2 + 2).min(area.height.saturating_sub(1));

    let card = Rect {
        x:      area.x + (area.width.saturating_sub(card_width)) / 2,
        y:      area.y + (area.height.saturating_sub(card_height)) / 2,
        width:  card_width,
        height: card_height,
    };

    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(" iClass BUAA ", theme::title_style()),
            Span::styled("统一认证登录 ", theme::subtitle_style()),
        ]))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER_FOCUS));

    let inner = block.inner(card);

    frame.render_widget(block, card);

    let [title, mode, fields, tail] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(3),
        Constraint::Length(2),
    ])
    .areas(inner);

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            " Controll Your Campus Life In Terminal",
            theme::subtitle_style(),
        ))),
        title,
    );

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(" 访问模式 ", theme::muted_style()),
            Span::styled(
                if app.login.use_vpn { "VPN" } else { "直连" },
                Style::default().fg(theme::INFO),
            ),
            Span::styled("   space 切换", theme::muted_style()),
        ])),
        mode,
    );

    let rows = Layout::vertical(vec![Constraint::Length(3); field_count as usize]).split(fields);

    let mut next_row = 0;

    if app.login.use_vpn {

        render_input(
            frame,
            rows[next_row],
            "统一认证账号",
            &app.login.vpn_username,
            app.login.current_focus() == LoginFocus::VpnUsername,
            false,
        );
    } else {

        render_input(
            frame,
            rows[next_row],
            "学号",
            &app.login.student_id,
            app.login.current_focus() == LoginFocus::StudentId,
            false,
        );
    }

    next_row += 1;

    render_input(
        frame,
        rows[next_row],
        "统一认证密码",
        &mask_password(&app.login.vpn_password),
        app.login.current_focus() == LoginFocus::VpnPassword,
        true,
    );

    next_row += 1;

    if app.login.captcha_required {

        render_input(
            frame,
            rows[next_row],
            "验证码",
            &app.login.captcha,
            app.login.current_focus() == LoginFocus::Captcha,
            false,
        );

        next_row += 1;
    }

    render_input(
        frame,
        rows[next_row],
        "记住我",
        if app.login.remember_me {

            "开启"
        } else {

            "关闭"
        },
        app.login.current_focus() == LoginFocus::RememberMe,
        false,
    );

    let status = app
        .latest_events()
        .last()
        .map(|entry| {

            let (tag, style) = event_tag(entry.level);

            Line::from(vec![
                Span::styled(tag, style),
                Span::raw(" "),
                Span::styled(entry.message.clone(), theme::text_style()),
            ])
        })
        .unwrap_or_else(|| {

            Line::from(Span::styled(
                "tab 切换字段 · space 切换选项 · enter 登录",
                theme::muted_style(),
            ))
        });

    let hint = app
        .pending_captcha_login
        .as_ref()
        .map(|pending| {

            format!(
                "验证码图片: {} | enter 继续同一登录会话",
                pending.challenge.captcha_path
            )
        })
        .unwrap_or_else(|| "D 自检 · v 失败详情 · ? 帮助 · q 退出".to_string());

    let tail_rows = Layout::vertical([Constraint::Length(1); 2]).split(tail);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" "),
            Span::styled(app.version_text(), theme::muted_style()),
        ])),
        tail_rows[0],
    );

    frame.render_widget(
        Paragraph::new(Line::from(
            std::iter::once(Span::raw(" "))
                .chain(status.spans)
                .collect::<Vec<_>>(),
        )),
        tail_rows[1],
    );

    // The key hint sits below the card, aligned to it.
    let below = Rect {
        x:      card.x,
        y:      card.y + card.height,
        width:  card.width,
        height: 1.min(area.height.saturating_sub(card.y + card.height - area.y)),
    };

    if below.height > 0 {

        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                format!(" {hint}"),
                theme::muted_style(),
            )))
            .wrap(Wrap { trim: true }),
            below,
        );
    }
}

/// Renders the shared workspace shell before delegating to the active tab body.

/// Lays out the workspace as one chrome around one framed content zone.
///
/// Why:
/// Each tab used to stack its own bordered boxes: tabs, a hint box repeating
/// the tab bar, a session box, a week box, the content, a detail box, and a
/// six-row event box. Four of those held a single line but cost three rows
/// each. On a 40-row terminal that was twelve rows of frame around four rows
/// of text, and the content -- the reason to be on the screen -- got the rest.
///
/// How:
/// Three borderless rows carry identity, status, and the latest event. Only
/// the content zone is framed, and it receives every remaining row. Each tab
/// contributes its status line and its content; the chrome is shared.

fn render_workspace(frame: &mut Frame, app: &App) {

    let [top_bar, status_strip, content, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(8),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    render_top_bar(frame, top_bar, app);

    render_status_strip(frame, status_strip, app);

    let content_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER_IDLE));

    let content_inner = content_block.inner(content);

    frame.render_widget(content_block, content);

    match app.active_tab {
        WorkspaceTab::Schedule => render_schedule(frame, content_inner, app),
        WorkspaceTab::IClass => render_iclass(frame, content_inner, app),
        WorkspaceTab::Bykc => render_bykc(frame, content_inner, app),
    }

    render_footer(frame, footer, app);
}

/// Top bar: app name, tabs, and identity, on one borderless row.

fn render_top_bar(frame: &mut Frame, area: Rect, app: &App) {

    let [tabs_area, identity_area] =
        Layout::horizontal([Constraint::Min(30), Constraint::Length(48)]).areas(area);

    let brand = " iClass BUAA  ";

    let mut spans = vec![Span::styled(brand, theme::title_style())];

    let tabs = [
        (WorkspaceTab::Schedule, app.schedule.portal_label()),
        (WorkspaceTab::IClass, "iClass"),
        (WorkspaceTab::Bykc, "BYKC"),
    ];

    // Track each label's columns so the same text the user sees is what they
    // can click. Widths are measured in display cells, which is what the
    // terminal counts.
    let mut column = tabs_area.x + display_width(brand) as u16;

    for (tab, label) in tabs {

        let text = format!(" {label} ");

        let width = display_width(&text) as u16;

        if tab == app.active_tab {

            spans.push(Span::styled(text, theme::selection_style()));
        } else {

            spans.push(Span::styled(text, theme::muted_style()));
        }

        spans.push(Span::raw(" "));

        app.record_hotspot(
            Rect {
                x: column,
                y: tabs_area.y,
                width,
                height: 1,
            },
            HotAction::WorkspaceTab(tab),
        );

        column += width + 1;
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), tabs_area);

    let identity = if let Some(session) = &app.session {

        Line::from(vec![
            Span::styled(session.user_name.clone(), theme::text_style()),
            Span::styled(format!(" ({})", session.user_id), theme::muted_style()),
            Span::styled(" · ", theme::muted_style()),
            Span::styled(
                if session.use_vpn { "VPN" } else { "直连" },
                Style::default().fg(theme::INFO),
            ),
            Span::styled(" · ", theme::muted_style()),
            Span::styled(app.version_short(), app.version_style()),
            Span::raw(" "),
        ])
    } else {

        Line::from(Span::styled("未登录 ", theme::muted_style()))
    };

    frame.render_widget(
        Paragraph::new(identity).alignment(Alignment::Right),
        identity_area,
    );
}

/// Status strip: one row of the active tab's live context.
///
/// Why:
/// Session, week, and loading state used to sit in three separate boxes. On
/// one row they read as a single sentence about where the user is.

fn render_status_strip(frame: &mut Frame, area: Rect, app: &App) {

    let line = match app.active_tab {
        WorkspaceTab::Schedule => schedule_status_line(app),
        WorkspaceTab::IClass => iclass_status_line(app),
        WorkspaceTab::Bykc => bykc_status_line(app),
    };

    frame.render_widget(Paragraph::new(line), area);
}

/// Footer: the latest event on the left, global keys on the right.
///
/// Why:
/// A six-row event box on every screen spent a sixth of a small terminal on
/// history the user rarely reads. One row shows the most recent message; `e`
/// opens the full log when it matters.

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {

    let [event_area, keys_area] =
        Layout::horizontal([Constraint::Min(20), Constraint::Length(44)]).areas(area);

    let event_line = app
        .latest_events()
        .last()
        .map(|entry| {

            let (tag, style) = event_tag(entry.level);

            Line::from(vec![
                Span::raw(" "),
                Span::styled(tag, style),
                Span::raw(" "),
                Span::styled(entry.message.clone(), theme::text_style()),
            ])
        })
        .unwrap_or_else(|| Line::from(Span::styled(" 就绪", theme::muted_style())));

    frame.render_widget(Paragraph::new(event_line), event_area);

    let keys = Line::from(vec![
        Span::styled("tab", theme::label_style()),
        Span::styled(" 切换  ", theme::muted_style()),
        Span::styled("e", theme::label_style()),
        Span::styled(" 日志  ", theme::muted_style()),
        Span::styled("?", theme::label_style()),
        Span::styled(" 帮助  ", theme::muted_style()),
        Span::styled("q", theme::label_style()),
        Span::styled(" 退出 ", theme::muted_style()),
    ]);

    frame.render_widget(Paragraph::new(keys).alignment(Alignment::Right), keys_area);
}

/// Level tag and color for an event entry.

fn event_tag(level: EventLevel) -> (&'static str, Style) {

    match level {
        EventLevel::Info => ("INFO", Style::default().fg(theme::ACCENT)),
        EventLevel::Success => (" OK ", Style::default().fg(theme::OK)),
        EventLevel::Warn => ("WARN", Style::default().fg(theme::ACCENT_WARM)),
        EventLevel::Error => {
            (
                "ERR ",
                Style::default()
                    .fg(theme::ERROR)
                    .add_modifier(Modifier::BOLD),
            )
        }
    }
}

/// A separator between segments in a status line.

fn sep() -> Span<'static> {

    Span::styled("  ·  ", theme::muted_style())
}

/// Status line for the schedule tab: term, week, and import state.

fn schedule_status_line(app: &App) -> Line<'static> {

    let semester = app.schedule.current_semester();

    let week = app.schedule.current_week();

    let (state_text, state_style) = if app.schedule.updating {

        ("正在更新", Style::default().fg(theme::WARN))
    } else if semester.is_some() {

        ("离线可读", Style::default().fg(theme::OK))
    } else {

        ("尚未导入", theme::muted_style())
    };

    let mut spans = vec![Span::raw(" ")];

    spans.push(Span::styled(
        app.schedule
            .current_schedule()
            .map(|item| item.term_name.clone())
            .or_else(|| semester.map(|item| item.term_code.clone()))
            .unwrap_or_else(|| "无缓存学期".to_string()),
        Style::default().fg(theme::INFO),
    ));

    if let Some(week) = week {

        spans.push(sep());

        spans.push(Span::styled(week.name.clone(), theme::text_style()));

        spans.push(Span::styled(
            format!("  {} ~ {}", week.start_date, week.end_date),
            theme::muted_style(),
        ));
    }

    spans.push(sep());

    spans.push(theme::activity_badge_styled(
        app.tick,
        app.schedule.updating,
        state_text,
        state_style,
    ));

    Line::from(spans)
}

/// Status line for the iClass tab: week, date range, and course count.

fn iclass_status_line(app: &App) -> Line<'static> {

    let mut spans = vec![Span::raw(" ")];

    if let Some(week) = app.selected_week_group() {

        spans.push(Span::styled(
            week.label.clone(),
            Style::default().fg(theme::INFO),
        ));

        spans.push(sep());

        spans.push(Span::styled(
            format!("{} ~ {}", week.start_date, week.end_date),
            theme::muted_style(),
        ));

        spans.push(sep());

        spans.push(Span::styled(
            format!("{} 条课程", app.visible_courses_len()),
            theme::text_style(),
        ));
    } else {

        spans.push(Span::styled("当前没有可显示的周数据", theme::muted_style()));
    }

    spans.push(sep());

    spans.push(theme::activity_badge(
        app.tick,
        app.iclass_loading,
        if app.iclass_loading {

            "正在加载课程"
        } else {

            "就绪"
        },
    ));

    Line::from(spans)
}

/// Status line for the BYKC tab: counts, include_all, and category progress.

fn bykc_status_line(app: &App) -> Line<'static> {

    let mut spans = vec![Span::raw(" ")];

    spans.push(Span::styled(
        format!("可选 {}", app.bykc.courses.len()),
        theme::text_style(),
    ));

    spans.push(sep());

    spans.push(Span::styled(
        format!("已选 {}", app.bykc.chosen_courses.len()),
        theme::text_style(),
    ));

    spans.push(sep());

    spans.push(Span::styled(
        if app.bykc.include_all {

            "含已过期"
        } else {

            "仅可报名"
        },
        theme::muted_style(),
    ));

    let statistics = bykc_statistics_summary(app);

    if !statistics.is_empty() && statistics != "-" {

        spans.push(sep());

        spans.push(Span::styled(statistics, theme::muted_style()));
    }

    spans.push(sep());

    spans.push(theme::activity_badge(
        app.tick,
        app.bykc.loading,
        if app.bykc.loading {

            "正在加载"
        } else {

            "就绪"
        },
    ));

    Line::from(spans)
}

/// Sub-navigation for the six course views, drawn as one borderless row.
///
/// Why:
/// The six views used to announce themselves inside each view's own header
/// box, so switching views changed a title but never showed the alternatives.
/// One persistent row makes the number keys discoverable.

fn render_course_nav(frame: &mut Frame, area: Rect, app: &App) {

    let views = [
        (CourseView::Today, "1", "今日"),
        (CourseView::Schedule, "2", "课表"),
        (CourseView::Exams, "3", "考试"),
        (CourseView::Grades, "4", "成绩"),
        (CourseView::Classrooms, "5", "空教室"),
        (CourseView::Tasks, "6", "作业"),
    ];

    let mut spans = Vec::new();

    // Track each item's columns so the label the user sees is what they click.
    let mut column = area.x;

    for (view, key, label) in views {

        let selected = view == app.schedule.view;

        // Both branches draw " {key} {label} " worth of cells; only the styling
        // differs. The hotspot therefore always covers the whole item.
        let item_width = display_width(&format!(" {key} {label} ")) as u16;

        if selected {

            spans.push(Span::styled(
                format!(" {key} {label} "),
                theme::selection_style(),
            ));
        } else {

            spans.push(Span::styled(format!(" {key} "), theme::label_style()));

            spans.push(Span::styled(format!("{label} "), theme::muted_style()));
        }

        spans.push(Span::raw(" "));

        app.record_hotspot(
            Rect {
                x:      column,
                y:      area.y,
                width:  item_width,
                height: 1,
            },
            HotAction::CourseView(view),
        );

        column += item_width + 1;
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);

    if let Some(indicator) = search_indicator(app) {

        frame.render_widget(
            Paragraph::new(Line::from(vec![indicator, Span::raw(" ")])).alignment(Alignment::Right),
            area,
        );
    }
}

/// The search box state, shown at the nav row's right edge when active.

fn search_indicator(app: &App) -> Option<Span<'static>> {

    if app.schedule.filtering {

        Some(Span::styled(
            format!("/ {}_", app.schedule.query),
            Style::default().fg(theme::ACCENT),
        ))
    } else if !app.schedule.query.is_empty() {

        Some(Span::styled(
            format!("/ {}", app.schedule.query),
            theme::muted_style(),
        ))
    } else {

        None
    }
}

/// Splits the content zone into course nav, body, and a footer of `footer_rows`.

fn course_layout(area: Rect, footer_rows: u16) -> [Rect; 3] {

    Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(4),
        Constraint::Length(footer_rows),
    ])
    .areas(area)
}

/// Display width of a string in terminal cells.
///
/// Why:
/// Clickable regions are measured in cells, and a CJK glyph occupies two. Using
/// byte length or character count would put every hotspot after a Chinese label
/// in the wrong place.

fn display_width(text: &str) -> usize {

    text.chars()
        .map(|character| {

            let code = character as u32;

            // CJK, fullwidth forms, and the common wide ranges.
            if (0x1100..=0x115F).contains(&code)
                || (0x2E80..=0xA4CF).contains(&code)
                || (0xAC00..=0xD7A3).contains(&code)
                || (0xF900..=0xFAFF).contains(&code)
                || (0xFE30..=0xFE6F).contains(&code)
                || (0xFF00..=0xFF60).contains(&code)
                || (0xFFE0..=0xFFE6).contains(&code)
                || (0x20000..=0x3FFFD).contains(&code)
            {

                2
            } else {

                1
            }
        })
        .sum()
}

/// One summary row above a list view, replacing the old header box.

fn render_view_summary(frame: &mut Frame, area: Rect, spans: Vec<Span<'static>>) {

    let mut line = vec![Span::raw(" ")];

    line.extend(spans);

    frame.render_widget(Paragraph::new(Line::from(line)), area);
}

/// Placeholder row for an empty list, phrased by loading state.

fn empty_row(
    loading: bool,
    tick: u64,
    loading_label: &str,
    empty_label: &str,
) -> ListItem<'static> {

    if loading {

        ListItem::new(Line::from(vec![
            Span::raw(" "),
            theme::activity_badge(tick, true, loading_label),
        ]))
    } else {

        ListItem::new(Line::from(Span::styled(
            format!(" {empty_label}"),
            theme::muted_style(),
        )))
    }
}

/// Records one clickable region per visible list row.
///
/// Why:
/// Mapping a click to a list index needs the row height and the scroll offset,
/// both of which live here. Recording per-row lets the click handler stay a
/// lookup instead of re-deriving list geometry.
///
/// How:
/// `body` is the list's drawable area, `offset` the index of its first visible
/// row, and `count` how many rows exist. Rows past the area are not recorded,
/// since they cannot be clicked.

fn record_list_rows(app: &App, body: Rect, offset: usize, count: usize) {

    if body.height == 0 || count == 0 {

        return;
    }

    let visible = (body.height as usize).min(count.saturating_sub(offset));

    for row in 0..visible {

        app.record_hotspot(
            Rect {
                x:      body.x,
                y:      body.y + row as u16,
                width:  body.width,
                height: 1,
            },
            HotAction::ListRow {
                index: offset + row,
            },
        );
    }
}

/// Key hint row at the bottom of a content view.

fn render_key_hint(frame: &mut Frame, area: Rect, hint: &str) {

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            format!(" {hint}"),
            theme::muted_style(),
        ))),
        area,
    );
}

/// Names the campus code used by the empty-classroom query.
///
/// Why:
/// The code is a bare integer upstream. Showing "1 校区" tells the reader
/// nothing about which campus they are looking at.

fn campus_label(code: i64) -> &'static str {

    match code {
        1 => "学院路",
        2 => "沙河",
        3 => "杭州",
        _ => "未知",
    }
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

    let [nav, body, detail_area] = course_layout(area, 3);

    render_course_nav(frame, nav, app);

    let schedule = app.schedule.current_schedule();

    let columns = Layout::horizontal([Constraint::Ratio(1, 7); 7]).split(body);

    let labels = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];

    let today_index = Local::now().weekday().num_days_from_monday() as usize;

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

            vec![ListItem::new(Span::styled("—", theme::muted_style()))]
        } else {

            entries
                .iter()
                .map(|(index, entry)| {

                    let selected = *index == app.schedule.selected_entry;

                    let plain = |style: Style| if selected { Style::default() } else { style };

                    let lines = vec![
                        Line::from(Span::styled(
                            format!(
                                "{}-{}",
                                entry.begin_time.as_deref().unwrap_or("--:--"),
                                entry.end_time.as_deref().unwrap_or("--:--"),
                            ),
                            plain(theme::muted_style()),
                        )),
                        Line::from(Span::styled(
                            entry.course_name.clone(),
                            plain(theme::text_style()),
                        )),
                        Line::from(Span::styled(
                            entry
                                .place
                                .clone()
                                .unwrap_or_else(|| "未填写地点".to_string()),
                            plain(Style::default().fg(theme::INFO)),
                        )),
                    ];

                    let item = ListItem::new(lines);

                    if selected {

                        item.style(theme::selection_style())
                    } else {

                        item
                    }
                })
                .collect()
        };

        // Today's column gets the accent border so the eye lands on it first.
        let is_today = day == today_index;

        let list = List::new(items).block(
            Block::default()
                .title(labels[day])
                .borders(Borders::ALL)
                .border_style(Style::default().fg(if is_today {

                    theme::BORDER_FOCUS
                } else {

                    theme::BORDER_IDLE
                }))
                .title_style(if is_today {

                    theme::title_style()
                } else {

                    theme::subtitle_style()
                }),
        );

        frame.render_widget(list, *column);
    }

    let detail_lines = if let Some(entry) = app.schedule.selected_entry() {

        vec![
            Line::from(vec![
                Span::raw(" "),
                Span::styled(entry.course_name.clone(), theme::text_style()),
                Span::styled(format!("  {}", entry.course_code), theme::muted_style()),
            ]),
            Line::from(vec![
                Span::raw(" "),
                Span::styled(
                    format!(
                        "{} - {}",
                        entry.begin_time.as_deref().unwrap_or("--:--"),
                        entry.end_time.as_deref().unwrap_or("--:--")
                    ),
                    theme::muted_style(),
                ),
                sep(),
                Span::styled(
                    format!(
                        "第 {}-{} 节",
                        entry
                            .begin_section
                            .map_or("-".to_string(), |value| value.to_string()),
                        entry
                            .end_section
                            .map_or("-".to_string(), |value| value.to_string())
                    ),
                    theme::muted_style(),
                ),
                sep(),
                Span::styled(
                    entry
                        .place
                        .clone()
                        .unwrap_or_else(|| "未填写地点".to_string()),
                    Style::default().fg(theme::INFO),
                ),
                sep(),
                Span::styled(
                    entry
                        .weeks_and_teachers
                        .clone()
                        .unwrap_or_else(|| "未填写教师".to_string()),
                    theme::muted_style(),
                ),
            ]),
            Line::from(Span::styled(
                " ,/. 切学期  [ ] 或 h/l 切周  j/k 选课程  u 更新整学期",
                theme::muted_style(),
            )),
        ]
    } else {

        vec![
            Line::from(""),
            Line::from(Span::styled(
                " 当前周没有课程。首次使用请按 u 导入整学期课表",
                theme::muted_style(),
            )),
            Line::from(Span::styled(
                " ,/. 切学期  [ ] 或 h/l 切周  u 更新整学期",
                theme::muted_style(),
            )),
        ]
    };

    frame.render_widget(Paragraph::new(detail_lines), detail_area);
}

fn render_today_courses(frame: &mut Frame, area: Rect, app: &App) {

    let [nav, body, footer] = course_layout(area, 1);

    render_course_nav(frame, nav, app);

    let today = Local::now().date_naive();

    let entries = app.schedule.today_entries();

    let signed = entries
        .iter()
        .filter(|(_, entry)| {

            app.courses
                .iter()
                .find(|course| course.name == entry.course_name)
                .is_some_and(crate::model::CourseDetailItem::signed)
        })
        .count();

    let [summary_area, list_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(body);

    let mut summary = vec![
        Span::styled(
            today.format("%m-%d %a").to_string(),
            Style::default().fg(theme::INFO),
        ),
        sep(),
        Span::styled(format!("{} 门", entries.len()), theme::text_style()),
        sep(),
        Span::styled("签到 ", theme::muted_style()),
    ];

    summary.extend(theme::progress_bar(signed, entries.len(), 10));

    summary.push(Span::styled(
        format!(" {signed}/{}", entries.len()),
        Style::default().fg(if signed == entries.len() && !entries.is_empty() {

            theme::OK
        } else {

            theme::TEXT
        }),
    ));

    render_view_summary(frame, summary_area, summary);

    let items = if entries.is_empty() {

        vec![ListItem::new(Line::from(Span::styled(
            if app.schedule.semesters.is_empty() {

                " 暂无本地课表，按 u 导入整学期课表"
            } else {

                " 今天没有匹配课程"
            },
            theme::muted_style(),
        )))]
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

                let selected = *index == app.schedule.selected_entry;

                let plain = |style: Style| if selected { Style::default() } else { style };

                let line = Line::from(vec![
                    Span::raw(" "),
                    Span::styled(
                        format!(
                            "{}-{}  ",
                            entry.begin_time.as_deref().unwrap_or("--:--"),
                            entry.end_time.as_deref().unwrap_or("--:--"),
                        ),
                        plain(theme::muted_style()),
                    ),
                    Span::styled(
                        format!("{:<16}", entry.course_name),
                        plain(theme::text_style()),
                    ),
                    Span::styled(
                        format!("  {}", entry.place.as_deref().unwrap_or("未填写地点")),
                        plain(Style::default().fg(theme::INFO)),
                    ),
                    Span::styled(
                        format!("  {}", entry.weeks_and_teachers.as_deref().unwrap_or("")),
                        plain(theme::muted_style()),
                    ),
                    Span::raw("  "),
                    Span::styled(status_text, status_style),
                ]);

                let item = ListItem::new(line);

                if selected {

                    item.style(theme::selection_style())
                } else {

                    item
                }
            })
            .collect()
    };

    record_list_rows(app, list_area, 0, entries.len());

    frame.render_widget(List::new(items), list_area);

    render_key_hint(
        frame,
        footer,
        "j/k 选课程  / 搜索  r 刷新  u 更新整学期课表",
    );
}

fn render_exams(frame: &mut Frame, area: Rect, app: &App) {

    let [nav, body, footer] = course_layout(area, 1);

    render_course_nav(frame, nav, app);

    let [summary_area, list_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(body);

    render_view_summary(
        frame,
        summary_area,
        vec![Span::styled(
            format!("{} 场考试", app.schedule.exams.len()),
            theme::text_style(),
        )],
    );

    let items = if app.schedule.exams.is_empty() {

        vec![empty_row(
            app.schedule.academic_loading,
            app.tick,
            "正在拉取考试安排",
            "暂无考试数据，按 r 加载",
        )]
    } else {

        app.schedule
            .exams
            .iter()
            .map(|exam| {

                ListItem::new(Line::from(vec![
                    Span::raw(" "),
                    Span::styled(
                        exam.exam_date.as_deref().unwrap_or("未定日期").to_string(),
                        Style::default().fg(theme::INFO),
                    ),
                    Span::styled(
                        format!(
                            " {}-{}",
                            exam.start_time.as_deref().unwrap_or("--:--"),
                            exam.end_time.as_deref().unwrap_or("--:--"),
                        ),
                        theme::muted_style(),
                    ),
                    Span::raw("  "),
                    Span::styled(exam.course_name.clone(), theme::text_style()),
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

    record_list_rows(app, list_area, 0, app.schedule.exams.len());

    frame.render_widget(List::new(items), list_area);

    render_key_hint(frame, footer, "r 刷新");
}

fn render_grades(frame: &mut Frame, area: Rect, app: &App) {

    let [nav, body, footer] = course_layout(area, 1);

    render_course_nav(frame, nav, app);

    let [summary_area, list_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(body);

    // When several terms are loaded the per-term figure is no longer what the
    // list shows, so the summary reports the GPA instead of only the count.
    let summary = crate::academic::summarize_grades(&app.schedule.grades);

    let mut spans = vec![Span::styled(
        format!("{} 门成绩", app.schedule.grades.len()),
        theme::text_style(),
    )];

    if app.schedule.grades_all_terms {

        spans.push(sep());

        spans.push(Span::styled("全部学期", Style::default().fg(theme::INFO)));
    }

    if let Some(gpa) = summary.weighted_gpa {

        spans.push(sep());

        spans.push(Span::styled(
            format!("GPA {gpa:.2}"),
            Style::default().fg(theme::OK).add_modifier(Modifier::BOLD),
        ));

        spans.push(Span::styled(
            format!("  {:.1} 学分", summary.total_credits),
            theme::muted_style(),
        ));
    }

    if let Some(score) = summary.weighted_score {

        spans.push(sep());

        spans.push(Span::styled(
            format!("加权分 {score:.1}"),
            theme::text_style(),
        ));
    }

    if summary.failed > 0 {

        spans.push(sep());

        spans.push(Span::styled(
            format!("{} 门不及格", summary.failed),
            Style::default()
                .fg(theme::ERROR)
                .add_modifier(Modifier::BOLD),
        ));
    }

    render_view_summary(frame, summary_area, spans);

    let items = if app.schedule.grades.is_empty() {

        vec![empty_row(
            app.schedule.academic_loading,
            app.tick,
            "正在拉取成绩",
            "暂无成绩数据，按 r 加载",
        )]
    } else {

        app.schedule
            .grades
            .iter()
            .map(|grade| {

                let score = grade.score.as_deref().unwrap_or("-").trim();

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

                let mut row = vec![
                    Span::raw(" "),
                    Span::styled(format!("{score:>5}"), score_style),
                    Span::raw("  "),
                ];

                // With several terms merged, the term is what disambiguates two
                // courses with the same name.
                if app.schedule.grades_all_terms {

                    row.push(Span::styled(
                        format!("{:<12}", grade.term_code),
                        theme::muted_style(),
                    ));
                }

                row.push(Span::styled(grade.course_name.clone(), theme::text_style()));

                ListItem::new(Line::from(vec![
                    Span::raw(" "),
                    Span::styled(format!("{score:>5}"), score_style),
                    Span::raw("  "),
                    Span::styled(
                        if app.schedule.grades_all_terms {

                            format!("{:<12}", grade.term_code)
                        } else {

                            String::new()
                        },
                        theme::muted_style(),
                    ),
                    Span::styled(grade.course_name.clone(), theme::text_style()),
                    Span::styled(
                        format!(
                            "  {} 学分",
                            grade
                                .credit
                                .map(|value| value.to_string())
                                .unwrap_or_else(|| "-".to_string()),
                        ),
                        theme::muted_style(),
                    ),
                    Span::styled(
                        format!("  绩点 {}", grade.grade_point.as_deref().unwrap_or("-")),
                        Style::default().fg(theme::INFO),
                    ),
                    Span::styled(
                        format!("  {}", grade.passed.as_deref().unwrap_or("")),
                        theme::muted_style(),
                    ),
                ]))
            })
            .collect()
    };

    record_list_rows(app, list_area, 0, app.schedule.grades.len());

    frame.render_widget(List::new(items), list_area);

    render_key_hint(frame, footer, "r 刷新当前学期  A 加载全部学期");
}

fn render_classrooms(frame: &mut Frame, area: Rect, app: &App) {

    let [nav, body, footer] = course_layout(area, 1);

    render_course_nav(frame, nav, app);

    let [summary_area, list_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(body);

    render_view_summary(
        frame,
        summary_area,
        vec![
            Span::styled(
                format!("{} 校区", campus_label(app.schedule.classroom_campus)),
                Style::default().fg(theme::INFO),
            ),
            sep(),
            Span::styled(app.schedule.classroom_date.clone(), theme::text_style()),
            sep(),
            Span::styled(
                format!("{} 间空闲", app.schedule.classrooms.len()),
                theme::muted_style(),
            ),
        ],
    );

    let items = if app.schedule.classrooms.is_empty() {

        vec![empty_row(
            app.schedule.academic_loading,
            app.tick,
            "正在查询空教室",
            "暂无空教室数据，按 r 加载",
        )]
    } else {

        app.schedule
            .classrooms
            .iter()
            .map(|room| {

                ListItem::new(Line::from(vec![
                    Span::raw(" "),
                    Span::styled(format!("{:<8}", room.building), theme::label_style()),
                    Span::styled(format!("{:<10}", room.name), theme::text_style()),
                    Span::styled("空闲节次 ", theme::muted_style()),
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

    record_list_rows(app, list_area, 0, app.schedule.classrooms.len());

    frame.render_widget(List::new(items), list_area);

    render_key_hint(frame, footer, "r 刷新");
}

fn render_tasks(frame: &mut Frame, area: Rect, app: &App) {

    let [nav, body, footer] = course_layout(area, 1);

    render_course_nav(frame, nav, app);

    let [summary_area, list_area] =
        Layout::vertical([Constraint::Length(1), Constraint::Min(1)]).areas(body);

    let pending = app
        .schedule
        .tasks
        .iter()
        .filter(|task| task.status.contains("未提交") || task.status.contains("未作答"))
        .count();

    render_view_summary(
        frame,
        summary_area,
        vec![
            Span::styled(
                format!("{} 项作业", app.schedule.tasks.len()),
                theme::text_style(),
            ),
            sep(),
            Span::styled(
                format!("{pending} 项待提交"),
                if pending > 0 {

                    Style::default().fg(theme::WARN)
                } else {

                    theme::muted_style()
                },
            ),
        ],
    );

    let items = if app.schedule.tasks.is_empty() {

        vec![empty_row(
            app.schedule.academic_loading,
            app.tick,
            "正在拉取作业",
            "暂无作业数据，按 r 加载",
        )]
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
                    Span::raw(" "),
                    Span::styled(format!("{:<6}", task.status), status_style),
                    Span::raw(" "),
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
                ]))
            })
            .collect()
    };

    record_list_rows(app, list_area, 0, app.schedule.tasks.len());

    frame.render_widget(List::new(items), list_area);

    render_key_hint(frame, footer, "r 刷新");
}

/// Sign status text plus the color that carries its meaning.
///
/// Why:
/// This is the value a user scans for on the today list. Coloring it by state
/// lets them spot "can sign now" and "already ended" without reading each row.

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

/// Renders the iClass weekly grid plus the selected-course detail panel.
///
/// How:
/// The layout is split into session info, week info, seven day columns, one
/// detail block, and one status block. Selection highlighting is derived from
/// the absolute selected course index so horizontal and vertical navigation stay
/// aligned with the same underlying flat course list.

fn render_iclass(frame: &mut Frame, area: Rect, app: &App) {

    let [day_header, grid, detail_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(8),
        Constraint::Length(4),
    ])
    .areas(area);

    let columns = Layout::horizontal([Constraint::Ratio(1, 7); 7]).split(grid);

    let week_start = app
        .selected_week_group()
        .and_then(|group| NaiveDate::parse_from_str(&group.start_date, "%Y-%m-%d").ok());

    let labels = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];

    let today = Local::now().date_naive();

    // Day labels ride above each column so the grid keeps one frame per day
    // and the labels stay next to what they label.
    let header_columns = Layout::horizontal([Constraint::Ratio(1, 7); 7]).split(day_header);

    for (offset, column) in header_columns.iter().enumerate() {

        let label = match week_start {
            Some(start) => {

                let date = start + Duration::days(offset as i64);

                let is_today = date == today;

                Span::styled(
                    format!(" {} {}", labels[offset], date.format("%m/%d")),
                    if is_today {

                        theme::title_style()
                    } else {

                        theme::subtitle_style()
                    },
                )
            }
            None => Span::styled(format!(" {}", labels[offset]), theme::subtitle_style()),
        };

        frame.render_widget(Paragraph::new(Line::from(label)), *column);
    }

    let selected_absolute_index = app.selected_course_absolute_index();

    for (offset, column) in columns.iter().enumerate() {

        let Some(week_start) = week_start else {

            frame.render_widget(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(theme::BORDER_IDLE)),
                *column,
            );

            continue;
        };

        let date = week_start + Duration::days(offset as i64);

        let date_key = date.format("%Y-%m-%d").to_string();

        let courses_in_day: Vec<usize> = app
            .visible_course_indices()
            .iter()
            .copied()
            .filter(|index| app.courses[*index].date == date_key)
            .collect();

        let items = if courses_in_day.is_empty() {

            vec![ListItem::new(Span::styled("—", theme::muted_style()))]
        } else {

            courses_in_day
                .iter()
                .map(|index| {

                    let course = &app.courses[*index];

                    let selected = Some(*index) == selected_absolute_index;

                    let plain = |style: Style| if selected { Style::default() } else { style };

                    let lines = vec![
                        Line::from(Span::styled(
                            format!("{}-{}", course.start_time, course.end_time),
                            plain(theme::muted_style()),
                        )),
                        Line::from(Span::styled(
                            course.name.clone(),
                            plain(theme::text_style()),
                        )),
                        Line::from(Span::styled(
                            if course.signed() { "已签到" } else { "" },
                            plain(Style::default().fg(theme::OK)),
                        )),
                    ];

                    let item = ListItem::new(lines);

                    if selected {

                        item.style(theme::selection_style())
                    } else {

                        item
                    }
                })
                .collect()
        };

        let is_today = date == today;

        let day_list = List::new(items).block(Block::default().borders(Borders::ALL).border_style(
            Style::default().fg(if is_today {

                theme::BORDER_FOCUS
            } else {

                theme::BORDER_IDLE
            }),
        ));

        let selected_in_day = courses_in_day
            .iter()
            .position(|index| Some(*index) == selected_absolute_index);

        let mut list_state = ListState::default().with_selected(selected_in_day);

        frame.render_stateful_widget(day_list, *column, &mut list_state);

        // This grid is seven independent lists, so a flat row index would be
        // meaningless. Record each visible row as the absolute index of the
        // course it shows, offset by however far ListState scrolled.
        let body = Block::default().borders(Borders::ALL).inner(*column);

        let offset = list_state.offset();

        let visible = (body.height as usize).min(courses_in_day.len().saturating_sub(offset));

        for row in 0..visible {

            if let Some(absolute) = courses_in_day.get(offset + row) {

                app.record_hotspot(
                    Rect {
                        x:      body.x,
                        y:      body.y + row as u16,
                        width:  body.width,
                        height: 1,
                    },
                    HotAction::ListRow { index: *absolute },
                );
            }
        }
    }

    let detail_lines = if let Some(course) = app.selected_course() {

        vec![
            Line::from(vec![
                Span::raw(" "),
                Span::styled(course.name.clone(), theme::text_style()),
                Span::styled(
                    if course.signed() {

                        "  已签到"
                    } else {

                        "  未签到"
                    },
                    if course.signed() {

                        Style::default().fg(theme::OK)
                    } else {

                        Style::default().fg(theme::WARN)
                    },
                ),
            ]),
            Line::from(vec![
                Span::raw(" "),
                Span::styled(course.date.clone(), theme::muted_style()),
                sep(),
                Span::styled(
                    format!("{} - {}", course.start_time, course.end_time),
                    theme::muted_style(),
                ),
                sep(),
                Span::styled(course.course_sched_id.clone(), theme::muted_style()),
            ]),
            Line::from(Span::styled(
                " r 刷新  s 签到  g 终端二维码  G 外部二维码  H/L 切周  Shift+X 退出登录",
                theme::muted_style(),
            )),
            Line::from(vec![
                Span::styled(" 二维码输出: ", theme::muted_style()),
                Span::styled(
                    app.external_qr_path
                        .as_ref()
                        .map(|path| path.display().to_string())
                        .unwrap_or_else(|| "-".to_string()),
                    theme::text_style(),
                ),
                Span::styled(
                    format!(
                        "  {}",
                        app.external_qr_status.as_deref().unwrap_or("未启动")
                    ),
                    theme::muted_style(),
                ),
            ]),
        ]
    } else {

        vec![
            Line::from(""),
            Line::from(Span::styled(
                " 当前没有课程。r 刷新  H/L 切周",
                theme::muted_style(),
            )),
            Line::from(""),
            Line::from(""),
        ]
    };

    frame.render_widget(Paragraph::new(detail_lines), detail_area);
}

/// Renders the BYKC workspace with view tabs, list content, detail, and status.

/// Renders the BYKC workspace: a view switch row, the list, and a summary.

fn render_bykc(frame: &mut Frame, area: Rect, app: &App) {

    let [nav, list_area, summary_area] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Min(6),
        Constraint::Length(9),
    ])
    .areas(area);

    // View switch as one row, matching the course nav on the schedule tab.
    let views = [
        (BykcView::Courses, "1", "可选课程"),
        (BykcView::Chosen, "2", "已选课程"),
    ];

    let mut spans = Vec::new();

    let mut column = nav.x;

    for (view, key, label) in views {

        let item_width = display_width(&format!(" {key} {label} ")) as u16;

        if view == app.bykc.view {

            spans.push(Span::styled(
                format!(" {key} {label} "),
                theme::selection_style(),
            ));
        } else {

            spans.push(Span::styled(format!(" {key} "), theme::label_style()));

            spans.push(Span::styled(format!("{label} "), theme::muted_style()));
        }

        spans.push(Span::raw(" "));

        app.record_hotspot(
            Rect {
                x:      column,
                y:      nav.y,
                width:  item_width,
                height: 1,
            },
            HotAction::BykcView(view),
        );

        column += item_width + 1;
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), nav);

    match app.bykc.view {
        BykcView::Courses => render_bykc_courses_list(frame, list_area, app),
        BykcView::Chosen => render_bykc_chosen_list(frame, list_area, app),
    }

    render_bykc_detail(frame, summary_area, app);
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

    // ListState scrolls to keep the selection visible, so the first row on
    // screen is whatever offset it settled on, not zero. Read it back after
    // rendering so click targets line up with what was actually drawn.
    let body = Block::default().borders(Borders::ALL).inner(area);

    record_list_rows(app, body, state.offset(), app.bykc.courses.len());
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

    let body = Block::default().borders(Borders::ALL).inner(area);

    record_list_rows(app, body, state.offset(), app.bykc.chosen_courses.len());
}

/// Renders the inline BYKC summary under the course list.
///
/// Why:
/// This panel gets ten usable rows. The old version stacked twelve or more
/// label/value lines into it, so the tail was always clipped and it duplicated
/// the popup. A summary that fits, plus a pointer to `o` for the rest, tells
/// the reader more than a truncated copy of everything.
///
/// How:
/// Course name and sign-in verdict on the first line, then only the fields a
/// student checks while scrolling the list: when it runs, where, and what the
/// sign-in window is. Empty fields are omitted rather than rendered as dashes.

fn render_bykc_detail(frame: &mut Frame, area: Rect, app: &App) {

    let lines = if let Some(detail) = app.bykc.selected_cached_detail() {

        let (sign_text, sign_style) = bykc_sign_state(detail);

        let mut lines = vec![Line::from(vec![
            Span::styled(detail.course_name.clone(), theme::text_style()),
            Span::raw("  "),
            Span::styled(sign_text, sign_style),
        ])];

        lines.extend(field(
            "分类",
            bykc_category_label(&detail.category, &detail.sub_category),
        ));

        lines.extend(field("教师", detail.course_teacher.clone()));

        lines.extend(field("地点", detail.course_position.clone()));

        lines.extend(field(
            "档期",
            format!(
                "{} ~ {}",
                empty_dash(&detail.course_start_date),
                empty_dash(&detail.course_end_date)
            ),
        ));

        lines.extend(field("签到窗口", window_text(detail, true)));

        lines.extend(field("签退窗口", window_text(detail, false)));

        lines.extend(field(
            "人数",
            format!(
                "{}/{}",
                detail.course_current_count, detail.course_max_count
            ),
        ));

        lines.push(Line::from(Span::styled(
            "o 完整详情 | s 报名/签到 | u 签退 | x 退选 | a include_all",
            theme::muted_style(),
        )));

        lines
    } else {

        let fallback = match app.bykc.view {
            BykcView::Courses => {
                app.bykc.selected_course().map(|course| {

                    let mut lines = vec![Line::from(Span::styled(
                        course.course_name.clone(),
                        theme::text_style(),
                    ))];

                    lines.extend(field(
                        "分类",
                        bykc_category_label(&course.category, &course.sub_category),
                    ));

                    lines.extend(field("状态", course.status.clone()));

                    lines.extend(field("教师", course.course_teacher.clone()));

                    lines.extend(field("地点", course.course_position.clone()));

                    lines.extend(field(
                        "档期",
                        format!(
                            "{} ~ {}",
                            empty_dash(&course.course_start_date),
                            empty_dash(&course.course_end_date)
                        ),
                    ));

                    lines.extend(field(
                        "选课时间",
                        format!(
                            "{} ~ {}",
                            empty_dash(&course.course_select_start_date),
                            empty_dash(&course.course_select_end_date)
                        ),
                    ));

                    if let Some(hint) = deselect_hint(app, course) {

                        lines.extend(field("退选", hint));
                    }

                    lines.push(Line::from(Span::styled(
                        "o 完整详情 | s 报名 | x 退选",
                        theme::muted_style(),
                    )));

                    lines
                })
            }
            BykcView::Chosen => {
                app.bykc.selected_chosen_course().map(|course| {

                    let mut lines = vec![Line::from(Span::styled(
                        course.course_name.clone(),
                        theme::text_style(),
                    ))];

                    lines.extend(field(
                        "分类",
                        bykc_category_label(&course.category, &course.sub_category),
                    ));

                    lines.extend(field("教师", course.course_teacher.clone()));

                    lines.extend(field("地点", course.course_position.clone()));

                    lines.extend(field(
                        "档期",
                        format!(
                            "{} ~ {}",
                            empty_dash(&course.course_start_date),
                            empty_dash(&course.course_end_date)
                        ),
                    ));

                    if let Some(config) = course.sign_config.as_ref() {

                        let window = format!(
                            "{} ~ {}",
                            empty_dash(&config.sign_start_date),
                            empty_dash(&config.sign_end_date)
                        );

                        lines.extend(field("签到窗口", window));
                    }

                    lines.extend(field("签到备注", course.sign_info.clone()));

                    lines.push(Line::from(Span::styled(
                        "o 完整详情 | s 签到 | u 签退 | x 退选",
                        theme::muted_style(),
                    )));

                    lines
                })
            }
        };

        fallback.unwrap_or_else(|| {

            vec![Line::from(Span::styled(
                "当前没有可显示的博雅课程",
                theme::muted_style(),
            ))]
        })
    };

    let detail = Paragraph::new(lines)
        .block(
            Block::default()
                .title("详情")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER_IDLE))
                .title_style(theme::title_style()),
        )
        .wrap(Wrap { trim: true });

    frame.render_widget(detail, area);
}

/// Explains whether the selected course can still be dropped.

fn deselect_hint(app: &App, course: &crate::bykc::BykcCourse) -> Option<String> {

    if let Some(chosen) = app.bykc.chosen_course_for(course.id) {

        return Some(
            if can_deselect_bykc_course(&chosen.course_cancel_end_date) {

                format!(
                    "可退选，截止 {}",
                    empty_dash(&chosen.course_cancel_end_date)
                )
            } else {

                format!(
                    "已过退选时间 {}",
                    empty_dash(&chosen.course_cancel_end_date)
                )
            },
        );
    }

    course
        .selected
        .then(|| "已报，但未找到退选记录".to_string())
}

/// Renders the BYKC course detail as a single, grouped panel.
///
/// Why:
/// The previous layout drew two nested boxes ("BYKC 详情" around "详细信息"),
/// which wasted the frame, cramped the body, and said nothing the title could
/// not. Fourteen fields were stacked in one flat list, so sign-in windows --
/// the reason to open this at all -- sat below three date ranges, and empty
/// fields printed as "-" columns of noise.
///
/// How:
/// One border, a live sign-in summary in the title, then fields grouped by what
/// the reader wants to know, in the order they want it. Blank fields disappear
/// entirely, values are colored by meaning, and the body scrolls so a long
/// description is reachable instead of being silently clipped.

fn render_bykc_detail_popup(frame: &mut Frame, app: &App) {

    let Some(detail) = app.bykc.selected_cached_detail() else {

        return;
    };

    // Take the whole screen rather than floating a three-quarter box over the
    // list. A floating box left the workspace's own borders and text visible on
    // every side, which read as the detail being covered even though it was
    // drawn last. Clearing the entire frame removes the background outright, so
    // there is no competing text to be confused with the panel's own.
    frame.render_widget(Clear, frame.area());

    let area = frame.area();

    let (sign_text, sign_style) = bykc_sign_state(detail);

    let block = Block::default()
        .title(Line::from(vec![
            Span::styled(" 选课详情 ", theme::title_style()),
            Span::styled(detail.course_name.clone(), Style::default().fg(theme::TEXT)),
            Span::styled("  ", theme::muted_style()),
            Span::styled(sign_text, sign_style),
            Span::raw(" "),
        ]))
        .borders(Borders::ALL)
        .border_style(Style::default().fg(theme::BORDER_FOCUS));

    let inner = block.inner(area);

    frame.render_widget(block, area);

    // Reserve the last inner row for the scroll hint so the body never draws
    // underneath it.
    let [body_area, hint_area] =
        Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas(inner);

    let mut lines: Vec<Line> = Vec::new();

    lines.push(section("课程"));

    lines.extend(field(
        "分类",
        bykc_category_label(&detail.category, &detail.sub_category),
    ));

    lines.extend(field("教师", detail.course_teacher.clone()));

    lines.extend(field_styled(
        "地点",
        detail.course_position.clone(),
        Style::default().fg(theme::INFO),
    ));

    if let Some(contact) = contact_line(detail) {

        lines.extend(field("联系", contact));
    }

    lines.extend(field(
        "档期",
        format!(
            "{} ~ {}",
            empty_dash(&detail.course_start_date),
            empty_dash(&detail.course_end_date)
        ),
    ));

    lines.push(Line::from(""));

    lines.push(section("签到"));

    lines.extend(field(
        "模式",
        bykc_sign_type_label(detail.course_sign_type).to_string(),
    ));

    lines.extend(field(
        "自主签到",
        bykc_self_sign_value(
            detail
                .sign_config
                .as_ref()
                .is_some_and(|config| !config.sign_points.is_empty()),
        )
        .to_string(),
    ));

    lines.extend(field_styled(
        "签到窗口",
        window_text(detail, true),
        window_style(detail, true),
    ));

    lines.extend(field_styled(
        "签退窗口",
        window_text(detail, false),
        window_style(detail, false),
    ));

    if let Some(config) = detail.sign_config.as_ref()
        && !config.sign_points.is_empty()
    {

        lines.extend(field(
            "签到点数",
            format!("{} 个", config.sign_points.len()),
        ));
    }

    lines.push(Line::from(""));

    lines.push(section("选课"));

    lines.extend(field_styled(
        "状态",
        detail.status.clone(),
        bykc_status_style(&detail.status),
    ));

    lines.extend(field(
        "选课时间",
        format!(
            "{} ~ {}",
            empty_dash(&detail.course_select_start_date),
            empty_dash(&detail.course_select_end_date)
        ),
    ));

    lines.extend(field_styled(
        "退选截止",
        empty_dash(&detail.course_cancel_end_date),
        if can_deselect_bykc_course(&detail.course_cancel_end_date) {

            Style::default().fg(theme::WARN)
        } else {

            theme::muted_style()
        },
    ));

    lines.extend(field_styled(
        "人数",
        format!(
            "{}/{}",
            detail.course_current_count, detail.course_max_count
        ),
        capacity_style(detail.course_current_count, detail.course_max_count),
    ));

    if !detail.course_desc.trim().is_empty() {

        lines.push(Line::from(""));

        lines.push(section("简介"));

        for paragraph in detail.course_desc.lines() {

            lines.push(Line::from(Span::styled(
                paragraph.to_string(),
                theme::text_style(),
            )));
        }
    }

    let body = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((app.bykc.detail_scroll, 0));

    frame.render_widget(body, body_area);

    render_scroll_hint(frame, hint_area, app);
}

/// A section heading inside the detail panel.
///
/// Why:
/// Grouping is what makes fourteen fields scannable. A colored label plus a rule
/// marks the boundary without spending a whole line on decoration.

fn section(title: &str) -> Line<'static> {

    Line::from(vec![
        Span::styled(title.to_string(), theme::label_style()),
        Span::styled("  ", theme::muted_style()),
        Span::styled("─".repeat(24), Style::default().fg(theme::BORDER_IDLE)),
    ])
}

/// A label/value row, or nothing at all when the value is empty.
///
/// Why:
/// An absent value carries no information, and printing "-" for it turns the
/// panel into a wall of dashes that hides the fields that do have content.

fn field(label: &str, value: impl Into<String>) -> Option<Line<'static>> {

    field_styled(label, value, theme::text_style())
}

/// A label/value row with a caller-chosen value style.
///
/// Why:
/// Most rows are neutral, but a handful carry the answer to "can I act on
/// this now": status, sign windows, capacity, deadlines. Coloring those by
/// meaning lets the panel be scanned instead of read.

fn field_styled(
    label: &str,
    value: impl Into<String>,
    value_style: Style,
) -> Option<Line<'static>> {

    let value = value.into();

    let value = value.trim();

    if value.is_empty() {

        return None;
    }

    Some(Line::from(vec![
        Span::styled(format!("  {label:<10}"), theme::muted_style()),
        Span::styled(value.to_string(), value_style),
    ]))
}

/// Style for a BYKC selection status string.
///
/// Why:
/// "已报" and "已过退选" are the two states that change what the user can do
/// next, so they get color; everything else stays neutral.

fn bykc_status_style(status: &str) -> Style {

    if status.contains("已报") || status.contains("已选") {

        Style::default().fg(theme::OK)
    } else if status.contains("已满") || status.contains("过期") || status.contains("截止") {

        Style::default().fg(theme::ERROR)
    } else if status.contains("可报") || status.contains("报名中") {

        Style::default().fg(theme::ACCENT)
    } else {

        theme::text_style()
    }
}

/// Style for a sign or sign-out window, by whether it is open now.
///
/// Why:
/// The window text is a date range, and whether it is currently open is the
/// only thing that matters about it. Parsing the range and coloring it by
/// open/upcoming/closed saves the reader from comparing timestamps by eye.

fn window_style(detail: &crate::bykc::BykcCourseDetail, sign_in: bool) -> Style {

    let Some(config) = detail.sign_config.as_ref() else {

        return theme::muted_style();
    };

    let (start, end) = if sign_in {

        (&config.sign_start_date, &config.sign_end_date)
    } else {

        (&config.sign_out_start_date, &config.sign_out_end_date)
    };

    if start.trim().is_empty() && end.trim().is_empty() {

        return theme::muted_style();
    }

    match (
        parse_bykc_datetime(start.as_str()),
        parse_bykc_datetime(end.as_str()),
    ) {
        (Some(opened), Some(closes)) => {

            let now = Local::now().naive_local();

            if now < opened {

                Style::default().fg(theme::INFO)
            } else if now > closes {

                theme::muted_style()
            } else {

                Style::default().fg(theme::OK).add_modifier(Modifier::BOLD)
            }
        }
        _ => theme::text_style(),
    }
}

/// Parses the timestamp formats BYKC uses in sign windows.

fn parse_bykc_datetime(value: &str) -> Option<chrono::NaiveDateTime> {

    let value = value.trim();

    for format in ["%Y-%m-%d %H:%M:%S", "%Y-%m-%d %H:%M"] {

        if let Ok(parsed) = chrono::NaiveDateTime::parse_from_str(value, format) {

            return Some(parsed);
        }
    }

    // Date-only values mean midnight, which is what the upstream sends when a
    // window has no time component.
    NaiveDate::parse_from_str(value, "%Y-%m-%d")
        .ok()
        .and_then(|date| date.and_hms_opt(0, 0, 0))
}

/// Style for a `current/max` capacity string.

fn capacity_style(current: i32, max: i32) -> Style {

    if max <= 0 {

        return theme::muted_style();
    }

    if current >= max {

        Style::default().fg(theme::ERROR)
    } else if current * 10 >= max * 9 {

        Style::default().fg(theme::WARN)
    } else {

        Style::default().fg(theme::OK)
    }
}

/// Joins the contact name and phone when either is present.

fn contact_line(detail: &crate::bykc::BykcCourseDetail) -> Option<String> {

    let name = detail.course_contact.trim();

    let phone = detail.course_contact_mobile.trim();

    match (name.is_empty(), phone.is_empty()) {
        (true, true) => None,
        (false, true) => Some(name.to_string()),
        (true, false) => Some(phone.to_string()),
        (false, false) => Some(format!("{name} {phone}")),
    }
}

/// Formats a sign or sign-out window, preferring the configured points.

fn window_text(detail: &crate::bykc::BykcCourseDetail, sign_in: bool) -> String {

    let Some(config) = detail.sign_config.as_ref() else {

        return String::new();
    };

    let (start, end) = if sign_in {

        (&config.sign_start_date, &config.sign_end_date)
    } else {

        (&config.sign_out_start_date, &config.sign_out_end_date)
    };

    if start.trim().is_empty() && end.trim().is_empty() {

        return String::new();
    }

    format!("{} ~ {}", empty_dash(start), empty_dash(end))
}

/// The one-line sign-in verdict shown in the panel title.
///
/// Why:
/// Whether signing is possible right now is the question this screen exists to
/// answer, so it belongs where the eye lands first rather than seven rows down.

fn bykc_sign_state(detail: &crate::bykc::BykcCourseDetail) -> (String, Style) {

    if detail.can_sign {

        return ("● 可签到".to_string(), Style::default().fg(theme::OK));
    }

    if detail.can_sign_out {

        return ("● 可签退".to_string(), Style::default().fg(theme::OK));
    }

    let has_window = detail.sign_config.as_ref().is_some_and(|config| {

        !config.sign_start_date.trim().is_empty() || !config.sign_end_date.trim().is_empty()
    });

    if has_window {

        (
            "○ 当前不在签到窗口".to_string(),
            Style::default().fg(theme::MUTED),
        )
    } else {

        ("○ 无需签到".to_string(), Style::default().fg(theme::MUTED))
    }
}

/// Draws the scroll affordance on the panel's last inner row.
///
/// Why:
/// A clipped body with no hint reads as "that is all there is". Showing the
/// offset confirms the view actually moved.

fn render_scroll_hint(frame: &mut Frame, hint_area: Rect, app: &App) {

    let mut spans = vec![Span::styled("j/k 或 ↑↓ 滚动", theme::muted_style())];

    if app.bykc.detail_scroll > 0 {

        spans.push(Span::styled(
            format!("  已滚动 {} 行", app.bykc.detail_scroll),
            Style::default().fg(theme::INFO),
        ));
    }

    spans.push(Span::styled("    esc/o/enter 关闭", theme::muted_style()));

    frame.render_widget(Paragraph::new(Line::from(spans)), hint_area);
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

    // The one place a spinner earns its keep: a blocking wait that would
    // otherwise be indistinguishable from a hung process.
    let popup = Paragraph::new(Line::from(vec![theme::activity_badge(
        app.tick,
        true,
        "处理中，请稍候",
    )]))
    .block(
        Block::default()
            .title("请稍候")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(theme::BORDER_FOCUS))
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
        .block(
            Block::default()
                .title("事件日志")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(theme::BORDER_FOCUS))
                .title_style(theme::title_style()),
        )
        .wrap(Wrap { trim: true })
        .scroll((app.event_log_offset as u16, 0));

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

#[cfg(test)]

mod tests {

    use super::*;
    use crate::bykc::{BykcCourseDetail, BykcSignConfig};
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn fixture() -> BykcCourseDetail {

        BykcCourseDetail {
            id: 1,
            course_name: "航空发动机原理".to_string(),
            course_position: "沙河校区 J3-201".to_string(),
            course_contact: "张老师".to_string(),
            course_contact_mobile: "13800000000".to_string(),
            course_teacher: "张老师".to_string(),
            course_start_date: "2026-03-02".to_string(),
            course_end_date: "2026-06-19".to_string(),
            course_select_start_date: "2026-02-20".to_string(),
            course_select_end_date: "2026-02-25".to_string(),
            course_cancel_end_date: "2026-03-08".to_string(),
            course_max_count: 80,
            course_current_count: 63,
            category: "博雅".to_string(),
            sub_category: "工程".to_string(),
            status: "已报".to_string(),
            selected: true,
            // Long enough to prove the body scrolls instead of clipping.
            course_desc: (1..=40)
                .map(|index| format!("第 {index} 行课程简介内容"))
                .collect::<Vec<_>>()
                .join("\n"),
            course_sign_type: Some(1),
            sign_config: Some(BykcSignConfig {
                sign_start_date:     "2026-03-02 08:00".to_string(),
                sign_end_date:       "2026-03-02 08:30".to_string(),
                sign_out_start_date: "2026-03-02 11:00".to_string(),
                sign_out_end_date:   "2026-03-02 11:30".to_string(),
                sign_points:         Vec::new(),
            }),
            checkin: Some(0),
            pass: None,
            can_sign: true,
            can_sign_out: false,
        }
    }

    /// Renders one frame and returns the visible text, rows joined by newlines,
    /// with all spaces removed.
    ///
    /// Why:
    /// A wide glyph occupies two cells and the second holds a filler space, and
    /// alignment padding surrounds every label. Dropping spaces leaves exactly
    /// the visible content, which is what these tests assert on rather than
    /// the exact column layout.

    fn render_text(width: u16, height: u16, draw: impl FnOnce(&mut Frame)) -> String {

        let backend = TestBackend::new(width, height);

        let mut terminal = Terminal::new(backend).expect("终端应可创建");

        terminal.draw(draw).expect("应可渲染");

        let buffer = terminal.backend().buffer().clone();

        (0..buffer.area.height)
            .map(|y| {

                (0..buffer.area.width)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
                    .replace([' ', '\u{3000}'], "")
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Renders into `app` so hotspots land on it, then returns the text.

    fn render_into(app: &App, width: u16, height: u16) -> String {

        render_text(width, height, |frame| render_workspace(frame, app))
    }

    #[test]

    fn tab_hotspots_cover_the_labels_that_are_drawn() {

        let mut app = App::default();

        app.screen = Screen::Workspace;

        let width = 100;

        let output = render_into(&app, width, 30);

        let hotspots = app.hotspots.borrow().clone();

        let tab_hotspots: Vec<_> = hotspots
            .iter()
            .filter(|hotspot| matches!(hotspot.action, HotAction::WorkspaceTab(_)))
            .collect();

        assert_eq!(tab_hotspots.len(), 3, "应有三个页签热区：\n{output}");

        for hotspot in &tab_hotspots {

            assert_eq!(hotspot.area.y, 0, "页签应在第一行");

            assert_eq!(hotspot.area.height, 1);

            // Every hotspot must sit inside the drawn row.
            assert!(
                hotspot.area.x + hotspot.area.width <= width,
                "热区超出屏幕：{:?}",
                hotspot.area
            );
        }

        // Hotspots must not overlap each other.
        for pair in tab_hotspots.windows(2) {

            let (left, right) = (pair[0], pair[1]);

            assert!(
                left.area.x + left.area.width <= right.area.x,
                "页签热区重叠：{:?} 与 {:?}",
                left.area,
                right.area
            );
        }
    }

    #[test]

    fn tab_hotspot_starts_after_the_brand_text() {

        let mut app = App::default();

        app.screen = Screen::Workspace;

        render_into(&app, 100, 30);

        let hotspots = app.hotspots.borrow().clone();

        let first = hotspots
            .iter()
            .find(|hotspot| matches!(hotspot.action, HotAction::WorkspaceTab(_)))
            .expect("应有页签热区");

        // " iClass BUAA  " is 14 cells, so the first tab label starts there.
        assert_eq!(
            first.area.x, 14,
            "首个页签热区应紧接应用名之后：{:?}",
            first.area
        );
    }

    #[test]

    fn course_nav_hotspots_cover_all_six_views() {

        let mut app = App::default();

        app.screen = Screen::Workspace;

        app.active_tab = WorkspaceTab::Schedule;

        render_into(&app, 100, 30);

        let hotspots = app.hotspots.borrow().clone();

        let nav: Vec<_> = hotspots
            .iter()
            .filter(|hotspot| matches!(hotspot.action, HotAction::CourseView(_)))
            .collect();

        assert_eq!(nav.len(), 6, "六个课程视图都应有热区");

        for pair in nav.windows(2) {

            assert!(
                pair[0].area.x + pair[0].area.width <= pair[1].area.x,
                "导航热区重叠：{:?} 与 {:?}",
                pair[0].area,
                pair[1].area
            );
        }
    }

    #[test]

    fn clicking_a_tab_hotspot_switches_the_tab() {

        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

        let mut app = App::default();

        app.screen = Screen::Workspace;

        app.active_tab = WorkspaceTab::IClass;

        render_into(&app, 100, 30);

        // Find the BYKC tab's recorded region and click its first cell.
        let target = app
            .hotspots
            .borrow()
            .iter()
            .find(|hotspot| hotspot.action == HotAction::WorkspaceTab(WorkspaceTab::Bykc))
            .map(|hotspot| hotspot.area)
            .expect("应有 BYKC 页签热区");

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        app.handle_mouse(
            MouseEvent {
                kind:      MouseEventKind::Down(MouseButton::Left),
                column:    target.x,
                row:       target.y,
                modifiers: KeyModifiers::NONE,
            },
            &tx,
        );

        assert_eq!(app.active_tab, WorkspaceTab::Bykc, "点击页签应切换到 BYKC");
    }

    #[test]

    fn clicking_outside_any_hotspot_does_nothing() {

        use crossterm::event::{KeyModifiers, MouseButton, MouseEvent, MouseEventKind};

        let mut app = App::default();

        app.screen = Screen::Workspace;

        app.active_tab = WorkspaceTab::IClass;

        render_into(&app, 100, 30);

        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel();

        // Bottom-right corner of the content frame: border, no widget.
        app.handle_mouse(
            MouseEvent {
                kind:      MouseEventKind::Down(MouseButton::Left),
                column:    99,
                row:       28,
                modifiers: KeyModifiers::NONE,
            },
            &tx,
        );

        assert_eq!(
            app.active_tab,
            WorkspaceTab::IClass,
            "空白处点击不应改变状态"
        );
    }

    #[test]

    fn bykc_detail_popup_is_not_overlapped_by_workspace_text() {

        let detail = fixture();

        let mut app = app_with_detail(detail);

        app.screen = Screen::Workspace;

        app.active_tab = WorkspaceTab::Bykc;

        for height in [18u16, 24, 30, 40, 50] {

            let out = render_text(100, height, |frame| render(frame, &app));

            // The panel takes the whole screen, so no workspace chrome may
            // survive anywhere in the frame.
            for (row, line) in out.lines().enumerate() {

                for chrome in ["iClassBUAA", "可选课程", "课程列表", "tab切换"] {

                    assert!(
                        !line.contains(chrome),
                        "第 {row} 行仍有工作区内容 {chrome}，详情面板未独占屏幕：\n{out}"
                    );
                }
            }

            assert!(out.contains("选课详情"), "详情面板应占据整屏：\n{out}");

            let _ = height;
        }
    }

    #[test]

    fn workspace_chrome_is_borderless_rows_around_one_frame() {

        let mut app = App::default();

        app.screen = Screen::Workspace;

        let output = render_text(110, 30, |frame| render_workspace(frame, &app));

        let rows: Vec<&str> = output.lines().collect();

        assert!(
            rows[0].contains("iClassBUAA"),
            "顶栏应显示应用名：\n{output}"
        );

        assert!(!rows[0].contains('┌'), "顶栏不应有边框：\n{output}");

        assert!(!rows[1].contains('┌'), "状态行不应有边框：\n{output}");

        assert!(
            rows[2].starts_with('┌'),
            "内容区应从第三行开始加框：\n{output}"
        );

        let last = rows.last().expect("应有页脚");

        assert!(last.contains("切换"), "页脚应给出快捷键：\n{output}");

        assert!(!last.contains('└'), "页脚不应是边框：\n{output}");

        for gone in ["工作区", "切换提示", "会话", "周视图", "事件"] {

            assert!(!output.contains(gone), "旧框标题 {gone} 仍在：\n{output}");
        }
    }

    #[test]

    fn workspace_frame_count_is_bounded_per_tab() {

        // Count top-left corners as a proxy for bordered boxes. The iClass grid
        // legitimately frames seven day columns; nothing else should add any.
        let mut app = App::default();

        app.screen = Screen::Workspace;

        app.active_tab = WorkspaceTab::Bykc;

        let bykc = render_text(110, 30, |frame| render_workspace(frame, &app));

        let bykc_boxes = bykc.matches('┌').count();

        assert!(
            bykc_boxes <= 3,
            "BYKC 页边框数应为内容框加课程/详情框，实际 {bykc_boxes}：\n{bykc}"
        );

        app.active_tab = WorkspaceTab::IClass;

        let iclass = render_text(110, 30, |frame| render_workspace(frame, &app));

        let iclass_boxes = iclass.matches('┌').count();

        assert!(
            iclass_boxes <= 8,
            "iClass 页最多内容框加七个日列，实际 {iclass_boxes}：\n{iclass}"
        );
    }

    fn render_popup(app: &App, width: u16, height: u16) -> String {

        render_text(width, height, |frame| render_bykc_detail_popup(frame, app))
    }

    fn app_with_detail(detail: BykcCourseDetail) -> App {

        let mut app = App::default();

        // The popup resolves its target through the list selection, so the
        // course must exist in the list for the cached detail to be found.
        app.bykc.courses.push(crate::bykc::BykcCourse {
            id: detail.id,
            course_name: detail.course_name.clone(),
            ..Default::default()
        });

        app.bykc.selected_course = 0;

        app.bykc.detail_course_id = Some(detail.id);

        app.bykc.detail_cache.insert(detail.id, detail);

        app.bykc.show_detail_popup = true;

        app
    }

    /// Returns the foreground color of the first character of `needle`.
    ///
    /// How:
    /// Wide glyphs occupy two cells and the second holds a filler space, and
    /// labels are padded with real spaces. Both are dropped, leaving the visible
    /// characters in order; the needle is located in that string and mapped back
    /// to the cell it came from.

    fn color_of(width: u16, height: u16, app: &App, needle: &str) -> Option<Color> {

        let backend = TestBackend::new(width, height);

        let mut terminal = Terminal::new(backend).expect("终端应可创建");

        terminal
            .draw(|frame| render_bykc_detail_popup(frame, app))
            .expect("应可渲染");

        let buffer = terminal.backend().buffer().clone();

        for y in 0..buffer.area.height {

            let mut text = String::new();

            let mut source: Vec<(usize, u16)> = Vec::new();

            for x in 0..buffer.area.width {

                let symbol = buffer[(x, y)].symbol();

                if symbol.is_empty() || symbol == " " {

                    continue;
                }

                for character in symbol.chars() {

                    text.push(character);

                    source.push((text.chars().count() - 1, x));
                }
            }

            if let Some(byte_index) = text.find(needle) {

                let char_index = text[..byte_index].chars().count();

                if let Some((_, column)) = source.iter().find(|(index, _)| *index == char_index) {

                    return Some(buffer[(*column, y)].fg);
                }
            }
        }

        None
    }

    #[test]

    fn detail_popup_highlights_status_and_capacity_by_meaning() {

        let signed_up = app_with_detail(fixture());

        // "已报" is a positive state and must not render in plain body text.
        let status = color_of(100, 40, &signed_up, "已报").expect("应找到状态");

        assert_eq!(status, theme::OK, "已报应用 OK 色高亮");

        // 63/80 is under 90% capacity, so it is comfortably open.
        let capacity = color_of(100, 40, &signed_up, "63/80").expect("应找到人数");

        assert_eq!(capacity, theme::OK, "未满的人数应用 OK 色");

        // A full course must read as a problem.
        let full = app_with_detail(BykcCourseDetail {
            course_current_count: 80,
            ..fixture()
        });

        let capacity = color_of(100, 40, &full, "80/80").expect("应找到人数");

        assert_eq!(capacity, theme::ERROR, "已满的人数应用 ERROR 色");

        // A place is scanned for, so it gets the info color rather than body.
        let place = color_of(100, 40, &signed_up, "沙河").expect("应找到地点");

        assert_eq!(place, theme::INFO, "地点应用 INFO 色");
    }

    #[test]

    fn detail_popup_draws_a_single_frame_and_shows_sign_state() {

        let app = app_with_detail(fixture());

        let output = render_popup(&app, 90, 28);

        // One border only: the old layout nested "BYKC 详情" around "详细信息".
        assert!(
            !output.contains("详细信息") && !output.contains("BYKC 详情"),
            "浮窗不应再有两层标题：\n{output}"
        );

        // The actionable verdict belongs on the first line.
        assert!(output.contains("可签到"), "标题应显示签到状态：\n{output}");

        assert!(
            output.contains("航空发动机原理"),
            "应显示课程名：\n{output}"
        );

        // Grouped sections rather than one flat list.
        for heading in ["课程", "签到", "选课"] {

            assert!(output.contains(heading), "缺少分组 {heading}：\n{output}");
        }

        assert!(
            output.contains("13800000000"),
            "联系方式应合并显示：\n{output}"
        );

        assert!(output.contains("63/80"), "应显示人数：\n{output}");
    }

    #[test]

    fn detail_popup_omits_empty_fields_instead_of_dashing_them() {

        let detail = BykcCourseDetail {
            course_teacher: String::new(),
            course_position: String::new(),
            course_contact: String::new(),
            course_contact_mobile: String::new(),
            course_desc: String::new(),
            sign_config: None,
            ..fixture()
        };

        let app = app_with_detail(detail);

        let output = render_popup(&app, 90, 28);

        assert!(!output.contains("教师"), "空教师字段应整行省略：\n{output}");

        assert!(!output.contains("联系"), "空联系字段应整行省略：\n{output}");

        assert!(
            !output.contains("简介"),
            "空简介不应产生分组标题：\n{output}"
        );
    }

    #[test]

    fn detail_popup_reaches_the_end_of_a_long_description() {

        let mut app = app_with_detail(fixture());

        // The fixture description is far taller than any terminal, so the last
        // line must only become visible after scrolling.
        let last = "第40行课程简介内容";

        let top = render_popup(&app, 100, 30);

        assert!(!top.contains(last), "简介末行本应需要滚动才能看到：\n{top}");

        let mut found = None;

        for offset in 1..=80 {

            app.bykc.detail_scroll = offset;

            if render_popup(&app, 100, 30).contains(last) {

                found = Some(offset);

                break;
            }
        }

        assert!(found.is_some(), "滚动后应能看到简介末行");

        // Scrolling past the end must still show the footer hint rather than
        // painting garbage.
        app.bykc.detail_scroll = 500;

        let past_end = render_popup(&app, 100, 30);

        assert!(
            past_end.contains("esc/o/enter关闭"),
            "滚动超出末尾后页脚提示应仍在：\n{past_end}"
        );
    }
}
