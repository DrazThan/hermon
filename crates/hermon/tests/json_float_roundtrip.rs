use hermon::remote::proto::{AgentMsg, Decoded, decode_agent_msg, encode_agent_msg};
use hermon::source::{LastEvent, SessionMeta};

const COST: f64 = 0.12345678901234568_f64;

fn meta(id: &str, started_at: f64, last_ts: f64) -> SessionMeta {
    SessionMeta {
        id: id.to_string(),
        started_at,
        ended: false,
        model: "claude-sonnet-5".into(),
        title: "t".into(),
        in_tok: 10,
        out_tok: 20,
        cost: Some(COST),
        last_ts,
        turn_done: true,
        tool_pending: false,
        force_live: false,
        last_tool: "Bash".into(),
        last_line: "hi".into(),
        last_event: Some(LastEvent::ToolUse("Bash".into())),
    }
}

fn cases() -> Vec<SessionMeta> {
    [
        (
            "ci_started_epoch_last",
            1793146428.7989237_f64,
            1789466400.0_f64,
        ),
        (
            "epoch_started_adjacent_last",
            1789466400.0_f64,
            1793146428.7989235_f64,
        ),
        (
            "adjacent_started_ci_last",
            1793146428.7989235_f64,
            1793146428.7989237_f64,
        ),
    ]
    .into_iter()
    .map(|(id, started_at, last_ts)| meta(id, started_at, last_ts))
    .collect()
}

fn assert_meta_float_bits(label: &str, got: &SessionMeta, want: &SessionMeta) {
    assert_eq!(
        got.started_at.to_bits(),
        want.started_at.to_bits(),
        "{label}: started_at bits"
    );
    assert_eq!(
        got.last_ts.to_bits(),
        want.last_ts.to_bits(),
        "{label}: last_ts bits"
    );
    let (Some(got_cost), Some(want_cost)) = (got.cost, want.cost) else {
        panic!(
            "{label}: expected Some(cost), got {:?} want {:?}",
            got.cost, want.cost
        );
    };
    assert_eq!(
        got_cost.to_bits(),
        want_cost.to_bits(),
        "{label}: cost bits"
    );
}

#[test]
fn session_meta_json_roundtrip_preserves_float_bits() {
    for want in cases() {
        let encoded = serde_json::to_string(&want).unwrap();
        let got = serde_json::from_str::<SessionMeta>(&encoded).unwrap();
        assert_meta_float_bits(&want.id, &got, &want);
        assert_eq!(got, want, "{}: metadata", want.id);
    }
}

#[test]
fn remote_snap_json_roundtrip_preserves_float_bits() {
    let msg = AgentMsg::Snap { sessions: cases() };
    let decoded = decode_agent_msg(&encode_agent_msg(&msg));
    let (Decoded::Msg(AgentMsg::Snap { sessions: got }), AgentMsg::Snap { sessions: want }) =
        (&decoded, &msg)
    else {
        panic!("expected Decoded::Msg(AgentMsg::Snap), got {decoded:?}");
    };
    assert_eq!(got.len(), want.len(), "snap session count");
    assert_eq!(
        got.iter().map(|meta| meta.id.as_str()).collect::<Vec<_>>(),
        want.iter().map(|meta| meta.id.as_str()).collect::<Vec<_>>(),
        "snap session order"
    );
    for (got_meta, want_meta) in got.iter().zip(want.iter()) {
        assert_meta_float_bits(&want_meta.id, got_meta, want_meta);
    }
    assert_eq!(decoded, Decoded::Msg(msg), "snap message");
}
