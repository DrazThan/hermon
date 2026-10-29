//! Run-bound GitHub code host.
use crate::hub::{Block, CodeHost};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};
#[derive(Debug, Clone)]
pub struct GithubConfig {
    pub workdir: PathBuf,
    pub repo: String,
    pub remote: String,
    pub base: String,
    pub branch: String,
    pub run_id: String,
    pub title: String,
    pub plan: String,
    pub review_summary: String,
    pub env: BTreeMap<String, String>,
}
pub struct GithubCodeHost;
impl GithubCodeHost {
    pub fn new(_config: GithubConfig, _state_dir: impl AsRef<Path>) -> Result<Self, Block> {
        Ok(Self)
    }
}
impl CodeHost for GithubCodeHost {
    fn open_pr(&mut self, _run_id: &str) -> Result<String, Block> {
        unimplemented!("Slice 7 open_pr")
    }
    fn merged(&mut self, _pr: &str) -> Result<bool, Block> {
        unimplemented!("Slice 7 merged")
    }
}
