//! Typed, versioned wire messages for the host/agent protocol (#88).
//!
//! Transport is line-delimited JSON over stdio (one frame per line), wired
//! up by later tickets (#89 agent, #90 host). This module only owns the
//! message shapes and pure encode/decode helpers so both sides — and their
//! tests — call the same functions.
//!
//! Decoding is panic-free and defensive, same philosophy as the renderers
//! (`crate::render::claude::render_claude_line`): a line that isn't valid
//! JSON, is truncated, or names a message type we don't recognise decodes
//! to [`Decoded::ParseSkip`] instead of an `Err` or a panic. Unknown *fields*
//! on an otherwise-valid message are silently ignored (serde's default
//! behavior — pinned by a test in this module so `deny_unknown_fields` never
//! creeps in and breaks forward compatibility).

use serde::{Deserialize, Serialize};

use crate::render::StyledLine;
use crate::source::{Replay, SessionMeta};

/// Wire protocol version this build speaks. Version *policy* (what to do on
/// a mismatch) is the host's job in later tickets — this ticket only pins
/// the constant and the fact that decoding never depends on its value.
pub const PROTO_VERSION: u32 = 1;

/// Messages sent from the remote agent to the host.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum AgentMsg {
    /// Always the first line an agent sends.
    Hello {
        proto_version: u32,
        hostname: String,
        sources: Vec<String>,
    },
    /// A full snapshot of the agent's current sessions.
    Snap { sessions: Vec<SessionMeta> },
    /// New lines for a tail the host previously opened with `OpenTail`.
    Tail { key: String, lines: Vec<StyledLine> },
    /// The agent is closing the connection.
    Bye { reason: String },
}

/// Messages sent from the host to a remote agent.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum HostCmd {
    /// Start tailing one session, replaying `replay` worth of history first.
    OpenTail { key: String, replay: Replay },
    /// Stop tailing a session opened with `OpenTail`.
    CloseTail { key: String },
    /// Terminate the agent process.
    Shutdown,
}

/// Result of decoding one line of the wire protocol: either a well-formed
/// message, or a silent skip for anything hostile (garbage bytes, truncated
/// JSON, an unrecognised message type). Never an `Err` — there is nothing
/// for a caller to do with a decode failure except skip the line, so the
/// type says that directly instead of via a `Result` a caller could
/// `.unwrap()`.
#[derive(Debug, Clone, PartialEq)]
pub enum Decoded<T> {
    Msg(T),
    ParseSkip,
}

/// Encode one [`AgentMsg`] as a single JSON line (no trailing newline).
/// Serialization of these owned, plain-data structs cannot fail in
/// practice, but the helper still never panics: it falls back to an empty
/// string rather than unwrapping.
pub fn encode_agent_msg(msg: &AgentMsg) -> String {
    serde_json::to_string(msg).unwrap_or_default()
}

/// Decode one line of the wire protocol as an [`AgentMsg`]. Never panics:
/// anything that isn't a valid, recognised `AgentMsg` decodes to
/// [`Decoded::ParseSkip`].
pub fn decode_agent_msg(line: &str) -> Decoded<AgentMsg> {
    match serde_json::from_str::<AgentMsg>(line) {
        Ok(msg) => Decoded::Msg(msg),
        Err(_) => Decoded::ParseSkip,
    }
}

/// Encode one [`HostCmd`] as a single JSON line (no trailing newline). See
/// [`encode_agent_msg`] for the no-panic rationale.
pub fn encode_host_cmd(cmd: &HostCmd) -> String {
    serde_json::to_string(cmd).unwrap_or_default()
}

/// Decode one line of the wire protocol as a [`HostCmd`]. See
/// [`decode_agent_msg`] for the no-panic contract.
pub fn decode_host_cmd(line: &str) -> Decoded<HostCmd> {
    match serde_json::from_str::<HostCmd>(line) {
        Ok(cmd) => Decoded::Msg(cmd),
        Err(_) => Decoded::ParseSkip,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::render::{Seg, Sem};
    use crate::source::LastEvent;

    fn sample_meta() -> SessionMeta {
        SessionMeta {
            id: "s1".into(),
            started_at: 1.0,
            ended: false,
            model: "claude-sonnet-5".into(),
            title: "t".into(),
            in_tok: 10,
            out_tok: 20,
            cost: Some(0.5),
            last_ts: 2.0,
            turn_done: true,
            tool_pending: false,
            force_live: false,
            last_tool: "Bash".into(),
            last_line: "hi".into(),
            last_event: Some(LastEvent::ToolUse("Bash".into())),
        }
    }

    fn sample_line() -> StyledLine {
        StyledLine(vec![
            Seg::new(Sem::Bold, "▶ Bash"),
            Seg::new(Sem::Plain, " ok"),
        ])
    }

    // --- round trips: every AgentMsg variant ---

    #[test]
    fn round_trips_hello() {
        let msg = AgentMsg::Hello {
            proto_version: PROTO_VERSION,
            hostname: "box".into(),
            sources: vec!["claude".into(), "hermes".into()],
        };
        let line = encode_agent_msg(&msg);
        assert_eq!(decode_agent_msg(&line), Decoded::Msg(msg));
    }

    #[test]
    fn round_trips_snap() {
        let msg = AgentMsg::Snap {
            sessions: vec![sample_meta()],
        };
        let line = encode_agent_msg(&msg);
        assert_eq!(decode_agent_msg(&line), Decoded::Msg(msg));
    }

    #[test]
    fn round_trips_tail() {
        let msg = AgentMsg::Tail {
            key: "C:abcdef".into(),
            lines: vec![sample_line()],
        };
        let line = encode_agent_msg(&msg);
        assert_eq!(decode_agent_msg(&line), Decoded::Msg(msg));
    }

    #[test]
    fn round_trips_bye() {
        let msg = AgentMsg::Bye {
            reason: "shutdown".into(),
        };
        let line = encode_agent_msg(&msg);
        assert_eq!(decode_agent_msg(&line), Decoded::Msg(msg));
    }

    // --- round trips: every HostCmd variant ---

    #[test]
    fn round_trips_open_tail() {
        let msg = HostCmd::OpenTail {
            key: "C:abcdef".into(),
            replay: Replay::DEFAULT,
        };
        let line = encode_host_cmd(&msg);
        assert_eq!(decode_host_cmd(&line), Decoded::Msg(msg));
    }

    #[test]
    fn round_trips_close_tail() {
        let msg = HostCmd::CloseTail {
            key: "C:abcdef".into(),
        };
        let line = encode_host_cmd(&msg);
        assert_eq!(decode_host_cmd(&line), Decoded::Msg(msg));
    }

    #[test]
    fn round_trips_shutdown() {
        let msg = HostCmd::Shutdown;
        let line = encode_host_cmd(&msg);
        assert_eq!(decode_host_cmd(&line), Decoded::Msg(msg));
    }

    // --- hostile input: never a panic, always ParseSkip ---

    #[test]
    fn garbage_line_is_parse_skip() {
        assert_eq!(decode_agent_msg("not json at all"), Decoded::ParseSkip);
        assert_eq!(decode_host_cmd("not json at all"), Decoded::ParseSkip);
    }

    #[test]
    fn empty_line_is_parse_skip() {
        assert_eq!(decode_agent_msg(""), Decoded::ParseSkip);
        assert_eq!(decode_host_cmd(""), Decoded::ParseSkip);
    }

    #[test]
    fn truncated_json_is_parse_skip() {
        let full = encode_agent_msg(&AgentMsg::Bye {
            reason: "bye".into(),
        });
        let truncated = &full[..full.len() / 2];
        assert_eq!(decode_agent_msg(truncated), Decoded::ParseSkip);
    }

    #[test]
    fn unknown_message_type_is_parse_skip() {
        assert_eq!(
            decode_agent_msg(r#"{"type":"NotARealVariant"}"#),
            Decoded::ParseSkip
        );
        assert_eq!(
            decode_host_cmd(r#"{"type":"NotARealVariant"}"#),
            Decoded::ParseSkip
        );
    }

    #[test]
    fn valid_json_wrong_shape_is_parse_skip() {
        // Well-formed JSON, but neither an object with a "type" tag nor
        // anything else AgentMsg/HostCmd can decode.
        assert_eq!(decode_agent_msg("42"), Decoded::ParseSkip);
        assert_eq!(decode_agent_msg("[1,2,3]"), Decoded::ParseSkip);
        assert_eq!(decode_agent_msg("null"), Decoded::ParseSkip);
    }

    // --- forward compatibility ---

    #[test]
    fn unknown_extra_fields_are_ignored_not_rejected() {
        // Pins serde's default "ignore unknown fields" behavior so nobody
        // adds `#[serde(deny_unknown_fields)]` later and breaks forward
        // compatibility with a newer agent/host.
        let line = r#"{"type":"Bye","reason":"done","extra_field_from_the_future":123}"#;
        assert_eq!(
            decode_agent_msg(line),
            Decoded::Msg(AgentMsg::Bye {
                reason: "done".into(),
            })
        );
    }

    #[test]
    fn hello_with_future_proto_version_decodes_fine() {
        // Version *policy* is the host's job in later tickets; decoding
        // itself must not gate on the value.
        let line = r#"{"type":"Hello","proto_version":999,"hostname":"h","sources":[]}"#;
        assert_eq!(
            decode_agent_msg(line),
            Decoded::Msg(AgentMsg::Hello {
                proto_version: 999,
                hostname: "h".into(),
                sources: vec![],
            })
        );
    }

    #[test]
    fn open_tail_replay_round_trips() {
        let msg = HostCmd::OpenTail {
            key: "H:xyz".into(),
            replay: Replay { bytes: 1, rows: 2 },
        };
        let line = encode_host_cmd(&msg);
        assert_eq!(decode_host_cmd(&line), Decoded::Msg(msg));
    }
}
