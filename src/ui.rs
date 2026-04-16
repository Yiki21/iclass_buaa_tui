use chrono::{Duration, NaiveDate, TimeZone};
use qrcode::QrCode;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Tabs, Wrap},
};
use tui_qrcode::{QrCodeWidget, QuietZone, Scaling};

use crate::app::{App, BykcView, LoginFocus, Screen, WorkspaceTab};

pub fn render(frame: &mut Frame, app: &App) {
    match app.screen {
        Screen::Login => render_login(frame, app),
        Screen::Workspace => render_workspace(frame, app),
    }

    if app.busy {
        render_busy_popup(frame);
    } else if app.active_tab == WorkspaceTab::IClass && app.qr_display.is_some() {
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
        .border_style(Style::default().fg(Color::Cyan));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let mut constraints = vec![Constraint::Length(3), Constraint::Length(3)];
    if app.login.use_vpn {
        constraints.push(Constraint::Length(3));
        constraints.push(Constraint::Length(3));
    } else {
        constraints.push(Constraint::Length(3));
    }
    constraints.push(Constraint::Min(3));

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints(constraints)
        .split(inner);

    let title = Paragraph::new(vec![
        Line::from(Span::styled(
            "Ratatui + Tokio 终端工具",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("登录后可在 iClass 与 BYKC 间切换"),
        Line::from("VPN 模式下直接使用 VPN 账号登录，不再单独输入学号"),
        Line::from("tab 切换字段，space 切换 VPN，enter 登录，? 帮助，q 退出"),
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

    let status_index = if app.login.use_vpn {
        render_input(
            frame,
            chunks[2],
            "VPN 账号",
            &app.login.vpn_username,
            app.login.current_focus() == LoginFocus::VpnUsername,
            false,
        );
        render_input(
            frame,
            chunks[3],
            "VPN 密码",
            &mask_password(&app.login.vpn_password),
            app.login.current_focus() == LoginFocus::VpnPassword,
            true,
        );
        4
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

    let status = Paragraph::new(app.status.as_str())
        .block(Block::default().title("状态").borders(Borders::ALL))
        .wrap(Wrap { trim: true });
    frame.render_widget(status, chunks[status_index]);
}

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
        WorkspaceTab::IClass => render_iclass(frame, chunks[2], app),
        WorkspaceTab::Bykc => render_bykc(frame, chunks[2], app),
    }
}

fn render_workspace_tabs(frame: &mut Frame, area: Rect, app: &App) {
    let titles = [" iClass ", " BYKC "]
        .into_iter()
        .map(Line::from)
        .collect::<Vec<_>>();
    let selected = match app.active_tab {
        WorkspaceTab::IClass => 0,
        WorkspaceTab::Bykc => 1,
    };

    let tabs = Tabs::new(titles)
        .block(
            Block::default()
                .title("工作区 | tab / shift+tab 切换")
                .borders(Borders::ALL),
        )
        .select(selected)
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(tabs, area);
}

fn render_workspace_hint(frame: &mut Frame, area: Rect, app: &App) {
    let current = match app.active_tab {
        WorkspaceTab::IClass => "iClass",
        WorkspaceTab::Bykc => "BYKC",
    };
    let hint = Paragraph::new(format!(
        "当前页: {current} | tab: 下一个标签 | shift+tab: 上一个标签 | ?: 帮助"
    ))
    .block(Block::default().title("切换提示").borders(Borders::ALL))
    .wrap(Wrap { trim: true });
    frame.render_widget(hint, area);
}

fn render_iclass(frame: &mut Frame, area: Rect, app: &App) {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(8),
            Constraint::Length(4),
        ])
        .split(area);

    let header_text = if let Some(session) = &app.session {
        format!(
            "用户: {} ({}) | 模式: {} | 共 {} 条课程",
            session.user_name,
            session.user_id,
            if session.use_vpn { "VPN" } else { "直连" },
            app.courses.len()
        )
    } else {
        "未登录".to_string()
    };
    let header =
        Paragraph::new(header_text).block(Block::default().title("会话").borders(Borders::ALL));
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

    for (offset, area) in day_columns.into_iter().enumerate() {
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
                        Style::default().fg(Color::Green)
                    } else {
                        Style::default()
                    };
                    if Some(*index) == selected_absolute_index {
                        style = style
                            .fg(Color::Black)
                            .bg(Color::Cyan)
                            .add_modifier(Modifier::BOLD);
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
                Span::styled("课程: ", Style::default().fg(Color::Yellow)),
                Span::raw(course.name.as_str()),
            ]),
            Line::from(vec![
                Span::styled("日期: ", Style::default().fg(Color::Yellow)),
                Span::raw(course.date.as_str()),
            ]),
            Line::from(vec![
                Span::styled("时间: ", Style::default().fg(Color::Yellow)),
                Span::raw(format!("{} - {}", course.start_time, course.end_time)),
            ]),
            Line::from(vec![
                Span::styled("签到: ", Style::default().fg(Color::Yellow)),
                Span::raw(if course.signed() {
                    "已签到"
                } else {
                    "未签到"
                }),
            ]),
            Line::from(vec![
                Span::styled("courseSchedId: ", Style::default().fg(Color::Yellow)),
                Span::raw(course.course_sched_id.as_str()),
            ]),
            Line::from(""),
            Line::from("操作: r 刷新 | s 直接签到 | g 二维码签到 | Shift+X 退出登录"),
        ]
    } else {
        vec![Line::from("当前没有课程")]
    };

    let detail = Paragraph::new(detail_lines)
        .block(Block::default().title("详情").borders(Borders::ALL))
        .wrap(Wrap { trim: true });
    frame.render_widget(detail, vertical[3]);

    let status = Paragraph::new(app.status.as_str())
        .block(Block::default().title("状态").borders(Borders::ALL))
        .wrap(Wrap { trim: true });
    frame.render_widget(status, vertical[4]);
}

fn render_bykc(frame: &mut Frame, area: Rect, app: &App) {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(12),
            Constraint::Length(4),
        ])
        .split(area);

    let header_text = if let Some(session) = &app.session {
        format!(
            "用户: {} ({}) | VPN: {} | 可选 {} 门 | 已选 {} 门",
            session.user_name,
            session.user_id,
            if session.use_vpn { "开启" } else { "关闭" },
            app.bykc.courses.len(),
            app.bykc.chosen_courses.len(),
        )
    } else {
        "未登录".to_string()
    };
    let header = Paragraph::new(header_text)
        .block(Block::default().title("BYKC 会话").borders(Borders::ALL));
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
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Green)
                .add_modifier(Modifier::BOLD),
        );
    frame.render_widget(tabs, vertical[1]);

    match app.bykc.view {
        BykcView::Courses => render_bykc_courses_list(frame, vertical[2], app),
        BykcView::Chosen => render_bykc_chosen_list(frame, vertical[2], app),
    }

    render_bykc_detail(frame, vertical[3], app);

    let status = Paragraph::new(app.status.as_str())
        .block(Block::default().title("状态").borders(Borders::ALL))
        .wrap(Wrap { trim: true });
    frame.render_widget(status, vertical[4]);
}

fn render_bykc_courses_list(frame: &mut Frame, area: Rect, app: &App) {
    let items = if app.bykc.courses.is_empty() {
        vec![ListItem::new("暂无课程")]
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
                        } else if can_deselect_bykc_course_label(&chosen.course_cancel_end_date) {
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
                    "[{}] {} | {} | {}/{}",
                    selected_tag,
                    course.course_name,
                    if course.course_teacher.is_empty() {
                        "未知教师"
                    } else {
                        course.course_teacher.as_str()
                    },
                    course.course_current_count,
                    course.course_max_count,
                );
                ListItem::new(label)
            })
            .collect()
    };

    let list = List::new(items)
        .block(Block::default().title("课程列表").borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    let mut state = ListState::default()
        .with_selected((!app.bykc.courses.is_empty()).then_some(app.bykc.selected_course));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_bykc_chosen_list(frame: &mut Frame, area: Rect, app: &App) {
    let items = if app.bykc.chosen_courses.is_empty() {
        vec![ListItem::new("暂无已选课程")]
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
                let label = format!(
                    "[{}] {} | checkin={} | {}",
                    attendance, course.course_name, course.checkin, course.sign_info
                );
                ListItem::new(label)
            })
            .collect()
    };

    let list = List::new(items)
        .block(Block::default().title("已选课程").borders(Borders::ALL))
        .highlight_style(
            Style::default()
                .fg(Color::Black)
                .bg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        );
    let mut state = ListState::default()
        .with_selected((!app.bykc.chosen_courses.is_empty()).then_some(app.bykc.selected_chosen));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_bykc_detail(frame: &mut Frame, area: Rect, app: &App) {
    let lines = if let Some(detail) = app.bykc.selected_cached_detail() {
        vec![
            Line::from(vec![
                Span::styled("课程: ", Style::default().fg(Color::Yellow)),
                Span::raw(detail.course_name.as_str()),
            ]),
            Line::from(vec![
                Span::styled("教师: ", Style::default().fg(Color::Yellow)),
                Span::raw(if detail.course_teacher.is_empty() {
                    "-"
                } else {
                    detail.course_teacher.as_str()
                }),
            ]),
            Line::from(vec![
                Span::styled("地点: ", Style::default().fg(Color::Yellow)),
                Span::raw(if detail.course_position.is_empty() {
                    "-"
                } else {
                    detail.course_position.as_str()
                }),
            ]),
            Line::from(vec![
                Span::styled("状态: ", Style::default().fg(Color::Yellow)),
                Span::raw(detail.status.as_str()),
            ]),
            Line::from(vec![
                Span::styled("选课时间: ", Style::default().fg(Color::Yellow)),
                Span::raw(format!(
                    "{} ~ {}",
                    empty_dash(&detail.course_select_start_date),
                    empty_dash(&detail.course_select_end_date)
                )),
            ]),
            Line::from(vec![
                Span::styled("签到窗口: ", Style::default().fg(Color::Yellow)),
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
                Span::styled("签退窗口: ", Style::default().fg(Color::Yellow)),
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
            BykcView::Courses => app.bykc.selected_course().map(|course| {
                vec![
                    Line::from(vec![
                        Span::styled("课程: ", Style::default().fg(Color::Yellow)),
                        Span::raw(course.course_name.as_str()),
                    ]),
                    Line::from(vec![
                        Span::styled("状态: ", Style::default().fg(Color::Yellow)),
                        Span::raw(course.status.as_str()),
                    ]),
                    Line::from(vec![
                        Span::styled("教师: ", Style::default().fg(Color::Yellow)),
                        Span::raw(empty_dash(&course.course_teacher)),
                    ]),
                    Line::from(vec![
                        Span::styled("地点: ", Style::default().fg(Color::Yellow)),
                        Span::raw(empty_dash(&course.course_position)),
                    ]),
                    Line::from(vec![
                        Span::styled("上课时间: ", Style::default().fg(Color::Yellow)),
                        Span::raw(format!(
                            "{} ~ {}",
                            empty_dash(&course.course_start_date),
                            empty_dash(&course.course_end_date)
                        )),
                    ]),
                    Line::from(vec![
                        Span::styled("选课时间: ", Style::default().fg(Color::Yellow)),
                        Span::raw(format!(
                            "{} ~ {}",
                            empty_dash(&course.course_select_start_date),
                            empty_dash(&course.course_select_end_date)
                        )),
                    ]),
                    Line::from(vec![
                        Span::styled("退选提示: ", Style::default().fg(Color::Yellow)),
                        Span::raw(
                            app.bykc
                                .chosen_course_for(course.id)
                                .map(|chosen| {
                                    if can_deselect_bykc_course_label(
                                        &chosen.course_cancel_end_date,
                                    ) {
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
                        Span::styled("简介: ", Style::default().fg(Color::Yellow)),
                        Span::raw(empty_dash(&course.course_desc)),
                    ]),
                    Line::from("按 s 报名，已报课程可按 x 退选；如需补充字段可按 o 或 enter"),
                ]
            }),
            BykcView::Chosen => app.bykc.selected_chosen_course().map(|course| {
                vec![
                    Line::from(vec![
                        Span::styled("课程: ", Style::default().fg(Color::Yellow)),
                        Span::raw(course.course_name.as_str()),
                    ]),
                    Line::from(vec![
                        Span::styled("签到状态: ", Style::default().fg(Color::Yellow)),
                        Span::raw(course.checkin.to_string()),
                    ]),
                    Line::from(vec![
                        Span::styled("教师: ", Style::default().fg(Color::Yellow)),
                        Span::raw(empty_dash(&course.course_teacher)),
                    ]),
                    Line::from(vec![
                        Span::styled("地点: ", Style::default().fg(Color::Yellow)),
                        Span::raw(empty_dash(&course.course_position)),
                    ]),
                    Line::from(vec![
                        Span::styled("上课时间: ", Style::default().fg(Color::Yellow)),
                        Span::raw(format!(
                            "{} ~ {}",
                            empty_dash(&course.course_start_date),
                            empty_dash(&course.course_end_date)
                        )),
                    ]),
                    Line::from(vec![
                        Span::styled("签到窗口: ", Style::default().fg(Color::Yellow)),
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
                        Span::styled("签退窗口: ", Style::default().fg(Color::Yellow)),
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
                        Span::styled("签到备注: ", Style::default().fg(Color::Yellow)),
                        Span::raw(empty_dash(&course.sign_info)),
                    ]),
                    Line::from("按 s 签到，u 签退，x 退选；如需补充字段可按 o 或 enter"),
                ]
            }),
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
        .border_style(Style::default().fg(Color::Green));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let lines = vec![
        Line::from(vec![
            Span::styled("课程: ", Style::default().fg(Color::Yellow)),
            Span::raw(detail.course_name.as_str()),
        ]),
        Line::from(vec![
            Span::styled("教师: ", Style::default().fg(Color::Yellow)),
            Span::raw(empty_dash(&detail.course_teacher)),
        ]),
        Line::from(vec![
            Span::styled("地点: ", Style::default().fg(Color::Yellow)),
            Span::raw(empty_dash(&detail.course_position)),
        ]),
        Line::from(vec![
            Span::styled("联系人: ", Style::default().fg(Color::Yellow)),
            Span::raw(empty_dash(&detail.course_contact)),
        ]),
        Line::from(vec![
            Span::styled("联系电话: ", Style::default().fg(Color::Yellow)),
            Span::raw(empty_dash(&detail.course_contact_mobile)),
        ]),
        Line::from(vec![
            Span::styled("上课时间: ", Style::default().fg(Color::Yellow)),
            Span::raw(format!(
                "{} ~ {}",
                empty_dash(&detail.course_start_date),
                empty_dash(&detail.course_end_date)
            )),
        ]),
        Line::from(vec![
            Span::styled("选课时间: ", Style::default().fg(Color::Yellow)),
            Span::raw(format!(
                "{} ~ {}",
                empty_dash(&detail.course_select_start_date),
                empty_dash(&detail.course_select_end_date)
            )),
        ]),
        Line::from(vec![
            Span::styled("退选截止: ", Style::default().fg(Color::Yellow)),
            Span::raw(empty_dash(&detail.course_cancel_end_date)),
        ]),
        Line::from(vec![
            Span::styled("状态: ", Style::default().fg(Color::Yellow)),
            Span::raw(detail.status.as_str()),
        ]),
        Line::from(vec![
            Span::styled("签到窗口: ", Style::default().fg(Color::Yellow)),
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
            Span::styled("签退窗口: ", Style::default().fg(Color::Yellow)),
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
            Span::styled("简介: ", Style::default().fg(Color::Yellow)),
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
    let widget = Paragraph::new(display)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(if focused {
                    Style::default().fg(Color::Cyan)
                } else {
                    Style::default()
                }),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(widget, area);
}

fn render_busy_popup(frame: &mut Frame) {
    let area = centered_rect(40, 20, frame.area());
    frame.render_widget(Clear, area);
    let popup = Paragraph::new("处理中...")
        .block(Block::default().title("请稍候").borders(Borders::ALL))
        .wrap(Wrap { trim: true });
    frame.render_widget(popup, area);
}

fn render_qr_popup(frame: &mut Frame, app: &App) {
    let Some(qr) = &app.qr_display else {
        return;
    };

    let area = centered_rect(70, 80, frame.area());
    frame.render_widget(Clear, area);

    let outer = Block::default()
        .title("二维码签到")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([Constraint::Min(10), Constraint::Length(6)])
        .split(inner);

    if let Ok(code) = QrCode::new(qr.qr_url.as_bytes()) {
        let widget = QrCodeWidget::new(code)
            .quiet_zone(QuietZone::Disabled)
            .scaling(Scaling::Min);
        frame.render_widget(widget, sections[0]);
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

fn render_help_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(70, 70, frame.area());
    frame.render_widget(Clear, area);

    let lines = vec![
        Line::from(Span::styled(
            "全局",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("tab / shift+tab: 切换 iClass / BYKC"),
        Line::from("?: 打开或关闭帮助"),
        Line::from("Shift+X: 退出登录"),
        Line::from("q / esc: 退出程序"),
        Line::from(""),
        Line::from(Span::styled(
            "iClass",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("h/j/k/l: 周视图内移动"),
        Line::from("[ ] / H L: 切换周"),
        Line::from("r: 刷新课程"),
        Line::from("s: 直接签到"),
        Line::from("g: 打开或关闭二维码签到"),
        Line::from(""),
        Line::from(Span::styled(
            "BYKC",
            Style::default()
                .fg(Color::Yellow)
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

fn mask_password(value: &str) -> String {
    "*".repeat(value.chars().count())
}

fn empty_dash(value: &str) -> String {
    if value.trim().is_empty() {
        "-".to_string()
    } else {
        value.to_string()
    }
}

fn can_deselect_bykc_course_label(course_cancel_end_date: &str) -> bool {
    let value = course_cancel_end_date.trim();
    if value.is_empty() {
        return true;
    }

    chrono::NaiveDateTime::parse_from_str(value, "%Y-%m-%d %H:%M:%S")
        .map(|deadline| chrono::Local::now().naive_local() <= deadline)
        .unwrap_or(true)
}
