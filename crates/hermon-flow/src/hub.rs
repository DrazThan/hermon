//! Deterministic workflow hub; adapters own all external effects.
use crate::workflow::Workflow;
use anyhow::Result;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BlockKind {
    OwnerQuestion,
    TestDefect,
    Capability,
    Transient,
    ScopeChange,
    Unknown,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Block {
    pub kind: BlockKind,
    pub reason: String,
}
impl Block {
    pub fn new(kind: BlockKind, reason: impl Into<String>) -> Self {
        Self {
            kind,
            reason: reason.into(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum Status {
    Ready,
    Running,
    Gate,
    Blocked,
    Done,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Event {
    pub kind: String,
    pub step: String,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alert {
    pub step: String,
    pub reason: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Attempt {
    pub step: String,
    pub number: u32,
    pub handle: String,
    pub started_at: u64,
    pub family: String,
    pub freezes: BTreeMap<String, String>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run {
    pub id: String,
    pub current: String,
    pub status: Status,
    pub active: Option<Attempt>,
    pub attempts: Vec<Attempt>,
    pub events: Vec<Event>,
    pub alerts: Vec<Alert>,
    pub block: Option<Block>,
    pub freezes: BTreeMap<String, String>,
    pub pr: Option<String>,
    pub runs: BTreeMap<String, u32>,
    pub retries: BTreeMap<String, u32>,
    pub check_failures: BTreeMap<String, u32>,
    pub gate_requests: BTreeMap<String, u32>,
    pub unsettled: bool,
}
impl Run {
    pub fn new(id: impl Into<String>, start: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            current: start.into(),
            status: Status::Ready,
            active: None,
            attempts: vec![],
            events: vec![],
            alerts: vec![],
            block: None,
            freezes: BTreeMap::new(),
            pr: None,
            runs: BTreeMap::new(),
            retries: BTreeMap::new(),
            check_failures: BTreeMap::new(),
            gate_requests: BTreeMap::new(),
            unsettled: false,
        }
    }
}
#[derive(Debug, Clone)]
pub struct StartRequest {
    pub run_id: String,
    pub step: String,
    pub bot: String,
    pub number: u32,
    pub freezes: BTreeMap<String, String>,
}
#[derive(Debug, Clone)]
pub enum Poll {
    Running,
    Finished {
        process_success: bool,
        completion: bool,
        result: Option<String>,
    },
    /// The worker may still be doing work despite a dead viewer/process.
    Unsettled,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Settlement {
    Confirmed,
    Unsettled,
}
pub trait Agent {
    fn family(&self, bot: &str) -> String;
    fn available(&self, bot: &str) -> bool;
    fn start(&mut self, request: &StartRequest) -> std::result::Result<String, Block>;
    fn poll(&mut self, handle: &str) -> std::result::Result<Poll, Block>;
    fn stop(&mut self, handle: &str) -> std::result::Result<Settlement, Block>;
}
pub trait Tracker {
    /// True only for an exact authorized owner reply to this gate request.
    fn gate_approved(
        &mut self,
        run_id: &str,
        step: &str,
        request: u32,
    ) -> std::result::Result<bool, Block>;
    fn alert(&mut self, run_id: &str, alert: &Alert) -> Result<()>;
}
pub trait CodeHost {
    fn open_pr(&mut self, run_id: &str) -> std::result::Result<String, Block>;
    /// True only when merged and the merge commit is an ancestor of the base.
    fn merged(&mut self, pr: &str) -> std::result::Result<bool, Block>;
}
pub trait Git {
    fn check(
        &mut self,
        step: &str,
        checks: &[String],
        freezes: &BTreeMap<String, String>,
    ) -> std::result::Result<(), Block>;
    fn evidence(
        &mut self,
        step: &str,
        tag: &str,
        revision: u32,
    ) -> std::result::Result<String, Block>;
}
pub trait Clock {
    fn now(&self) -> u64;
}
pub struct Dependencies<'a> {
    pub agent: &'a mut dyn Agent,
    pub tracker: &'a mut dyn Tracker,
    pub code_host: &'a mut dyn CodeHost,
    pub git: &'a mut dyn Git,
    pub clock: &'a dyn Clock,
}
pub fn advance(_run: &mut Run, _workflow: &Workflow, _deps: &mut Dependencies<'_>) -> Result<()> {
    Ok(())
}
pub fn retry(_run: &mut Run, _reason: &str) -> Result<()> {
    Ok(())
}
pub fn override_step(_run: &mut Run, _workflow: &Workflow, _reason: &str) -> Result<()> {
    Ok(())
}
