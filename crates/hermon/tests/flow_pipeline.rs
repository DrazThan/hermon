//! Independent, frozen acceptance tests for issue #124.
use hermon::{
    engine::{Event, UiCmd},
    flow_ui::{FlowAlerts, HubLock, open_session, pipeline_rows},
    notify::{AlertKind, NotifyCfg},
    roster::RosterRow,
    source::Liveness,
};
use hermon_flow::{
    control::RunRecord,
    hub::{Attempt, Block, BlockKind, Event as RunEvent, Run, Status},
    intake::hash,
};
use serde_json::json;
use std::{collections::BTreeMap, fs, path::Path, sync::mpsc};

const WORKFLOW: &str = "version: 1\nname: pipeline-fixture\nsteps:\n  plan:\n    bot: planner\n    next: owner_go\n  owner_go:\n    gate: owner_go\n    next: work\n  work:\n    bot: implementer\n    next: docs\n  docs:\n    bot: writer\nlimits:\n  stall_alert_minutes: 2\n";

fn record(root: &Path) -> RunRecord {
    RunRecord {
        schema: 1,
        ticket: "TICKET-124".into(),
        workflow_name: "pipeline-fixture".into(),
        workflow_hash: hash(WORKFLOW),
        workflow_source: WORKFLOW.into(),
        template_hashes: BTreeMap::new(),
        templates: BTreeMap::new(),
        project: root.join("project.yaml"),
        worktree: root.to_owned(),
        base: "main".into(),
        base_commit: "fixture".into(),
        branch: "flow/TICKET-124".into(),
        paused: false,
        cancelled: false,
        run: Run::new("TICKET-124", "plan"),
        notices: vec![],
        started: true,
    }
}

fn event(record: &mut RunRecord, kind: &str, step: &str, reason: &str) {
    record.run.events.push(RunEvent {
        kind: kind.into(),
        step: step.into(),
        reason: reason.into(),
    });
}

fn running(root: &Path, number: u32, session: Option<&str>) -> RunRecord {
    let mut record = record(root);
    let handle = root.join("steps/work").join(number.to_string());
    fs::create_dir_all(&handle).unwrap();
    fs::write(
        handle.join("process.json"),
        serde_json::to_vec(&json!({
            "attempt": {"pid": 1234,"process_group":1235,"session_id": session,
                "log_path":handle.join("stdout.log"),"stderr_path":handle.join("stderr.log"),
                "timed_out":false,"cancelled":false},
            "finished":false,"process_success":false,"completion":false,
            "result":null,"exit_code":null
        }))
        .unwrap(),
    )
    .unwrap();
    let attempt = Attempt {
        step: "work".into(),
        number,
        handle: handle.to_str().unwrap().into(),
        started_at: 100,
        family: "fixture".into(),
        freezes: BTreeMap::new(),
    };
    record.run.current = "work".into();
    record.run.status = Status::Running;
    record.run.active = Some(attempt.clone());
    record.run.attempts.push(attempt);
    record.run.runs.insert("work".into(), number);
    event(&mut record, "started", "plan", "planner");
    event(&mut record, "step_done", "plan", "plan ready");
    event(&mut record, "route", "plan", "owner_go");
    event(&mut record, "gate_passed", "owner_go", "owner_go");
    event(&mut record, "route", "owner_go", "work");
    event(&mut record, "block", "work", "first attempt lacked a tool");
    event(&mut record, "retry", "work", "tool installed");
    event(&mut record, "started", "work", "implementer");
    record
}

fn roster(id: &str, key: &str) -> RosterRow {
    RosterRow {
        id: id.into(),
        key: key.into(),
        state: Liveness::Live,
        model: String::new(),
        last_tool: String::new(),
        last_line: String::new(),
        in_tok: 0,
        out_tok: 0,
        cost: None,
        elapsed: None,
        last_ts: 0.0,
        title: String::new(),
        attn_elapsed: None,
    }
}

#[test]
fn hub_single_instance_lock() {
    let root = tempfile::tempdir().unwrap();
    let data = root.path().join("nested/data");
    let first = HubLock::acquire(&data)
        .unwrap()
        .expect("first hub owns data");
    assert!(HubLock::acquire(&data).unwrap().is_none());
    let other = HubLock::acquire(&root.path().join("other")).unwrap();
    assert!(
        other.is_some(),
        "independent data directories may host hubs"
    );
    drop(first);
    let second = HubLock::acquire(&data)
        .unwrap()
        .expect("drop releases lock");
    assert!(HubLock::acquire(&data).unwrap().is_none());
    drop(second);
    assert!(HubLock::acquire(&data).unwrap().is_some());
}

#[test]
fn foreground_hub_uses_same_single_instance_lock() {
    let root = tempfile::tempdir().unwrap();
    let _owner = HubLock::acquire(root.path()).unwrap().unwrap();
    let out = std::process::Command::new(env!("CARGO_BIN_EXE_hermon"))
        .args(["hub", "--once", "--no-notify", "--data-dir"])
        .arg(root.path())
        .output()
        .unwrap();
    assert!(!out.status.success(), "second hub must refuse to dispatch");
    let message = String::from_utf8_lossy(&out.stderr).to_lowercase();
    assert!(
        message.contains("hub") && (message.contains("lock") || message.contains("already")),
        "report the lock owner conflict: {message}"
    );
}

#[test]
fn pipeline_view_rows_from_events() {
    let root = tempfile::tempdir().unwrap();
    let record = running(root.path(), 2, Some("opaque-session-124"));
    let rows = pipeline_rows(&record, 145).unwrap();
    assert_eq!(rows.len(), 4, "include future workflow steps");
    let plan = rows.iter().find(|r| r.step == "plan").unwrap();
    assert_eq!(plan.run_id, "TICKET-124");
    assert_eq!(plan.bot.as_deref(), Some("planner"));
    assert_eq!(plan.state.to_lowercase(), "done");
    let work = rows.iter().find(|r| r.step == "work").unwrap();
    assert_eq!(work.bot.as_deref(), Some("implementer"));
    assert_eq!(work.attempt, 2);
    assert_eq!(work.age_secs, Some(45));
    assert_eq!(work.state.to_lowercase(), "running");
    assert_eq!(
        work.last_block.as_deref(),
        Some("first attempt lacked a tool")
    );
    assert_eq!(work.session_id.as_deref(), Some("opaque-session-124"));
    let docs = rows.iter().find(|r| r.step == "docs").unwrap();
    assert_eq!(docs.attempt, 0);
    assert!(docs.age_secs.is_none());
    assert!(docs.session_id.is_none());
    assert_ne!(docs.state.to_lowercase(), "done");
    let gate = rows.iter().find(|r| r.step == "owner_go").unwrap();
    assert!(gate.bot.is_none());
    assert_eq!(gate.state.to_lowercase(), "done");
}

#[test]
fn pipeline_session_uses_recorded_id_and_running_step_only() {
    let root = tempfile::tempdir().unwrap();
    let mut record = running(root.path(), 2, Some("opaque-session-124"));
    let rows = pipeline_rows(&record, 90).unwrap();
    let work = rows.iter().find(|r| r.step == "work").unwrap();
    assert_eq!(work.age_secs, Some(0), "clock rollback saturates");
    let roster = vec![
        roster("opaque-session-124-suffix", "C:wrong"),
        roster("opaque-session-124", "C:exact"),
    ];
    assert_eq!(open_session(work, &roster).as_deref(), Some("C:exact"));
    assert!(
        open_session(work, &roster[..1]).is_none(),
        "never guess by prefix"
    );
    record.run.status = Status::Done;
    record.run.active = None;
    let rows = pipeline_rows(&record, 145).unwrap();
    assert!(rows.iter().all(|row| open_session(row, &roster).is_none()));
}

#[test]
fn pipeline_missing_session_receipt_is_still_visible() {
    let root = tempfile::tempdir().unwrap();
    let record = running(root.path(), 1, None);
    fs::remove_file(Path::new(&record.run.active.as_ref().unwrap().handle).join("process.json"))
        .unwrap();
    let rows = pipeline_rows(&record, 145).unwrap();
    let work = rows.iter().find(|r| r.step == "work").unwrap();
    assert_eq!(work.state.to_lowercase(), "running");
    assert!(
        work.session_id.is_none(),
        "receipt can appear after the start event"
    );
    assert!(open_session(work, &[roster("unrelated", "C:wrong")]).is_none());
}

#[test]
fn tui_and_gui_open_running_pipeline_session_pane() {
    let root = tempfile::tempdir().unwrap();
    let record = running(root.path(), 1, Some("opaque-session-124"));
    let rows = pipeline_rows(&record, 145).unwrap();
    let index = rows.iter().position(|row| row.step == "work").unwrap();
    let roster = vec![
        roster("another", "C:wrong"),
        roster("opaque-session-124", "C:exact"),
    ];
    let mut tui = hermon::ui::App::default();
    tui.apply_event(Event::Roster(roster.clone()));
    tui.take_commands();
    tui.apply_event(Event::Pipeline(rows.clone()));
    assert!(tui.select_pipeline(index));
    assert_eq!(tui.selected_id.as_deref(), Some("opaque-session-124"));
    assert!(
        tui.take_commands()
            .contains(&UiCmd::OpenPane("C:exact".into()))
    );
    assert!(!tui.select_pipeline(rows.len()));

    let (_events, rx) = mpsc::channel();
    let (tx, commands) = mpsc::channel();
    let mut gui = hermon::gui::App::new(rx, tx, vec![], 6);
    gui.apply_event(Event::Roster(roster));
    gui.apply_event(Event::Pipeline(rows));
    assert!(gui.select_pipeline(index));
    assert_eq!(gui.selected_id.as_deref(), Some("opaque-session-124"));
    assert!(
        commands
            .try_iter()
            .any(|c| c == UiCmd::OpenPane("C:exact".into()))
    );
    assert!(!gui.select_pipeline(usize::MAX));
}

#[test]
fn alert_on_stall() {
    let root = tempfile::tempdir().unwrap();
    let record = running(root.path(), 1, Some("opaque-session-124"));
    let cfg = NotifyCfg::default();
    let mut alerts = FlowAlerts::default();
    assert!(alerts.poll(&record, 219, &cfg, false).unwrap().is_empty());
    let first = alerts.poll(&record, 220, &cfg, false).unwrap();
    assert_eq!(first.len(), 1, "stall alerts at configured threshold");
    assert_eq!(first[0].kind, AlertKind::Stuck);
    assert!(format!("{first:?}").contains("TICKET-124"));
    assert!(format!("{first:?}").contains("work"));
    assert!(alerts.poll(&record, 221, &cfg, false).unwrap().is_empty());
    let next = running(root.path(), 2, Some("second-session"));
    assert_eq!(
        alerts.poll(&next, 220, &cfg, false).unwrap().len(),
        1,
        "a new stalled attempt earns a fresh alert"
    );
}

#[test]
fn alert_on_owner_needed() {
    let root = tempfile::tempdir().unwrap();
    let mut record = record(root.path());
    record.run.current = "owner_go".into();
    record.run.status = Status::Gate;
    event(&mut record, "gate_opened", "owner_go", "owner_go");
    let cfg = NotifyCfg::default();
    let mut alerts = FlowAlerts::default();
    let first = alerts.poll(&record, 200, &cfg, false).unwrap();
    assert_eq!(first.len(), 1);
    assert_eq!(first[0].kind, AlertKind::PermWait);
    assert!(format!("{first:?}").contains("owner_go"));
    assert!(alerts.poll(&record, 201, &cfg, false).unwrap().is_empty());
    record.run.status = Status::Ready;
    assert!(alerts.poll(&record, 202, &cfg, false).unwrap().is_empty());
    record.run.status = Status::Gate;
    record.run.gate_requests.insert("owner_go".into(), 2);
    event(&mut record, "gate_opened", "owner_go", "owner_go");
    assert_eq!(alerts.poll(&record, 203, &cfg, false).unwrap().len(), 1);

    record.run.current = "work".into();
    record.run.status = Status::Blocked;
    record.run.block = Some(Block::new(
        BlockKind::OwnerQuestion,
        "Which API should I use?",
    ));
    event(&mut record, "blocked", "work", "Which API should I use?");
    let question = alerts.poll(&record, 204, &cfg, false).unwrap();
    assert_eq!(question.len(), 1);
    assert_eq!(question[0].kind, AlertKind::PermWait);
    assert!(question[0].detail.contains("Which API should I use?"));
}

#[test]
fn flow_alerts_honor_mute_config_and_terminal_states() {
    let root = tempfile::tempdir().unwrap();
    let mut record = running(root.path(), 1, None);
    let cfg = NotifyCfg::default();
    let mut alerts = FlowAlerts::default();
    assert!(alerts.poll(&record, 220, &cfg, true).unwrap().is_empty());
    assert!(
        alerts
            .poll(&record, 220, &cfg.clone().silenced(), false)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        alerts.poll(&record, 220, &cfg, false).unwrap().len(),
        1,
        "silencing must not consume an alert that was never delivered"
    );
    for status in [Status::Ready, Status::Done] {
        record.run.status = status;
        assert!(
            FlowAlerts::default()
                .poll(&record, 999, &cfg, false)
                .unwrap()
                .is_empty()
        );
    }
    record.run.status = Status::Blocked;
    record.run.block = Some(Block::new(
        BlockKind::Capability,
        "Install the missing tool",
    ));
    assert!(
        FlowAlerts::default()
            .poll(&record, 999, &cfg, true)
            .unwrap()
            .is_empty()
    );
    assert!(
        FlowAlerts::default()
            .poll(&record, 999, &cfg.clone().silenced(), false)
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        FlowAlerts::default()
            .poll(&record, 999, &cfg, false)
            .unwrap()
            .len(),
        1
    );
    record.cancelled = true;
    record.run.cancelled = true;
    assert!(
        FlowAlerts::default()
            .poll(&record, 999, &cfg, false)
            .unwrap()
            .is_empty()
    );
}
