//! Roster acceptance tests: one row per session across all three fixture
//! stores, and the `hermon ls` binary end to end.

mod common;

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command as Proc;

use chrono::DateTime;
use rusqlite::{Connection, params};
use tempfile::TempDir;

use common::{fixture_path, temp_db_from_schema};
use hermon::render::StyledLine;
use hermon::roster::{
    RosterRow, Sources, TICKER_LIMIT, api_call_ticker, build_roster, roster_lines,
};
use hermon::source::{Attn, Liveness};

const NOW: f64 = 1_800_000_000.0;
const IDLE: f64 = 180.0;
const FRESH: f64 = 3_600.0;

// ------------------------------------------------------------- fixtures

/// A Claude transcript root holding one session: a user prompt, a `Read`
/// tool call, then a closing line of assistant text.
fn claude_dir(now: f64) -> TempDir {
    let dir = TempDir::new().expect("create temp dir");
    let project = dir.path().join("-Users-taloz-code-hermon");
    fs::create_dir_all(&project).expect("create project dir");
    let body = format!(
        "{}{}{}",
        user_line(now - 300.0),
        tool_use_line(now - 120.0),
        format_args!(
            concat!(
                r#"{{"type":"assistant","timestamp":"{}","message":{{"role":"assistant","#,
                r#""model":"claude-fable-5","content":[{{"type":"text","text":"Done."}}],"#,
                r#""usage":{{"input_tokens":10,"output_tokens":10}}}},"costUSD":0.05}}"#,
                "\n",
            ),
            iso(now - 20.0),
        ),
    );
    fs::write(
        project.join("d4e5f6a7-1234-5678-9abc-def012345678.jsonl"),
        body,
    )
    .expect("write transcript");
    dir
}

fn user_line(ts: f64) -> String {
    format!(
        "{{\"type\":\"user\",\"timestamp\":\"{}\",\
         \"message\":{{\"role\":\"user\",\"content\":\"port the roster\"}}}}\n",
        iso(ts)
    )
}

fn tool_use_line(ts: f64) -> String {
    format!(
        concat!(
            r#"{{"type":"assistant","timestamp":"{}","message":{{"role":"assistant","#,
            r#""model":"claude-fable-5","content":[{{"type":"tool_use","name":"Read","#,
            r#""input":{{"file_path":"hermon.py"}}}}],"#,
            r#""usage":{{"input_tokens":100,"cache_read_input_tokens":25,"output_tokens":20}}}},"#,
            r#""costUSD":0.2}}"#,
            "\n",
        ),
        iso(ts),
    )
}

fn iso(ts: f64) -> String {
    DateTime::from_timestamp(ts as i64, 0)
        .expect("valid timestamp")
        .format("%Y-%m-%dT%H:%M:%SZ")
        .to_string()
}

/// A Hermes state.db with one mid-turn session and one long-finished one.
fn hermes_db(now: f64) -> (TempDir, PathBuf) {
    let (dir, db_path) = temp_db_from_schema(&fixture_path("hermes_schema.sql"));
    let conn = Connection::open(&db_path).expect("open temp db for seeding");
    conn.execute(
        "INSERT INTO sessions (id, source, model, title, started_at, ended_at,
                               input_tokens, output_tokens, cache_read_tokens,
                               estimated_cost_usd)
         VALUES ('sess_b356d8', 'tui', 'gpt-5.1-codex', 'Wire up ls', ?1, NULL,
                 100, 20, 50, 0.05)",
        [now - 600.0],
    )
    .expect("insert live session");
    conn.execute(
        "INSERT INTO messages (session_id, role, tool_name, content, timestamp)
         VALUES ('sess_b356d8', 'tool', 'terminal', '{\"output\":\"ok\",\"exit_code\":0}', ?1)",
        [now - 10.0],
    )
    .expect("insert message");

    conn.execute(
        "INSERT INTO sessions (id, source, model, started_at, ended_at)
         VALUES ('sess_stale1', 'tui', 'gpt-5.1-codex', ?1, NULL)",
        [now - 90_000.0],
    )
    .expect("insert stale session");
    conn.execute(
        "INSERT INTO messages (session_id, role, content, finish_reason, timestamp)
         VALUES ('sess_stale1', 'assistant', 'bye', 'stop', ?1)",
        [now - 80_000.0],
    )
    .expect("insert stale message");
    conn.close().expect("close seeding connection");
    (dir, db_path)
}

/// An OpenCode db with one session sitting on a pending tool call.
fn opencode_db(now: f64) -> (TempDir, PathBuf) {
    let (dir, db_path) = temp_db_from_schema(&fixture_path("opencode_schema.sql"));
    let conn = Connection::open(&db_path).expect("open temp db for seeding");
    conn.pragma_update(None, "foreign_keys", false)
        .expect("disable foreign keys");
    conn.execute(
        "INSERT INTO session (id, project_id, slug, directory, title, version, model,
         cost, tokens_input, tokens_output, tokens_cache_read, tokens_cache_write,
         time_created, time_updated, time_archived)
         VALUES ('ses_fiiDPP', 'prj', 'slug', '/tmp', 'Refactor tests', '1',
                 '{\"id\":\"claude-sonnet-5\",\"providerID\":\"github-copilot\"}',
                 0.25, 1000, 200, 500, 50, ?1, ?2, NULL)",
        params![(now - 900.0) * 1000.0, (now - 30.0) * 1000.0],
    )
    .expect("insert session");
    conn.execute(
        "INSERT INTO message (id, session_id, time_created, time_updated, data)
         VALUES ('msg_1', 'ses_fiiDPP', ?1, ?1, '{\"role\":\"assistant\",\"finish\":\"tool-calls\"}')",
        params![(now - 30.0) * 1000.0],
    )
    .expect("insert message");
    conn.execute(
        "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
         VALUES ('prt_1', 'msg_1', 'ses_fiiDPP', ?1, ?1, '{\"type\":\"tool\",\"tool\":\"bash\"}')",
        params![(now - 30.0) * 1000.0],
    )
    .expect("insert part");
    conn.close().expect("close seeding connection");
    (dir, db_path)
}

struct Fixtures {
    _claude: TempDir,
    _hermes: TempDir,
    _opencode: TempDir,
    claude_dir: PathBuf,
    hermes_db: PathBuf,
    opencode_db: PathBuf,
}

/// The three stores seeded with sessions positioned relative to `now`.
fn fixtures_at(now: f64) -> Fixtures {
    let claude = claude_dir(now);
    let (hermes, hermes_db) = hermes_db(now);
    let (opencode, opencode_db) = opencode_db(now);
    Fixtures {
        claude_dir: claude.path().to_path_buf(),
        _claude: claude,
        _hermes: hermes,
        _opencode: opencode,
        hermes_db,
        opencode_db,
    }
}

fn fixtures() -> Fixtures {
    fixtures_at(NOW)
}

fn wall_clock_now() -> f64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after the epoch")
        .as_secs_f64()
}

impl Fixtures {
    fn sources(&self) -> Sources {
        Sources::new(
            self.claude_dir.to_str().unwrap(),
            self.hermes_db.to_str().unwrap(),
            self.opencode_db.to_str().unwrap(),
        )
    }
}

fn find<'a>(rows: &'a [RosterRow], key: &str) -> &'a RosterRow {
    rows.iter()
        .find(|r| r.key == key)
        .unwrap_or_else(|| panic!("no row {key} in {:?}", keys(rows)))
}

fn keys(rows: &[RosterRow]) -> Vec<&str> {
    rows.iter().map(|r| r.key.as_str()).collect()
}

fn plain(lines: &[StyledLine]) -> Vec<String> {
    lines.iter().map(StyledLine::to_plain).collect()
}

// ----------------------------------------------------------------- tests

#[test]
fn roster_has_one_row_per_session_across_all_sources() {
    let fx = fixtures();
    let rows = build_roster(&mut fx.sources(), NOW, FRESH, IDLE);

    // The stale, finished Hermes session is outside the window and dropped.
    assert_eq!(keys(&rows), vec!["H:b356d8", "C:345678", "O:fiiDPP"]);

    let claude = find(&rows, "C:345678");
    assert_eq!(claude.state, Liveness::Live);
    assert_eq!(claude.model, "claude-fable-5");
    assert_eq!(claude.last_tool, "Read");
    assert_eq!(claude.in_tok, 100 + 25 + 10);
    assert_eq!(claude.out_tok, 30);
    assert!((claude.cost - 0.25).abs() < 1e-9);
    assert_eq!(claude.elapsed, Some(280.0));
    assert_eq!(claude.title, "");

    let hermes = find(&rows, "H:b356d8");
    assert_eq!(hermes.state, Liveness::Live);
    assert_eq!(hermes.model, "gpt-5.1-codex");
    assert_eq!(hermes.last_tool, "terminal");
    assert_eq!(hermes.in_tok, 150);
    assert_eq!(hermes.out_tok, 20);
    assert!((hermes.cost - 0.05).abs() < 1e-9);
    assert_eq!(hermes.elapsed, Some(590.0));
    assert_eq!(hermes.title, "Wire up ls");

    let opencode = find(&rows, "O:fiiDPP");
    assert_eq!(opencode.state, Liveness::Live);
    assert_eq!(opencode.model, "claude-sonnet-5");
    assert_eq!(opencode.last_tool, "bash");
    assert_eq!(opencode.in_tok, 1550);
    assert_eq!(opencode.out_tok, 200);
    assert!((opencode.cost - 0.25).abs() < 1e-9);
    assert_eq!(opencode.elapsed, Some(870.0));
    assert_eq!(opencode.title, "Refactor tests");
}

#[test]
fn rows_are_ordered_by_most_recent_activity() {
    let fx = fixtures();
    let rows = build_roster(&mut fx.sources(), NOW, FRESH, IDLE);
    let stamps: Vec<f64> = rows.iter().map(|r| r.last_ts).collect();
    let mut sorted = stamps.clone();
    sorted.sort_by(|a, b| b.total_cmp(a));
    assert_eq!(stamps, sorted);
}

#[test]
fn finished_sessions_stay_until_the_window_closes() {
    let fx = fixtures();

    // 24h later the once-live sessions have all gone quiet and aged out.
    let rows = build_roster(&mut fx.sources(), NOW + 86_400.0, FRESH, IDLE);
    assert_eq!(keys(&rows), Vec::<&str>::new());

    // The stale Hermes session is only 80_000s old; widen the window and it
    // comes back, marked done.
    let rows = build_roster(&mut fx.sources(), NOW, 100_000.0, IDLE);
    assert_eq!(find(&rows, "H:stale1").state, Liveness::Done);
}

#[test]
fn a_tool_call_gone_quiet_needs_attention() {
    // A transcript whose last event is an unanswered tool call, silent past
    // the permission-prompt threshold: probably waiting on a human.
    let dir = TempDir::new().expect("create temp dir");
    fs::write(dir.path().join("s.jsonl"), tool_use_line(NOW - 45.0)).expect("write transcript");
    let mut sources = Sources::new(
        dir.path().to_str().unwrap(),
        "/nonexistent/state.db",
        "/nonexistent/opencode.db",
    );
    let rows = build_roster(&mut sources, NOW, FRESH, IDLE);
    assert_eq!(rows[0].state, Liveness::Attention(Attn::PermWait));

    // OpenCode's pending tool call, 20 minutes on, has blown its ceiling.
    let fx = fixtures();
    let rows = build_roster(&mut fx.sources(), NOW + 1_200.0, FRESH, IDLE);
    assert_eq!(
        find(&rows, "O:fiiDPP").state,
        Liveness::Attention(Attn::Stuck)
    );
}

#[test]
fn rendered_roster_shows_glyphs_columns_and_totals() {
    let fx = fixtures();
    let rows = build_roster(&mut fx.sources(), NOW, FRESH, IDLE);
    let lines = plain(&roster_lines(&rows, &[], NOW));

    assert!(
        lines[0].starts_with("hermon · 3 session(s) · "),
        "{lines:?}"
    );
    assert!(lines[1].starts_with("  id        model"), "{}", lines[1]);

    let hermes = lines
        .iter()
        .find(|l| l.contains("H:b356d8"))
        .expect("hermes row rendered");
    assert!(hermes.starts_with("● H:b356d8  gpt-5.1-codex"), "{hermes}");
    assert!(hermes.contains("terminal"), "{hermes}");
    assert!(hermes.contains("      150       20   0.0500"), "{hermes}");
    assert!(hermes.ends_with("9m50s  Wire up ls"), "{hermes}");

    assert_eq!(
        lines.last().unwrap(),
        "3 live · 0 done · Σ $0.55 · 1,835 in"
    );
}

#[test]
fn missing_stores_yield_an_empty_roster() {
    let mut sources = Sources::new(
        "/nonexistent/claude/projects",
        "/nonexistent/state.db",
        "/nonexistent/opencode.db",
    );
    let rows = build_roster(&mut sources, NOW, FRESH, IDLE);
    assert_eq!(rows, Vec::new());

    let lines = plain(&roster_lines(&rows, &[], NOW));
    assert!(lines.iter().any(|l| l.contains("(no sessions in window")));
    assert_eq!(lines.last().unwrap(), "0 live · 0 done · Σ $0.00 · 0 in");
}

#[test]
fn ticker_lines_join_the_roster() {
    let dir = TempDir::new().expect("create temp dir");
    let log = dir.path().join("agent.log");
    fs::write(
        &log,
        "09:41:07,123 INFO [hermes.b356d8] agent.conversation_loop: API call #7: \
         model=gpt-5.1-codex provider=openai in=12000 out=350 latency=2.4s\n",
    )
    .expect("write log");

    let ticker = api_call_ticker(&log, TICKER_LIMIT);
    let lines = plain(&roster_lines(&[], &ticker, NOW));
    assert_eq!(
        lines.last().unwrap(),
        "  09:41:07 b356d8 #  7 gpt-5.1-codex@openai in=12,000 out=350 2.4s"
    );
}

// ------------------------------------------------------------ the binary

fn run_ls(args: &[&str], no_color: bool) -> String {
    let mut cmd = Proc::new(env!("CARGO_BIN_EXE_hermon"));
    cmd.arg("ls").args(args);
    if no_color {
        cmd.env("NO_COLOR", "1");
    } else {
        cmd.env_remove("NO_COLOR");
    }
    let out = cmd.output().expect("run hermon ls");
    assert!(out.status.success(), "hermon ls failed: {out:?}");
    String::from_utf8(out.stdout).expect("utf-8 stdout")
}

fn store_args(fx: &Fixtures, log: &Path) -> Vec<String> {
    vec![
        "--claude-dir".into(),
        fx.claude_dir.display().to_string(),
        "--hermes-db".into(),
        fx.hermes_db.display().to_string(),
        "--opencode-db".into(),
        fx.opencode_db.display().to_string(),
        "--hermes-log".into(),
        log.display().to_string(),
    ]
}

#[test]
fn ls_with_no_stores_prints_an_empty_roster() {
    let stdout = run_ls(
        &[
            "--claude-dir",
            "/nonexistent/projects",
            "--hermes-db",
            "/nonexistent/state.db",
            "--opencode-db",
            "/nonexistent/opencode.db",
            "--hermes-log",
            "/nonexistent/agent.log",
        ],
        true,
    );
    assert!(stdout.contains("hermon · 0 session(s)"), "{stdout}");
    assert!(stdout.contains("(no sessions in window"), "{stdout}");
    assert!(
        stdout.contains("0 live · 0 done · Σ $0.00 · 0 in"),
        "{stdout}"
    );
}

#[test]
fn ls_prints_a_row_per_fixture_session_without_ansi() {
    // Seeded against the wall clock so the binary, which has no injectable
    // `now`, sees the same live sessions the unit tests do.
    let fx = fixtures_at(wall_clock_now());
    let log = fx.claude_dir.join("agent.log"); // absent: no ticker
    let args = store_args(&fx, &log);
    let args: Vec<&str> = args.iter().map(String::as_str).collect();
    let stdout = run_ls(&args, true);

    assert!(stdout.contains("hermon · 3 session(s)"), "{stdout}");
    assert!(stdout.contains("● C:345678  claude-fable-5"), "{stdout}");
    assert!(stdout.contains("● H:b356d8  gpt-5.1-codex"), "{stdout}");
    assert!(stdout.contains("● O:fiiDPP  claude-sonnet-5"), "{stdout}");
    assert!(
        stdout.contains("3 live · 0 done · Σ $0.55 · 1,835 in"),
        "{stdout}"
    );
    assert!(
        !stdout.contains('\u{1b}'),
        "NO_COLOR output carries escapes"
    );

    // Not a tty either way, so color stays off even without NO_COLOR.
    let colorless = run_ls(&args, false);
    assert!(!colorless.contains('\u{1b}'), "{colorless}");
}

#[test]
fn render_prints_the_replay_without_ansi_under_no_color() {
    let fx = fixtures_at(wall_clock_now());
    let log = fx.claude_dir.join("agent.log");
    let mut args = vec!["C:345678".to_string()];
    args.extend(store_args(&fx, &log));

    let mut cmd = Proc::new(env!("CARGO_BIN_EXE_hermon"));
    cmd.arg("render").args(&args).env("NO_COLOR", "1");
    cmd.stdout(std::process::Stdio::piped());
    let mut child = cmd.spawn().expect("spawn hermon render");
    let mut stdout = child.stdout.take().expect("piped stdout");

    let read = std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 4096];
        let n = stdout.read(&mut buf).unwrap_or(0);
        String::from_utf8_lossy(&buf[..n]).to_string()
    });
    let output = read.join().expect("read replay output");
    let _ = child.kill();
    let _ = child.wait();

    assert!(!output.is_empty(), "expected replayed transcript lines");
    assert!(
        !output.contains('\u{1b}'),
        "NO_COLOR output carries escapes: {output}"
    );
}

#[test]
fn render_replay_bytes_zero_skips_all_existing_transcript_content() {
    let fx = fixtures_at(wall_clock_now());
    let log = fx.claude_dir.join("agent.log");
    let mut args = vec![
        "C:345678".to_string(),
        "--replay-bytes".to_string(),
        "0".to_string(),
    ];
    args.extend(store_args(&fx, &log));

    let mut cmd = Proc::new(env!("CARGO_BIN_EXE_hermon"));
    cmd.arg("render").args(&args).env("NO_COLOR", "1");
    cmd.stdout(std::process::Stdio::piped());
    let mut child = cmd.spawn().expect("spawn hermon render");
    let mut stdout = child.stdout.take().expect("piped stdout");

    // No replay means no output arrives before the pane tick, unlike the
    // default-replay case above where the seeded transcript prints
    // immediately — assert the read times out instead of racing it.
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        use std::io::Read;
        let mut buf = [0u8; 4096];
        let n = stdout.read(&mut buf).unwrap_or(0);
        let _ = tx.send(String::from_utf8_lossy(&buf[..n]).to_string());
    });
    let output = rx.recv_timeout(std::time::Duration::from_millis(500));

    let _ = child.kill();
    let _ = child.wait();

    assert!(
        output.is_err(),
        "--replay-bytes 0 should skip the seeded transcript, got {output:?}"
    );
}
