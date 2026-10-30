//! Thin terminal presentation of the shared workflow control API.
use crate::cli::{DoctorArgs, FlowRunArgs, FlowStatusArgs};
use anyhow::Result;
use hermon_flow::{control, intake, workflow::Workflow};
use std::{
    path::Path,
    time::{SystemTime, UNIX_EPOCH},
};

pub fn hub(args: crate::cli::HubArgs) -> Result<()> {
    let mut host =
        hermon_flow::host::Host::acquire(&args.data.data_dir, &std::env::current_exe()?)?
            .ok_or_else(|| {
                anyhow::anyhow!("hub already running for {}", args.data.data_dir.display())
            })?
            .embedded_worker();
    let notifier = crate::notify::probe();
    let mut alerts = crate::flow_ui::FlowAlerts::default();
    let runtime = crate::arbitration::runtime_dir();
    let flag = if args.no_notify {
        crate::arbitration::NotifyFlag::ForceOff
    } else {
        crate::arbitration::NotifyFlag::Default
    };
    // The terminal host shares watch's notifier priority. Recheck each tick so
    // a menubar started later can take over banners while this host keeps runs
    // moving. The claim also expires automatically if this process crashes.
    let _claim = runtime
        .as_ref()
        .filter(|_| !args.no_notify)
        .and_then(|dir| crate::arbitration::claim(dir, crate::arbitration::UiKind::Watch).ok());
    loop {
        let decision =
            crate::arbitration::decide(runtime.as_deref(), crate::arbitration::UiKind::Watch, flag);
        let cfg = if decision.notify {
            crate::notify::NotifyCfg::default()
        } else {
            crate::notify::NotifyCfg::default().silenced()
        };
        for record in host.tick()? {
            for alert in alerts.poll(&record, hermon_flow::host::now(), &cfg, false)? {
                crate::notify::deliver(&notifier, "hermon", &alert.label, &alert.detail);
            }
        }
        if args.once {
            return Ok(());
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

fn workflow_path(path: &Path) -> std::path::PathBuf {
    if path.is_dir() {
        path.join("workflow.yaml")
    } else {
        path.to_owned()
    }
}
pub fn intake(args: FlowRunArgs) -> Result<()> {
    let id = args.run_id.unwrap_or_else(|| {
        format!(
            "run-{}",
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
        )
    });
    let record = intake::create(intake::Request {
        workflow: &workflow_path(&args.workflow),
        project: &args.project,
        data_dir: &args.data.data_dir,
        ticket: &args.ticket,
        run_id: &id,
        base: args.base.as_deref(),
        start: args.start.as_deref(),
        confirmed: args.confirm,
    })?;
    println!(
        "Created {}: {} on {} (step {}).",
        record.run.id, record.workflow_name, record.base, record.run.current
    );
    println!("Worktree: {}", record.worktree.display());
    Ok(())
}
pub fn doctor(args: DoctorArgs) -> Result<()> {
    let project = intake::Project::load(&args.project)?;
    let path = workflow_path(&args.workflow);
    let workflow = Workflow::load(&path)?;
    intake::templates(&project, &workflow, &path)?;
    let report = hermon_flow::doctor::check(&project, &workflow, args.ticket.as_deref())?;
    if args.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for check in &report.checks {
            if check.ok {
                println!("OK {}", check.name);
            } else {
                println!(
                    "FAIL {}: {}",
                    check.name,
                    check.fix.as_deref().unwrap_or("capability missing")
                );
            }
        }
    }
    report.require_pass()
}
pub fn status(args: FlowStatusArgs) -> Result<()> {
    let runs = control::status(&args.data.data_dir, args.run.as_deref())?;
    if args.json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({"schema":1,"runs":runs}))?
        );
    } else {
        for record in runs {
            let state = if record.cancelled {
                "cancelled".into()
            } else if record.paused {
                "paused".into()
            } else {
                format!("{:?}", record.run.status)
            };
            println!(
                "{}  {}  {}  {}",
                record.run.id, record.workflow_name, record.run.current, state
            );
            if let Some(block) = record.run.block {
                println!("  {}", block.reason);
            }
        }
    }
    Ok(())
}
pub fn control(data: &Path, id: &str, action: control::Action) -> Result<()> {
    let record = control::apply(data, id, action)?;
    println!(
        "{}: {} ({:?})",
        record.run.id, record.run.current, record.run.status
    );
    Ok(())
}
