use hermon::source::{Replay, Source, Tailer, grok::GrokSource};
use std::{fs, io::Write, path::Path};
use tempfile::TempDir;
fn append(p: &Path, bytes: &[u8]) {
    fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(p)
        .unwrap()
        .write_all(bytes)
        .unwrap();
}
fn line(text: &str) -> String {
    format!(
        "{{\"type\":\"assistant\",\"content\":{}}}\n",
        serde_json::to_string(text).unwrap()
    )
}
fn plain(t: &mut dyn Tailer) -> Vec<String> {
    t.poll().iter().map(|l| l.to_plain()).collect()
}
fn setup(bytes: u64) -> (TempDir, std::path::PathBuf, Box<dyn Tailer>) {
    let root = TempDir::new().unwrap();
    let p = root.path().join("sessions/project/id");
    fs::create_dir_all(&p).unwrap();
    let p = p.join("chat_history.jsonl");
    fs::write(&p, line("old")).unwrap();
    let mut src = GrokSource::new(root.path());
    src.sessions();
    let t = src.open_tailer("id", Replay { bytes, rows: 0 }).unwrap();
    (root, p, t)
}
#[test]
fn replay_append_utf8_malformed_and_limits() {
    let (_root, p, mut t) = setup(0);
    assert!(plain(&mut *t).is_empty());
    let bytes = line("雪").into_bytes();
    let split = bytes.iter().position(|b| *b == 0xe9).unwrap() + 1;
    append(&p, &bytes[..split]);
    assert!(plain(&mut *t).is_empty());
    append(&p, &bytes[split..]);
    assert_eq!(plain(&mut *t), ["雪"]);
    assert!(plain(&mut *t).is_empty());
    append(&p, b"malformed\n");
    append(&p, line("after").as_bytes());
    assert_eq!(plain(&mut *t), ["after"]);
    append(&p, &vec![b'x'; 300000]);
    assert!(plain(&mut *t).is_empty());
    append(&p, b"\n");
    append(&p, line("recovered").as_bytes());
    assert_eq!(plain(&mut *t), ["recovered"]);
    append(&p, line(&"x".repeat(1000)).as_bytes());
    assert!(plain(&mut *t)[0].chars().count() <= 200);
    let (_r, _p, mut replay) = setup(1000);
    assert_eq!(plain(&mut *replay), ["old"]);
    let (_r, _p, mut replay) = setup(5);
    assert!(plain(&mut *replay).is_empty());
}
#[test]
fn truncation_replacement_and_missing_recovery() {
    let (_root, p, mut t) = setup(1000);
    assert_eq!(plain(&mut *t), ["old"]);
    fs::write(&p, line("new")).unwrap();
    let out = plain(&mut *t);
    assert!(out[0].contains("reset"));
    assert_eq!(out[1], "new");
    fs::write(&p, "").unwrap();
    assert!(plain(&mut *t)[0].contains("reset"));
    append(&p, line("after").as_bytes());
    assert_eq!(plain(&mut *t), ["after"]);
    fs::write(p.with_extension("tmp"), line("atomic")).unwrap();
    fs::rename(p.with_extension("tmp"), &p).unwrap();
    assert_eq!(plain(&mut *t)[1], "atomic");
    fs::remove_file(&p).unwrap();
    assert!(plain(&mut *t)[0].contains("unavailable"));
    assert!(plain(&mut *t).is_empty());
    append(&p, line("recreated").as_bytes());
    assert_eq!(plain(&mut *t)[1], "recreated");
}
#[test]
fn reasoning_hidden_and_semantic_styles_sanitized() {
    let (_root, p, mut t) = setup(0);
    t.poll();
    append(&p, include_bytes!("fixtures/grok/chat_history.jsonl"));
    let lines = t.poll();
    assert!(
        lines
            .iter()
            .any(|l| l.0.iter().any(|s| s.sem == hermon::render::Sem::Tool))
    );
    assert!(!lines.iter().any(|l| l.to_plain().contains("DO-NOT-RENDER")));
    append(&p, line("\u{1b}[31m hello").as_bytes());
    assert!(!plain(&mut *t)[0].contains('\u{1b}'));
}

#[test]
fn zero_replay_recovers_and_incomplete_initial_record_is_skipped() {
    let (_root, p, mut t) = setup(0);
    assert!(plain(&mut *t).is_empty());
    fs::write(&p, line("replacement")).unwrap();
    assert_eq!(plain(&mut *t)[1], "replacement");
    fs::remove_file(&p).unwrap();
    t.poll();
    append(&p, line("reborn").as_bytes());
    assert_eq!(plain(&mut *t)[1], "reborn");
    let root = TempDir::new().unwrap();
    let dir = root.path().join("sessions/p/s");
    fs::create_dir_all(&dir).unwrap();
    let p = dir.join("chat_history.jsonl");
    fs::write(&p, b"{\"type\":\"assistant\",\"content\":\"partial").unwrap();
    let mut src = GrokSource::new(root.path());
    src.sessions();
    let mut t = src.open_tailer("s", Replay { bytes: 0, rows: 0 }).unwrap();
    t.poll();
    append(&p, b"\"}\n");
    append(&p, line("fresh").as_bytes());
    assert_eq!(plain(&mut *t), ["fresh"]);
}

#[test]
fn replay_is_bounded_and_newline_boundary_is_preserved() {
    let (_root, p, mut t) = setup(u64::MAX);
    fs::write(
        &p,
        format!(
            "{}{}\n{}",
            line("too old"),
            "x".repeat(5 * 1024 * 1024),
            line("recent")
        ),
    )
    .unwrap();
    assert_eq!(plain(&mut *t), ["recent"]);
    assert!(plain(&mut *t).is_empty());
}

#[test]
fn initially_missing_transcript_resumes_with_zero_replay() {
    let root = TempDir::new().unwrap();
    let dir = root.path().join("sessions/p/s");
    fs::create_dir_all(&dir).unwrap();
    let mut src = GrokSource::new(root.path());
    src.sessions();
    let mut t = src.open_tailer("s", Replay { bytes: 0, rows: 0 }).unwrap();
    assert!(plain(&mut *t)[0].contains("unavailable"));
    assert!(plain(&mut *t).is_empty());
    fs::write(dir.join("chat_history.jsonl"), line("arrived")).unwrap();
    assert_eq!(plain(&mut *t), ["arrived"]);
}
