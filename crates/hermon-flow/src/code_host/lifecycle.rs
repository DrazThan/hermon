//! Schema-2 publication: stable identity, managed content and exact-head authority.
use super::*;
use crate::ci::{CheckRun, CiReceipt, Observation, Policy};

const BEGIN: &str = "<!-- hermon-managed:start -->";
const END: &str = "<!-- hermon-managed:end -->";
const PROJECTION: &str = "{total_count, check_runs: [.check_runs[] | {id, name, head_sha, status, conclusion, html_url, app: {slug: .app.slug}, check_suite: {id: .check_suite.id}}]}";

fn capability(message: impl std::fmt::Display) -> Block {
    block(BlockKind::Capability, message)
}
impl GithubCodeHost {
    pub(super) fn validate_managed_identity(&self, pr: &PullRequest) -> Result<(), Block> {
        let (_, managed, _) = managed_parts(&pr.body)?;
        if pr.body.matches("<!-- hermon-run:").count() != 1
            || !managed.lines().any(|line| line == self.marker)
        {
            return Err(capability("conflicting or misplaced lifecycle run marker"));
        }
        Ok(())
    }
    fn lifecycle_only(&self) -> Result<(), Block> {
        if self.state.schema != 2 {
            return Err(capability("schema-2 lifecycle is required"));
        }
        self.verify_remote()
    }
    fn persist(&self) -> Result<(), Block> {
        self.store.write_run(&self.state).map_err(capability)
    }
    fn local_candidate(&self, c: &Candidate) -> Result<(), Block> {
        if !valid_sha(&c.sha)
            || c.reference.is_empty()
            || c.reference.starts_with('-')
            || c.reference.chars().any(char::is_control)
        {
            return Err(capability("invalid accepted candidate identity"));
        }
        let sha =
            crate::checks::worktree_at(&self.config.workdir, &self.config.branch, &c.reference)
                .map_err(capability)?;
        if sha != c.sha {
            return Err(capability("worktree differs from accepted candidate"));
        }
        Ok(())
    }
    fn remote_head(&self) -> Result<Option<String>, Block> {
        let reference = format!("refs/heads/{}", self.config.branch);
        let out = self.checked(
            "git",
            &[
                "ls-remote",
                "--heads",
                "--",
                &self.config.remote,
                &reference,
            ],
        )?;
        if out.is_empty() {
            return Ok(None);
        }
        let parts: Vec<_> = out.split_whitespace().collect();
        if parts.len() != 2 || parts[1] != reference || !valid_sha(parts[0]) {
            return Err(capability("invalid remote branch observation"));
        }
        Ok(Some(parts[0].into()))
    }
    fn push_head(&mut self, c: &Candidate) -> Result<(), Block> {
        self.lifecycle_only()?;
        self.local_candidate(c)?;
        let remote = self.remote_head()?;
        if let Some(sha) = &remote {
            if sha != &c.sha && self.state.pushed_sha.as_ref() != Some(sha) {
                return Err(capability(
                    "unexpected remote head; refusing to overwrite external commits",
                ));
            }
            if sha != &c.sha {
                self.checked("git", &["merge-base", "--is-ancestor", sha, &c.sha])
                    .map_err(|_| {
                        capability("candidate does not fast-forward the published head")
                    })?;
            }
        } else if self.state.pushed_sha.is_some() {
            return Err(capability("published remote branch disappeared"));
        }
        if remote.as_deref() != Some(&c.sha) {
            self.checked(
                "git",
                &[
                    "push",
                    "--no-follow-tags",
                    "--",
                    &self.config.remote,
                    &format!("{}:refs/heads/{}", c.sha, self.config.branch),
                ],
            )?;
        }
        if self.remote_head()?.as_deref() != Some(&c.sha) {
            return Err(capability("pushed candidate readback differs"));
        }
        if self.state.pushed_sha.as_deref() != Some(&c.sha) {
            self.state.ready_intent = None;
            self.state.ready_receipt = None;
            self.state.ready_policy = None;
        }
        self.state.pushed_sha = Some(c.sha.clone());
        self.persist()
    }
    fn view_pr(&self, url: &str) -> Result<PullRequest, Block> {
        let number = self.number(url).map_err(capability)?.to_string();
        let output = self.gh(&[
            "pr",
            "view",
            &number,
            "--repo",
            &self.config.repo,
            "--json",
            LIFECYCLE_FIELDS,
        ])?;
        let pr: PullRequest = serde_json::from_str(&output)
            .map_err(|_| capability("invalid lifecycle PR response"))?;
        self.validate(&pr).map_err(capability)?;
        if pr.url != url
            || pr.is_draft.is_none()
            || !pr.head_ref_oid.as_deref().is_some_and(valid_sha)
        {
            return Err(capability(
                "incomplete or inconsistent lifecycle PR identity",
            ));
        }
        managed_parts(&pr.body)?;
        Ok(pr)
    }
    fn open_head(&self, pr: &PullRequest, c: &Candidate) -> Result<bool, Block> {
        if pr.state != "OPEN" {
            return Err(capability(format!("run PR is {}; expected OPEN", pr.state)));
        }
        if self.state.pushed_sha.as_deref() != Some(&c.sha) {
            return Err(capability("candidate was not pushed by this run"));
        }
        let head = pr
            .head_ref_oid
            .as_deref()
            .ok_or_else(|| capability("PR head is missing"))?;
        if head == c.sha {
            return Ok(true);
        }
        // A known ancestor can lag briefly after a successful push; it cannot pass CI.
        let (status, _) = self.command("git", &["merge-base", "--is-ancestor", head, &c.sha])?;
        if status.success() {
            return Ok(false);
        }
        Err(capability(
            "PR head unexpectedly moved away from accepted candidate",
        ))
    }
    fn managed_body(&self, c: &Candidate, auth: Option<&Authorization>) -> Result<String, Block> {
        if self.config.plan.len() > 32 * 1024 {
            return Err(capability("accepted plan exceeds 32 KiB"));
        }
        let review = auth
            .map(|a| a.review.summary.as_str())
            .unwrap_or("Review pending");
        if [self.config.plan.as_str(), review].iter().any(|text| {
            [BEGIN, END, "<!-- hermon-run:"]
                .iter()
                .any(|marker| text.contains(marker))
        }) {
            return Err(capability(
                "publication content contains reserved Hermon markers",
            ));
        }
        let ci = if let Some(auth) = auth {
            auth.ci
                .checks
                .iter()
                .map(|c| format!("{}: success ({})", c.name, c.html_url))
                .collect::<Vec<_>>()
                .join("\n")
        } else {
            "CI pending".into()
        };
        let red = c.red_reference.as_deref().unwrap_or("not configured");
        let amendments = if let Some(receipt) = &c.amendments {
            if let Some(auth) = auth {
                format!("Amendments: {}.", auth.amendment_disposition)
            } else {
                format!(
                    "Amendments: {} review pending; digest {}.",
                    if receipt.empty {
                        "empty;"
                    } else {
                        "nonempty; explicit acceptance required;"
                    },
                    receipt.diff_sha256
                )
            }
        } else {
            "Amendments: not applicable to this workflow.".into()
        };
        Ok(format!(
            "{BEGIN}\nRun: {}\nCandidate: {}\nGREEN evidence: {}\nOriginal RED evidence: {red}\nEvidence tags and notes remain local in the run store.\n\n## Plan\n\n{}\n\n## CI\n\n{ci}\n\n## Review summary\n\n{review}\n\n{amendments}\n{}\n{END}",
            self.config.run_id, c.sha, c.reference, self.config.plan, self.marker
        ))
    }
    fn update_body(
        &mut self,
        pr: &PullRequest,
        c: &Candidate,
        auth: Option<&Authorization>,
    ) -> Result<PullRequest, Block> {
        let (before, _, after) = managed_parts(&pr.body)?;
        let managed = self.managed_body(c, auth)?;
        let body = format!("{before}{managed}{after}");
        if body != pr.body {
            let mut file = tempfile::NamedTempFile::new().map_err(capability)?;
            file.write_all(body.as_bytes()).map_err(capability)?;
            let path = file
                .path()
                .to_str()
                .ok_or_else(|| capability("body path is not UTF-8"))?;
            let edited = self.gh(&[
                "pr",
                "edit",
                &pr.number.to_string(),
                "--repo",
                &self.config.repo,
                "--body-file",
                path,
            ]);
            let observed = self.view_pr(&pr.url)?;
            if observed.body != body {
                edited?;
                return Err(capability("PR body edit readback differs"));
            }
        }
        let observed = self.view_pr(&pr.url)?;
        if observed.body != body {
            return Err(capability("PR body changed during update"));
        }
        self.state.body_digest = Some(crate::intake::hash(&managed));
        self.persist()?;
        Ok(observed)
    }
    pub(super) fn open_draft(&mut self, run: &str, c: &Candidate) -> Result<String, Block> {
        self.lifecycle_only()?;
        if run != self.config.run_id {
            return Err(capability("draft belongs to another run"));
        }
        let existing = self.lookup()?;
        if existing.is_none() && self.state.pr.is_some() {
            return Err(capability("recorded PR is missing"));
        }
        if let Some(pr) = &existing {
            if pr.state != "OPEN" || self.state.pr.as_ref().is_some_and(|url| url != &pr.url) {
                return Err(capability("draft PR identity or state changed"));
            }
            managed_parts(&pr.body)?;
        }
        self.push_head(c)?;
        let pr = match existing {
            Some(pr) => self.view_pr(&pr.url)?,
            None => {
                let body = self.managed_body(c, None)?;
                let mut file = tempfile::NamedTempFile::new().map_err(capability)?;
                file.write_all(body.as_bytes()).map_err(capability)?;
                let path = file
                    .path()
                    .to_str()
                    .ok_or_else(|| capability("body path is not UTF-8"))?;
                let created = self.gh(&[
                    "pr",
                    "create",
                    "--repo",
                    &self.config.repo,
                    "--base",
                    &self.config.base,
                    "--head",
                    &self.config.branch,
                    "--title",
                    &self.config.title,
                    "--body-file",
                    path,
                    "--draft",
                ]);
                // Reconcile even a lost response; never create twice after an uncertain mutation.
                let found = self.lookup()?;
                let pr = match found {
                    Some(pr) => pr,
                    None => {
                        created?;
                        return Err(capability("created PR is missing from readback"));
                    }
                };
                self.view_pr(&pr.url)?
            }
        };
        if !self.open_head(&pr, c)? {
            return Err(Block::new(
                BlockKind::Transient,
                "published PR head is propagating",
            ));
        }
        let pr = self.update_body(&pr, c, None)?;
        self.local_candidate(c)?;
        self.save_pr(pr.url)
    }
    pub(super) fn synchronize(&mut self, url: &str, c: &Candidate) -> Result<(), Block> {
        self.lifecycle_only()?;
        if self.state.pr.as_deref() != Some(url) {
            return Err(capability("PR differs from recorded run"));
        }
        self.push_head(c)?;
        let observed = self.view_pr(url)?;
        if !self.open_head(&observed, c)? {
            return Err(Block::new(
                BlockKind::Transient,
                "synchronized PR head is propagating",
            ));
        }
        self.local_candidate(c)?;
        Ok(())
    }
    pub(super) fn observe_checks(
        &mut self,
        url: &str,
        c: &Candidate,
        policy: &Policy,
        digest: &str,
    ) -> Result<Observation, Block> {
        self.lifecycle_only()?;
        policy.validate().map_err(capability)?;
        if self.state.pr.as_deref() != Some(url) || digest.is_empty() {
            return Err(capability("CI identity or policy digest missing"));
        }
        self.local_candidate(c)?;
        if self.remote_head()?.as_deref() != Some(&c.sha) {
            return Err(capability(
                "remote head differs from accepted pushed candidate",
            ));
        }
        let before = self.view_pr(url)?;
        if !self.open_head(&before, c)? {
            return Ok(Observation::Waiting {
                missing: policy.selected_checks.clone(),
                checks: vec![],
            });
        }
        let endpoint = format!(
            "repos/{}/commits/{}/check-runs?filter=all&per_page=100",
            self.config.repo, c.sha
        );
        let output = self.gh(&[
            "api",
            "--hostname",
            "github.com",
            "--method",
            "GET",
            &endpoint,
            "--paginate",
            "--jq",
            PROJECTION,
        ])?;
        #[derive(Deserialize)]
        struct Page {
            total_count: usize,
            check_runs: Vec<CheckRun>,
        }
        let mut total = None;
        let mut all = BTreeMap::new();
        for line in output.lines() {
            let page: Page = serde_json::from_str(line)
                .map_err(|_| capability("malformed projected check-runs page"))?;
            if total.is_some_and(|count| count != page.total_count) {
                return Err(Block::new(
                    BlockKind::Transient,
                    "CI pagination changed; retrying complete observation",
                ));
            }
            total = Some(page.total_count);
            for check in page.check_runs {
                if check.id == 0 {
                    return Err(capability("check-run identity is missing"));
                }
                if let Some(previous) = all.insert(check.id, check.clone()) {
                    if previous != check {
                        return Err(capability("conflicting duplicate check-run ID"));
                    }
                    return Err(Block::new(
                        BlockKind::Transient,
                        "duplicate CI pagination entry",
                    ));
                }
            }
        }
        if total != Some(all.len()) {
            return Err(Block::new(
                BlockKind::Transient,
                "incomplete check-runs pagination",
            ));
        }
        let mut latest: BTreeMap<String, CheckRun> = BTreeMap::new();
        for check in all
            .into_values()
            .filter(|c| policy.selected_checks.contains(&c.name))
        {
            if check.app.slug != "github-actions"
                || check.head_sha != c.sha
                || check.check_suite.id == 0
                || !check.html_url.starts_with("https://github.com/")
                || check.html_url.chars().any(char::is_control)
            {
                return Err(capability(
                    "selected check-run has inconsistent SHA, app or identity",
                ));
            }
            if latest.get(&check.name).is_none_or(|old| old.id < check.id) {
                latest.insert(check.name.clone(), check);
            }
        }
        let after = self.view_pr(url)?;
        if !self.open_head(&after, c)? {
            return Ok(Observation::Waiting {
                missing: policy.selected_checks.clone(),
                checks: vec![],
            });
        }
        self.local_candidate(c)?;
        if self.remote_head()?.as_deref() != Some(&c.sha) {
            return Err(capability("remote head changed during CI observation"));
        }
        let mut missing = vec![];
        let mut pending = false;
        let mut failures = vec![];
        for name in &policy.selected_checks {
            let Some(check) = latest.get(name) else {
                missing.push(name.clone());
                continue;
            };
            match (check.status.as_str(), check.conclusion.as_deref()) {
                ("completed", Some("success")) => {}
                ("queued" | "in_progress" | "waiting" | "requested" | "pending", None) => {
                    pending = true
                }
                (
                    "completed",
                    Some(
                        reason @ ("failure" | "cancelled" | "timed_out" | "action_required"
                        | "startup_failure" | "stale" | "skipped" | "neutral"),
                    ),
                ) => failures.push(format!("{name}: {reason} ({})", check.html_url)),
                _ => return Err(capability(format!("unknown selected check state: {name}"))),
            }
        }
        let checks: Vec<_> = latest.into_values().collect();
        // Preserve the complete bounded projected observation outside diagnostic/event text.
        self.store
            .append_event(
                &serde_json::json!({"candidate_sha":c.sha,"checks":checks,"missing":missing}),
            )
            .map_err(capability)?;
        if !failures.is_empty() {
            let mut reason = String::from("GitHub CI failed:\n");
            let mut omitted = 0;
            for failure in failures {
                if reason.len() + failure.len() <= 7800 {
                    reason.push_str(&failure);
                    reason.push('\n');
                } else {
                    omitted += 1;
                }
            }
            if omitted > 0 {
                reason.push_str(&format!(
                    "{omitted} additional failing jobs; see code-host observations"
                ));
            }
            return Err(Block::new(BlockKind::CiFailed, reason));
        }
        if !missing.is_empty() || pending {
            return Ok(Observation::Waiting { missing, checks });
        }
        Ok(Observation::Passed(CiReceipt {
            candidate_sha: c.sha.clone(),
            policy_digest: digest.into(),
            checks,
        }))
    }
    pub(super) fn mark_ready(
        &mut self,
        auth: &Authorization,
        policy: &Policy,
    ) -> Result<(), Block> {
        self.lifecycle_only()?;
        auth.validate(policy, &auth.ci.policy_digest)
            .map_err(capability)?;
        if auth.run_id != self.config.run_id || self.state.pr.as_deref() != Some(&auth.pr) {
            return Err(capability("ready authorization belongs to another run/PR"));
        }
        self.local_candidate(&auth.candidate)?;
        let pr = self.view_pr(&auth.pr)?;
        if !self.open_head(&pr, &auth.candidate)? {
            return Err(Block::new(
                BlockKind::Transient,
                "ready head is propagating",
            ));
        }
        self.state.ready_intent = Some(auth.clone());
        self.state.ready_policy = Some(policy.clone());
        self.persist()?;
        let pr = self.update_body(&pr, &auth.candidate, Some(auth))?;
        if pr.is_draft != Some(false) {
            let ready = self.gh(&[
                "pr",
                "ready",
                &pr.number.to_string(),
                "--repo",
                &self.config.repo,
            ]);
            let observed = self.view_pr(&auth.pr)?;
            if observed.is_draft != Some(false) {
                ready?;
                return Err(capability("PR was not readied"));
            }
        }
        let observed = self.view_pr(&auth.pr)?;
        if !self.open_head(&observed, &auth.candidate)?
            || observed.is_draft != Some(false)
            || managed_parts(&observed.body)?.1 != self.managed_body(&auth.candidate, Some(auth))?
        {
            return Err(capability("ready readback differs from authorization"));
        }
        self.local_candidate(&auth.candidate)?;
        self.state.ready_receipt = Some(auth.clone());
        self.persist()
    }
    pub fn ready_authorization(&self) -> Option<&Authorization> {
        self.state.ready_receipt.as_ref()
    }
    pub(super) fn validate_ready_merge(&self, pr: &PullRequest) -> Result<(), Block> {
        let auth = self
            .state
            .ready_receipt
            .as_ref()
            .ok_or_else(|| capability("merge requires finalized ready authorization"))?;
        let policy = self
            .state
            .ready_policy
            .as_ref()
            .ok_or_else(|| capability("ready policy is missing"))?;
        auth.validate(policy, &auth.ci.policy_digest)
            .map_err(capability)?;
        if auth.run_id != self.config.run_id
            || auth.pr != pr.url
            || self.state.ready_intent.as_ref() != Some(auth)
            || self.state.pushed_sha.as_deref() != Some(&auth.candidate.sha)
            || pr.head_ref_oid.as_deref() != Some(&auth.candidate.sha)
            || pr.is_draft != Some(false)
            || pr.state == "CLOSED"
        {
            return Err(capability(
                "merge PR head/state differs from finalized ready candidate",
            ));
        }
        Ok(())
    }
}
fn valid_sha(sha: &str) -> bool {
    matches!(sha.len(), 40 | 64) && sha.bytes().all(|b| b.is_ascii_hexdigit())
}
fn managed_parts(body: &str) -> Result<(&str, &str, &str), Block> {
    if body.matches(BEGIN).count() != 1 || body.matches(END).count() != 1 {
        return Err(capability(
            "missing or duplicate Hermon management delimiters",
        ));
    }
    let start = body.find(BEGIN).unwrap();
    let end = body.find(END).unwrap() + END.len();
    if end < start + BEGIN.len() {
        return Err(capability("invalid Hermon management delimiter order"));
    }
    Ok((&body[..start], &body[start..end], &body[end..]))
}
