//! Renders normalized Gemini CLI messages to displayable lines.
//!
//! v1 covers the observed text roles (`user`, `model`/`assistant`, `info`).
//! Tool-call and tool-result shapes are not mapped: no verified fixture
//! exists yet, and prose is not treated as a call.

use super::{Seg, Sem, StyledLine, clip};

/// Tool/user text clip, matching the other renderers.
pub(crate) const TEXT_CLIP: usize = 120;

/// Semantic role of one normalized Gemini message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GeminiRole {
    User,
    Assistant,
    Info,
    System,
    Other,
}

/// Render one normalized message. Empty text and `<session_context>`
/// scaffolding emit nothing — they are setup, not chat.
pub fn render_gemini_message(role: GeminiRole, text: &str, scaffold: bool) -> Vec<StyledLine> {
    if scaffold || text.trim().is_empty() {
        return Vec::new();
    }
    let body = clip(text, TEXT_CLIP);
    if body.is_empty() {
        return Vec::new();
    }
    match role {
        GeminiRole::User => vec![StyledLine(vec![Seg::new(Sem::User, format!("» {body}"))])],
        GeminiRole::Assistant => vec![StyledLine(vec![Seg::new(Sem::Plain, body)])],
        GeminiRole::Info => vec![StyledLine(vec![Seg::new(Sem::Dim, format!("· {body}"))])],
        GeminiRole::System => vec![StyledLine(vec![Seg::new(Sem::Dim, "· system")])],
        GeminiRole::Other => vec![StyledLine(vec![Seg::new(Sem::Dim, format!("· {body}"))])],
    }
}

/// Pane-visible notice when reconstructed state edited or removed messages
/// the append-only [`StyledLine`] interface cannot erase.
pub fn revision_notice() -> StyledLine {
    StyledLine(vec![Seg::new(
        Sem::Dim,
        "· session revised — showing current context",
    )])
}

/// One-shot notice when a parse/state cap stopped a complete reconstruct.
pub fn limit_notice() -> StyledLine {
    StyledLine(vec![Seg::new(
        Sem::Dim,
        "· gemini history truncated — reconstruction hit a parse/state limit",
    )])
}

pub fn dim_status(text: &str) -> StyledLine {
    StyledLine(vec![Seg::new(Sem::Dim, text)])
}
