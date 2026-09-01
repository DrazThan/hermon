//! Pure sort/filter core for the fleet view: no rendering, no key handling.
//!
//! The UI (a later ticket) owns a [`ViewState`], mutates it from key events
//! (`toggle_sort`, `set_filter`, …) and calls [`apply`] each tick to turn the
//! roster into the row order it paints. Everything here is deterministic and
//! side-effect free so the whole surface is unit-testable.

use std::cmp::Ordering;
use std::collections::HashSet;

use crate::roster::RosterRow;
use crate::source::Liveness;

/// A sortable roster column. `InOut` orders by total token traffic
/// (`in_tok + out_tok`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SortKey {
    Model,
    Tool,
    InOut,
    Cost,
    Elapsed,
}

impl SortKey {
    /// The five keys in the order the palette's `[1]`-`[5]` chips pick them.
    pub const ALL: [SortKey; 5] = [
        SortKey::Model,
        SortKey::Tool,
        SortKey::InOut,
        SortKey::Cost,
        SortKey::Elapsed,
    ];

    /// The chip label the sort/filter palette and header show.
    pub fn label(self) -> &'static str {
        match self {
            SortKey::Model => "model",
            SortKey::Tool => "tool",
            SortKey::InOut => "in/out",
            SortKey::Cost => "cost",
            SortKey::Elapsed => "elapsed",
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum SortDir {
    #[default]
    Asc,
    Desc,
}

impl SortDir {
    fn flipped(self) -> Self {
        match self {
            SortDir::Asc => SortDir::Desc,
            SortDir::Desc => SortDir::Asc,
        }
    }

    /// The arrow the header and palette chips show next to the active key.
    pub fn arrow(self) -> &'static str {
        match self {
            SortDir::Asc => "\u{2191}",
            SortDir::Desc => "\u{2193}",
        }
    }
}

/// Everything the view remembers between ticks: active sort, attention-first
/// flag, the parsed filter, and the pinned-session set (carried for the pin
/// ticket; [`apply`] does not consume it yet).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ViewState {
    /// `None` keeps the roster's own order (newest activity first).
    pub sort_key: Option<SortKey>,
    pub sort_dir: SortDir,
    /// Group ⏸/⚠ rows first, then live, then done; the sort key still
    /// orders rows within each group.
    pub attention_first: bool,
    pub filter: Filter,
    /// Session ids ([`RosterRow::id`]) the user has pinned.
    pub pinned: HashSet<String>,
}

impl ViewState {
    /// Activate `key`, or flip the direction if `key` is already active.
    /// A newly activated key always starts ascending.
    pub fn toggle_sort(&mut self, key: SortKey) {
        if self.sort_key == Some(key) {
            self.sort_dir = self.sort_dir.flipped();
        } else {
            self.sort_key = Some(key);
            self.sort_dir = SortDir::Asc;
        }
    }

    /// Parse `input` and install it as the active filter. On error the
    /// previous filter is kept and the message is returned for the UI to
    /// show next to the input line.
    pub fn set_filter(&mut self, input: &str) -> Result<(), String> {
        self.filter = Filter::parse(input)?;
        Ok(())
    }

    /// Drops the active sort and filter (the palette's `[c]`). Leaves
    /// `attention_first` and `pinned` alone — those are set outside the
    /// palette and `[c]` only clears what the palette itself controls.
    pub fn clear(&mut self) {
        self.sort_key = None;
        self.sort_dir = SortDir::default();
        self.filter = Filter::default();
    }

    pub fn pin(&mut self, id: &str) {
        self.pinned.insert(id.to_string());
    }

    pub fn unpin(&mut self, id: &str) {
        self.pinned.remove(id);
    }

    pub fn is_pinned(&self, id: &str) -> bool {
        self.pinned.contains(id)
    }
}

// ------------------------------------------------------------------ filter

/// String-valued filter keys and the [`RosterRow`] field each reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum StrField {
    /// `model` → [`RosterRow::model`]
    Model,
    /// `tool` → [`RosterRow::last_tool`]
    Tool,
    /// `key` → [`RosterRow::key`] (the `C:0f865f` label)
    Key,
    /// `title` → [`RosterRow::title`]
    Title,
}

impl StrField {
    fn value(self, r: &RosterRow) -> &str {
        match self {
            StrField::Model => &r.model,
            StrField::Tool => &r.last_tool,
            StrField::Key => &r.key,
            StrField::Title => &r.title,
        }
    }
}

/// Numeric filter keys and the [`RosterRow`] field each reads.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum NumField {
    /// `cost` → [`RosterRow::cost`], dollars
    Cost,
    /// `elapsed` → [`RosterRow::elapsed`], seconds; a row with no start
    /// time has no elapsed and never matches an `elapsed` term
    Elapsed,
    /// `in` → [`RosterRow::in_tok`]
    In,
    /// `out` → [`RosterRow::out_tok`]
    Out,
}

impl NumField {
    fn value(self, r: &RosterRow) -> Option<f64> {
        match self {
            NumField::Cost => r.cost,
            NumField::Elapsed => r.elapsed,
            NumField::In => Some(r.in_tok as f64),
            NumField::Out => Some(r.out_tok as f64),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Op {
    Eq,
    Gt,
    Lt,
}

/// One parsed `key<op>value` term.
#[derive(Debug, Clone, PartialEq)]
enum Term {
    /// `field=pattern`, case-insensitive, `*` matches any run of characters.
    Glob {
        field: StrField,
        pattern: String,
    },
    Cmp {
        field: NumField,
        op: Op,
        value: f64,
    },
}

impl Term {
    fn matches(&self, r: &RosterRow) -> bool {
        match self {
            Term::Glob { field, pattern } => glob_match(pattern, field.value(r)),
            Term::Cmp { field, op, value } => field.value(r).is_some_and(|v| match op {
                Op::Eq => v == *value,
                Op::Gt => v > *value,
                Op::Lt => v < *value,
            }),
        }
    }
}

/// A parsed filter: whitespace-separated terms, all of which must match
/// (AND). The empty filter matches every row.
///
/// Mini-language, one term per word:
/// - `key=value` on string keys (`model`, `tool`, `key`, `title`) — whole-
///   value glob match, case-insensitive, `*` is the only wildcard:
///   `model=claude*`, `key=C:*`.
/// - `key>n` / `key<n` / `key=n` on numeric keys (`cost`, `in`, `out`, and
///   `elapsed` with optional `s`/`m`/`h` suffix): `cost>1.00`,
///   `elapsed>10m`, `in<50000`.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filter {
    terms: Vec<Term>,
    /// The original words, one per term, in the same order — kept so the
    /// header and palette can show the filter back as chips without
    /// re-serializing a [`Term`].
    raw: Vec<String>,
}

impl Filter {
    /// Parse the mini-language. The first malformed term aborts the parse
    /// with a message naming the term and what was wrong — never a panic.
    pub fn parse(input: &str) -> Result<Filter, String> {
        let words: Vec<&str> = input.split_whitespace().collect();
        let terms = words
            .iter()
            .map(|w| parse_term(w))
            .collect::<Result<Vec<_>, _>>()?;
        let raw = words.into_iter().map(str::to_string).collect();
        Ok(Filter { terms, raw })
    }

    pub fn is_empty(&self) -> bool {
        self.terms.is_empty()
    }

    /// One chip's text per term, in filter order — what the header and the
    /// palette's filter row display.
    pub fn chips(&self) -> &[String] {
        &self.raw
    }

    /// The terms rejoined the way they'd be retyped, for prefilling the
    /// palette's input when it reopens over an already-active filter.
    pub fn as_input(&self) -> String {
        self.raw.join(" ")
    }

    fn matches(&self, r: &RosterRow) -> bool {
        self.terms.iter().all(|t| t.matches(r))
    }
}

fn parse_term(word: &str) -> Result<Term, String> {
    let err = |msg: String| format!("bad filter term \"{word}\": {msg}");

    let Some(at) = word.find(['=', '>', '<']) else {
        return Err(err("expected key=value, key>n or key<n".to_string()));
    };
    let (key, rest) = word.split_at(at);
    let (op, value) = (&rest[..1], &rest[1..]);
    if key.is_empty() {
        return Err(err("missing key".to_string()));
    }

    let str_field = match key {
        "model" => Some(StrField::Model),
        "tool" => Some(StrField::Tool),
        "key" => Some(StrField::Key),
        "title" => Some(StrField::Title),
        _ => None,
    };
    let num_field = match key {
        "cost" => Some(NumField::Cost),
        "elapsed" => Some(NumField::Elapsed),
        "in" => Some(NumField::In),
        "out" => Some(NumField::Out),
        _ => None,
    };

    match (str_field, num_field, op) {
        (Some(field), _, "=") => Ok(Term::Glob {
            field,
            pattern: value.to_string(),
        }),
        (Some(_), _, _) => Err(err(format!(
            "\"{op}\" needs a numeric key (cost, elapsed, in, out)"
        ))),
        (_, Some(field), _) => {
            let parsed = if field == NumField::Elapsed {
                parse_duration(value)
            } else {
                value.parse::<f64>().ok()
            };
            let value = parsed.ok_or_else(|| err(format!("\"{value}\" is not a number")))?;
            let op = match op {
                "=" => Op::Eq,
                ">" => Op::Gt,
                _ => Op::Lt,
            };
            Ok(Term::Cmp { field, op, value })
        }
        _ => Err(err(format!(
            "unknown key \"{key}\" (model, tool, key, title, cost, elapsed, in, out)"
        ))),
    }
}

/// `10s` / `5m` / `2h` to seconds; a bare number is already seconds.
fn parse_duration(s: &str) -> Option<f64> {
    let (num, mult) = if let Some(n) = s.strip_suffix('s') {
        (n, 1.0)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60.0)
    } else if let Some(n) = s.strip_suffix('h') {
        (n, 3600.0)
    } else {
        (s, 1.0)
    };
    num.parse::<f64>().ok().map(|v| v * mult)
}

/// Case-insensitive whole-string match where `*` matches any run of
/// characters (including none). Classic two-pointer matcher with
/// backtracking to the last `*`.
fn glob_match(pattern: &str, text: &str) -> bool {
    let p: Vec<char> = pattern.to_lowercase().chars().collect();
    let t: Vec<char> = text.to_lowercase().chars().collect();
    let (mut pi, mut ti) = (0, 0);
    let mut star: Option<(usize, usize)> = None;
    while ti < t.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some((pi, ti));
            pi += 1;
        } else if pi < p.len() && p[pi] == t[ti] {
            pi += 1;
            ti += 1;
        } else if let Some((sp, st)) = star {
            pi = sp + 1;
            ti = st + 1;
            star = Some((sp, st + 1));
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|&c| c == '*')
}

// ------------------------------------------------------------------- apply

/// [`apply`]'s result: the visible rows plus the counts the filter line
/// shows as-you-type ("2 of 10").
#[derive(Debug)]
pub struct ViewOutput<'a> {
    pub rows: Vec<&'a RosterRow>,
    /// Rows that passed the filter (`rows.len()`, kept explicit for the UI).
    pub matched: usize,
    /// Rows before filtering.
    pub total: usize,
}

/// ⏸/⚠ first, then live, then done.
fn attn_rank(state: Liveness) -> u8 {
    match state {
        Liveness::Attention(_) => 0,
        Liveness::Live => 1,
        Liveness::Done => 2,
    }
}

/// Pinned rows first, so they land on grid page 1 and the top of the list
/// regardless of sort or attention grouping.
fn pin_rank(state: &ViewState, id: &str) -> u8 {
    u8::from(!state.is_pinned(id))
}

fn key_cmp(a: &RosterRow, b: &RosterRow, key: SortKey) -> Ordering {
    match key {
        SortKey::Model => a.model.cmp(&b.model),
        SortKey::Tool => a.last_tool.cmp(&b.last_tool),
        // Saturating: both counts come off a remote's wire, where nothing
        // stops an agent reporting u64::MAX and panicking a debug build's
        // sort out from under the roster.
        SortKey::InOut => a
            .in_tok
            .saturating_add(a.out_tok)
            .cmp(&b.in_tok.saturating_add(b.out_tok)),
        // A missing cost sorts before any known cost.
        SortKey::Cost => match (a.cost, b.cost) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(x), Some(y)) => x.total_cmp(&y),
        },
        // A missing elapsed sorts before any measured one.
        SortKey::Elapsed => match (a.elapsed, b.elapsed) {
            (None, None) => Ordering::Equal,
            (None, Some(_)) => Ordering::Less,
            (Some(_), None) => Ordering::Greater,
            (Some(x), Some(y)) => x.total_cmp(&y),
        },
    }
}

/// Filter then sort the roster. The sort is stable, so rows that compare
/// equal keep the roster's order (newest first) and don't jitter between
/// ticks; with `sort_key: None` the roster order is kept outright. The
/// attention grouping ignores the sort direction — attention rows stay on
/// top either way.
pub fn apply<'a>(rows: &'a [RosterRow], state: &ViewState) -> ViewOutput<'a> {
    let mut out: Vec<&RosterRow> = rows.iter().filter(|r| state.filter.matches(r)).collect();
    out.sort_by(|a, b| {
        let pin_group = pin_rank(state, &a.id).cmp(&pin_rank(state, &b.id));
        let attn_group = if state.attention_first {
            attn_rank(a.state).cmp(&attn_rank(b.state))
        } else {
            Ordering::Equal
        };
        pin_group
            .then(attn_group)
            .then_with(|| match state.sort_key {
                None => Ordering::Equal,
                Some(key) => {
                    let ord = key_cmp(a, b, key);
                    match state.sort_dir {
                        SortDir::Asc => ord,
                        SortDir::Desc => ord.reverse(),
                    }
                }
            })
    });
    ViewOutput {
        matched: out.len(),
        total: rows.len(),
        rows: out,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::source::Attn;

    /// A row whose sortable fields are all derived from one small integer,
    /// so tests can spell an expected order as a key list.
    fn row(key: &str, n: u64) -> RosterRow {
        RosterRow {
            id: format!("id-{key}"),
            key: key.to_string(),
            state: Liveness::Live,
            model: format!("model-{n}"),
            last_tool: format!("tool-{n}"),
            last_line: String::new(),
            in_tok: n * 100,
            out_tok: n,
            cost: Some(n as f64 * 0.5),
            elapsed: Some(n as f64 * 60.0),
            last_ts: 0.0,
            title: format!("title {n}"),
            attn_elapsed: None,
        }
    }

    fn keys(out: &ViewOutput) -> Vec<String> {
        out.rows.iter().map(|r| r.key.clone()).collect()
    }

    fn sorted(rows: &[RosterRow], key: SortKey, dir: SortDir) -> Vec<String> {
        let state = ViewState {
            sort_key: Some(key),
            sort_dir: dir,
            ..ViewState::default()
        };
        keys(&apply(rows, &state))
    }

    // ------------------------------------------------------------- sorting

    #[test]
    fn every_sort_key_orders_both_directions() {
        let rows = [row("b", 2), row("c", 3), row("a", 1)];
        for key in [
            SortKey::Model,
            SortKey::Tool,
            SortKey::InOut,
            SortKey::Cost,
            SortKey::Elapsed,
        ] {
            assert_eq!(sorted(&rows, key, SortDir::Asc), ["a", "b", "c"], "{key:?}");
            assert_eq!(
                sorted(&rows, key, SortDir::Desc),
                ["c", "b", "a"],
                "{key:?}"
            );
        }
    }

    /// The token counts are a remote agent's to choose, so their sum is not
    /// an arithmetic the sort may overflow on.
    #[test]
    fn in_out_sort_survives_wire_controlled_token_counts() {
        let mut rows = [row("a", 1), row("b", 2), row("c", 3)];
        rows[1].in_tok = u64::MAX;
        rows[1].out_tok = u64::MAX;
        rows[2].in_tok = u64::MAX;
        rows[2].out_tok = 1;
        assert_eq!(sorted(&rows, SortKey::InOut, SortDir::Asc)[0], "a");
        assert_eq!(sorted(&rows, SortKey::InOut, SortDir::Desc)[2], "a");
    }

    #[test]
    fn no_sort_key_keeps_roster_order() {
        let rows = [row("b", 2), row("c", 3), row("a", 1)];
        assert_eq!(keys(&apply(&rows, &ViewState::default())), ["b", "c", "a"]);
    }

    #[test]
    fn missing_elapsed_sorts_before_any_measured_elapsed() {
        let mut rows = [row("a", 3), row("b", 1), row("c", 2)];
        rows[2].elapsed = None;
        assert_eq!(
            sorted(&rows, SortKey::Elapsed, SortDir::Asc),
            ["c", "b", "a"]
        );
        assert_eq!(
            sorted(&rows, SortKey::Elapsed, SortDir::Desc),
            ["a", "b", "c"]
        );
    }

    #[test]
    fn equal_keys_keep_input_order_in_both_directions() {
        // Same cost everywhere: the sort must not move anything.
        let rows = [row("x", 5), row("y", 5), row("z", 5)];
        assert_eq!(sorted(&rows, SortKey::Cost, SortDir::Asc), ["x", "y", "z"]);
        assert_eq!(sorted(&rows, SortKey::Cost, SortDir::Desc), ["x", "y", "z"]);
    }

    #[test]
    fn ties_within_a_sorted_run_keep_input_order() {
        let mut rows = [row("x", 2), row("y", 1), row("z", 2)];
        rows[0].cost = Some(2.0);
        rows[1].cost = Some(1.0);
        rows[2].cost = Some(2.0);
        assert_eq!(sorted(&rows, SortKey::Cost, SortDir::Asc), ["y", "x", "z"]);
        assert_eq!(sorted(&rows, SortKey::Cost, SortDir::Desc), ["x", "z", "y"]);
    }

    #[test]
    fn toggle_sort_activates_then_flips_then_switches() {
        let mut state = ViewState::default();
        state.toggle_sort(SortKey::Cost);
        assert_eq!(state.sort_key, Some(SortKey::Cost));
        assert_eq!(state.sort_dir, SortDir::Asc);

        state.toggle_sort(SortKey::Cost);
        assert_eq!(state.sort_dir, SortDir::Desc);
        state.toggle_sort(SortKey::Cost);
        assert_eq!(state.sort_dir, SortDir::Asc);

        // A different key resets to ascending.
        state.toggle_sort(SortKey::Cost);
        state.toggle_sort(SortKey::Model);
        assert_eq!(state.sort_key, Some(SortKey::Model));
        assert_eq!(state.sort_dir, SortDir::Asc);
    }

    // ----------------------------------------------------- attention mode

    #[test]
    fn attention_groups_come_first_then_live_then_done() {
        let mut rows = [
            row("done", 1),
            row("live", 2),
            row("perm", 3),
            row("stuck", 4),
        ];
        rows[0].state = Liveness::Done;
        rows[2].state = Liveness::Attention(Attn::PermWait);
        rows[3].state = Liveness::Attention(Attn::Stuck);
        let state = ViewState {
            attention_first: true,
            ..ViewState::default()
        };
        assert_eq!(
            keys(&apply(&rows, &state)),
            ["perm", "stuck", "live", "done"]
        );
    }

    #[test]
    fn sort_key_orders_within_each_attention_group() {
        let mut rows = [row("d2", 4), row("a2", 2), row("d1", 3), row("a1", 1)];
        rows[0].state = Liveness::Done;
        rows[1].state = Liveness::Attention(Attn::Stuck);
        rows[2].state = Liveness::Done;
        rows[3].state = Liveness::Attention(Attn::PermWait);
        let state = ViewState {
            sort_key: Some(SortKey::Cost),
            attention_first: true,
            ..ViewState::default()
        };
        assert_eq!(keys(&apply(&rows, &state)), ["a1", "a2", "d1", "d2"]);
    }

    #[test]
    fn descending_sort_reverses_within_groups_not_the_groups() {
        let mut rows = [row("d1", 1), row("d2", 2), row("a1", 3), row("a2", 4)];
        rows[0].state = Liveness::Done;
        rows[1].state = Liveness::Done;
        rows[2].state = Liveness::Attention(Attn::Stuck);
        rows[3].state = Liveness::Attention(Attn::Stuck);
        let state = ViewState {
            sort_key: Some(SortKey::Cost),
            sort_dir: SortDir::Desc,
            attention_first: true,
            ..ViewState::default()
        };
        assert_eq!(keys(&apply(&rows, &state)), ["a2", "a1", "d2", "d1"]);
    }

    // ---------------------------------------------------------------- glob

    #[test]
    fn glob_star_alone_matches_everything() {
        assert!(glob_match("*", ""));
        assert!(glob_match("*", "anything at all"));
    }

    #[test]
    fn glob_mid_string_star_backtracks() {
        assert!(glob_match("cla*de", "claude"));
        assert!(glob_match("cla*de", "clade"));
        assert!(glob_match("c*d*e", "claude"));
        assert!(!glob_match("cla*de", "claudes"), "must match whole string");
        assert!(!glob_match("cla*de", "xclaude"));
    }

    #[test]
    fn glob_without_star_is_exact_and_case_insensitive() {
        assert!(glob_match("Claude", "claude"));
        assert!(!glob_match("claude", "claud"));
        assert!(!glob_match("", "x"));
        assert!(glob_match("", ""));
    }

    #[test]
    fn glob_trailing_stars_and_prefix_suffix() {
        assert!(glob_match("claude*", "claude-sonnet-5"));
        assert!(glob_match("*sonnet*", "claude-sonnet-5"));
        assert!(glob_match("claude**", "claude"));
        assert!(!glob_match("*sonnet", "claude-sonnet-5"));
    }

    // -------------------------------------------------------------- filter

    fn fleet() -> Vec<RosterRow> {
        let mut rows: Vec<RosterRow> = (1..=10).map(|n| row(&format!("r{n}"), n)).collect();
        rows[0].model = "claude-sonnet-5".to_string();
        rows[1].model = "claude-opus-5".to_string();
        rows[2].model = "gpt-6".to_string();
        rows
    }

    fn filtered(rows: &[RosterRow], input: &str) -> Vec<String> {
        let state = ViewState {
            filter: Filter::parse(input).expect("filter parses"),
            ..ViewState::default()
        };
        keys(&apply(rows, &state))
    }

    #[test]
    fn string_terms_glob_their_documented_fields() {
        let rows = fleet();
        assert_eq!(filtered(&rows, "model=claude*"), ["r1", "r2"]);
        assert_eq!(filtered(&rows, "tool=tool-4"), ["r4"]);
        assert_eq!(filtered(&rows, "key=r1*"), ["r1", "r10"]);
        assert_eq!(filtered(&rows, "title=*5"), ["r5"]);
        assert!(filtered(&rows, "model=nomatch*").is_empty());
    }

    #[test]
    fn numeric_comparisons_on_cost_in_out() {
        let rows = fleet();
        // cost is n * 0.5, in is n * 100, out is n.
        assert_eq!(filtered(&rows, "cost>4.0"), ["r9", "r10"]);
        assert_eq!(filtered(&rows, "cost<1.00"), ["r1"]);
        assert_eq!(filtered(&rows, "cost=1.5"), ["r3"]);
        assert_eq!(filtered(&rows, "in>850"), ["r9", "r10"]);
        assert_eq!(filtered(&rows, "out<2"), ["r1"]);
    }

    #[test]
    fn elapsed_comparisons_parse_duration_suffixes() {
        let rows = fleet(); // elapsed is n minutes
        assert_eq!(filtered(&rows, "elapsed>480s"), ["r9", "r10"]);
        assert_eq!(filtered(&rows, "elapsed>8m"), ["r9", "r10"]);
        assert_eq!(filtered(&rows, "elapsed<0.05h"), ["r1", "r2"]);
        assert_eq!(
            filtered(&rows, "elapsed>480"),
            ["r9", "r10"],
            "bare seconds"
        );
    }

    #[test]
    fn rows_without_elapsed_never_match_elapsed_terms() {
        let mut rows = fleet();
        rows[9].elapsed = None;
        assert_eq!(filtered(&rows, "elapsed>8m"), ["r9"]);
        assert_eq!(filtered(&rows, "elapsed<1h"), keys_of(&rows[..9]));
    }

    fn keys_of(rows: &[RosterRow]) -> Vec<String> {
        rows.iter().map(|r| r.key.clone()).collect()
    }

    #[test]
    fn terms_are_anded() {
        let rows = fleet();
        assert_eq!(filtered(&rows, "model=claude* cost>0.6"), ["r2"]);
        assert!(filtered(&rows, "model=claude* cost>99").is_empty());
    }

    #[test]
    fn empty_filter_matches_everything() {
        let rows = fleet();
        assert!(Filter::parse("").expect("empty parses").is_empty());
        assert_eq!(filtered(&rows, "  ").len(), 10);
    }

    #[test]
    fn match_count_reports_matched_of_total() {
        let rows = fleet();
        let state = ViewState {
            filter: Filter::parse("model=claude*").expect("filter parses"),
            ..ViewState::default()
        };
        let out = apply(&rows, &state);
        assert_eq!((out.matched, out.total), (2, 10), "\"2 of 10\"");
        assert_eq!(out.rows.len(), out.matched);
    }

    #[test]
    fn chips_and_as_input_round_trip_the_typed_words() {
        let filter = Filter::parse("model=claude* cost>1.5").expect("parses");
        assert_eq!(filter.chips(), ["model=claude*", "cost>1.5"]);
        assert_eq!(filter.as_input(), "model=claude* cost>1.5");
        assert!(Filter::default().chips().is_empty());
    }

    // ------------------------------------------------------- parse errors

    #[test]
    fn malformed_terms_report_errors_not_panics() {
        for (input, needle) in [
            ("cost>abc", "\"abc\" is not a number"),
            ("elapsed>10x", "\"10x\" is not a number"),
            ("cost>", "\"\" is not a number"),
            ("unknownkey=x", "unknown key \"unknownkey\""),
            ("=", "missing key"),
            ("=value", "missing key"),
            ("bareword", "expected key=value"),
            ("model>x", "needs a numeric key"),
        ] {
            let err = Filter::parse(input).expect_err(input);
            assert!(err.contains(input), "{err:?} names the term");
            assert!(err.contains(needle), "{err:?} explains {input:?}");
        }
    }

    #[test]
    fn one_bad_term_fails_the_whole_parse() {
        assert!(Filter::parse("model=claude* cost>abc").is_err());
    }

    #[test]
    fn set_filter_keeps_the_old_filter_on_error() {
        let mut state = ViewState::default();
        state.set_filter("cost>1").expect("valid filter");
        let before = state.filter.clone();
        let err = state.set_filter("cost>abc").expect_err("invalid filter");
        assert!(err.contains("not a number"));
        assert_eq!(state.filter, before);
    }

    #[test]
    fn clear_resets_sort_and_filter_but_not_attention_or_pins() {
        let mut state = ViewState {
            sort_key: Some(SortKey::Cost),
            sort_dir: SortDir::Desc,
            attention_first: true,
            filter: Filter::parse("cost>1").expect("parses"),
            ..ViewState::default()
        };
        state.pin("id-a");
        state.clear();
        assert_eq!(state.sort_key, None);
        assert_eq!(state.sort_dir, SortDir::Asc);
        assert!(state.filter.is_empty());
        assert!(
            state.attention_first,
            "attention toggle is outside the palette"
        );
        assert!(state.is_pinned("id-a"));
    }

    #[test]
    fn sort_key_label_and_dir_arrow_cover_every_variant() {
        for key in SortKey::ALL {
            assert!(!key.label().is_empty());
        }
        assert_eq!(SortDir::Asc.arrow(), "\u{2191}");
        assert_eq!(SortDir::Desc.arrow(), "\u{2193}");
    }

    // ---------------------------------------------------------------- pins

    /// A pinned row sorts to the front regardless of sort key or direction —
    /// what puts it on grid page 1.
    #[test]
    fn pinned_rows_sort_first_regardless_of_sort_key() {
        let rows = [row("a", 1), row("b", 2), row("c", 3)];
        let mut state = ViewState {
            sort_key: Some(SortKey::Cost),
            sort_dir: SortDir::Desc,
            ..ViewState::default()
        };
        state.pin("id-a");
        assert_eq!(keys(&apply(&rows, &state)), ["a", "c", "b"]);
    }

    /// Pinning multiple rows keeps them all up front, ordered by the active
    /// sort among themselves; attention grouping is still an inner group.
    #[test]
    fn pin_group_is_outermost_ahead_of_attention_grouping() {
        let mut rows = [row("stuck", 1), row("pinned-done", 2), row("live", 3)];
        rows[0].state = Liveness::Attention(Attn::Stuck);
        rows[1].state = Liveness::Done;
        let mut state = ViewState {
            attention_first: true,
            ..ViewState::default()
        };
        state.pin("id-pinned-done");
        assert_eq!(
            keys(&apply(&rows, &state)),
            ["pinned-done", "stuck", "live"]
        );
    }

    /// A pinned session hidden by the active filter still doesn't show up in
    /// `apply`'s rows — the filter's job is untouched by pinning. (The pane
    /// grid keeping its slot anyway is the UI's concern, not this pure core.)
    #[test]
    fn a_pinned_row_hidden_by_the_filter_is_still_filtered_out() {
        let rows = fleet();
        let mut state = ViewState {
            filter: Filter::parse("model=claude*").expect("parses"),
            ..ViewState::default()
        };
        state.pin("id-r3"); // r3 is gpt-6, filtered out by model=claude*
        assert_eq!(keys(&apply(&rows, &state)), ["r1", "r2"]);
    }

    #[test]
    fn pin_unpin_is_pinned_round_trip() {
        let mut state = ViewState::default();
        assert!(!state.is_pinned("id-a"));
        state.pin("id-a");
        state.pin("id-b");
        assert!(state.is_pinned("id-a") && state.is_pinned("id-b"));
        state.unpin("id-a");
        assert!(!state.is_pinned("id-a"));
        assert!(state.is_pinned("id-b"));
        state.unpin("id-a"); // unpinning twice is a no-op
        assert_eq!(state.pinned.len(), 1);
    }
}
