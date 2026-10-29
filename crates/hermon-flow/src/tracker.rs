//! Central Linear app adapter and durable request replies.
use crate::{
    hub::{Alert, Block, BlockKind, Tracker},
    templates::Templates,
};
use anyhow::{Result, bail};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
};

pub struct LinearConfig {
    pub endpoint: String,
    pub token: String,
    pub issue_id: String,
    pub owner_ids: BTreeSet<String>,
    pub owner_mention: String,
}

#[derive(Debug, Clone)]
pub struct Issue {
    pub id: String,
    pub title: String,
    pub description: String,
}
#[derive(Debug, Clone)]
pub struct Comment {
    pub id: String,
    pub body: String,
    pub user_id: String,
    pub parent_id: Option<String>,
}
#[derive(Debug, Clone)]
pub struct CommentPage {
    pub comments: Vec<Comment>,
    pub next_cursor: Option<String>,
}

pub struct LinearTracker;
impl LinearTracker {
    pub fn new(
        _config: LinearConfig,
        _templates: Templates,
        _state_dir: impl AsRef<Path>,
    ) -> Result<Self> {
        Ok(Self)
    }
    pub fn post_comment(
        &mut self,
        _kind: &str,
        _run_id: &str,
        _request_id: &str,
        _values: &BTreeMap<String, String>,
    ) -> Result<String> {
        bail!("posting is not implemented")
    }
    pub fn read_issue(&mut self) -> Result<Issue> {
        bail!("issue reads are not implemented")
    }
    pub fn find_labeled(&mut self, _label: &str) -> Result<Vec<Issue>> {
        bail!("label reads are not implemented")
    }
    pub fn list_comments_since(&mut self, _cursor: Option<&str>) -> Result<CommentPage> {
        bail!("comment polling is not implemented")
    }
}
impl Tracker for LinearTracker {
    fn gate_approved(
        &mut self,
        _run_id: &str,
        _step: &str,
        _request: u32,
    ) -> std::result::Result<bool, Block> {
        Err(Block::new(
            BlockKind::Unknown,
            "gate replies are not implemented",
        ))
    }
    fn alert(&mut self, _run_id: &str, _alert: &Alert) -> Result<()> {
        bail!("alerts are not implemented")
    }
}
