//! Shared workflow presentation, exact session navigation and desktop alerts.
use crate::{
    bots::routing_key,
    notify::{Alert, AlertKind, NotifyCfg},
    roster::RosterRow,
};
use anyhow::Result;
pub use hermon_flow::host::HubLock;
use hermon_flow::{control::RunRecord, hub::Status, workflow::Workflow};
use std::{
    collections::{BTreeMap, HashMap},
    fs,
    path::{Path, PathBuf},
    sync::mpsc,
    thread,
    time::Duration,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PipelineRow {
    pub run_id: String,
    pub step: String,
    pub bot: Option<String>,
    pub attempt: u32,
    pub age_secs: Option<u64>,
    pub state: String,
    pub last_block: Option<String>,
    pub session_id: Option<String>,
}
impl PipelineRow {
    pub fn label(&self) -> String {
        let age = self
            .age_secs
            .map(|s| format!("{}m {}s", s / 60, s % 60))
            .unwrap_or_else(|| "—".into());
        crate::render::sanitize(&format!(
            "{} / {} · {} · #{} · {} · {}{}",
            self.run_id,
            self.step,
            self.bot.as_deref().unwrap_or("owner/action"),
            self.attempt,
            age,
            self.state,
            self.last_block
                .as_ref()
                .map(|b| format!(" · {b}"))
                .unwrap_or_default()
        ))
    }
}
pub fn pipeline_rows(record: &RunRecord, now: u64) -> Result<Vec<PipelineRow>> {
    let workflow = Workflow::from_str("pipeline workflow", &record.workflow_source)?;
    // Follow the normal pipeline from intake; include routed/otherwise unvisited
    // steps afterwards so alternate branches and future work remain visible.
    let start = record
        .run
        .events
        .iter()
        .find(|e| e.kind == "intake")
        .map(|e| e.step.as_str())
        .or_else(|| record.run.events.first().map(|e| e.step.as_str()))
        .unwrap_or(&record.run.current);
    let mut names = vec![];
    let mut next = Some(start);
    while let Some(name) = next {
        if names.iter().any(|n| n == name) {
            break;
        }
        names.push(name.to_string());
        next = workflow.steps.get(name).and_then(|s| s.next.as_deref());
    }
    for name in workflow.steps.keys() {
        if !names.contains(name) {
            names.push(name.clone());
        }
    }
    names
        .into_iter()
        .filter_map(|name| workflow.steps.get(&name).map(|step| (name, step)))
        .map(|(name, step)| {
            let attempt = record.run.attempts.iter().rev().find(|a| a.step == name);
            let current = record.run.current == name;
            let finished = record.run.events.iter().rev().find(|e| {
                e.step == name
                    && matches!(
                        e.kind.as_str(),
                        "step_done" | "gate_passed" | "override" | "started" | "gate_opened"
                    )
            });
            let state = if current {
                if record.cancelled || record.run.cancelled {
                    "cancelled".into()
                } else if record.paused || record.run.paused {
                    "paused".into()
                } else {
                    format!("{:?}", record.run.status).to_lowercase()
                }
            } else if finished.is_some_and(|e| {
                matches!(e.kind.as_str(), "step_done" | "gate_passed" | "override")
            }) {
                "done".into()
            } else {
                "pending".into()
            };
            let last_block = record
                .run
                .events
                .iter()
                .rev()
                .find(|e| {
                    e.step == name
                        && matches!(
                            e.kind.as_str(),
                            "block" | "blocked" | "host_error" | "timeout"
                        )
                })
                .map(|e| e.reason.clone())
                .or_else(|| {
                    current
                        .then(|| record.run.block.as_ref().map(|b| b.reason.clone()))
                        .flatten()
                });
            let session_id = if current && state == "running" {
                record
                    .run
                    .active
                    .as_ref()
                    .filter(|a| a.step == name)
                    .and_then(|a| {
                        // Receipt creation trails the start event; missing metadata is
                        // expected and never grounds for guessing a session ID.
                        let value: serde_json::Value = serde_json::from_slice(
                            &fs::read(Path::new(&a.handle).join("process.json")).ok()?,
                        )
                        .ok()?;
                        value["attempt"]["session_id"]
                            .as_str()
                            .filter(|s| !s.is_empty())
                            .map(str::to_owned)
                    })
            } else {
                None
            };
            Ok(PipelineRow {
                run_id: record.run.id.clone(),
                step: name,
                bot: step.bot.clone(),
                attempt: attempt.map(|a| a.number).unwrap_or(0),
                age_secs: attempt.map(|a| now.saturating_sub(a.started_at)),
                state,
                last_block,
                session_id,
            })
        })
        .collect()
}
/// No shortened display key, PID, model or title can authorize navigation.
pub fn open_session(row: &PipelineRow, roster: &[RosterRow]) -> Option<String> {
    if row.state != "running" {
        return None;
    }
    let session = row.session_id.as_deref()?;
    let qualified = session.starts_with("hr1:");
    if qualified && crate::bots::Route::decode(session).is_none_or(|r| !r.origin.is_empty()) {
        return None;
    }
    let mut matches = roster.iter().filter(|row| {
        if row.id.starts_with("hr1:") {
            crate::bots::Route::decode(&row.id).is_some_and(|route| {
                route.origin.is_empty()
                    && if qualified {
                        row.id == session
                    } else {
                        route.vendor == session.as_bytes()
                    }
            })
        } else {
            !qualified && row.id == session
        }
    });
    let matched = matches.next()?;
    if matches.next().is_some() {
        return None;
    }
    Some(routing_key(matched).to_owned())
}

#[derive(Default)]
pub struct FlowAlerts {
    conditions: HashMap<String, String>,
}
impl FlowAlerts {
    pub fn poll(
        &mut self,
        record: &RunRecord,
        now: u64,
        cfg: &NotifyCfg,
        muted: bool,
    ) -> Result<Vec<Alert>> {
        let run = &record.run;
        let condition = if record.cancelled
            || run.cancelled
            || record.paused
            || run.paused
            || run.status == Status::Done
        {
            None
        } else if let Some(error) = run.events.last().filter(|e| e.kind == "host_error") {
            Some((
                AlertKind::PermWait,
                error.reason.clone(),
                format!("host:{}", error.reason),
            ))
        } else if matches!(run.status, Status::Gate | Status::Blocked) {
            let reason = run
                .block
                .as_ref()
                .map(|b| b.reason.clone())
                .unwrap_or_else(|| "Owner approval required".into());
            let generation = run
                .events
                .iter()
                .filter(|e| {
                    e.step == run.current
                        && matches!(
                            e.kind.as_str(),
                            "blocked" | "block" | "gate_opened" | "retry" | "answer" | "approve"
                        )
                })
                .count();
            Some((
                AlertKind::PermWait,
                reason.clone(),
                format!(
                    "owner:{}:{generation}:{:?}:{reason}",
                    run.current,
                    run.gate_requests.get(&run.current)
                ),
            ))
        } else if run.status == Status::Running {
            let workflow = Workflow::from_str("alert workflow", &record.workflow_source)?;
            run.active
                .as_ref()
                .filter(|a| {
                    now.saturating_sub(a.started_at)
                        >= u64::from(workflow.limits.stall_alert_minutes) * 60
                })
                .map(|a| {
                    (
                        AlertKind::Stuck,
                        "Attempt stalled; worker is still running".into(),
                        format!("stall:{}:{}:{}", a.step, a.number, a.started_at),
                    )
                })
        } else {
            None
        };
        let Some((kind, reason, key)) = condition else {
            self.conditions.remove(&run.id);
            return Ok(vec![]);
        };
        if muted
            || (kind == AlertKind::Stuck && !cfg.stuck)
            || (kind == AlertKind::PermWait && !cfg.perm_wait)
            || self.conditions.get(&run.id) == Some(&key)
        {
            return Ok(vec![]);
        }
        self.conditions.insert(run.id.clone(), key);
        Ok(vec![Alert {
            key: format!("flow:{}:{}", run.id, run.current),
            label: format!("{} / {}", run.id, run.current),
            kind,
            detail: reason,
        }])
    }
}

/// Runs checks and network operations away from the session engine. Shutdown
/// requests stop dispatch after the current reconciliation; workers retain their
/// own supervision and deadlines if the application exits.
pub(crate) struct HostThread {
    stop: mpsc::Sender<()>,
    handle: Option<thread::JoinHandle<()>>,
}
impl HostThread {
    pub fn start(data: &Path) -> Result<Option<Self>> {
        let Some(mut host) = hermon_flow::host::Host::acquire(data, &std::env::current_exe()?)?
        else {
            return Ok(None);
        };
        host = host.embedded_worker();
        let (stop, rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            loop {
                if rx.try_recv().is_ok() {
                    break;
                }
                if let Err(error) = host.tick() {
                    eprintln!("hermon hub: {error:#}");
                }
                if !matches!(
                    rx.recv_timeout(Duration::from_secs(1)),
                    Err(mpsc::RecvTimeoutError::Timeout)
                ) {
                    break;
                }
            }
        });
        Ok(Some(Self {
            stop,
            handle: Some(handle),
        }))
    }
}
impl Drop for HostThread {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if self.handle.as_ref().is_some_and(|h| h.is_finished()) {
            let _ = self.handle.take().unwrap().join();
        }
    }
}
pub(crate) struct FlowFeed {
    data: PathBuf,
    records: BTreeMap<String, RunRecord>,
    alerts: FlowAlerts,
    _host: Option<HostThread>,
}
impl FlowFeed {
    pub fn new(data: PathBuf, host: bool) -> Self {
        let hosting = if host {
            match HostThread::start(&data) {
                Ok(Some(host)) => Some(host),
                Ok(None) => {
                    eprintln!(
                        "hermon: hub already running for {}; pipeline monitoring remains available",
                        data.display()
                    );
                    None
                }
                Err(e) => {
                    eprintln!("hermon: cannot host hub: {e:#}");
                    None
                }
            }
        } else {
            None
        };
        Self {
            data,
            records: BTreeMap::new(),
            alerts: FlowAlerts::default(),
            _host: hosting,
        }
    }
    pub fn poll(
        &mut self,
        now: u64,
        cfg: &NotifyCfg,
        muted: bool,
    ) -> Result<(Vec<PipelineRow>, Vec<Alert>)> {
        for record in hermon_flow::host::snapshots(&self.data)? {
            self.records.insert(record.run.id.clone(), record);
        }
        self.records
            .retain(|id, _| self.data.join("runs").join(id).join("run.json").is_file());
        let mut rows = vec![];
        let mut alerts = vec![];
        for record in self.records.values() {
            rows.extend(pipeline_rows(record, now)?);
            alerts.extend(self.alerts.poll(record, now, cfg, muted)?);
        }
        Ok((rows, alerts))
    }
}
