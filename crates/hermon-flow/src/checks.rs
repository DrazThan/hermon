//! Project checks and bounded git evidence (Slice 5 public contract).
use crate::hub::{Block, BlockKind, Git};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::PathBuf,
};

#[derive(Debug, Clone)]
pub enum TestReportFormat {
    Json,
    Libtest,
}

#[derive(Debug, Clone)]
pub struct CheckCommand {
    pub argv: Vec<String>,
    pub timeout_seconds: u64,
    pub report: Option<TestReportFormat>,
}

#[derive(Debug, Clone)]
pub struct CheckConfig {
    pub workdir: PathBuf,
    pub base: String,
    pub ticket: String,
    pub branch: String,
    pub log_dir: PathBuf,
    pub env: BTreeMap<String, String>,
    pub paths: BTreeMap<String, Vec<String>>,
    pub commands: BTreeMap<String, CheckCommand>,
}

#[derive(Debug, Clone, Default)]
pub struct AttemptContext {
    pub number: u32,
    pub bot: String,
    pub model: Option<String>,
    pub new_tests: BTreeSet<String>,
    pub baseline_failures: BTreeSet<String>,
}

pub struct GitChecks;
impl GitChecks {
    pub fn new(_config: CheckConfig) -> Result<Self, Block> {
        Ok(Self)
    }
    pub fn set_attempt(&mut self, _context: AttemptContext) {}
}
impl Git for GitChecks {
    fn check(
        &mut self,
        _step: &str,
        _checks: &[String],
        _freezes: &BTreeMap<String, String>,
    ) -> Result<(), Block> {
        Err(Block::new(BlockKind::Unknown, "Slice 5 not implemented"))
    }
    fn evidence(&mut self, _step: &str, _tag: &str, _revision: u32) -> Result<String, Block> {
        Err(Block::new(BlockKind::Unknown, "Slice 5 not implemented"))
    }
}
