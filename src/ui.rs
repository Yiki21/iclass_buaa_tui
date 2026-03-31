use chrono::{Duration, NaiveDate, TimeZone};
use qrcode::QrCode;
use ratatui::{
    Frame,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
};
use tui_qrcode::{Colors, QrCodeWidget, QuietZone, Scaling};

use crate::app::{App, LoginFocus, Screen};

pub fn render(frame: &mut Frame, app: &App) {
    match app.screen {
        Screen::Login => render_login(frame, app),
        Screen::Courses => render_courses(frame, app),
    }

    if app.busy {
        render_busy_popup(frame);
    } else if app.qr_display.is_some() {
        render_qr_popup(frame, app);
    }

    if app.show_help {
        render_help_popup(frame, app);
    }
}

fn render_login(frame: &mut Frame, app: &App) {
    let area = frame.area();
    frame.render_widget(Clear, area);

    let outer = Block::default()
        .title("BUAA iClass Rust")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Cyan));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let mut constraints = vec![
        Constraint::Length(3),
        Constraint::Length(3),
        Constraint::Length(3),
    ];
    if app.login.use_vpn {
        constraints.push(Constraint::Length(3));
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
            "Ratatui + Tokio 终端版签到工具",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("tab 切换字段，space 切换 VPN，enter 登录，? 帮助，q 退出"),
        Line::from("登录页已改为全屏表单，避免输入框重叠"),
    ]);
    frame.render_widget(title, chunks[0]);

    render_input(
        frame,
        chunks[1],
        "学号",
        &app.login.student_id,
        app.login.current_focus() == LoginFocus::StudentId,
        false,
    );

    let vpn_value = if app.login.use_vpn {
        "开启"
    } else {
        "关闭"
    };
    render_input(
        frame,
        chunks[2],
        "VPN 模式",
        vpn_value,
        app.login.current_focus() == LoginFocus::UseVpn,
        false,
    );

    let status_index = if app.login.use_vpn {
        render_input(
            frame,
            chunks[3],
            "VPN 账号",
            &app.login.vpn_username,
            app.login.current_focus() == LoginFocus::VpnUsername,
            false,
        );
        render_input(
            frame,
            chunks[4],
            "VPN 密码",
            &mask_password(&app.login.vpn_password),
            app.login.current_focus() == LoginFocus::VpnPassword,
            true,
        );
        5
    } else {
        3
    };

    let status = Paragraph::new(app.status.as_str())
        .block(Block::default().title("状态").borders(Borders::ALL))
        .wrap(Wrap { trim: true });
    frame.render_widget(status, chunks[status_index]);
}

fn render_courses(frame: &mut Frame, app: &App) {
    let vertical = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(3),
            Constraint::Min(12),
            Constraint::Length(8),
            Constraint::Length(4),
        ])
        .split(frame.area());

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
            "当前周: {} | {} ~ {} | {} 条课程 | H/L 或 [ ] 切周 | ? 帮助",
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

    let detail_text = if let Some(course) = app.selected_course() {
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
            Line::from("操作:"),
            Line::from("  h/l 跨天移动"),
            Line::from("  j/k 当天上下移动"),
            Line::from("  H/L 或 [ ] 切换周"),
            Line::from("  s 直接签到"),
            Line::from("  g 二维码签到/关闭"),
            Line::from("  r 刷新课程"),
            Line::from("  ? 打开帮助页"),
            Line::from("  Shift+X 退出登录"),
            Line::from("  q 退出程序"),
        ]
    } else {
        vec![Line::from("暂无选中课程")]
    };
    let detail = Paragraph::new(detail_text)
        .block(Block::default().title("详情").borders(Borders::ALL))
        .wrap(Wrap { trim: true });
    frame.render_widget(detail, vertical[3]);

    let footer = Paragraph::new(app.status.as_str())
        .block(Block::default().title("状态").borders(Borders::ALL))
        .wrap(Wrap { trim: true });
    frame.render_widget(footer, vertical[4]);
}

fn render_input(
    frame: &mut Frame,
    area: Rect,
    label: &str,
    value: &str,
    focused: bool,
    sensitive: bool,
) {
    let border_style = if focused {
        Style::default().fg(Color::Cyan)
    } else if sensitive {
        Style::default().fg(Color::Magenta)
    } else {
        Style::default()
    };

    let paragraph = Paragraph::new(value).block(
        Block::default()
            .title(label)
            .borders(Borders::ALL)
            .border_style(border_style),
    );
    frame.render_widget(paragraph, area);
}

fn render_busy_popup(frame: &mut Frame) {
    let area = centered_rect(40, 5, frame.area());
    frame.render_widget(Clear, area);
    let popup = Paragraph::new("网络请求处理中...")
        .block(
            Block::default()
                .title("请稍候")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        )
        .wrap(Wrap { trim: true });
    frame.render_widget(popup, area);
}

fn render_qr_popup(frame: &mut Frame, app: &App) {
    let Some(qr) = &app.qr_display else {
        return;
    };

    let area = centered_rect(78, 82, frame.area());
    frame.render_widget(Clear, area);

    let sections = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Length(3),
            Constraint::Min(10),
            Constraint::Length(6),
        ])
        .split(area);

    let title = if app.qr_refreshing {
        "二维码签到（自动刷新中）"
    } else {
        "二维码签到"
    };
    let outer = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Green));
    frame.render_widget(outer, area);

    let generated_at = chrono::Local
        .timestamp_millis_opt(qr.timestamp)
        .single()
        .map(|time| time.format("%F %T").to_string())
        .unwrap_or_else(|| qr.timestamp.to_string());

    let summary = Paragraph::new(vec![
        Line::from(format!("courseSchedId: {}", qr.course_sched_id)),
        Line::from(format!("生成时间: {}", generated_at)),
        Line::from("按 g 关闭二维码刷新"),
    ])
    .wrap(Wrap { trim: true });
    frame.render_widget(summary, sections[0]);

    let qr_block = Block::default().title("QR").borders(Borders::ALL);
    let qr_inner = qr_block.inner(sections[1]);
    frame.render_widget(qr_block, sections[1]);

    if let Ok(code) = QrCode::new(qr.qr_url.as_bytes()) {
        let qr_area = centered_qr_rect(qr_inner, code.width() as u16);
        let widget = QrCodeWidget::new(code)
            .quiet_zone(QuietZone::Disabled)
            .scaling(Scaling::Exact(1, 1))
            .colors(Colors::Inverted);
        frame.render_widget(widget, qr_area);
    } else {
        let fallback = Paragraph::new("二维码渲染失败").wrap(Wrap { trim: true });
        frame.render_widget(fallback, qr_inner);
    }

    let url = Paragraph::new(qr.qr_url.as_str())
        .block(Block::default().title("链接").borders(Borders::ALL))
        .wrap(Wrap { trim: false });
    frame.render_widget(url, sections[2]);
}

fn render_help_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(82, 86, frame.area());
    frame.render_widget(Clear, area);

    let outer = Block::default()
        .title("Helper")
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Yellow));
    let inner = outer.inner(area);
    frame.render_widget(outer, area);

    let mut lines = vec![
        Line::from(Span::styled(
            "全局",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from("  ? 打开或关闭帮助页"),
        Line::from("  q 退出程序"),
        Line::from("  esc 关闭帮助页或退出当前弹层"),
        Line::from(""),
    ];

    match app.screen {
        Screen::Login => {
            lines.push(Line::from(Span::styled(
                "登录页",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from("  tab / shift+tab 切换字段"));
            lines.push(Line::from("  space 切换 VPN 开关"));
            lines.push(Line::from("  enter 提交登录"));
            lines.push(Line::from(""));
        }
        Screen::Courses => {
            lines.push(Line::from(Span::styled(
                "课程页",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )));
            lines.push(Line::from("  h / l 在周日历里左右移动"));
            lines.push(Line::from("  j / k 在当天课程里上下移动"));
            lines.push(Line::from("  H / L 或 [ / ] 切换周"));
            lines.push(Line::from("  s 直接签到"));
            lines.push(Line::from("  g 打开或关闭二维码签到"));
            lines.push(Line::from("  r 刷新课程"));
            lines.push(Line::from("  Shift+X 退出登录"));
            lines.push(Line::from(""));
        }
    }

    if app.qr_display.is_some() {
        lines.push(Line::from(Span::styled(
            "二维码弹层",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        )));
        lines.push(Line::from("  g 关闭二维码自动刷新"));
        lines.push(Line::from(""));
    }

    lines.push(Line::from(Span::styled(
        "说明",
        Style::default()
            .fg(Color::Cyan)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from("  帮助页打开后，? / esc / q 都可以关闭帮助页"));
    lines.push(Line::from("  帮助页会覆盖在课程页、登录页和二维码弹层之上"));

    let body = Paragraph::new(lines)
        .block(Block::default().title("快捷键").borders(Borders::ALL))
        .wrap(Wrap { trim: true });
    frame.render_widget(body, inner);
}

fn mask_password(password: &str) -> String {
    if password.is_empty() {
        String::new()
    } else {
        "*".repeat(password.chars().count())
    }
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let popup_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage((100 - height) / 2),
            Constraint::Percentage(height),
            Constraint::Percentage((100 - height) / 2),
        ])
        .split(area);

    Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage((100 - width) / 2),
            Constraint::Percentage(width),
            Constraint::Percentage((100 - width) / 2),
        ])
        .split(popup_layout[1])[1]
}

fn centered_qr_rect(area: Rect, modules: u16) -> Rect {
    let qr_width = modules.min(area.width.max(1));
    let qr_height = qr_width.div_ceil(2).min(area.height.max(1));
    let left = area.x + area.width.saturating_sub(qr_width) / 2;
    let top = area.y + area.height.saturating_sub(qr_height) / 2;

    Rect {
        x: left,
        y: top,
        width: qr_width,
        height: qr_height,
    }
}
