# Delivery and Completion Workflow

Use this workflow at the end of every architecture or implementation slice and
before saying that a phase is complete. Completion requires fresh evidence. A
previous agent report, an old CI result, a successful compile, or a type
 declaration does not prove the required behavior.

## Execution Loop

1. Inspect the current diff and identify the exact requested slice.
2. List the business invariants and acceptance criteria before editing.
3. Implement the smallest change with a real caller and a clear owner.
4. Run focused tests that exercise the original behavior and the new boundary.
5. Run the applicable workspace, fixture, smoke, and packaging checks.
6. Review the diff for redundant logic, unused abstractions, leaked concrete
   types, stale documentation, security regressions, and unrelated churn.
7. Reconcile the capability matrix and dated architecture assessment.
8. Re-read the acceptance criteria and report every unverified item.

## Required Baseline Checks

Unless the change is explicitly narrower and the reason is recorded, run:

```sh
cargo fmt --all -- --check
git diff --check
cargo xtask agent validate
cargo xtask check
cargo run --locked -- --vault examples/vault --timezone Asia/Shanghai validate
```

For release or package changes also run the release workflow. For process,
execution, sandbox, Agent, or scheduling changes run the relevant focused smoke
and systemd checks. For TUI changes run the terminal-size and visual checks. For
platform claims run native or CI checks on that platform. Never list a command
as passed unless it ran in the current verification cycle.

## Capability Labels

Use exactly these labels and state their evidence scope.

- `Implemented`
- `Verified`
- `Partially Implemented`
- `Planned`
- `Blocked`
- `Unsupported`

Keep `Supported`, `Available`, `Unavailable`, and `Degraded` for platform
capability state. Do not describe macOS or Windows runtime support from a Linux
cross-build. Do not describe ResourceScheduler, a process plugin protocol,
Managed Codex distribution, or cross-device synchronization as implemented until
their prerequisites and end-to-end tests exist.

## A–G Delivery Report

Every completed slice reports:

### A. Completed Work

State the source behavior and architecture that changed.

### B. Changed Files

List new, modified, moved, or removed files and their purpose. Mention any
pre-existing user files deliberately left untouched.

### C. Invariants Preserved

Explain evidence for Vault revisions and conflicts, Daemon and Worker lifecycle,
Workflow Attempt transitions, Agent Policy, idempotency, backup bytes, and data
authority as applicable.

### D. Validation

List exact commands, focused tests, exit status, and relevant result counts.
List checks that could not run and why.

### E. Capability Status

Classify every affected capability with the labels above and state platform or
runtime scope.

### F. Remaining Risks

Record platform differences, transaction guarantees, security degradation,
Agent protocol compatibility, configuration or data migration, performance,
maintenance cost, and unverified external effects.

### G. Next Milestone

State the next engineering goal, prerequisites, concrete tasks, and acceptance
commands. Do not leave a vague TODO in place of a bounded milestone.

## Final Repository Check

Inspect `git status --short`, `git diff --stat`, the complete relevant diff, and
all user-data paths. Do not stage, commit, publish, or push unless the user has
explicitly authorized that action in the current task. Preserve the Apache-2.0
license and third-party notices.
