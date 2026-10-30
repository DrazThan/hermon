//! Independent supplemental review contracts for qualified roster IDs and host isolation.
use hermon::{
    bots::Route,
    engine::{Event, UiCmd},
    flow_ui::{PipelineRow, open_session},
    roster::RosterRow,
    source::Liveness,
};
use hermon_flow::{
    control::{RunRecord, save},
    host::{Host, snapshots},
    hub::{Run, Status},
    intake::hash,
};
use std::{collections::BTreeMap, fs, path::Path, sync::mpsc};

fn pipeline() -> PipelineRow {
    PipelineRow {
        run_id: "REVIEW-124".into(),
        step: "work".into(),
        bot: Some("worker".into()),
        attempt: 1,
        age_secs: Some(42),
        state: "running".into(),
        last_block: None,
        session_id: Some("opaque-session".into()),
    }
}
fn row(origin: &str, store: &str, session: &str) -> RosterRow {
    let id = Route {
        origin: origin.as_bytes().to_vec(),
        source: "X".into(),
        store: store.as_bytes().to_vec(),
        vendor: session.as_bytes().to_vec(),
    }
    .encode();
    RosterRow {
        id,
        key: "X:opaque".into(),
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
fn pipeline_opens_qualified_local_vendor_session() {
    let selected = row("", "local-codex-store", "opaque-session");
    let unrelated = row("", "local-codex-store", "opaque-session-suffix");
    assert_eq!(
        open_session(&pipeline(), &[unrelated, selected.clone()]),
        Some(selected.id)
    );
}

#[test]
fn pipeline_refuses_ambiguous_local_vendor_sessions() {
    assert!(
        open_session(
            &pipeline(),
            &[
                row("", "first-store", "opaque-session"),
                row("", "second-store", "opaque-session"),
            ]
        )
        .is_none()
    );
}

#[test]
fn pipeline_never_routes_local_run_to_remote_store() {
    let remote = row("container", "remote-store", "opaque-session");
    assert!(open_session(&pipeline(), std::slice::from_ref(&remote)).is_none());
    let local = row("", "local-store", "opaque-session");
    assert_eq!(
        open_session(&pipeline(), &[remote.clone(), local.clone()]),
        Some(local.id)
    );
    let mut encoded_receipt = pipeline();
    encoded_receipt.session_id = Some(remote.id.clone());
    assert!(
        open_session(&encoded_receipt, &[remote]).is_none(),
        "a receipt cannot authorize remote-origin routing"
    );
}

#[test]
fn malformed_qualified_roster_id_is_not_navigation_authority() {
    let mut invalid = row("", "store", "opaque-session");
    invalid.id = "hr1:invalid".into();
    let mut receipt = pipeline();
    receipt.session_id = Some(invalid.id.clone());
    assert!(open_session(&receipt, &[invalid]).is_none());
}

#[test]
fn both_uis_open_qualified_pipeline_session() {
    let selected = row("", "local-store", "opaque-session");
    let unrelated = row("", "local-store", "another-session");
    let roster = vec![unrelated, selected.clone()];
    let mut tui = hermon::ui::App::default();
    tui.apply_event(Event::Roster(roster.clone()));
    tui.take_commands();
    tui.apply_event(Event::Pipeline(vec![pipeline()]));
    assert!(tui.select_pipeline(0));
    assert_eq!(tui.selected_id, Some(selected.id.clone()));
    assert!(
        tui.take_commands()
            .contains(&UiCmd::OpenPane(selected.id.clone()))
    );
    let (_events, rx) = mpsc::channel();
    let (tx, commands) = mpsc::channel();
    let mut gui = hermon::gui::App::new(rx, tx, vec![], 6);
    gui.apply_event(Event::Roster(roster));
    gui.apply_event(Event::Pipeline(vec![pipeline()]));
    assert!(gui.select_pipeline(0));
    assert_eq!(gui.selected_id, Some(selected.id.clone()));
    assert!(
        commands
            .try_iter()
            .any(|c| c == UiCmd::OpenPane(selected.id.clone()))
    );
}

fn save_record(data: &Path, id: &str, paused: bool, cancelled: bool) -> RunRecord {
    let source = "version: 1\nname: isolation\nsteps:\n  work:\n    bot: worker\n";
    let mut run = Run::new(id, "work");
    run.paused = paused;
    run.cancelled = cancelled;
    let record = RunRecord {
        schema: 1,
        ticket: id.into(),
        workflow_name: "isolation".into(),
        workflow_hash: hash(source),
        workflow_source: source.into(),
        template_hashes: BTreeMap::new(),
        templates: BTreeMap::new(),
        project: data.join("missing-project.yaml"),
        worktree: data.to_owned(),
        base: "main".into(),
        base_commit: "fixture".into(),
        branch: format!("flow/{id}"),
        paused,
        cancelled,
        run,
        notices: vec![],
        started: true,
    };
    let dir = data.join("runs").join(id);
    fs::create_dir_all(&dir).unwrap();
    save(&dir, &record).unwrap();
    record
}

#[test]
fn host_isolates_broken_run_and_does_not_dispatch_paused_or_cancelled() {
    let data = tempfile::tempdir().unwrap();
    let broken = data.path().join("runs/00-broken");
    fs::create_dir_all(&broken).unwrap();
    fs::write(broken.join("run.json"), "{").unwrap();
    save_record(data.path(), "01-paused", true, false);
    save_record(data.path(), "02-cancelled", false, true);
    save_record(data.path(), "03-unconfigured", false, false);
    let mut host = Host::acquire(data.path(), &std::env::current_exe().unwrap())
        .unwrap()
        .unwrap();
    let records = host.tick().unwrap();
    assert_eq!(
        records.len(),
        3,
        "one invalid snapshot cannot stop other runs"
    );
    for record in records.iter().filter(|r| r.run.id != "03-unconfigured") {
        assert_eq!(record.run.status, Status::Ready);
        assert!(record.run.active.is_none());
        assert!(record.run.attempts.is_empty());
        assert!(
            record.run.events.is_empty(),
            "paused/cancelled run requires no project configuration"
        );
    }
    let unconfigured = records
        .iter()
        .find(|r| r.run.id == "03-unconfigured")
        .unwrap();
    assert!(
        unconfigured
            .run
            .events
            .iter()
            .any(|e| e.kind == "host_error"),
        "configuration failure must become visible owner attention"
    );
    let repeat = host.tick().unwrap();
    assert_eq!(
        repeat
            .iter()
            .find(|r| r.run.id == "03-unconfigured")
            .unwrap()
            .run
            .events
            .len(),
        unconfigured.run.events.len(),
        "unchanged host error does not append endlessly"
    );
}

#[test]
fn busy_run_snapshot_does_not_block_other_run_or_transcript_polling() {
    let data = tempfile::tempdir().unwrap();
    save_record(data.path(), "busy", true, false);
    save_record(data.path(), "available", true, false);
    let guard = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(data.path().join("runs/busy/run.lock"))
        .unwrap();
    guard.lock().unwrap();
    let path = data.path().to_owned();
    let (tx, rx) = mpsc::channel();
    let reader = std::thread::spawn(move || {
        tx.send(snapshots(&path)).unwrap();
    });
    let records = rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .expect("snapshot refresh blocked on an executing run")
        .unwrap();
    assert_eq!(records.len(), 1);
    assert_eq!(records[0].run.id, "available");
    drop(guard);
    reader.join().unwrap();
    assert_eq!(
        snapshots(data.path()).unwrap().len(),
        2,
        "next refresh reattaches to previously busy run"
    );
}
