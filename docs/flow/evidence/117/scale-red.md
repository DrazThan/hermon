# Supplemental graph-scale RED and freeze V3

Parent code review identified recursive cycle checking over the workflow graph. YAML nesting limits do not bound a long, shallow chain of steps. Before the graph implementation fix, the independent test writer added `flow_workflow_large_linear_graph_loads` in a separate integration test file. It loads a valid 6,000-step linear graph in a child test process, asserting successful loading; the child contains a possible stack-overflow abort so it does not kill the parent harness.

On 2026-10-29, `cargo test -p hermon-flow --test workflow_scale --locked` compiled successfully, then exited **101**: **1 failed, 0 passed**. The child aborted with **SIGABRT**, reporting **stack overflow** while loading the long graph with the original recursive cycle validator. Exact output: [red-scale.log](red-scale.log). This is a behavioral failure, not a compiler failure or synthetic error.

The supplemental test is frozen before the iterative graph fix. [frozen-v3.sha256](frozen-v3.sha256) supersedes V2 by adding only `workflow_scale.rs` to its unchanged four files. V2 lint-only correction evidence and V1 RED evidence remain intact. Verify the current five-file acceptance suite from repository root with:

```sh
shasum -a 256 -c docs/flow/evidence/117/frozen-v3.sha256
```
