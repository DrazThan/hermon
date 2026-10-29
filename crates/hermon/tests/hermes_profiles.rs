//! Hermes profile discovery, overrides, collisions, ticker ownership, and the
//! primary behavioral RED.
//!
//! Public smoke checklist — not executed here. Compilation is not a GUI or
//! TUI result. Pending visible checks, each requiring a working build and
//! public interaction with --no-notify:
//! visible owner labels and pane headers; choose each bot; distinct selected
//! and pinned sessions; list, grid, preview, and zoom identity; clear and
//! AND-composed selector/text filter; inferred attention and recent-finished
//! wording, counts, and last recorded activity; current-view totals;
//! bot-filtered local ticker; warnings still visible under a bot filter.
//! GUI checks need the public window, not private sync_panes/toggle_pin.

mod common;

use std::fs;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use hermon::remote::proto::{HostCmd, encode_host_cmd};
use hermon::source::Replay;

use common::hermes_profiles::*;

fn ls(home: &Home) -> CmdOut {
    hermon(
        home.path(),
        &discovery_ls_args(home.path()),
        Duration::from_secs(20),
    )
}

fn dump(out: &CmdOut) -> String {
    format!(
        "status={:?} timed_out={}\nstdout:\n{}\nstderr:\n{}",
        out.status, out.timed_out, out.stdout, out.stderr
    )
}

#[test]
fn hr1_decoder_is_strict_and_lossless() {
    let id = local_hr1("H", b"store", b"vendor-id");
    assert_eq!(id, "hr1::H:73746f7265:76656e646f722d6964");
    let route = decode_hr1(&id).expect("empty origin");
    assert!(route.origin.is_empty());
    assert_eq!(route.source, "H");
    assert_eq!(route.store, b"store");
    assert_eq!(route.vendor_id, b"vendor-id");

    let wire = wire_session_id("Gm", b"root", b"raw");
    let decoded = decode_wire_id(&wire).expect("wire");
    assert_eq!(decoded.outer_source, "Gm");
    assert!(decoded.route.origin.is_empty());
    assert_eq!(decoded.route.vendor_id, b"raw");

    assert!(decode_hr1("sess-main-fixture").is_err(), "no raw fallback");
    assert!(decode_hr1("H:abcdef").is_err());
    assert!(decode_hr1("hr1:AA:H:aa:bb").is_err(), "uppercase");
    assert!(decode_hr1("hr1:a:H:aa:bb").is_err(), "odd length");
    assert!(decode_hr1("hr1::Nope:aa:bb").is_err(), "unknown source");
    assert!(decode_hr1("hr1::H:aa").is_err(), "truncated");
    assert!(
        decode_wire_id("H:686c6c6f").is_err(),
        "no second hex encoding"
    );
    assert!(decode_wire_id("C:hr1::H:aa:bb").is_err(), "source mismatch");
}

#[test]
fn ls_default_discovery_includes_worker_profile() {
    let home = seed_main_and_worker();
    let args = discovery_ls_args(home.path());
    assert!(
        !args
            .iter()
            .any(|a| a == "--hermes-db" || a == "--hermes-home" || a == "--bot"),
        "primary RED must not pass new or explicit hermes-db flags: {args:?}"
    );
    let out = hermon(home.path(), &args, Duration::from_secs(20));
    assert!(
        out.success(),
        "existing ls must succeed before the worker assertion\n{}",
        dump(&out)
    );
    assert!(
        out.stdout.contains("main-fixture"),
        "main title missing\n{}",
        dump(&out)
    );
    assert!(
        out.stdout.contains("worker-fixture"),
        "RED: worker profile title absent from default discovery\n{}",
        dump(&out)
    );
}

#[test]
fn explicit_hermes_db_excludes_profiles_even_at_the_default_path() {
    let home = seed_main_and_worker();
    let mut args = discovery_ls_args(home.path());
    args.push("--hermes-db".into());
    args.push(home.main_db().display().to_string());
    let out = hermon(home.path(), &args, Duration::from_secs(20));
    assert!(out.success(), "{}", dump(&out));
    assert!(out.stdout.contains("main-fixture"), "{}", dump(&out));
    assert!(
        !out.stdout.contains("worker-fixture"),
        "explicit --hermes-db, including the ordinary default path, must not scan profiles\n{}",
        dump(&out)
    );
}

#[test]
fn hermes_home_replaces_process_home() {
    let hidden = seed_main_and_worker();
    let shown = empty_home();
    seed_finished(
        &shown.main_db(),
        "sess-other-home",
        "other-home-fixture",
        "other-body",
    );
    let mut args = discovery_ls_args(hidden.path());
    args.push("--hermes-home".into());
    args.push(shown.path().join(".hermes").display().to_string());
    let out = hermon(hidden.path(), &args, Duration::from_secs(20));
    assert!(out.success(), "{}", dump(&out));
    assert!(out.stdout.contains("other-home-fixture"), "{}", dump(&out));
    assert!(
        !out.stdout.contains("main-fixture") && !out.stdout.contains("worker-fixture"),
        "process HOME must not be scanned when --hermes-home is set\n{}",
        dump(&out)
    );
}

#[test]
fn hermes_home_conflicts_with_explicit_hermes_db() {
    let home = seed_main_and_worker();
    let mut args = discovery_ls_args(home.path());
    args.extend([
        "--hermes-home".into(),
        home.path().join(".hermes").display().to_string(),
        "--hermes-db".into(),
        home.main_db().display().to_string(),
    ]);
    let out = hermon(home.path(), &args, Duration::from_secs(15));
    assert!(
        !out.success(),
        "conflicting overrides must fail\n{}",
        dump(&out)
    );
    assert!(
        out.stderr.contains("cannot be used with"),
        "expected a clap conflict, not a missing-flag usage error\n{}",
        dump(&out)
    );
}

#[test]
fn absent_home_creates_nothing() {
    let home = empty_home();
    let out = ls(&home);
    assert!(out.success(), "{}", dump(&out));
    assert!(
        !out.stdout.contains("main-fixture"),
        "empty install must not invent sessions\n{}",
        dump(&out)
    );
    assert!(
        !home.main_db().exists(),
        "missing state.db must not be created"
    );
    assert!(
        !home.path().join(".hermes").join("profiles").exists(),
        "missing profiles dir must not be created"
    );
}

#[test]
fn corrupt_profile_warns_and_keeps_the_healthy_store() {
    let home = empty_home();
    seed_finished(&home.main_db(), "sess-main", "main-fixture", "main-body");
    let corrupt = home.profile_db("corrupt");
    fs::create_dir_all(corrupt.parent().unwrap()).unwrap();
    fs::write(&corrupt, b"this is not a sqlite database").unwrap();
    let out = ls(&home);
    assert!(out.success(), "{}", dump(&out));
    assert!(out.stdout.contains("main-fixture"), "{}", dump(&out));
    let combined = out.combined();
    assert!(
        combined.contains("corrupt"),
        "unavailable profile must be named in a warning\n{}",
        dump(&out)
    );
    assert!(
        out.stdout.contains("1 session")
            || out.stdout.contains("1 live")
            || out.stdout.contains("1 done"),
        "a warning must not be counted as a session\n{}",
        dump(&out)
    );
}

#[test]
fn agent_discovers_a_profile_created_after_start() {
    let home = empty_home();
    seed_finished(&home.main_db(), "sess-main", "main-fixture", "main-body");
    let agent = Agent::spawn(home.path(), &[]);
    let first = agent.recv_until(Duration::from_secs(8), |line| line.contains("main-fixture"));
    assert!(
        first.iter().any(|line| line.contains("main-fixture")),
        "agent never saw the main fixture: {first:?}"
    );
    seed_finished(
        &home.profile_db("worker"),
        "sess-worker",
        "worker-fixture",
        "worker-body",
    );
    let later = agent.recv_until(Duration::from_secs(8), |line| {
        line.contains("worker-fixture")
    });
    assert!(
        later.iter().any(|line| line.contains("worker-fixture")),
        "RED: worker profile created after start never appeared: {later:?}"
    );
}

#[cfg(unix)]
#[test]
fn symlink_outside_db_is_discovered_and_retarget_does_not_redirect() {
    let home = empty_home();
    let outside = home.path().join("outside");
    fs::create_dir_all(&outside).unwrap();
    let db_a = outside.join("a.db");
    let db_b = outside.join("b.db");
    seed_finished(&db_a, "shared-vendor", "outside-a", "sentinel-a");
    seed_finished(&db_b, "shared-vendor", "outside-b", "sentinel-b");
    let link = home.profile_db("alpha");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(&db_a, &link).unwrap();

    let mut agent = Agent::spawn(home.path(), &[]);
    let first = agent.recv_until(Duration::from_secs(8), |line| line.contains("outside-a"));
    let wire_a =
        snap_wire_for_title(first.iter().map(String::as_str), "outside-a").unwrap_or_else(|| {
            panic!("RED: symlinked profile store never appeared in a snap: {first:?}")
        });
    agent.write_line(&encode_host_cmd(&HostCmd::OpenTail {
        key: wire_a.clone(),
        replay: Replay::DEFAULT,
    }));
    let mut saw_a = false;
    let _ = agent.recv_until(Duration::from_secs(6), |line| {
        if let Some((key, text)) = tail_frame(line)
            && key == wire_a
            && text.contains("sentinel-a")
        {
            saw_a = true;
            return true;
        }
        false
    });
    assert!(saw_a, "admitted route A did not tail sentinel-a");
    agent.write_line(&encode_host_cmd(&HostCmd::CloseTail {
        key: wire_a.clone(),
    }));

    fs::remove_file(&link).unwrap();
    std::os::unix::fs::symlink(&db_b, &link).unwrap();
    let mut wire_b = None;
    let after = agent.recv_until(Duration::from_secs(8), |line| {
        if let Some(id) = snap_wire_for_title([line], "outside-b") {
            wire_b = Some(id);
            return true;
        }
        false
    });
    let wire_b = wire_b.unwrap_or_else(|| {
        panic!("retargeted store B did not appear in a snapshot within 8s: {after:?}")
    });
    assert_ne!(wire_b, wire_a, "store B must be a different admitted route");
    agent.write_line(&encode_host_cmd(&HostCmd::OpenTail {
        key: wire_b.clone(),
        replay: Replay::DEFAULT,
    }));
    let mut saw_b = false;
    let _ = agent.recv_until(Duration::from_secs(6), |line| {
        if let Some((key, text)) = tail_frame(line)
            && key == wire_b
            && text.contains("sentinel-b")
            && !text.contains("sentinel-a")
        {
            saw_b = true;
            return true;
        }
        false
    });
    assert!(saw_b, "new route B must tail sentinel-b and not sentinel-a");

    agent.write_line(&encode_host_cmd(&HostCmd::OpenTail {
        key: wire_a.clone(),
        replay: Replay::DEFAULT,
    }));
    let mut old_tails = Vec::new();
    let _ = agent.recv_until(Duration::from_secs(3), |line| {
        if let Some((key, text)) = tail_frame(line)
            && key == wire_a
        {
            old_tails.push(text);
        }
        false
    });
    assert!(
        old_tails.iter().all(|text| !text.contains("sentinel-b")),
        "old route A must not be redirected onto sentinel-b; refusal is allowed, got {old_tails:?}"
    );
}

fn snap_wire_for_title<'a>(
    lines: impl IntoIterator<Item = &'a str>,
    title: &str,
) -> Option<String> {
    for line in lines {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if value.get("type").and_then(|t| t.as_str()) != Some("Snap") {
            continue;
        }
        for session in value
            .get("sessions")
            .and_then(|s| s.as_array())
            .into_iter()
            .flatten()
        {
            if session.get("title").and_then(|t| t.as_str()) != Some(title) {
                continue;
            }
            if let Some(id) = session.get("id").and_then(|id| id.as_str())
                && decode_wire_id(id).is_ok()
            {
                return Some(id.to_string());
            }
        }
    }
    None
}

fn tail_frame(line: &str) -> Option<(String, String)> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    if value.get("type").and_then(|t| t.as_str()) != Some("Tail") {
        return None;
    }
    let key = value.get("key")?.as_str()?.to_string();
    Some((key, json_text(value.get("lines"))))
}

fn json_text(value: Option<&serde_json::Value>) -> String {
    let mut out = String::new();
    if let Some(value) = value {
        walk_text(value, &mut out);
    }
    out
}

fn walk_text(value: &serde_json::Value, out: &mut String) {
    match value {
        serde_json::Value::Object(map) => {
            if let Some(serde_json::Value::String(text)) = map.get("text") {
                out.push_str(text);
            }
            for (key, child) in map {
                if key != "text" {
                    walk_text(child, out);
                }
            }
        }
        serde_json::Value::Array(items) => {
            for item in items {
                walk_text(item, out);
            }
        }
        _ => {}
    }
}

#[cfg(unix)]
#[test]
fn dangling_and_loop_symlinks_warn_without_hanging() {
    let home = empty_home();
    seed_finished(&home.main_db(), "sess-main", "main-fixture", "main-body");
    let profiles = home.path().join(".hermes").join("profiles");
    fs::create_dir_all(&profiles).unwrap();
    std::os::unix::fs::symlink(
        home.path().join("missing-target"),
        profiles.join("dangling"),
    )
    .unwrap();
    std::os::unix::fs::symlink("loop", profiles.join("loop")).unwrap();
    let out = hermon(
        home.path(),
        &discovery_ls_args(home.path()),
        Duration::from_secs(8),
    );
    assert!(
        !out.timed_out,
        "symlink discovery must not hang\n{}",
        dump(&out)
    );
    assert!(out.success(), "{}", dump(&out));
    let combined = out.combined();
    assert!(
        combined.contains("dangling") && combined.contains("loop"),
        "loop and dangling profile links must warn\n{}",
        dump(&out)
    );
    assert!(out.stdout.contains("main-fixture"), "{}", dump(&out));
}

#[test]
fn nested_profile_db_is_not_a_discovered_bot() {
    let home = empty_home();
    seed_finished(&home.main_db(), "sess-main", "main-fixture", "main-body");
    let nested = home
        .path()
        .join(".hermes")
        .join("profiles")
        .join("worker")
        .join("nested")
        .join("state.db");
    seed_finished(&nested, "sess-nested", "nested-secret", "nested-body");
    let out = ls(&home);
    assert!(out.success(), "{}", dump(&out));
    assert!(out.stdout.contains("main-fixture"), "{}", dump(&out));
    assert!(
        !out.stdout.contains("nested-secret"),
        "discovery must not walk below profiles/<entry>/state.db\n{}",
        dump(&out)
    );
}

#[cfg(unix)]
#[test]
fn canonical_symlink_alias_is_not_a_second_bot() {
    let home = empty_home();
    seed_finished(
        &home.main_db(),
        "sess-shared",
        "shared-title",
        "shared-body",
    );
    let link = home.profile_db("alias");
    fs::create_dir_all(link.parent().unwrap()).unwrap();
    std::os::unix::fs::symlink(home.main_db(), &link).unwrap();
    let out = ls(&home);
    assert!(out.success(), "{}", dump(&out));
    assert_eq!(
        out.stdout.matches("shared-title").count(),
        1,
        "main wins; the symlink alias is not a second session\n{}",
        dump(&out)
    );
    assert!(
        out.combined().contains("alias"),
        "the losing profile name must appear in registry diagnostics\n{}",
        dump(&out)
    );
}

#[test]
fn literal_path_with_query_and_hash_is_readable_and_unmodified() {
    let home = empty_home();
    let db = home.path().join("weird?#store.db");
    seed_finished(&db, "sess-literal", "literal-fixture", "literal-body");
    let before = file_bytes(&db);
    let mut args = vec![
        "ls".into(),
        "--fresh-window".into(),
        FRESH_WINDOW.into(),
        "--hermes-db".into(),
        db.display().to_string(),
    ];
    args.extend(absent_source_args(home.path(), true));
    let out = hermon(home.path(), &args, Duration::from_secs(20));
    assert!(out.success(), "{}", dump(&out));
    assert!(
        out.stdout.contains("literal-fixture"),
        "a filename containing ? and # must open as a literal path\n{}",
        dump(&out)
    );
    assert_eq!(
        file_bytes(&db),
        before,
        "read-only open must not rewrite the db"
    );
    assert!(
        !home.main_db().exists(),
        "explicit db must not create the default store"
    );
}

#[test]
fn read_only_open_leaves_fixture_bytes_unchanged() {
    let home = empty_home();
    let db = home.path().join("plain-store.db");
    seed_finished(&db, "sess-plain", "plain-fixture", "plain-body");
    let before = file_bytes(&db);
    let mut args = vec![
        "ls".into(),
        "--fresh-window".into(),
        FRESH_WINDOW.into(),
        "--hermes-db".into(),
        db.display().to_string(),
    ];
    args.extend(absent_source_args(home.path(), true));
    let out = hermon(home.path(), &args, Duration::from_secs(20));
    assert!(out.success(), "{}", dump(&out));
    assert!(out.stdout.contains("plain-fixture"), "{}", dump(&out));
    assert_eq!(file_bytes(&db), before);
}

fn collision_home() -> (Home, String, String) {
    let home = empty_home();
    seed_finished(
        &home.main_db(),
        "alpha-session-abcdef",
        "alpha-title",
        "sentinel-alpha",
    );
    seed_finished(
        &home.profile_db("worker"),
        "beta-session-abcdef",
        "beta-title",
        "sentinel-beta",
    );
    let main = expected_local_id(&home.main_db(), "alpha-session-abcdef");
    let worker = expected_local_id(&home.profile_db("worker"), "beta-session-abcdef");
    (home, main, worker)
}

fn render_capture(home: &Home, key: &str, extra: &[String]) -> CmdOut {
    let mut args = vec![
        "render".into(),
        key.to_string(),
        "--fresh-window".into(),
        FRESH_WINDOW.into(),
    ];
    args.extend(absent_source_args(home.path(), true));
    args.extend(extra.iter().cloned());
    hermon(home.path(), &args, Duration::from_secs(3))
}

#[test]
fn same_suffix_short_alias_is_ambiguous_and_full_route_is_not() {
    let (home, main_route, worker_route) = collision_home();
    let short = render_capture(&home, "H:abcdef", &[]);
    let short_text = short.combined();
    assert!(
        short_text.contains("ambiguous"),
        "short alias shared by two stores must be ambiguous\n{}",
        dump(&short)
    );
    assert!(
        short_text.contains(&main_route) && short_text.contains(&worker_route),
        "ambiguous error must list both qualified routes\nmain={main_route}\nworker={worker_route}\n{}",
        dump(&short)
    );

    let full = render_capture(&home, &main_route, &[]);
    assert!(
        full.stdout.contains("sentinel-alpha"),
        "full route must tail only its store\n{}",
        dump(&full)
    );
    assert!(
        !full.stdout.contains("sentinel-beta"),
        "full route leaked the other store\n{}",
        dump(&full)
    );
}

#[test]
fn identical_vendor_id_in_two_stores_has_independent_routes() {
    let home = empty_home();
    seed_finished(
        &home.main_db(),
        "same-vendor-id",
        "main-same",
        "sentinel-main",
    );
    seed_finished(
        &home.profile_db("worker"),
        "same-vendor-id",
        "worker-same",
        "sentinel-worker",
    );
    let main_route = expected_local_id(&home.main_db(), "same-vendor-id");
    let worker_route = expected_local_id(&home.profile_db("worker"), "same-vendor-id");
    assert_ne!(
        main_route, worker_route,
        "store bytes must separate the routes"
    );
    let listed = ls(&home);
    assert!(
        listed.stdout.contains("main-same") && listed.stdout.contains("worker-same"),
        "both stores must be listed\n{}",
        dump(&listed)
    );
    assert!(
        listed.combined().contains("route="),
        "ls must print route= detail for the colliding alias\n{}",
        dump(&listed)
    );
    let full = render_capture(&home, &worker_route, &[]);
    assert!(
        full.stdout.contains("sentinel-worker") && !full.stdout.contains("sentinel-main"),
        "worker route leaked or missed its sentinel\n{}",
        dump(&full)
    );
}

#[test]
fn bot_filter_does_not_disambiguate_a_short_alias() {
    let (home, main_route, worker_route) = collision_home();
    let out = render_capture(&home, "H:abcdef", &["--bot".into(), "worker".into()]);
    let text = out.combined();
    assert!(
        text.contains("ambiguous"),
        "--bot must not make a globally ambiguous alias unique\n{}",
        dump(&out)
    );
    assert!(
        text.contains(&main_route) && text.contains(&worker_route),
        "both qualified alternatives must still be listed\n{}",
        dump(&out)
    );
}

#[test]
fn bot_filter_exact_case_and_glob() {
    let home = seed_main_and_worker();
    for pattern in ["worker", "WORKER", "work*"] {
        let mut args = discovery_ls_args(home.path());
        args.extend(["--bot".into(), pattern.into()]);
        let out = hermon(home.path(), &args, Duration::from_secs(20));
        assert!(out.success(), "{pattern}: {}", dump(&out));
        assert!(
            out.stdout.contains("worker-fixture") && !out.stdout.contains("main-fixture"),
            "{pattern}\n{}",
            dump(&out)
        );
    }
}

#[test]
fn bot_filter_no_match_succeeds_with_zero_sessions() {
    let home = seed_main_and_worker();
    let mut args = discovery_ls_args(home.path());
    args.extend(["--bot".into(), "nomatch".into()]);
    let out = hermon(home.path(), &args, Duration::from_secs(20));
    assert!(out.success(), "{}", dump(&out));
    assert!(!out.stdout.contains("main-fixture"), "{}", dump(&out));
    assert!(!out.stdout.contains("worker-fixture"), "{}", dump(&out));
    assert!(
        out.stdout.contains("0 session") || out.stdout.contains("0 live"),
        "no-match ls must succeed with zero sessions\n{}",
        dump(&out)
    );
}

#[test]
fn bot_default_excludes_unknown_owner_and_other_profiles() {
    let home = seed_main_and_worker();
    let claude = home.path().join("absent").join("claude");
    let project = claude.join("-fixture-project");
    fs::create_dir_all(&project).unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs_f64();
    let iso = chrono::DateTime::from_timestamp(now as i64, 0)
        .unwrap()
        .format("%Y-%m-%dT%H:%M:%SZ");
    fs::write(
        project.join("11111111-2222-3333-4444-555555555555.jsonl"),
        format!(
            "{{\"type\":\"user\",\"timestamp\":\"{iso}\",\"message\":{{\"role\":\"user\",\"content\":\"claude-unknown-text\"}}}}\n"
        ),
    )
    .unwrap();
    let mut args = discovery_ls_args(home.path());
    let idx = args.iter().position(|a| a == "--claude-dir").unwrap();
    args[idx + 1] = claude.display().to_string();
    args.extend(["--bot".into(), "default".into()]);
    let out = hermon(home.path(), &args, Duration::from_secs(20));
    assert!(out.success(), "{}", dump(&out));
    assert!(out.stdout.contains("main-fixture"), "{}", dump(&out));
    assert!(!out.stdout.contains("worker-fixture"), "{}", dump(&out));
    assert!(
        !out.stdout.contains("C:555555"),
        "unknown non-Hermes owner must not match bot=default\n{}",
        dump(&out)
    );
}

#[test]
fn blank_bot_argument_is_rejected() {
    let home = seed_main_and_worker();
    let mut args = discovery_ls_args(home.path());
    args.extend(["--bot".into(), "".into()]);
    let out = hermon(home.path(), &args, Duration::from_secs(15));
    assert!(!out.success(), "{}", dump(&out));
    assert!(
        out.stderr.to_lowercase().contains("blank") || out.stderr.to_lowercase().contains("empty"),
        "blank --bot must be rejected\n{}",
        dump(&out)
    );
}

#[test]
fn profile_log_is_labeled_and_not_crowded_out() {
    let home = empty_home();
    seed_finished(&home.main_db(), "sess-main", "main-fixture", "main-body");
    seed_finished(
        &home.profile_db("worker"),
        "sess-worker",
        "worker-fixture",
        "worker-body",
    );
    let mut main_log = String::new();
    for n in 1..=6 {
        main_log.push_str(&api_log_line(
            &format!("01:00:{n:02}"),
            &format!("main{n:02}"),
            n,
        ));
    }
    write_log(&home.main_log(), &main_log);
    write_log(
        &home.profile_log("worker"),
        &api_log_line("02:00:00", "wrklog", 9),
    );
    set_mtime(&home.main_log(), 1_000);
    set_mtime(&home.profile_log("worker"), 2_000);
    let mut args = vec!["ls".into(), "--fresh-window".into(), FRESH_WINDOW.into()];
    args.extend(absent_source_args(home.path(), false));
    let out = hermon(home.path(), &args, Duration::from_secs(20));
    assert!(out.success(), "{}", dump(&out));
    assert!(
        out.stdout.contains("wrklog"),
        "profile log must be discovered; file recency must not drop it under the four-entry cap\n{}",
        dump(&out)
    );
    assert!(
        out.stdout.contains("recent log tails (file recency)"),
        "ticker caption\n{}",
        dump(&out)
    );
}

#[test]
fn bot_filter_applies_to_ticker_before_the_four_entry_cap() {
    let home = empty_home();
    seed_finished(&home.main_db(), "sess-main", "main-fixture", "main-body");
    seed_finished(
        &home.profile_db("worker"),
        "sess-worker",
        "worker-fixture",
        "worker-body",
    );
    let mut noisy = String::new();
    for n in 1..=6 {
        noisy.push_str(&api_log_line(
            &format!("03:00:{n:02}"),
            &format!("nois{n:02}"),
            n,
        ));
    }
    write_log(&home.main_log(), &noisy);
    write_log(
        &home.profile_log("worker"),
        &api_log_line("04:00:00", "wrkcap", 1),
    );
    set_mtime(&home.main_log(), 5_000);
    set_mtime(&home.profile_log("worker"), 1_000);
    let mut args = vec!["ls".into(), "--fresh-window".into(), FRESH_WINDOW.into()];
    args.extend(absent_source_args(home.path(), false));
    args.extend(["--bot".into(), "worker".into()]);
    let out = hermon(home.path(), &args, Duration::from_secs(20));
    assert!(out.success(), "{}", dump(&out));
    assert!(
        out.stdout.contains("wrkcap"),
        "bot filter must run before the four-entry cap\n{}",
        dump(&out)
    );
    assert!(
        !out.stdout.contains("nois01"),
        "other bot calls must not survive a worker filter\n{}",
        dump(&out)
    );
    assert!(
        out.stdout.contains("bot-filtered API calls"),
        "ticker help must say other session filters do not apply\n{}",
        dump(&out)
    );
}

#[test]
fn explicit_hermes_log_is_exclusive_and_unknown_without_a_registered_path() {
    let home = empty_home();
    seed_finished(&home.main_db(), "sess-main", "main-fixture", "main-body");
    write_log(&home.main_log(), &api_log_line("01:00:01", "mainlg", 1));
    write_log(
        &home.profile_log("worker"),
        &api_log_line("01:00:02", "wrklg", 2),
    );
    let explicit = home.path().join("custom-agent.log");
    write_log(&explicit, &api_log_line("01:00:03", "custlg", 3));
    let mut args = discovery_ls_args(home.path());
    let idx = args.iter().position(|a| a == "--hermes-log").unwrap();
    args[idx + 1] = explicit.display().to_string();
    let out = hermon(home.path(), &args, Duration::from_secs(20));
    assert!(out.success(), "{}", dump(&out));
    assert!(out.stdout.contains("custlg"), "{}", dump(&out));
    assert!(
        !out.stdout.contains("mainlg") && !out.stdout.contains("wrklg"),
        "explicit --hermes-log must not also read registered logs\n{}",
        dump(&out)
    );
}

#[test]
fn empty_profile_stays_listed_and_finished_fixture_is_recent_not_active() {
    let home = empty_home();
    seed_finished(&home.main_db(), "sess-main", "main-fixture", "main-body");
    create_db(&home.profile_db("emptybot"));
    let out = ls(&home);
    assert!(out.success(), "{}", dump(&out));
    assert!(out.stdout.contains("main-fixture"), "{}", dump(&out));
    assert!(
        out.combined().contains("emptybot"),
        "a healthy empty DB must stay in the inventory\n{}",
        dump(&out)
    );
    assert!(
        out.stdout.contains("All discovered bots") && out.stdout.contains("recent"),
        "overview must label the window and recent-finished, not treat the fixed session as active\n{}",
        dump(&out)
    );
    assert!(
        out.stdout.contains("Selected view"),
        "current-view totals must be labeled\n{}",
        dump(&out)
    );
}

#[test]
fn hostile_profile_label_is_sanitized() {
    let home = empty_home();
    let slug = "evil\u{1b}[31mbot";
    seed_finished(
        &home.profile_db(slug),
        "sess-evil",
        "evil-title",
        "evil-body",
    );
    let out = ls(&home);
    assert!(out.success(), "{}", dump(&out));
    let title = out.stdout.contains("evil-title");
    let esc = out.combined().contains('\u{1b}');
    assert!(
        title && !esc,
        "title_present={title} esc_present={esc}\n{}",
        dump(&out)
    );
}

#[test]
fn smoke_cli_lists_colliding_profiles_empty_bot_and_corrupt_store() {
    let home = empty_home();
    seed_finished(
        &home.main_db(),
        "same-vendor-id",
        "main-same",
        "sentinel-main",
    );
    seed_finished(
        &home.profile_db("left"),
        "same-vendor-id",
        "left-same",
        "sentinel-left",
    );
    seed_finished(
        &home.profile_db("right"),
        "same-vendor-id",
        "right-same",
        "sentinel-right",
    );
    create_db(&home.profile_db("emptybot"));
    let corrupt = home.profile_db("corrupt");
    fs::create_dir_all(corrupt.parent().unwrap()).unwrap();
    fs::write(&corrupt, b"not sqlite").unwrap();
    let out = ls(&home);
    assert!(out.success(), "{}", dump(&out));
    for title in ["main-same", "left-same", "right-same"] {
        assert!(out.stdout.contains(title), "{title}\n{}", dump(&out));
    }
    assert!(out.combined().contains("emptybot"), "{}", dump(&out));
    assert!(out.combined().contains("corrupt"), "{}", dump(&out));
}
