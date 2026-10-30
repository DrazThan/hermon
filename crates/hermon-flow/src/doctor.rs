//! Capability preflight with the same explicit environment used by project checks.
use crate::{
    agent::{BotConfig, OwnedChild, Protocol, protocol_observation, substitute},
    intake::Project,
    workflow::Workflow,
};
use anyhow::{Context, Result, bail, ensure};
use serde::Serialize;
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::Read,
    os::unix::{fs::PermissionsExt, process::CommandExt},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

#[derive(Debug, Serialize)]
pub struct Finding {
    pub name: String,
    pub ok: bool,
    pub fix: Option<String>,
}
#[derive(Debug, Serialize)]
pub struct Report {
    pub schema: u32,
    pub checks: Vec<Finding>,
}
impl Report {
    pub fn passed(&self) -> bool {
        self.checks.iter().all(|c| c.ok)
    }
    pub fn require_pass(&self) -> Result<()> {
        let failures = self
            .checks
            .iter()
            .filter(|c| !c.ok)
            .map(|c| {
                format!(
                    "{}: {}",
                    c.name,
                    c.fix.as_deref().unwrap_or("capability missing")
                )
            })
            .collect::<Vec<_>>();
        ensure!(
            failures.is_empty(),
            "doctor failed; {}",
            failures.join("; ")
        );
        Ok(())
    }
    fn check(&mut self, name: String, result: Result<()>) {
        self.checks.push(Finding {
            name,
            ok: result.is_ok(),
            fix: result.err().map(|e| format!("{e:#}")),
        });
    }
}

/// This creates only temporary probe files, never a run, branch or worktree.
pub fn check(project: &Project, workflow: &Workflow, ticket: Option<&str>) -> Result<Report> {
    let mut report = Report {
        schema: 1,
        checks: vec![],
    };
    let temporary = tempfile::tempdir()?;
    let bots: BTreeSet<_> = workflow
        .steps
        .values()
        .filter_map(|s| s.bot.as_ref())
        .collect();
    for bot in bots {
        let probe = || -> Result<()> {
            let config = project
                .bots
                .get(bot)
                .with_context(|| format!("configure missing bot {bot} in project.bots"))?;
            let config = match config {
                BotConfig::Command(config) => config,
                BotConfig::HermesAcp(config) => {
                    return crate::agent::hermes_acp::probe(config, &project.repo, &project.env);
                }
                BotConfig::Hermes { .. } => bail!(
                    "bot {bot}: configure a command adapter with verified owned-worker cancellation; Hermes shared gateway supervision is unavailable"
                ),
            };
            let dir = tempfile::tempdir_in(temporary.path())?;
            let task = dir.path().join("task.md");
            fs::write(
                &task,
                format!(
                    "Doctor capability check. Reply briefly that you are ready. Do not change the repository or start other steps.\nWrite {} using this JSON schema: {{\"schema\":1,\"status\":\"done\",\"summary\":\"ready\",\"artifacts\":[],\"block\":null,\"verdict\":null,\"questions\":[]}}\n",
                    dir.path().join("result.json").display()
                ),
            )?;
            let argv = config
                .start
                .iter()
                .map(|a| substitute(a, &task, &project.repo, None))
                .collect::<Result<Vec<_>>>()?;
            let output = execute(
                project,
                &argv,
                if config.task_stdin { Some(&task) } else { None },
                30,
            )?;
            ensure!(
                protocol_observation(&config.protocol, &output).1,
                "bot {bot}: configure a working completion protocol; no successful completion event"
            );
            if matches!(config.protocol, Protocol::ResultFile) {
                let result: serde_json::Value = serde_json::from_reader(
                    File::open(dir.path().join("result.json"))
                        .context("bot must write result.json for the doctor probe")?,
                )?;
                ensure!(
                    result["schema"] == 1
                        && result["status"] == "done"
                        && result["summary"]
                            .as_str()
                            .is_some_and(|s| !s.trim().is_empty())
                        && result.get("block").is_some_and(|b| b.is_null()),
                    "bot {bot}: doctor result must report successful completion"
                );
            }
            Ok(())
        };
        report.check(format!("bot {bot}"), probe());
    }
    let required: BTreeSet<_> = workflow
        .steps
        .values()
        .flat_map(|s| &s.check)
        .filter_map(|check| {
            check.strip_prefix("cmd:").or(match check.as_str() {
                "lint" => Some("lint"),
                "tests_fail" => Some("test"),
                "project_check" => Some("project_check"),
                _ => None,
            })
        })
        .collect();
    for name in required {
        if !project.commands.contains_key(name) {
            report.check(
                format!("command {name}"),
                Err(anyhow::anyhow!(
                    "configure commands.{name}; required by workflow checks"
                )),
            );
        }
    }
    for (name, command) in &project.commands {
        report.check(
            format!("command {name}"),
            execute(project, &command.argv, None, command.timeout_seconds).map(|_| ()),
        );
    }
    report.check(
        "gh auth".into(),
        execute(
            project,
            &[
                "gh".into(),
                "auth".into(),
                "status".into(),
                "--hostname".into(),
                "github.com".into(),
            ],
            None,
            15,
        )
        .map(|_| ()),
    );
    report.check(
        format!("gh repository {}", project.code_host.repo),
        execute(
            project,
            &[
                "gh".into(),
                "repo".into(),
                "view".into(),
                project.code_host.repo.clone(),
                "--json".into(),
                "nameWithOwner".into(),
            ],
            None,
            15,
        )
        .map(|_| ()),
    );
    let auth = || -> Result<()> {
        let mut tracker = project.tracker(
            temporary.path().join("tracker"),
            ticket.unwrap_or("doctor"),
            crate::templates::Templates::load(None, None)?,
        )?;
        tracker.authenticate()?;
        if ticket.is_some() {
            tracker.read_issue()?;
        }
        Ok(())
    };
    report.check("tracker auth".into(), auth());
    Ok(report)
}

pub(crate) fn executable(project: &Project, program: &str) -> Result<PathBuf> {
    let candidates = if Path::new(program).is_absolute() || program.contains('/') {
        vec![project.repo.join(program)]
    } else {
        std::env::split_paths(project.env.get("PATH").map(String::as_str).unwrap_or(""))
            .map(|p| project.repo.join(p).join(program))
            .collect()
    };
    for path in candidates {
        if path
            .metadata()
            .is_ok_and(|m| m.is_file() && m.permissions().mode() & 0o111 != 0)
        {
            return fs::canonicalize(path).map_err(Into::into);
        }
    }
    bail!(
        "{program} not found or not executable in configured PATH; install it or fix project.env.PATH"
    )
}

/// Bound output and wall time, with an owned process group even on failure.
/// Child output is never embedded in errors: it may contain credentials.
pub(crate) fn execute(
    project: &Project,
    argv: &[String],
    input: Option<&Path>,
    timeout: u64,
) -> Result<String> {
    let program = argv.first().context("configure nonempty command argv")?;
    ensure!(timeout > 0, "command timeout must be positive");
    let stdout = tempfile::NamedTempFile::new()?;
    let stderr = tempfile::NamedTempFile::new()?;
    let mut command = Command::new(executable(project, program)?);
    command
        .args(&argv[1..])
        .current_dir(&project.repo)
        .env_clear()
        .envs(&project.env)
        .stdin(match input {
            Some(path) => Stdio::from(File::open(path)?),
            None => Stdio::null(),
        })
        .stdout(stdout.reopen()?)
        .stderr(stderr.reopen()?)
        .process_group(0);
    if program == "gh" {
        command.env("GH_HOST", "github.com");
    }
    let mut owned = OwnedChild::new(command.spawn().context("start configured command")?);
    let started = Instant::now();
    while !owned.exited()? {
        ensure!(
            started.elapsed() < Duration::from_secs(timeout),
            "command timed out; fix command or timeout"
        );
        ensure!(
            stdout.as_file().metadata()?.len() <= 2 * 1024 * 1024
                && stderr.as_file().metadata()?.len() <= 2 * 1024 * 1024,
            "command output exceeded limit"
        );
        thread::sleep(Duration::from_millis(10));
    }
    let status = owned.finish()?;
    ensure!(
        status.success(),
        "command failed (exit {:?}); fix the named capability under project.env",
        status.code()
    );
    let mut text = String::new();
    File::open(stdout.path())?
        .take(2 * 1024 * 1024 + 1)
        .read_to_string(&mut text)?;
    ensure!(
        text.len() <= 2 * 1024 * 1024,
        "command output exceeded limit"
    );
    Ok(text)
}
