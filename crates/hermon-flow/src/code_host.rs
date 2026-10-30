//! Run-bound GitHub PR publication and owner-merge observation through `gh`.
use crate::{
    agent::OwnedChild,
    hub::{Block, BlockKind, CodeHost},
    store::RunStore,
};
use anyhow::{Context, Result, ensure};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::{self, File},
    io::{Read, Write},
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, ExitStatus, Stdio},
    thread,
    time::{Duration, Instant},
};

/// Host-owned intake and reviewed artifacts. The host holds the run lock and
/// gives this adapter its own state directory, separate from the hub snapshot.
#[derive(Debug, Clone)]
pub struct GithubConfig {
    pub workdir: PathBuf,
    /// GitHub.com repository in owner/name form.
    pub repo: String,
    pub remote: String,
    /// Branch selected at intake, not a commit or the repository default.
    pub base: String,
    pub branch: String,
    pub run_id: String,
    pub title: String,
    pub plan: String,
    pub review_summary: String,
    /// Complete command environment, including PATH and gh authentication.
    pub env: BTreeMap<String, String>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct State {
    schema: u32,
    binding: String,
    remote_hash: String,
    pr: Option<String>,
    #[serde(default)]
    merge_commit: Option<String>,
}
#[derive(Deserialize)]
struct Owner {
    login: String,
}
#[derive(Deserialize)]
struct Commit {
    oid: String,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct PullRequest {
    number: u64,
    url: String,
    state: String,
    base_ref_name: String,
    head_ref_name: String,
    body: String,
    head_repository_owner: Owner,
    merge_commit: Option<Commit>,
}
const PR_FIELDS: &str =
    "number,url,state,baseRefName,headRefName,body,headRepositoryOwner,mergeCommit";

pub struct GithubCodeHost {
    config: GithubConfig,
    store: RunStore,
    state: State,
    marker: String,
}
impl GithubCodeHost {
    pub fn new(mut config: GithubConfig, state_dir: impl AsRef<Path>) -> Result<Self, Block> {
        let prepare = || -> Result<Self> {
            config.workdir = fs::canonicalize(&config.workdir)?;
            ensure!(config.workdir.is_dir(), "workdir must be a directory");
            let parts: Vec<_> = config.repo.split('/').collect();
            ensure!(
                parts.len() == 2
                    && parts.iter().all(|part| !part.is_empty()
                        && *part != "."
                        && *part != ".."
                        && part
                            .chars()
                            .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))),
                "repository must be owner/name"
            );
            ensure!(
                !config.remote.is_empty()
                    && config
                        .remote
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c))
                    && !config.remote.starts_with('-'),
                "invalid git remote name"
            );
            ensure!(
                !config.run_id.is_empty()
                    && config.run_id.len() <= 256
                    && config
                        .run_id
                        .chars()
                        .all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c)),
                "invalid run ID"
            );
            ensure!(
                [&config.title, &config.plan, &config.review_summary]
                    .iter()
                    .all(|value| !value.trim().is_empty() && !value.contains('\0')),
                "title, plan and review summary are required"
            );
            for (key, value) in &config.env {
                ensure!(
                    !key.is_empty() && !key.contains(['=', '\0']) && !value.contains('\0'),
                    "invalid command environment"
                );
            }
            let store = RunStore::new(state_dir)?;
            let mut host = Self {
                config,
                store,
                state: State {
                    schema: 1,
                    binding: String::new(),
                    remote_hash: String::new(),
                    pr: None,
                    merge_commit: None,
                },
                marker: String::new(),
            };
            for branch in [&host.config.base, &host.config.branch] {
                ensure!(!branch.starts_with('-'), "invalid branch name");
                host.git(&["check-ref-format", &format!("refs/heads/{branch}")])?;
            }
            ensure!(
                host.config.base != host.config.branch,
                "base and run branch must differ"
            );
            let remote_hash = host.remote_hash()?;
            // Hash the binding, never the environment: credentials stay out of
            // durable files. JSON encoding preserves boundaries between fields.
            let binding = format!(
                "{:x}",
                Sha256::digest(serde_json::to_vec(&(
                    &host.config.workdir,
                    &host.config.repo,
                    &host.config.remote,
                    &host.config.base,
                    &host.config.branch,
                    &host.config.run_id,
                    &host.config.title,
                    &host.config.plan,
                    &host.config.review_summary,
                ))?)
            );
            host.state = host.store.read_run()?.unwrap_or(State {
                schema: 1,
                binding: binding.clone(),
                remote_hash,
                pr: None,
                merge_commit: None,
            });
            ensure!(
                host.state.schema == 1 && host.state.binding == binding,
                "code host state belongs to another intake or artifact revision"
            );
            host.marker = format!(
                "<!-- hermon-run:{} binding:{binding} remote:{} -->",
                host.config.run_id, host.state.remote_hash
            );
            host.store.write_run(&host.state)?;
            Ok(host)
        };
        prepare().map_err(|e| block(BlockKind::Capability, e))
    }
    fn body(&self) -> String {
        format!(
            "Run: {}\n\n## Plan\n\n{}\n\n## Review summary\n\n{}\n\n{}\n",
            self.config.run_id, self.config.plan, self.config.review_summary, self.marker
        )
    }
    pub fn merge_commit(&self) -> Option<&str> {
        self.state.merge_commit.as_deref()
    }
    fn remote_hash(&self) -> Result<String> {
        // Git fetch uses the fetch URL, while push can use one or more separate
        // pushurls. Bind all endpoints, including insteadOf URL expansions.
        let fetch = self.git(&["remote", "get-url", "--all", &self.config.remote])?;
        let push = self.git(&["remote", "get-url", "--push", "--all", &self.config.remote])?;
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(fetch, push))?)
        ))
    }
    fn verify_remote(&self) -> Result<(), Block> {
        if self
            .remote_hash()
            .map_err(|e| block(BlockKind::Capability, e))?
            != self.state.remote_hash
        {
            return Err(Block::new(
                BlockKind::Capability,
                "run remote endpoint changed",
            ));
        }
        Ok(())
    }
    fn number(&self, url: &str) -> Result<u64> {
        let (repo_url, number) = url.rsplit_once("/pull/").context("invalid PR URL")?;
        ensure!(
            repo_url.eq_ignore_ascii_case(&format!("https://github.com/{}", self.config.repo)),
            "PR belongs to another repository"
        );
        let parsed: u64 = number.parse().context("invalid PR number")?;
        ensure!(
            parsed > 0 && parsed.to_string() == number,
            "invalid PR number"
        );
        Ok(parsed)
    }
    fn validate(&self, pr: &PullRequest) -> Result<()> {
        ensure!(
            self.number(&pr.url)? == pr.number,
            "PR URL and number disagree"
        );
        ensure!(
            pr.base_ref_name == self.config.base && pr.head_ref_name == self.config.branch,
            "PR base/head differs from intake"
        );
        let owner = self.config.repo.split('/').next().unwrap();
        ensure!(
            pr.head_repository_owner.login.eq_ignore_ascii_case(owner),
            "PR head belongs to another owner"
        );
        ensure!(
            pr.body.lines().any(|line| line == self.marker),
            "PR has no matching run marker"
        );
        ensure!(
            matches!(pr.state.as_str(), "OPEN" | "CLOSED" | "MERGED"),
            "invalid PR state"
        );
        Ok(())
    }
    fn save_pr(&mut self, url: String) -> Result<String, Block> {
        self.state.pr = Some(url.clone());
        self.store
            .write_run(&self.state)
            .map_err(|e| block(BlockKind::Unknown, e))?;
        Ok(url)
    }
    fn lookup(&self) -> Result<Option<PullRequest>, Block> {
        let output = self.gh(&[
            "pr",
            "list",
            "--repo",
            &self.config.repo,
            "--base",
            &self.config.base,
            "--head",
            &self.config.branch,
            "--state",
            "all",
            "--limit",
            "100",
            "--json",
            PR_FIELDS,
        ])?;
        let prs: Vec<PullRequest> = serde_json::from_str(&output)
            .map_err(|_| Block::new(BlockKind::Unknown, "invalid PR lookup response"))?;
        // A run owns its branch. Ambiguous, closed or foreign PRs require the
        // owner to resolve them; they cannot authorize a second publication.
        if prs.len() > 1 {
            return Err(Block::new(
                BlockKind::Capability,
                "multiple PRs use the run branch",
            ));
        }
        let pr = prs.into_iter().next();
        if let Some(pr) = &pr {
            self.validate(pr)
                .map_err(|e| block(BlockKind::Capability, e))?;
            if pr.state == "CLOSED" {
                return Err(Block::new(
                    BlockKind::Capability,
                    "run PR was closed without merging",
                ));
            }
        }
        Ok(pr)
    }
    fn gh(&self, args: &[&str]) -> Result<String, Block> {
        self.checked("gh", args)
    }
    fn git(&self, args: &[&str]) -> Result<String> {
        self.checked("git", args)
            .map_err(|b| anyhow::anyhow!(b.reason))
    }
    fn checked(&self, program: &str, args: &[&str]) -> Result<String, Block> {
        let (status, output) = self.command(program, args)?;
        if !status.success() {
            return Err(Block::new(
                BlockKind::Transient,
                format!(
                    "{program} {} failed (exit {:?})",
                    args.first().unwrap_or(&"command"),
                    status.code()
                ),
            ));
        }
        Ok(output)
    }
    fn command(&self, program: &str, args: &[&str]) -> Result<(ExitStatus, String), Block> {
        let execute = || -> Result<(ExitStatus, String)> {
            let path = self
                .config
                .env
                .get("PATH")
                .context("configured PATH is required")?;
            let executable = std::env::split_paths(path)
                .map(|dir| self.config.workdir.join(dir).join(program))
                .find(|p| {
                    p.metadata()
                        .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
                })
                .context("command is missing from configured PATH")?;
            let stdout = tempfile::NamedTempFile::new()?;
            let stderr = tempfile::NamedTempFile::new()?;
            let mut command = Command::new(executable);
            command
                .args(args)
                .current_dir(&self.config.workdir)
                .env_clear()
                .envs(&self.config.env)
                .env("GIT_TERMINAL_PROMPT", "0")
                .env("GH_PROMPT_DISABLED", "1")
                .stdin(Stdio::null())
                .stdout(stdout.reopen()?)
                .stderr(stderr.reopen()?)
                .process_group(0);
            if program == "gh" {
                command.env("GH_HOST", "github.com");
            }
            let child = command.spawn().context("spawn code host command")?;
            let mut owned = OwnedChild::new(child);
            let started = Instant::now();
            while !owned.exited()? {
                ensure!(
                    started.elapsed() < Duration::from_secs(60),
                    "code host command timed out"
                );
                ensure!(
                    stdout.as_file().metadata()?.len() <= 2 * 1024 * 1024
                        && stderr.as_file().metadata()?.len() <= 2 * 1024 * 1024,
                    "code host output exceeded limit"
                );
                thread::sleep(Duration::from_millis(10));
            }
            let status = owned.finish()?;
            ensure!(
                stderr.as_file().metadata()?.len() <= 2 * 1024 * 1024,
                "code host output exceeded limit"
            );
            let mut output = String::new();
            File::open(stdout.path())?
                .take(2 * 1024 * 1024 + 1)
                .read_to_string(&mut output)?;
            ensure!(
                output.len() <= 2 * 1024 * 1024,
                "code host output exceeded limit"
            );
            Ok((status, output.trim().to_owned()))
        };
        execute().map_err(|e| {
            // Do not echo child output, arguments or authentication variables.
            let kind = if format!("{e:#}").contains("configured PATH")
                || format!("{e:#}").contains("spawn code host")
            {
                BlockKind::Capability
            } else {
                BlockKind::Transient
            };
            block(kind, e)
        })
    }
}
impl CodeHost for GithubCodeHost {
    fn open_pr(&mut self, run_id: &str) -> Result<String, Block> {
        if run_id != self.config.run_id {
            return Err(Block::new(
                BlockKind::Capability,
                "code host belongs to another run",
            ));
        }
        self.verify_remote()?;
        if let Some(pr) = self.lookup()? {
            if self.state.pr.as_ref().is_some_and(|saved| saved != &pr.url) {
                return Err(Block::new(BlockKind::Capability, "PR identity changed"));
            }
            return self.save_pr(pr.url);
        }
        if self.state.pr.is_some() {
            return Err(Block::new(
                BlockKind::Unknown,
                "recorded run PR is missing from lookup",
            ));
        }
        let source = format!("refs/heads/{}", self.config.branch);
        self.checked(
            "git",
            &[
                "push",
                "--no-follow-tags",
                "--",
                &self.config.remote,
                &format!("{source}:{source}"),
            ],
        )?;
        let mut body = tempfile::NamedTempFile::new().map_err(|e| block(BlockKind::Unknown, e))?;
        body.write_all(self.body().as_bytes())
            .map_err(|e| block(BlockKind::Unknown, e))?;
        let path = body
            .path()
            .to_str()
            .ok_or_else(|| Block::new(BlockKind::Capability, "body path must be UTF-8"))?;
        let url = self.gh(&[
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
        ])?;
        self.number(&url)
            .map_err(|e| block(BlockKind::Unknown, e))?;
        self.save_pr(url)
    }
    fn merged(&mut self, pr: &str) -> Result<bool, Block> {
        self.verify_remote()?;
        if self.state.pr.as_deref() != Some(pr) {
            return Err(Block::new(
                BlockKind::Capability,
                "merge gate PR differs from recorded run PR",
            ));
        }
        let number = self
            .number(pr)
            .map_err(|e| block(BlockKind::Capability, e))?
            .to_string();
        let output = self.gh(&[
            "pr",
            "view",
            &number,
            "--repo",
            &self.config.repo,
            "--json",
            PR_FIELDS,
        ])?;
        let observed: PullRequest = serde_json::from_str(&output)
            .map_err(|_| Block::new(BlockKind::Unknown, "invalid PR state response"))?;
        self.validate(&observed)
            .map_err(|e| block(BlockKind::Unknown, e))?;
        if observed.url != pr {
            return Err(Block::new(
                BlockKind::Unknown,
                "observed PR identity changed",
            ));
        }
        if observed.state != "MERGED" {
            return Ok(false);
        }
        let Some(commit) = observed.merge_commit else {
            return Ok(false);
        };
        if !matches!(commit.oid.len(), 40 | 64)
            || !commit.oid.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err(Block::new(BlockKind::Unknown, "invalid merge commit OID"));
        }
        // Fetch into a run-specific ref: local base branches and shared
        // FETCH_HEAD cannot establish fresh ancestry for this run.
        let target = format!("refs/hermon/code-host/{}/base", self.state.binding);
        self.checked(
            "git",
            &[
                "fetch",
                "--no-tags",
                "--",
                &self.config.remote,
                &format!("+refs/heads/{}:{target}", self.config.base),
            ],
        )?;
        let (status, _) = self.command(
            "git",
            &["merge-base", "--is-ancestor", &commit.oid, &target],
        )?;
        match status.code() {
            Some(0) => {
                self.state.merge_commit = Some(commit.oid);
                self.store
                    .write_run(&self.state)
                    .map_err(|e| block(BlockKind::Unknown, e))?;
                Ok(true)
            }
            Some(1) => Ok(false),
            _ => Err(Block::new(
                BlockKind::Unknown,
                "could not verify merge ancestry",
            )),
        }
    }
}
fn block(kind: BlockKind, error: impl std::fmt::Display) -> Block {
    Block::new(kind, error.to_string())
}
