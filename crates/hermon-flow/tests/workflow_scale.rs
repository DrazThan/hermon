use hermon_flow::workflow::Workflow;
use std::{fmt::Write, process::Command};

#[test]
fn flow_workflow_large_linear_graph_loads() {
    const CHILD: &str = "HERMON_117_LINEAR_GRAPH_CHILD";
    if std::env::var_os(CHILD).is_some() {
        let mut yaml = String::from("version: 1\nname: long-linear\nsteps:\n");
        for index in 0..6_000 {
            if index + 1 < 6_000 {
                writeln!(
                    yaml,
                    "  step_{index:05}: {{bot: worker, next: step_{:05}}}",
                    index + 1
                )
                .unwrap();
            } else {
                writeln!(yaml, "  step_{index:05}: {{bot: worker}}").unwrap();
            }
        }
        let workflow = Workflow::from_str("linear/workflow.yaml", &yaml).unwrap();
        assert_eq!(workflow.steps.len(), 6_000);
        return;
    }

    // Isolate a possible stack-overflow abort so the integration test reports a
    // normal assertion failure instead of terminating the whole test harness.
    let output = Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "flow_workflow_large_linear_graph_loads"])
        .env(CHILD, "1")
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "large acyclic workflow must load: status={}\nstdout={}\nstderr={}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
