//! Exact-candidate publication and CI authorization shared by hub and adapters.
use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Policy {
    pub selected_checks: Vec<String>,
    #[serde(default = "poll_default")]
    pub poll_interval_seconds: u64,
    #[serde(default = "missing_default")]
    pub no_checks_timeout_seconds: u64,
    #[serde(default = "timeout_default")]
    pub timeout_seconds: u64,
}
fn poll_default() -> u64 {
    30
}
fn missing_default() -> u64 {
    600
}
fn timeout_default() -> u64 {
    3600
}
impl Policy {
    pub fn validate(&self) -> Result<()> {
        let names: BTreeSet<_> = self.selected_checks.iter().collect();
        ensure!(
            !names.is_empty()
                && names.len() == self.selected_checks.len()
                && names.iter().all(|n| !n.trim().is_empty()
                    && n.len() <= 512
                    && !n.chars().any(char::is_control)),
            "code_host.ci.selected_checks must contain nonempty, unique exact job names"
        );
        ensure!(
            self.poll_interval_seconds > 0
                && self.no_checks_timeout_seconds > 0
                && self.timeout_seconds > 0
                && self.no_checks_timeout_seconds <= self.timeout_seconds,
            "code_host.ci deadlines must be positive seconds, with no_checks_timeout_seconds <= timeout_seconds"
        );
        Ok(())
    }
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PinnedPolicy {
    pub run_id: String,
    pub workflow_hash: String,
    pub policy: Policy,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Candidate {
    pub sha: String,
    pub reference: String,
    pub step: String,
    pub attempt: u32,
    pub family: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub red_reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amendments: Option<crate::checks::Amendments>,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReviewReceipt {
    pub candidate_sha: String,
    pub step: String,
    pub attempt: u32,
    pub family: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub amendments: Option<crate::checks::Amendments>,
}
impl ReviewReceipt {
    pub fn amendment_disposition(&self) -> String {
        self.amendments
            .as_ref()
            .map_or_else(|| "not_applicable".into(), |a| a.disposition())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckRun {
    pub id: u64,
    pub name: String,
    pub head_sha: String,
    pub status: String,
    pub conclusion: Option<String>,
    pub html_url: String,
    pub app: App,
    pub check_suite: Suite,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct App {
    pub slug: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Suite {
    pub id: u64,
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CiReceipt {
    pub candidate_sha: String,
    pub policy_digest: String,
    pub checks: Vec<CheckRun>,
}
#[derive(Debug, Clone)]
pub enum Observation {
    Waiting {
        missing: Vec<String>,
        checks: Vec<CheckRun>,
    },
    Passed(CiReceipt),
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Authorization {
    pub run_id: String,
    pub pr: String,
    pub candidate: Candidate,
    pub review: ReviewReceipt,
    pub ci: CiReceipt,
    pub amendment_disposition: String,
}
impl Authorization {
    pub fn validate(&self, policy: &Policy, digest: &str) -> Result<()> {
        policy.validate()?;
        ensure!(
            !self.run_id.is_empty()
                && !self.pr.is_empty()
                && !self.candidate.reference.is_empty()
                && !self.candidate.step.is_empty()
                && !self.review.step.is_empty()
                && matches!(self.candidate.sha.len(), 40 | 64)
                && self.candidate.sha.bytes().all(|b| b.is_ascii_hexdigit())
                && digest.len() == 64
                && digest.bytes().all(|b| b.is_ascii_hexdigit())
                && self.candidate.attempt > 0
                && self.review.attempt > 0
                && !self.candidate.family.is_empty()
                && !self.review.family.is_empty()
                && self.candidate.family != self.review.family
                && !self.review.summary.trim().is_empty()
                && self.review.candidate_sha == self.candidate.sha
                && self.ci.candidate_sha == self.candidate.sha
                && self.ci.policy_digest == digest
                && self.amendment_disposition == self.review.amendment_disposition()
                && self.candidate.amendments == self.review.amendments
                && self
                    .review
                    .amendments
                    .as_ref()
                    .is_none_or(|a| a.candidate_sha == self.candidate.sha
                        && a.diff_sha256.len() == 64
                        && a.diff_sha256.bytes().all(|b| b.is_ascii_hexdigit())),
            "missing or inconsistent candidate/review/CI/amendment authorization"
        );
        ensure!(
            self.ci.checks.len() == policy.selected_checks.len(),
            "incomplete CI receipt"
        );
        for name in &policy.selected_checks {
            let checks: Vec<_> = self.ci.checks.iter().filter(|c| &c.name == name).collect();
            ensure!(
                checks.len() == 1
                    && checks[0].head_sha == self.candidate.sha
                    && checks[0].app.slug == "github-actions"
                    && checks[0].id > 0
                    && checks[0].check_suite.id > 0
                    && checks[0].status == "completed"
                    && checks[0].conclusion.as_deref() == Some("success"),
                "CI receipt does not prove selected check {name}"
            );
        }
        Ok(())
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WaitState {
    pub step: String,
    pub visit: u32,
    pub expected_sha: String,
    pub first_wait: u64,
    pub next_poll: u64,
    pub missing: Vec<String>,
    pub checks: Vec<CheckRun>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub passed: Option<CiReceipt>,
}
