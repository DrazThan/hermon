//! Isolated HOME roots; no vendor CLI, credentials, or user stores.
use serde_json::{Value, json};
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
};
use tempfile::TempDir;

pub const ID: &str = "shared-session-123456";
pub struct Stores {
    pub _dir: TempDir,
    pub roots: [PathBuf; 3],
    pub files: [PathBuf; 3],
}
impl Stores {
    pub fn new() -> Self {
        let dir = TempDir::new().unwrap();
        let roots = ["codex home", "grok home", "gemini home"].map(|n| dir.path().join(n));
        let files = [
            roots[0].join("sessions/2026/09/15/rollout.jsonl"),
            roots[1].join(format!("sessions/project/{ID}/chat_history.jsonl")),
            roots[2].join("tmp/project/chats/session-test.jsonl"),
        ];
        for file in &files {
            fs::create_dir_all(file.parent().unwrap()).unwrap();
        }
        let ts = chrono::Utc::now().to_rfc3339();
        fs::write(&files[0], "").unwrap();
        append(
            &files[0],
            json!({"timestamp":ts,"type":"session_meta","payload":{"id":ID,"cwd":"/fixture","source":"exec"}}),
        );
        append(
            &files[0],
            json!({"timestamp":ts,"type":"turn_context","payload":{"model":"codex-model"}}),
        );
        append(
            &files[0],
            json!({"timestamp":ts,"type":"event_msg","payload":{"type":"task_started","turn_id":"turn"}}),
        );
        append(
            &files[0],
            json!({"timestamp":ts,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"UserMessage","id":"u1","content":[{"type":"input_text","text":"Codex title"}]}}}),
        );
        fs::write(&files[1], "").unwrap();
        append(&files[1], json!({"type":"user","content":"Grok title"}));
        fs::write(files[1].with_file_name("summary.json"), json!({"info":{"id":ID},"generated_title":"Grok title","current_model_id":"grok-model","updated_at":ts}).to_string()).unwrap();
        fs::write(
            files[1].with_file_name("usage.json"),
            json!({"session":{"inputTokens":100,"outputTokens":20}}).to_string(),
        )
        .unwrap();
        fs::write(&files[2], "").unwrap();
        append(
            &files[2],
            json!({"sessionId":ID,"startTime":ts,"lastUpdated":ts,"kind":"main"}),
        );
        append(
            &files[2],
            json!({"id":"u1","timestamp":ts,"type":"user","content":[{"text":"Gemini title"}]}),
        );
        Self {
            _dir: dir,
            roots,
            files,
        }
    }
    pub fn args(&self) -> Vec<String> {
        ["--codex-dir", "--grok-dir", "--gemini-dir"]
            .into_iter()
            .zip(&self.roots)
            .flat_map(|(flag, p)| [flag.to_string(), p.display().to_string()])
            .collect()
    }
    pub fn update(&self) {
        let ts = chrono::Utc::now().to_rfc3339();
        append(
            &self.files[0],
            json!({"timestamp":ts,"type":"event_msg","payload":{"type":"item_completed","item":{"type":"AgentMessage","id":"a1","text":"unique-codex"}}}),
        );
        append(
            &self.files[0],
            json!({"timestamp":ts,"type":"event_msg","payload":{"type":"task_complete","turn_id":"turn"}}),
        );
        append(
            &self.files[1],
            json!({"type":"assistant","content":"unique-grok"}),
        );
        append(
            &self.files[2],
            json!({"$set":{"messages":[{"id":"u1","timestamp":ts,"type":"user","content":[{"text":"Gemini title"}]},{"id":"m1","timestamp":ts,"type":"model","content":[{"text":"unique-gemini"}]}],"lastUpdated":ts}}),
        );
    }
}
fn append(path: &Path, value: Value) {
    writeln!(
        fs::OpenOptions::new().append(true).open(path).unwrap(),
        "{value}"
    )
    .unwrap();
}
