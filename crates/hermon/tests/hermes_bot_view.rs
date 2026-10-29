//! Shared filter grammar. Known-owner matching is covered by the binary
//! tests; this file only uses the existing `Filter` / `ViewState` / `apply`
//! seams and synthetic rows, which have no owner sidecar.

use hermon::roster::RosterRow;
use hermon::source::Liveness;
use hermon::view::{Filter, ViewState, apply};

fn row(id: &str, key: &str, title: &str, model: &str) -> RosterRow {
    RosterRow {
        id: id.to_string(),
        key: key.to_string(),
        state: Liveness::Done,
        model: model.to_string(),
        last_tool: "-".to_string(),
        last_line: String::new(),
        in_tok: 1,
        out_tok: 1,
        cost: Some(0.0),
        elapsed: Some(1.0),
        last_ts: 1_800_000_000.0,
        title: title.to_string(),
        attn_elapsed: None,
    }
}

#[test]
fn bot_grammar_parses_ands_and_never_matches_unknown_owners() {
    let parsed = Filter::parse("bot=Worker* title=keep").expect("bot= must parse");
    assert_eq!(parsed.chips(), ["bot=Worker*", "title=keep"]);
    assert_eq!(parsed.as_input(), "bot=Worker* title=keep");

    let mut view = ViewState::default();
    view.set_filter("bot=* title=keep")
        .expect("bot=* must parse and AND with title=");
    let rows = vec![
        row("id-keep", "H:aaaaaa", "keep", "fixture-model"),
        row("id-drop", "H:bbbbbb", "drop", "fixture-model"),
    ];
    let matched = apply(&rows, &view);
    assert!(
        matched.rows.is_empty(),
        "unknown ownership must not match bot=*, even when title matches: {:?}",
        matched.rows.iter().map(|r| &r.id).collect::<Vec<_>>()
    );

    view.set_filter("bot=default")
        .expect("bot=default must parse");
    assert!(
        apply(&rows, &view).rows.is_empty(),
        "unknown ownership must not match bot=default"
    );
    view.set_filter("bot=unknown")
        .expect("bot=unknown must parse");
    assert!(
        apply(&rows, &view).rows.is_empty(),
        "the word unknown is not a slug that matches missing ownership"
    );
}

#[test]
fn unrelated_filters_still_match_synthetic_rows() {
    let mut view = ViewState::default();
    view.set_filter("title=keep model=fixture*")
        .expect("existing grammar");
    let rows = vec![
        row("id-keep", "H:aaaaaa", "keep", "fixture-model"),
        row("id-drop", "H:bbbbbb", "drop", "other-model"),
    ];
    let matched = apply(&rows, &view);
    assert_eq!(matched.rows.len(), 1);
    assert_eq!(matched.rows[0].id, "id-keep");

    view.set_filter("key=H:* cost=0").expect("numeric AND");
    assert_eq!(apply(&rows, &view).rows.len(), 2);

    view.clear();
    let err = view
        .set_filter("nope=1")
        .expect_err("unknown key stays an error");
    assert!(err.contains("nope"), "{err}");
    assert!(
        view.filter.is_empty(),
        "a bad term must not install over a cleared filter"
    );
    view.set_filter("title=keep").expect("title filter");
    let err = view.set_filter("nope=2").expect_err("bad term");
    assert!(err.contains("nope"), "{err}");
    assert_eq!(
        view.filter.as_input(),
        "title=keep",
        "a bad term must keep the previous filter"
    );
}
