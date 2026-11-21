# Nightly check workflow

Build the native Rust page-table check and simulate each branch from the repository root.

```sh
cargo xtask workflow-example
cargo run --locked -- workflow validate target/examples/workflow/graph.yaml
cargo run --locked -- workflow simulate target/examples/workflow/graph.yaml --fixture examples/workflow/fixtures/passed.json
cargo run --locked -- workflow simulate target/examples/workflow/graph.yaml --fixture examples/workflow/fixtures/failed.json
cargo run --locked -- workflow simulate target/examples/workflow/graph.yaml --fixture examples/workflow/fixtures/script-error.json
```

`passed.json` goes straight to the report. `failed.json` represents a check that ran and found a problem. It takes the triage and approval route. `script-error.json` represents a runner failure and activates the explicit error handler. The trace includes the resolved inputs, outputs, outcome, and reason for every node. Compilation reads the script and ELF, and simulation takes external step results from the fixture.

## Definition schema

- `mcbtodo.workflow/v1` requires `id`, `name`, `entry`, and a `nodes` map. `edges`, `bindings`, `literals`, and workflow-level `capabilities` default to empty. Every node must be reachable from the entry through an acyclic control graph.
- Script and agent nodes declare input and output fields inline with `boolean`, `integer` (signed 64-bit), or `string` types. All fields are required. A binding names an upstream output, and a literal supplies a scalar directly. Each input gets exactly one value. A binding is valid when every control path to the consumer passes through an edge that supplies the producer's successful output.
- A condition compares its `value` input to `equals` and requires both `"true"` and `"false"` edges. Approval requires `approved` and `rejected` edges. Actions, scripts, and agents use `success` (the default) or `failure`. `report.create` accepts `summary`. `task.create` requires `project_id` and `title`.
- A script declares its runner, a relative source path, a positive timeout, and its scoped capabilities. The graph declares exactly the union of script capabilities and the agents' `agent.invoke` scopes. A script can declare `process.exec` once and scope it to a graph-relative native ELF. The daemon requires a grant for the node and exact publication, then exposes the pinned executable read-only via `PROD_EXECUTABLE`. Use a statically linked ELF with no additional runtime files. A draft may contain up to 2 MiB of scripts and executables, with at most 1 MiB of scripts.
- The fixture schema `mcbtodo.workflow-fixture/v1` maps node IDs to `succeeded`, `failed`, `approved`, or `rejected` results. Conditions evaluate their typed input. All other active nodes require a matching result. A failed result needs a message. All supplied results are validated against their node contracts, including results for skipped nodes.
- At joins the simulator waits for all active incoming branches in topological order. It skips unchosen branches and blocks successors of an unhandled failure. An explicit failure edge activates the handler and deactivates the failed node's normal success edges.

For an isolated run, publish the generated graph and authorize the `test` node by digest. To build a distinct publication with an injected page-offset failure, run `cargo xtask workflow-example --inject-failure --out-dir target/examples/workflow-fail`. Configure an Agent adapter for that publication and load the Rust OS project from `examples/vault` in the default Vault to review the proposed follow-up. `cargo xtask smoke --sandbox` exercises both publications in a temporary workspace with a deterministic test adapter; it is an integration fixture, not a production Agent service. This path is Linux-specific and requires the sandbox prerequisites described below. [Agent adapter](../../docs/agent-adapter.md) · [Workspace guide](../../README.md#工作流)

## Acceptance coverage

Build the workspace binaries with `cargo build --locked --workspace`. Run `cargo xtask smoke` for the shared checks and `cargo xtask smoke --sandbox` for the full fixture path, which requires the Linux sandbox and Nushell. The rows follow [Design §23](../../docs/design.md#23-一个贯穿全系统的端到端验收案例).

| Step | Smoke coverage |
| --- | --- |
| 1 | Calendar queries find the Rust OS activity period and deadline on their fixture dates with `busy: false`. |
| 2 | MCP lists the project and submits four WorkUnits with estimates and acceptance checklists. |
| 3 | Graph Diff checks both parallel branches and their join. TUI approval publishes all four WorkUnits. Rejected and stale proposals leave the WorkUnits absent. |
| 4 | The TUI creates the mapping TimeBlock. The calendar query checks the busy 09:00–11:00 Asia/Shanghai entry on November 20, 2026. |
| 5 | The CLI starts a timer for the mapping WorkUnit. The TUI reads its running state, then exits. The same session accrues time before the CLI pauses it. |
| 6 | The sandbox path binds the published workflow to the Test WorkUnit and verifies refusal before script authorization. Editing the draft in Helix is manual. |
| 7 | The smoke inspects the daily 23:00 Asia/Tokyo Cron rule and its `run_once` policy. The sandbox path removes the draft executable after publishing to verify execution uses the pinned snapshot. This tests the current Daemon execution path, not the planned independent Worker boundary. |
| 8 | A near-term one-time schedule runs the pinned check in the sandbox and the smoke reads the passing TestResult and captured log. The nightly Cron assertion checks configuration without waiting for its due time. |
| 9 | The injected failure produces a failed TestResult and log. The mock adapter receives only the diagnostic summary and returns a follow-up title. The smoke checks denial without an Agent grant and compares the fixture source and Test WorkUnit bytes before and after both review outcomes. |
| 10 | The reopened TUI shows the TimeBlock, the blocked Test WorkUnit in Graph, and the failed Workflow trace. It rejects one follow-up and approves another. Step 3 also exercises the AI proposal panel. |
| 11 | Explicit completion links the passing step from the bound revision and records one completion event with that Evidence. The failed step comes from a separate publication and remains queryable after backup and restore. The focus report checks the mapping session's paused duration in JSON and its WorkUnit in Markdown. |
| 12 | The smoke kills the daemon after a sandbox-local marker is written and before a step is recorded. Restart and retry with the same run ID both preserve `outcome_unknown` and zero recorded steps. |
