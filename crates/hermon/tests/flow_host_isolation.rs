//! Independent host review regression: one unreadable run lock must not starve others.
use hermon_flow::{
    control::{RunRecord, save},
    host::{Host, snapshots},
    hub::Run,
    intake::hash,
};
use std::{collections::BTreeMap, fs, path::Path};

fn paused_record(data: &Path, id: &str) {
    let workflow = "version: 1\nname: isolation\nsteps:\n  work:\n    bot: worker\n";
    let mut run = Run::new(id, "work");
    run.paused = true;
    let record = RunRecord {
        schema: 1,
        ticket: id.into(),
        workflow_name: "isolation".into(),
        workflow_hash: hash(workflow),
        workflow_source: workflow.into(),
        template_hashes: BTreeMap::new(),
        templates: BTreeMap::new(),
        project: data.join("unused-project.yaml"),
        worktree: data.to_owned(),
        base: "main".into(),
        base_commit: "fixture".into(),
        branch: format!("flow/{id}"),
        paused: true,
        cancelled: false,
        run,
        notices: vec![],
        started: true,
    };
    let directory = data.join("runs").join(id);
    fs::create_dir_all(&directory).unwrap();
    save(&directory, &record).unwrap();
}

#[test]
fn invalid_run_lock_cannot_starve_other_runs() {
    let root = tempfile::tempdir().unwrap();
    paused_record(root.path(), "00-broken-lock");
    paused_record(root.path(), "01-healthy");
    // Deterministic open failure on all platforms, unlike chmod in a root CI.
    fs::create_dir(root.path().join("runs/00-broken-lock/run.lock")).unwrap();
    let visible = snapshots(root.path()).unwrap();
    assert_eq!(visible.len(), 1);
    assert_eq!(visible[0].run.id, "01-healthy");
    let mut host = Host::acquire(root.path(), &std::env::current_exe().unwrap())
        .unwrap()
        .unwrap();
    for _ in 0..2 {
        let records = host
            .tick()
            .expect("one run lock error must be isolated to that run");
        assert_eq!(records.len(), 1);
        assert_eq!(records[0].run.id, "01-healthy");
        assert!(records[0].run.events.is_empty());
    }
}
