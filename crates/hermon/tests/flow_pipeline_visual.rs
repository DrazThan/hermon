//! Independent rendered TUI and unavailable-session selection regressions.
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use hermon::{
    engine::{Event, UiCmd},
    flow_ui::PipelineRow,
    roster::RosterRow,
    source::Liveness,
};
use ratatui::{Terminal, backend::TestBackend};
use std::sync::mpsc;

fn pipeline() -> PipelineRow {
    PipelineRow {
        run_id: "VISUAL-124".into(),
        step: "work".into(),
        bot: Some("worker".into()),
        attempt: 2,
        age_secs: Some(42),
        state: "running".into(),
        last_block: Some("previous missing credential".into()),
        session_id: Some("not-yet-in-roster".into()),
    }
}
fn unrelated() -> RosterRow {
    RosterRow {
        id: "unrelated-session".into(),
        key: "C:unrelated".into(),
        state: Liveness::Live,
        model: "unrelated-model".into(),
        last_tool: String::new(),
        last_line: "UNRELATED TRANSCRIPT".into(),
        in_tok: 0,
        out_tok: 0,
        cost: None,
        elapsed: None,
        last_ts: 0.0,
        title: "Unrelated session".into(),
        attn_elapsed: None,
    }
}
fn render(app: &hermon::ui::App) -> String {
    let mut terminal = Terminal::new(TestBackend::new(140, 35)).unwrap();
    terminal.draw(|frame| hermon::ui::draw(frame, app)).unwrap();
    let buffer = terminal.backend().buffer();
    (0..buffer.area.height)
        .map(|y| {
            (0..buffer.area.width)
                .map(|x| buffer[(x, y)].symbol())
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn tui_pipeline_keyboard_view_renders_run_step_bot_attempt_age_and_block() {
    let mut app = hermon::ui::App::default();
    app.apply_event(Event::Pipeline(vec![pipeline()]));
    app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
    assert!(app.show_pipeline);
    let frame = render(&app);
    for text in [
        "Pipelines",
        "VISUAL-124",
        "work",
        "worker",
        "#2",
        "0m 42s",
        "running",
        "previous missing credential",
    ] {
        assert!(
            frame.contains(text),
            "missing {text} in rendered frame:\n{frame}"
        );
    }
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(!app.show_pipeline);
}

#[test]
fn tui_unavailable_pipeline_selection_does_not_show_unrelated_session() {
    let mut app = hermon::ui::App::default();
    app.apply_event(Event::Roster(vec![unrelated()]));
    assert_eq!(app.selected_id.as_deref(), Some("unrelated-session"));
    app.take_commands();
    app.apply_event(Event::Pipeline(vec![pipeline()]));
    app.handle_key(KeyEvent::new(KeyCode::Char('v'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        app.selected_id.is_none(),
        "unavailable pipeline receipt must not display a different session"
    );
    assert!(
        app.take_commands()
            .contains(&UiCmd::ClosePane("C:unrelated".into()))
    );
    let frame = render(&app);
    assert!(frame.contains("Session unavailable"));
    assert!(!frame.contains("UNRELATED TRANSCRIPT"));
}

#[test]
fn gui_unavailable_pipeline_selection_clears_unrelated_session_pane() {
    let (_events, rx) = mpsc::channel();
    let (tx, commands) = mpsc::channel();
    let mut app = hermon::gui::App::new(rx, tx, vec![], 6);
    app.apply_event(Event::Roster(vec![unrelated()]));
    let mut initial = pipeline();
    initial.session_id = Some("unrelated-session".into());
    app.apply_event(Event::Pipeline(vec![initial]));
    assert!(app.select_pipeline(0));
    assert_eq!(app.selected_id.as_deref(), Some("unrelated-session"));
    let _ = commands.try_iter().collect::<Vec<_>>();
    app.apply_event(Event::Pipeline(vec![pipeline()]));
    app.show_pipeline = true;
    assert!(!app.select_pipeline(0));
    assert!(
        app.selected_id.is_none(),
        "unavailable pipeline receipt must not display a different session"
    );
    assert!(
        commands
            .try_iter()
            .any(|c| c == UiCmd::ClosePane("C:unrelated".into()))
    );
}
