use super::*;
use ratatui::{Terminal, backend::TestBackend};
use tokio::sync::mpsc;

fn academic_app(view: CourseView) -> App {

    let mut app = App::default();

    app.screen = Screen::Workspace;

    app.active_tab = WorkspaceTab::Schedule;

    app.schedule.view = view;

    app.schedule.grades = (0..50)
        .map(|i| {

            GradeItem {
                course_name: format!("成绩课程{i:02}"),
                score: Some("90".into()),
                ..Default::default()
            }
        })
        .collect();

    app.schedule.classrooms = (0..50)
        .map(|i| {

            ClassroomRoom {
                building: "一号楼".into(),
                name: format!("空教室{i:02}"),
                ..Default::default()
            }
        })
        .collect();

    app
}

fn draw(app: &App) -> String {

    let mut terminal = Terminal::new(TestBackend::new(100, 22)).unwrap();

    terminal
        .draw(|frame| crate::ui::render(frame, app))
        .unwrap();

    let buffer = terminal.backend().buffer();

    (0..22)
        .map(|y| {

            (0..100)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
        .replace([' ', '\u{3000}'], "")
}

#[test]

fn academic_lists_accept_arrows_pages_and_end_and_keep_selection_visible() {

    let (tx, _) = mpsc::unbounded_channel();

    for (view, last) in [
        (CourseView::Grades, "成绩课程49"),
        (CourseView::Classrooms, "空教室49"),
    ] {

        let mut app = academic_app(view);

        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE), &tx);

        assert_eq!(app.schedule.selected_entry, 1);

        draw(&app);

        let page = app.list_page_size.get();

        app.handle_key(KeyEvent::new(KeyCode::PageDown, KeyModifiers::NONE), &tx);

        assert_eq!(app.schedule.selected_entry, 1 + page);

        app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), &tx);

        assert_eq!(app.schedule.selected_entry, 49);

        assert!(draw(&app).contains(last));

        assert!(app.list_state.borrow().offset() > 0);

        app.handle_key(KeyEvent::new(KeyCode::Home, KeyModifiers::NONE), &tx);

        assert_eq!(app.schedule.selected_entry, 0);
    }
}

#[test]

fn wheel_moves_down_and_click_uses_the_scrolled_row_index() {

    let (tx, _) = mpsc::unbounded_channel();

    let mut app = academic_app(CourseView::Grades);

    app.handle_mouse(
        MouseEvent {
            kind:      MouseEventKind::ScrollDown,
            column:    10,
            row:       10,
            modifiers: KeyModifiers::NONE,
        },
        &tx,
    );

    assert_eq!(app.schedule.selected_entry, 3);

    app.handle_mouse(
        MouseEvent {
            kind:      MouseEventKind::ScrollUp,
            column:    10,
            row:       10,
            modifiers: KeyModifiers::NONE,
        },
        &tx,
    );

    assert_eq!(app.schedule.selected_entry, 0);

    app.handle_key(KeyEvent::new(KeyCode::End, KeyModifiers::NONE), &tx);

    draw(&app);

    let hotspot = app
        .hotspots
        .borrow()
        .iter()
        .find(|h| matches!(h.action, HotAction::ListRow { .. }))
        .unwrap()
        .clone();

    let HotAction::ListRow { index } = hotspot.action else {

        unreachable!()
    };

    assert!(index > 0);

    app.handle_mouse(
        MouseEvent {
            kind:      MouseEventKind::Down(MouseButton::Left),
            column:    hotspot.area.x,
            row:       hotspot.area.y,
            modifiers: KeyModifiers::NONE,
        },
        &tx,
    );

    assert_eq!(app.schedule.selected_entry, index);
}

fn evaluation_task(id: &str) -> crate::evaluation::EvaluationTask {

    crate::evaluation::EvaluationTask {
        rwid: id.into(),
        course: id.into(),
        ..Default::default()
    }
}

#[test]

fn logout_clears_business_state_and_ignores_old_account_results() {

    let (tx, _) = mpsc::unbounded_channel();

    let mut app = App::default();

    app.venue.loaded = true;

    app.eval.tasks = vec![evaluation_task("old")];

    app.eval.pending = Some(PendingWrite::EvalSubmitOne("old".into()));

    let old_generation = app.session_generation;

    app.logout();

    assert!(!app.venue.loaded);

    assert!(app.eval.tasks.is_empty());

    assert!(app.eval.pending.is_none());

    app.handle_async(
        AsyncEvent::Scoped {
            generation: old_generation,
            event:      Box::new(AsyncEvent::EvalTasks(Ok(vec![evaluation_task("late-old")]))),
        },
        &tx,
    );

    assert!(app.eval.tasks.is_empty());
}

#[test]

fn late_questionnaire_is_not_attached_to_another_course() {

    let (tx, _) = mpsc::unbounded_channel();

    let mut app = App::default();

    app.eval.tasks = vec![evaluation_task("a"), evaluation_task("b")];

    app.eval.selected = 1;

    app.handle_async(
        AsyncEvent::EvalQuestionnaire("a".into(), Ok(Default::default())),
        &tx,
    );

    assert!(app.eval.questionnaire_task.is_none());

    assert!(!app.eval.show_questionnaire);

    app.handle_async(
        AsyncEvent::EvalQuestionnaire("b".into(), Ok(Default::default())),
        &tx,
    );

    assert_eq!(app.eval.questionnaire_task.as_deref(), Some("b"));
}

#[test]

fn failed_evaluation_stays_pending_and_batch_success_is_per_course() {

    let (tx, _) = mpsc::unbounded_channel();

    let mut app = App::default();

    app.screen = Screen::Workspace;

    app.active_tab = WorkspaceTab::Eval;

    app.eval.tasks = vec![evaluation_task("a"), evaluation_task("b")];

    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE), &tx);

    app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE), &tx);

    assert!(
        !app.eval.tasks[0].evaluated,
        "No session means no completed evaluation"
    );

    app.eval.submitting = true;

    app.handle_async(
        AsyncEvent::Evaluations(Ok(vec![
            EvaluationCompletion {
                rwid:   "a".into(),
                course: "a".into(),
                result: Ok(()),
            },
            EvaluationCompletion {
                rwid:   "b".into(),
                course: "b".into(),
                result: Err("response lost".into()),
            },
        ])),
        &tx,
    );

    assert!(app.eval.tasks[0].evaluated);

    assert!(!app.eval.tasks[1].evaluated);

    assert!(!app.eval.submitting);
}

#[test]

fn sign_result_updates_its_request_target_not_the_current_selection() {

    let (tx, _) = mpsc::unbounded_channel();

    let mut app = App::default();

    let courses = ["a", "b"].map(|id| {

        CourseDetailItem {
            name: id.into(),
            id: id.into(),
            course_sched_id: id.into(),
            date: "2026-09-23".into(),
            sign_status: "0".into(),
            ..Default::default()
        }
    });

    app.handle_async(AsyncEvent::Refresh(Ok(courses.to_vec())), &tx);

    app.selected = 1;

    app.handle_async(
        AsyncEvent::Sign(
            "a".into(),
            Ok(SignOutcome {
                message:       "success".into(),
                success_like:  true,
                http_status:   200,
                server_status: "success".into(),
                raw_response:  serde_json::json!({}),
            }),
        ),
        &tx,
    );

    assert_eq!(app.courses[0].sign_status, "1");

    assert_eq!(app.courses[1].sign_status, "0");
}

#[test]

fn enter_cancels_a_pending_write_and_mouse_cannot_change_its_target() {

    let (tx, _) = mpsc::unbounded_channel();

    let mut app = academic_app(CourseView::Grades);

    app.active_tab = WorkspaceTab::Eval;

    app.eval.tasks = vec![evaluation_task("a"), evaluation_task("b")];

    app.eval.pending = Some(PendingWrite::EvalSubmitOne("a".into()));

    app.handle_mouse(
        MouseEvent {
            kind:      MouseEventKind::ScrollDown,
            column:    10,
            row:       10,
            modifiers: KeyModifiers::NONE,
        },
        &tx,
    );

    assert_eq!(app.eval.selected, 0);

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &tx);

    assert!(app.eval.pending.is_none());

    assert!(!app.eval.tasks[0].evaluated);
}

#[test]

fn timestamp_exam_dates_are_included_in_upcoming_exams() {

    let tomorrow = (Local::now() + ChronoDuration::days(1))
        .date_naive()
        .to_string();

    let mut app = App::default();

    app.schedule.exams.push(ExamItem {
        exam_date: Some(format!("{tomorrow} 00:00:00")),
        start_time: Some("10:00".into()),
        ..Default::default()
    });

    assert!(app.upcoming_exam(7).is_some());
}

#[test]

fn venue_form_edits_fields_without_triggering_global_shortcuts_or_submitting() {

    let (tx, mut rx) = mpsc::unbounded_channel();

    let mut app = App::default();

    app.screen = Screen::Workspace;

    app.active_tab = WorkspaceTab::Venue;

    app.handle_key(KeyEvent::new(KeyCode::Char('e'), KeyModifiers::NONE), &tx);

    assert!(app.venue.form.is_some());

    assert!(!app.show_event_log);

    assert!(draw(&app).contains("研讨室预约信息"));

    for c in "13800000000".chars() {

        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &tx);
    }

    app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &tx);

    for c in "theme".chars() {

        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &tx);
    }

    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &tx);

    assert!(app.venue.form.is_none());

    assert_eq!(app.venue.phone, "13800000000");

    assert_eq!(app.venue.theme, "theme");

    assert!(app.pending_write().is_none());

    assert!(rx.try_recv().is_err());
}
