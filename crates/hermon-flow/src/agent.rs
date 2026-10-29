//! Durable process supervision for flow agents.
use crate::hub::{Agent, Block, BlockKind, Poll, Settlement, StartRequest};
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    time::Duration,
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum Protocol {
    ResultFile,
    CodexJson,
    ClaudeJson,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CommandConfig {
    pub start: Vec<String>,
    pub resume: Option<Vec<String>>,
    pub task_stdin: bool,
    pub protocol: Protocol,
    pub family: String,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub enum BotConfig {
    Command(CommandConfig),
    Hermes { profile: String },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessAttempt {
    pub pid: u32,
    pub process_group: Option<u32>,
    pub session_id: Option<String>,
    pub log_path: PathBuf,
    pub stderr_path: PathBuf,
    pub timed_out: bool,
    pub cancelled: bool,
}
pub struct ProcessAgent;
impl ProcessAgent {
    pub fn new(
        _root: impl AsRef<Path>,
        _workdir: impl AsRef<Path>,
        _worker: impl AsRef<Path>,
        _bots: BTreeMap<String, BotConfig>,
        _timeout: Duration,
    ) -> Result<Self> {
        Ok(Self)
    }
    pub fn attempt(&self, _handle: &str) -> Result<ProcessAttempt> {
        anyhow::bail!("process observation not implemented")
    }
    pub fn set_resume(&mut self, _request: &StartRequest, _session: Option<String>) {}
}
impl Agent for ProcessAgent {
    fn family(&self, _bot: &str) -> String {
        String::new()
    }
    fn available(&self, _bot: &str) -> bool {
        true
    }
    fn start(&mut self, _request: &StartRequest) -> std::result::Result<String, Block> {
        Err(Block::new(
            BlockKind::Capability,
            "process launch not implemented",
        ))
    }
    fn poll(&mut self, _handle: &str) -> std::result::Result<Poll, Block> {
        Err(Block::new(
            BlockKind::Unknown,
            "process observation not implemented",
        ))
    }
    fn stop(&mut self, _handle: &str) -> std::result::Result<Settlement, Block> {
        Ok(Settlement::Unsettled)
    }
}
pub fn hermes_argv(_profile: &str, _task: &Path, _resume: Option<&str>) -> Vec<String> {
    vec![]
}
pub fn hermes_session_id(_stderr: &str) -> Option<String> {
    None
}
#[doc(hidden)]
pub fn worker_main() -> Result<()> {
    anyhow::bail!("worker not implemented")
}
