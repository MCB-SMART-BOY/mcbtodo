# Architecture Acceptance Assessment

Assessment baseline: 2026-11-21. This records what the checked-in implementation
can demonstrate, not what the product design intends to provide. The dated
evidence is a snapshot and must be refreshed when acceptance checks change;
documentation ownership and synchronization rules are in
[`docs/README.md`](README.md).

On 2026-11-21, `cargo xtask verify-crate-packages` was added to Linux CI and
release preparation. It reads publishable workspace members from Cargo
metadata, requires the `mcbtodo` package naming convention, `Apache-2.0`
metadata and `license-file` metadata, then checks each Cargo package listing
for a `LICENSE` archive entry. This automates the license-presence gate only;
package-content, third-party notice, repository metadata, and provenance
reviews remain required. The new command was locally validated on the complete
workspace; no GitHub-hosted workflow run is claimed.

On 2026-11-21, verification ran `cargo fmt --all -- --check`,
`cargo xtask agent validate`, `cargo xtask check`,
`cargo run --locked -- --vault examples/vault --timezone Asia/Shanghai
validate`, and `cargo xtask smoke --bin-dir target/debug --sandbox`; all
passed. A freshly built AArch64 release archive was extracted to a temporary
directory and its binaries passed `cargo xtask smoke --bin-dir
<extracted>/usr/bin --sandbox`, including the packaged Worker nonzero-exit
case. The workspace check includes the focused Linux production-cancellation
regression and the full workspace tests, Clippy, and doctests. The sandbox smoke
also verifies sampled Worker descendants exit after forced Daemon termination.
This confirms
the registered harness/workflow context, workspace checks, and fixture/sandbox
validation for this checkout. It does not close the remaining independently
supervised Worker failure-acceptance gaps in M5 or establish third-party
provenance compatibility or release authorization. The malformed-readiness
smoke runs the supplied Daemon binary with a fault-injection Worker substitute
and verifies fail-closed handling, zero step commits, and Worker reaping; it
does not test malformed output from the packaged Worker binary. Combined
packaged-Worker fault cases remain unverified.

On 2026-11-21, the Worker process integration test and `cargo xtask smoke`
also send a malformed Prepare frame directly to the Worker executable selected
from the target or extracted release directory. Both require a nonzero exit
and no readiness bytes on stdout. This covers the actual Worker binary's
rejection of malformed supervisor input. It does not make the Worker emit
malformed output, test Daemon supervision of a faulty packaged Worker, or
cover combined process faults.

## Milestones

| Milestone | Status | Evidence | Remaining acceptance gap |
| --- | --- | --- | --- |
| M0: domain and temporal rules | Partial | `crates/domain/src/{time,recurrence,graph}.rs`; `time::parse_date` is the shared strict `YYYY-MM-DD` parser used by CLI and MCP; domain and adapter boundary tests | The core types and DAG exist, but there is no single acceptance suite showing all serialized business objects and cross-timezone boundary cases together. |
| M1: Markdown Vault and CLI | Partial | `crates/vault/src/{lib.rs,persistence.rs,watch.rs}`; `crates/application/src/lib.rs`; `apps/mcbtodo/tests/cli.rs`; `apps/mcbtodo/src/tui/watch_tests.rs` | Loading, validation, atomic writes, CLI commands, and shared debounced Vault watching for TUI and daemon exist. The daemon retains its last valid Vault snapshot, derives owned reminder definitions from it, and rejects invalid external edits without discarding either snapshot or reminder definitions. Project creation, Task/Inbox capture, deadline updates, time-block/reminder creation, WorkUnit creation/completion, workflow binding, Inbox-to-Task conversion, revision-checked recurring Task materialization, and atomic iCalendar event import use application commands. Inbox conversion publishes a deterministic Task before marking the retained Inbox source; retries verify exact content and reject conflicting edits. Focus report metadata is now read from one application snapshot. A general daemon query/index service is not implemented; other reads still load Markdown directly. |
| M2: daemon, IPC, and time engine | Partial | `apps/mcbtodo-daemon/src/`; `crates/protocol/src/`; systemd deployment and smoke task | Focus persistence/recovery, notifications, bounded completion event reads, IPC, and daemon-owned Vault change validation exist. Focused coverage verifies valid external changes replace the cached snapshot and invalid Markdown preserves the last valid snapshot. Workflow Task creation obtains its Project revision through the shared application query and writes through the shared application command; Vault rechecks that token under the writer lock. Reminder Inbox publication and desktop delivery use a durable outbox with bounded exponential retry and restart recovery; delivery is at-least-once across a crash after the helper side effect, and occurrences older than the 24-hour scan horizon are intentionally not backfilled. Extended-running and multi-session delivery acceptance remains open. The completion poller's direct Vault event reads are storage-facing infrastructure, not unshared business writes. |
| M3: calendar-first TUI | Partial | `apps/mcbtodo/src/tui/`; `crates/platform/tests/workspace_paths.rs`; calendar, Inbox triage, and interaction tests | Calendar projection, key-driven creation, Inbox-to-Task conversion, graph/timeline views, and configurable Month/restore-last-view startup exist. Config validation covers the policy; private state storage tests cover bounded atomic writes, path confinement, symlink rejection, ownership and permissions. `views_menu_covers_calendar_and_planning_views_without_losing_date_cursor` exercises all nine `Space u` menu destinations and asserts the date cursor is retained; `week_continuation_hour_opens_the_same_multiday_occurrence_in_day_view` verifies a multi-day timed occurrence remains focused when opened from its continuation hour; `agenda_focus_preserves_a_multiday_all_day_occurrence_from_a_continuation_day` verifies a multi-day all-day occurrence remains focused from a non-start day through Day view and Vault reload; `recurring_occurrence_focus_survives_month_and_week_intermediates_for_multiday_agenda` verifies exact recurring-occurrence restoration through Month and Week while retaining the selected Agenda day. Saved-view names are allowlisted. Full calendar interaction acceptance and an interactive end-user session remain unverified. |
| M4: task graph and workflow compiler | Partial | `crates/domain/src/graph.rs`; `crates/workflow/src/`; `xtask/src/smoke.rs` | DAG validation, typed compilation, publication, and simulation exist. The Linux sandbox smoke exercises a four-node Graph Diff and a published workflow end to end; large-graph rendering and broader graph-scale behavior remain unproven. |
| M5: durable scheduler and script runner | Partial | `apps/mcbtodo-daemon/src/{run_store,runtime}.rs`; `apps/mcbtodo-daemon/src/runtime_tests.rs::{daemon_supervises_worker_process_and_persists_its_result,worker_spawn_failure_fails_the_claimed_run_and_invalidates_its_attempt,malformed_worker_readiness_frame_fails_closed_in_production_supervisor,mismatched_worker_result_after_start_becomes_unknown_without_replay,terminating_worker_kills_its_process_group_and_reaps_the_worker,cancelling_running_script_through_daemon_reaps_worker_and_records_unknown,worker_crash_after_start_becomes_unknown_and_is_not_replayed_after_runtime_restart,result_commit_failure_becomes_unknown_without_replay_after_restart}`; `crates/platform/src/process.rs`; `apps/mcbtodo-worker/tests/process_protocol.rs`; RunStore attempt tests; `apps/mcbtodo/src/workflow_runtime_cli.rs`; `crates/workflow/src/schedule.rs`; `xtask/src/smoke.rs`; Worker v1 contract in `docs/design.md` §11.3 | Durable scheduling, recovery, authorization, sandbox execution, and one-time operator resolution of `outcome_unknown` runs exist. The production Daemon starts one `mcbtodo-worker` process for script/Agent attempts, sends revision-bound source bytes, persists Started before Start, checks returned attempt identity, waits for successful child exit, and commits results through the Daemon-owned RunStore transaction. Process startup failures invalidate the claimed attempt and fail the current run; focused regressions cover process-spawn failure and a malformed readiness frame through the production supervisor, with no step commit. The mismatched Result regression emits a valid but mismatched frame from a fixture subprocess, passes it through the production `read_worker_messages` decoder and Started `poll_worker` path, then verifies process-group termination/reap, `outcome_unknown`, no step commit, and no replay after reopening the RunStore. A freshly extracted release archive now passes a sandbox smoke where an `exit 23` script is run through its packaged Worker; the Daemon persists one failed step and a trace reason containing the exit status. This does not exercise malformed frames or combined faults in the packaged Worker. Linux coverage also exercises production Daemon cancellation of a live sandbox script and SIGKILL of a Worker after Started. Worker startup arms a Linux parent-death signal and validates the expected Daemon PID; the sandbox smoke force-kills the Daemon after a script side effect and verifies sampled Worker descendants exit, then verifies restart recovery to `outcome_unknown` without replay. This process cleanup does not undo or determine the outcome of external side effects. Non-ESRCH process-group kill errors are surfaced. The store persists monotonic epochs and exact run/revision/node/step binding, and restart invalidates unresolved attempts. A database epoch is not an expiring Worker lease and cannot undo external effects. M5 remains partial: packaged-Worker protocol/fault combinations and external side-effect reconciliation are not accepted; reconciliation remains integration-specific. Result transaction failure and no replay, uncertain outcomes, and interruption state recovery have focused coverage. Distributed Worker clusters remain deferred. |
| M6: MCP and AI operations | Partial | `apps/mcbtodo/src/mcp/`; `proposal.rs`; `crates/platform/src/agent_policy.rs`; `crates/application/src/lib.rs`; `xtask/src/smoke.rs`; representative adapter matrix below | Typed tools, Vault-bound proposal approval, revisions, and policy parsing exist. CLI and MCP share application query projections; TUI uses the same projection rules over its already-loaded snapshot. Exposed writes share `VaultCommands`; MCP routes writes through human-reviewed proposals, unlike direct local CLI/TUI actions. The smoke submits an MCP-authored workflow graph/source snapshot, verifies it remains unpublished until TUI approval, then confirms publication does not authorize or enqueue execution. Representative shared scenarios are mapped to owner-layer and adapter tests below; the matrix is not exhaustive. Feature-set identity across adapters is not a product requirement. |
| M7: production readiness | Partial | `backup.rs`, `backup_tests.rs::symlinked_default_vault_is_backed_up_from_its_canonical_directory`, `proposal.rs::symlinked_default_vault_is_bound_to_the_default_path`, `doctor.rs`, `focus_report.rs`, CI and package/smoke tasks | Backup/restore, diagnostics, reporting, and Linux packaging exist. Default Vault symlink aliases resolve to the same canonical identity for proposal binding and backup; capture restores the contents as a real directory. On this ARM64 Linux host, a freshly built release archive was extracted and its packaged binaries passed both the modeled 12-step sandbox smoke (including the new packaged Worker nonzero-exit persistence case) and the systemd user-service recovery smoke. This does not establish a clean system installation, Wayland/TUI hardware acceptance, or extended daily-use reliability. |
| M8: selective integrations | Deferred by design | Design section 22 | No acceptance work is required for initial delivery. |

Historical checkpoint, 2026-11-21: the Daemon's in-process worker startup gate and atomic
pre-release attempt invalidation were added with focused tests. The worker
waits for a Daemon Start signal before invoking its execution closure; failed
pre-release startup marks the run failed and invalidates the matching claimed
or started attempt in one transaction. The daemon test target, full
`cargo xtask check`, Agent registry validation, and example Vault validation
passed. This was an intermediate implementation state; later entries below
record production Worker process integration.

On 2026-11-21, `mcbtodo-protocol::worker` gained a strict versioned frame
contract with attempt identity, a Prepare/Ready/Start/Cancel handshake, typed
script/Agent jobs, and identity-bound results. Focused protocol tests cover
round-trip identity, unknown-field rejection, source/frame bounds, and
oversized-frame rejection. The `mcbtodo-worker` binary now implements that
worker-side handshake and reuses the shared execution adapters. A process-level
integration test launches the actual binary, verifies its Ready identity,
confirms it remains blocked before Start, and checks that pre-start Cancel exits
without returning a result. The Linux archive build and CI include the binary.
At this historical checkpoint, before the production-wiring entry below, the
Daemon did not yet launch or supervise it; execution remained on the in-process
thread path. M5 remained Partial; production envelope binding, process
supervision, cross-process fencing, cancellation/reaping, and crash-recovery
acceptance remained open.

Historical checkpoint, 2026-11-21: the Agent socket adapter and Bubblewrap script sandbox moved
from the Daemon's private modules into `mcbtodo-execution`. Its 18 focused
tests passed after the move. This establishes a reusable crate boundary for a
Worker without duplicating security-sensitive execution code. At this
checkpoint, the Worker binary did not yet have a production Daemon supervisor;
the later production-wiring entry supersedes that state.

Later on 2026-11-21, the Daemon execution path was connected to the packaged
Worker protocol. It now launches and supervises `mcbtodo-worker` for script
and Agent attempts, uses the Ready/Started/Start gate, and validates identity
before its transactional result commit. The focused real-process success test
and a startup-failure test passed. The startup-failure regression verifies
that a post-claim spawn failure ends in Failed rather than leaving a run
Running. At that checkpoint, production-path cancellation/reaping and
independent crash/restart fault injection still required explicit acceptance;
the later Linux cancellation regression documented below closes the former
test gap only.

Also on 2026-11-21, the Linux process-group termination helper was hardened to
propagate failures other than `ESRCH` instead of silently reporting cleanup
success. `terminating_worker_kills_its_process_group_and_reaps_the_worker`
starts a real parent/descendant process pair, invokes that helper, and verifies
the descendant is no longer running and the Worker child is waited. A separate
production-path regression,
`cancelling_running_script_through_daemon_reaps_worker_and_records_unknown`,
starts a sandbox script with a live descendant, sends cancellation through
`Runtime::handle`, and verifies `OutcomeUnknown`, no additional step commit,
Worker reaping, and that the observed descendants are no longer running.
Production-path cancellation/reaping is therefore covered on Linux. This
checkpoint predates the later production-supervisor readiness and result-frame
injection tests below; those later entries supersede the corresponding gaps.
Packaged-binary failure-combination acceptance remains open.

## Documentation and Workflow Verification

On 2026-11-21, licensing guidance was narrowed to match the maintainer's
statement that this is their original software: the repository does not
request ownership certificates, rights attestations, private agreements, or
identity evidence. `docs/licensing.md` covers Apache-2.0 and review of
externally sourced material. `CONTRIBUTING.md`, the PR template, harness
instructions, and registered contribution/release workflows were synchronized.
Third-party license, attribution, and notice review remains required when
external material is included.

On 2026-11-21, `cargo xtask agent validate`, `cargo xtask check`, and
`cargo run --locked -- --vault examples/vault --timezone Asia/Shanghai validate`
passed in this workspace. The Markdown inventory, registered harness/workflow
documents, package names, filesystem paths, release claims, and licensing
wording were reviewed against the implementation and registry. The earlier
provenance-intake wording in this historical check is superseded by the
licensing update above. The project license is Apache-2.0; licensing guidance
is documented in `docs/licensing.md`. This is a repository-local consistency
check: it does not validate external links or replace review of third-party
license compatibility. The
documentation index links each registered workflow directly and indexes the
GitHub Actions CI and release-preparation workflows separately from the AI
registry. Release preparation runs the example Vault validation required by
the release workflow; manual dispatch validates the selected ref without
treating it as release authorization. The complete checkout includes many
example Vault records that are user-data-shaped fixtures rather than project
documentation; those fixtures are intentionally not indexed individually.
Local link target and
heading-anchor validation covered the 29 project Markdown files, including the
GitHub pull-request template and all registered harness/workflow instructions
(excluding user-data-shaped Vault fixtures). External URLs were not checked,
and the fixture content was not treated as project documentation.
The default-parallel workspace check initially exposed a race in concurrent
first-run config initialization: publishing with a hard link briefly left the
staged name and `config.toml` linked to the same inode, which a concurrent
reader correctly rejected. Unix initialization now publishes with a
same-directory no-replace atomic rename. The focused concurrency test and the
complete `cargo xtask check` passed after the fix; formatting, Agent registry
validation, example Vault validation, and Linux sandbox smoke also passed.

On 2026-11-21, a follow-up verification passed `cargo xtask agent validate`,
`cargo xtask check`, and
`cargo run --locked -- --vault examples/vault --timezone Asia/Shanghai
validate`. A separate local-link and heading-fragment check passed across all
28 project Markdown documents, excluding user-data-shaped `examples/vault/`
records. The Linux CI workflow now runs the example Vault validation on both
native architecture runners before package validation and the packaged
binary sandbox smoke; Dependabot checks Cargo and GitHub Actions weekly without
automatic merging. These are repository workflow controls, not evidence of a
successful GitHub-hosted run or configured branch protection. Architecture
milestones remain Partial: the application crate is not yet the sole shared
business-use-case boundary, packaged-Worker protocol/fault combinations and external
side-effect reconciliation remain unaccepted, and interactive daily-use
reliability is not established. At this assessment date the workspace had no
configured Git remote; the canonical repository was subsequently created as
`https://github.com/MCB-SMART-BOY/mcbtodo`.

During a later verification pass on 2026-11-21, the default-parallel
`cargo xtask check` run had one daemon test fail with a state-directory lock
contention error; the other tests in that run passed. The exact test passed in
isolation, and the complete daemon test target passed again with 16 test
threads. A subsequent `RUST_TEST_THREADS=1 cargo xtask check` passed across the
workspace. The lock failure did not reproduce, but its cause is not established;
the development workflow requires reporting it if it recurs rather than
treating a retry alone as an explanation.

On 2026-11-21, the Daemon RunStore gained persisted attempt claims, monotonic
epochs, binding checks, restart invalidation, and atomic result/attempt
commits, and the runtime began using that path for executable nodes. Focused
tests cover fencing, restart invalidation, transaction rollback, immediate
`outcome_unknown`, and no replay. The test
`result_commit_failure_becomes_unknown_without_replay_after_restart` checks
that the runtime marks uncertainty immediately after a failed commit and that
restart does not replay the attempt. These checks establish current
single-Daemon store behavior only; Worker process integration and production
cancellation/reaping were documented in later entries. Independent process
crash/restart acceptance remains open under M5. On 2026-11-21,
`cargo fmt --all -- --check`,
`cargo xtask check`, `cargo xtask agent validate`, the example Vault
validation, and `cargo xtask smoke --bin-dir target/debug --sandbox` all
passed after this update. The smoke result verifies the current Daemon/runtime
boundary only; it does not close the independent Worker crash/restart gaps above.

On 2026-11-21, the Linux regression
`worker_crash_after_start_becomes_unknown_and_is_not_replayed_after_runtime_restart`
passed through the production Daemon supervisor. It kills a live Worker process
group after Started, verifies `outcome_unknown` with no committed step, and
reopens the RunStore to verify the attempt is not replayed. This closes the
post-Started Worker-kill case only; it does not establish malformed identity
through the supervisor or the packaged-binary failure matrix. Separately,
`exercise_interrupted_worker` in the sandbox smoke force-kills the Daemon after
observing a script side effect, captures its observable Worker descendant PIDs,
and waits until those processes exit or become zombies before checking restart
recovery/no replay. Worker startup now arms `PDEATHSIG(SIGKILL)` and validates
the expected Daemon PID to cover the race before signal setup. This proves
process termination for the sampled local tree, not rollback of any external
effect or the state of an Agent's remote service.

On 2026-11-21, `malformed_worker_readiness_frame_fails_closed_in_production_supervisor`
passed through `Runtime::spawn_worker_at`: a child process emits an invalid
protocol frame in response to Prepare, and the Daemon fails the pre-start
attempt without recording a step or retaining an active Worker.
`mismatched_worker_result_after_start_becomes_unknown_without_replay` encodes a
valid but mismatched Result frame and has a fixture subprocess write it to
stdout. The production `read_worker_messages` decoder and Started `poll_worker`
state path process that real pipe; the test verifies child reaping,
`outcome_unknown`, no committed step, and restart no-replay. The fixture is not
the packaged Worker binary, so packaged-binary failure combinations remain
unverified.

Agent registry validation requires a readable UTF-8 root `AGENTS.md`, and the
cold-offline integration test prepares all 30 registered harness/workflow
combinations. `cargo xtask agent validate` also requires each registered
workflow to have a linked name in both workflow indexes and verifies local
source paths against those links. Focused integration tests reject missing
workflow labels and stale local paths. On
2026-11-21, cache helper tests verify private nested directories, no-follow
regular-file reads, single-link ownership, restrictive permissions, atomic
replacement, rejection of static nested/final symlinks, root-level cache files,
and descriptor anchoring after a directory rename and symlink replacement.
Unix cache traversal and publication use directory-relative operations after
opening and validating each directory. The non-Unix fallback remains
path-based and has not received equivalent race-resistance testing.

## End-to-End Evidence

`xtask/src/smoke.rs` runs the fixed Rust OS page-table scenario from design
section 23. `cargo xtask smoke --bin-dir target/debug --sandbox` passed on
2026-11-21 in this Linux workspace. The assertions cover:

| Design step | Smoke evidence |
| --- | --- |
| 1. Long-lived Project, active period, and deadline | `exercise_planning`: MCP project listing and calendar assertions for the non-busy active period and deadline. |
| 2. AI proposal for four WorkUnits | `exercise_planning`: MCP Graph Diff preview and proposal submission. |
| 3. DAG review and approval | `exercise_planning`: parallel branches/join assertions and interactive TUI approval; verifies all four Markdown WorkUnits are published. |
| 4. Scheduled calendar block | `create_time_block_in_tui` and the subsequent calendar projection assertion. |
| 5. Focus survives TUI exit and produces a report | `run`: daemon timer start, TUI exit, elapsed-time advance, pause/stop, JSON and Markdown report checks. |
| 6. Published workflow bound to the test WorkUnit | `exercise_sandbox`: publish fixture workflow and bind its immutable revision. |
| 7. Zoned Cron and missed-run policy | `exercise_cron`: Asia/Tokyo expression, `run_once`, pinned revision, and next-run instant. |
| 8. Sandboxed execution and artifacts | `exercise_sandbox`: authorized scheduled run, typed TestResult, and stdout/stderr artifact checks. |
| 9. Failure branch and constrained Agent review | `exercise_agent`: deterministic mock Agent, denied-before-authorization check, and approve/reject decisions without unauthorized Vault writes. |
| 10. User review across views | Graph proposal approval and Agent workflow decisions are driven through the TUI; current smoke does not assert every view/focus transition from the prose scenario. |
| 11. Evidence-backed completion and report | `exercise_sandbox` checks run/revision-bound Evidence and the completion event; `run` checks the daily Focus report. |
| 12. Crash between external effect and durable result | `exercise_interrupted_worker`: force-kills the daemon after observing the script side effect, asserts sampled Worker descendants exit, then asserts `outcome_unknown` and idempotent resubmission. This verifies local process cleanup and persisted state recovery/no replay, not reversal or reconciliation of the external effect. |
| MCP workflow publication boundary | `exercise_planning` submits exact graph/source bytes over MCP, verifies no revision exists while pending, and approves in the TUI; `run` verifies no run was auto-enqueued and a manual run is refused without script authorization. |
| Backup and restore continuity | `run`: restores into another home and checks proposal receipt, completion events, workflow artifacts, runs, and notifications. |

Steps 6, 8, 9, 11's workflow Evidence assertion, and 12 require `--sandbox`;
the ordinary smoke path does not claim those checks. The Agent is a protocol
test double, and the scenario uses deterministic fixture dates and data.

The smoke workflow portion still publishes its executable fixture through the
CLI for scheduling and sandbox execution. The MCP-authored snapshot journey
separately covers Base64 source capture, pre-approval non-publication, TUI
approval, and the boundary between publication and script authorization.
Focused tests continue to cover rejection, exact-byte publication, retry, and
cross-home backup/restore of a pending snapshot.

On 2026-11-21, `cargo xtask package --bin-dir target/release` produced an
AArch64 archive on this ARM64 host. After extraction, the packaged service unit
matched the source unit byte-for-byte, and
`cargo xtask smoke --bin-dir <extracted>/usr/bin --sandbox` passed. The packaged
release binaries also passed `cargo xtask smoke-systemd --bin-dir
<extracted>/usr/bin`; this exercises a transient systemd user service, kills
the daemon during a running focus session, and verifies recovery to
`needs_review` plus JSON and Markdown reports. The archive was validated as an
extracted artifact, not installed into the system's `/usr/bin` or user unit
directory. Clean installation and extended daily-use reliability remain open.

On 2026-11-21, a separate temporary HOME was initialized twice with the
release binary. The config remained unchanged, config and workspace directories
were mode `0700`, the config file was mode `0600`, and `doctor --json` reported
a healthy workspace with the stopped daemon as `offline`. This first-run
acceptance is now part of `cargo xtask smoke`; it verifies the user's directory
layout and initialization contract, not the privileged package installation
or an interactive session in the target desktop environment.

## Platform and Publication Boundary

The repository declares Linux x86_64 and AArch64 as its release targets, and
CI uses native Linux runners for both. The daemon implementation is Linux-only;
the release archive includes a systemd user unit. Other operating systems and
non-systemd service managers have not been established as supported targets.

The package archive and smoke checks demonstrate build and staged-artifact
behavior, not a published or installed release. The checked-in workflow does
not create GitHub Releases or publish crates.io packages. Cargo workspace
packages declare the Apache-2.0 SPDX identifier, include the workspace license
text in their package archives, and have package-specific descriptions. The
publishable workspace packages passed `cargo package --workspace --exclude
mcbtodo-xtask --locked --no-verify --allow-dirty` on 2026-11-21. Package
manifests provide descriptions and docs.rs documentation targets. Cargo still
warns that `license` and `license-file` are both declared; retaining both keeps
the SPDX identifier and includes the full Apache license in every nested crate
archive. The canonical repository URL is configured in workspace metadata.
Third-party license compatibility and package-content reviews remain release
preparation work. `cargo publish --dry-run --workspace --exclude
mcbtodo-xtask --locked --allow-dirty` packages all ten crates, but verification
stops at `mcbtodo-vault`: its isolated package build resolves
`mcbtodo-domain = 0.1.0` from the registry, whose `InboxEntry` API lacks the
`task_id` method required by this workspace. The internal crates therefore
need a verified publication/version sequence before a complete publish dry-run
can pass; no crate was published.

## Cross-Cutting Architecture Finding

WorkUnit completion evidence now requires a persisted workflow step trace with
`status = "succeeded"` as well as the existing revision-binding check. Failed,
skipped, missing, and malformed traces are rejected before application-layer
publication. The CLI has focused shape tests, and the application layer already
tests that rejected evidence leaves the WorkUnit unchanged.

On 2026-11-21, the production `Server::open` and `Runtime::open` boundaries
were tightened to accept one `WorkspacePaths` value and derive their state,
socket, focus database, and watcher paths from it. A focused test opens the
production constructor against a temporary home and verifies the socket and
both SQLite databases are created under that workspace. Tests that deliberately
inject arbitrary endpoint paths use test-only constructors. An explicit CLI/TUI `--vault` selection does not reconfigure the
background Daemon: watcher, reminders, and WorkUnit-completion triggers remain
bound to the default Vault. External Vault background service is not currently
implemented.

On 2026-11-21, CLI Vault, workflow authoring/runtime, and completion-evidence
paths were aligned to the single `WorkspacePaths` instance created at the CLI
entry point. `workflow_commands_use_the_supplied_workspace_state_directory`
publishes and lists a revision using an explicit temporary home and verifies
the revision file is under that workspace's state directory. The source audit
found `WorkspacePaths::discover()` only at CLI entry points, not in those
downstream command handlers. `cargo fmt --all -- --check`, `cargo xtask agent
validate`, `cargo xtask check`, and example Vault validation passed after the
change. This covers the workflow CLI path and workspace checks; it is not a
claim that every application boundary has an end-to-end alternate-home test.

On 2026-11-21, the CLI, TUI, and MCP proposal intake were tightened so reminder
creation fails before publication when the selected Vault is not the canonical
default Vault. This prevents a successful-looking reminder write that the
background Daemon would never scan. Other foreground edits to external Vaults
remain supported; operating a background service for them remains unimplemented.

The workspace already has the intended dependency direction at the crate level:
`mcbtodo`/`mcbtodo-daemon` depend on `domain`, `vault`, `workflow`, `platform`, and
`protocol`. The `application` crate owns shared read projections and
revision-bearing query results, plus several shared business write commands.
It is not yet the complete application boundary described by the design.

The frozen first-release process topology is implemented: the Daemon launches
and supervises a local Worker for script and Agent attempts, and Linux tests
cover production cancellation, Worker reaping, observed descendant termination,
a Started Worker SIGKILL that becomes `outcome_unknown` without replay after
Runtime restart, and Daemon-death smoke containment of sampled Worker
descendants. Production supervisor tests now cover malformed readiness and a
mismatched Result through a fixture subprocess and the production pipe reader.
An oversized trace regression also verifies that a Worker attempt is
invalidated atomically when its run fails. A freshly extracted Linux release
archive passes the sandbox smoke's nonzero script-exit case through its
packaged Worker, including durable failed-step and trace-reason assertions.
Packaged malformed-frame and combined-fault cases, plus external side-effect
reconciliation, remain unaccepted.
Parent-death signaling stops the local process tree but cannot undo or reconcile
external side effects. RunStore epochs are not Worker leases and do not fence
external effects from a disconnected process.
Distributed Worker clusters are explicitly outside first-release scope.

`WorkspacePaths` centralizes the intended home layout. Initialization and
platform-mediated user-config reads validate that `.config` is a real,
current-user-owned directory not writable by group/other users, and that
`.config/mcbtodo` is a private real directory with the same ownership. Config
and keymap reads share a 1 MiB-bounded, final-component no-follow open; the
opened file must be current-user-owned and not group/other-writable. Agent
policy has its own bounded parser and ownership/mode checks. Tests cover a
post-initialization config-directory symlink substitution, unsafe `.config`
permissions, group-writable config, hard-linked config and state files, and
rejecting paths outside the application config directory. This remains a
scoped defense, not a complete filesystem trust boundary: it does not pin
every ancestor with directory file descriptors, prevent same-user concurrent
path replacement, constrain hard links in Vault/runtime storage, validate
arbitrary caller-supplied Vault roots, or unify lock/cache/revision path
handling. Do not infer that these checks make every external or runtime path
safe.

Proposal binding canonicalizes both the selected Vault root and the configured
default Vault root, then requires an exact path match. A default-root symlink
therefore retains default binding; backup records the resolved directory's
device/inode, opens the canonical real directory without following a
final-component symlink, and rejects it if the opened identity changed.
Restore materializes a real directory. Focused regression tests cover
symlinked-root binding, backup and proposal restore, and replacement detection.
Symlinks inside the Vault tree remain rejected. External Vaults are still
excluded from default-only proposal snapshots and backups.

The daemon now shares Vault's debounced inotify watcher with the TUI and keeps
the last successfully validated snapshot when an external edit is invalid.
Reminder definitions are projected from that snapshot and passed to the
background occurrence scanner; a generation token discards in-flight results
from superseded snapshots. Due occurrences enter the queryable notification
inbox and a transactional desktop-delivery outbox together. The outbox retries
helper errors with bounded exponential backoff, recovers in-flight work after
restart, and stops after five attempts. This is at-least-once delivery across a
crash between the helper side effect and durable acknowledgement, not exactly
once. Occurrences outside the 24-hour catch-up window are not materialized.
This avoids reminder-specific Vault reloads while preserving asynchronous
scanning. The daemon still has no general indexed query service, and
CLI/TUI/MCP still load Markdown snapshots independently.
Calendar query-to-DTO projection is shared in `application`; the TUI projects
its already-loaded snapshot instead of rebuilding borrowed calendar entries in
each view. Calendar-row focus identity now includes the object UUID, calendar
kind, and concrete occurrence. Month/Week transitions retain a calendar
occurrence identity independently of the source document UUID, and selecting
a document in Outline, Graph, or Inbox clears that calendar focus. Exact
restoration is covered for recurring entries and multi-day all-day events
across Agenda-to-Day navigation and Vault reload; Week-to-Day navigation is
covered for a multi-day timed event selected on a continuation date. Focused
tests cover Agenda round trips through Month and Week, including restoration
of a later recurring occurrence on a multi-day Agenda range, as well as an
Agenda/Month/Week/Day/Outline round trip retaining document identity in
Outline. The full cross-view transition matrix, other multi-day entry types,
and interactive end-user session remain unverified.

The business entrypoints now share a broader application boundary, but still
perform use-case work independently for some reads and interactions:

- `apps/mcbtodo/src/main.rs` uses application queries for validation, Outline,
  Graph, and the ordinary single-day Calendar command; `doctor` uses the same
  `VaultQueries::validate` use case while keeping environment diagnostics in the
  adapter. Focus reports use `VaultQueries::focus_report_context` to validate a
  Project filter and obtain Project/WorkUnit metadata from one snapshot; daemon
  session reads and output formatting remain in the CLI adapter. Export and TUI startup
  still load `Vault` because they need storage-backed document access.
  `vault_cli.rs` serializes Inbox listing and obtains expected write revisions
  through application projections over the already-loaded snapshot, and routes
  Project/WorkUnit
  creation, Task/Inbox capture, completion, workflow binding, deadlines, time
  blocks, reminders, Inbox-to-Task conversion, and recurring Task occurrence materialization
  through application commands. iCalendar parsing remains in its CLI adapter,
  which translates to application-owned `CalendarEventDraft` values; the
  shared application command validates all text and delegates atomic batch
  publication to Vault. Workflow validation, simulation, publication, and
  revision lookup use the application workflow use cases.
- `apps/mcbtodo/src/mcp/tools.rs` uses application queries for project, task,
  WorkUnit, Inbox, calendar, and Graph Diff preview tools. The proposal
  subsystem uses the same Graph Diff calculation and application task-capture
  and Inbox conversion commands while retaining its snapshot, queue, and
  expected-revision coordination.
- `apps/mcbtodo/src/tui/mod.rs` and `tui/creation.rs` load/reload `Vault` for
  user actions. Project/Event creation, Task capture, deadline edits,
  time-block/reminder creation, Inbox-to-Task conversion, and WorkUnit dependency edits use
  application commands. The Inbox view limits its actionable list to entries
  without a Project and submits the snapshot revision with the selected
  Project; Vault creates a deterministic Task and retains the Inbox source.
  Stale revisions require an explicit reload and retry. The Task
  Graph, Outline, and Inbox views consume owned application projections of the
  validated DAG and document summaries rather than rebuilding business
  projections in the TUI; Vault remains available for source paths and document
  editing. Calendar, Agenda, and Timeline views now share the application
  calendar projection and keep layout-specific ordering and rendering in TUI.
  Picker path matching resolves paths through Vault without placing storage
  paths in the application DTO. Reminder and time-block selection candidates
  now come from `project_action_targets` using the same `TargetAction`
  eligibility rules as final target validation; TUI keeps hierarchy rendering
  and passes the snapshot's revision to the write command. Dependency-change validation,
  readiness impact, and the observed target revision come from the application
  preview; publication still uses the revision-checked application command and
  Vault writer lock.
- `apps/mcbtodo/src/proposal.rs` keeps proposal snapshots, validation,
  queue/idempotency, and apply-state coordination in the adapter. Its apply
  path uses shared commands for Task capture, WorkUnit completion, Inbox
  conversion, deadlines, time-blocks, reminders, and Graph Diff publication. The
  application query validates target-kind constraints and the expected target
  plus related revision set from one snapshot before the adapter records
  apply intent; the application command then revalidates the reviewed
  draft/diff and revisions under Vault's writer lock before publication. Inbox
  conversion is recoverable across its two Markdown writes but is not a
  cross-file atomic transaction.
  Enqueue resolves target title,
  kind, and observed revision through application `TargetAction` validation;
  Inbox conversion also validates its Project in the same snapshot. Graph Diff
  review gets the target, diff, and related revision expectations together
  from `VaultQueries::review_work_unit_graph_diff`, so they come from one Vault
  snapshot. The adapter maps its transport operation to the application action,
  while retaining queue locking, idempotency, proposal snapshots, and apply-state
  coordination.
- Workflow proposal approval publishes its exact reviewed snapshot through the
  application workflow use case; proposal status, queue locking, and receipt
  persistence remain adapter-owned.
- `apps/mcbtodo-daemon/src/runtime.rs` routes workflow Task creation through shared
  application query and command APIs. The query returns an opaque Project
  revision token; Vault still checks it under the writer lock. Completion-poller
  Vault reads are infrastructure for the bounded event stream, not unshared
  business writes.

Adapter parity means that adapters which expose the same business scenario
preserve its domain rules, revision behavior, and observable result; it does
not require identical command sets or interaction flows. TUI projections over
an already-loaded validated snapshot, CLI-only iCalendar parsing, proposal
queue/idempotency state, and daemon event polling are intentional ownership
differences. The representative scenario matrix below names shared scenarios
and links them to owner-layer and adapter-level evidence. Its remaining
limitation is scope: it is not an exhaustive command inventory, and future
adapter work must extend it for newly shared scenarios. Do not add a general
query/index service or duplicate Vault loads to make adapter feature sets
identical.

## Target Boundary

Complete the workspace `application` crate as the shared business use-case
boundary. It should own typed commands, queries, domain validation/orchestration,
and result types. Adapters should translate their transport/input into these
types and render the result; they must not independently compose business
Vault operations. Storage snapshot validation, event-log access, and atomic
bulk publication may remain Vault APIs when they are storage-specific contracts.
`application` may depend on `domain`, `vault`, `workflow`, `platform`, and
`protocol` where required. Those lower-level crates must not depend on
`application` or an adapter. Queries that load a Vault return owned DTOs so
callers do not depend on Vault document lifetimes; snapshot-based projection
functions are also available where an adapter already owns the validated
snapshot.

Recurring Task occurrence materialization, atomic iCalendar import, and proposal
Graph Diff publication now enter through `VaultCommands`. The parser remains in the CLI adapter because
the input format is transport-specific; the application owns its neutral
`CalendarEventDraft` and shared Markdown validation. Revision checking,
locking, stable occurrence identity, and atomic directory publication remain
inside Vault. Application fixture tests cover capture, create,
completion, deadline, time-block, reminder, dependency, Inbox conversion,
recurring occurrence materialization, and invalid-batch rejection; adapter
tests cover the corresponding CLI and proposal flows. Task capture applies the
same title/body policy as the other application create commands, with a
regression test proving invalid text is rejected before Vault publication.
Maintain an explicit scenario-to-adapter coverage matrix when expanding an
adapter, and add parity assertions only for scenarios that adapter exposes.
Storage-specific snapshot, locking, Markdown parsing, and atomic publication
stay inside Vault.

## Evidence Limits

`cargo xtask check`, the example Vault validation, and the sandboxed workspace
smoke establish build, lint, unit/integration-test, registry, fixture, and one
modeled end-to-end scenario on this Linux host. The release archive and its
packaged binaries were exercised natively on this ARM64 host. The sandboxed
smoke uses a deterministic mock Agent and fixed example data. These checks do
not establish behavior on other target machines, interactive Wayland/tmux
terminal usability, extended daily-use reliability, or parity for every
business operation. Those claims require dedicated platform and user-session
evidence.

## Representative Adapter Coverage

This matrix records cross-adapter behavior for representative shared
scenarios. It is not an exhaustive inventory of every command, and a blank
adapter cell means that entry point does not currently expose that scenario,
not that it should necessarily do so.

| Scenario | Application/Vault owner evidence | CLI | TUI | MCP | Adapter-specific contract |
| --- | --- | --- | --- | --- | --- |
| Calendar projection | `crates/application/src/lib.rs::tests::calendar_projection_is_owned_and_uses_display_timezone` | `apps/mcbtodo/tests/cli.rs::same_documents_drive_calendar_outline_and_dependencies` | `apps/mcbtodo/src/tui/calendar_tests.rs` day/week projection tests | `apps/mcbtodo/src/mcp/tests.rs::modern_stdio_reads_fixture_projections_without_a_handshake` | TUI reuses its validated snapshot; each adapter chooses its own date window and presentation. |
| Inbox triage | `crates/application/src/lib.rs` triage command tests; Vault persistence tests | `apps/mcbtodo/tests/cli.rs::inbox_capture_list_and_triage_are_a_complete_cli_workflow` | `apps/mcbtodo/src/tui/inbox_tests.rs::inbox_view_lists_entries_and_triage_uses_the_observed_revision` | `apps/mcbtodo/src/mcp/tests.rs::inbox_triage_proposal_is_revision_bound_idempotent_and_review_only` | MCP queues a revision-bound proposal; CLI/TUI local actions apply directly after explicit user action. |
| Deadline change | `crates/application/src/lib.rs::tests::deadline_command_publishes_and_returns_the_new_revision` | `apps/mcbtodo/tests/cli.rs::vault_commands_set_and_clear_a_deadline` | `apps/mcbtodo/src/tui/deadline_tests.rs::selected_outline_document_can_set_a_new_date_deadline`; `selected_outline_document_can_clear_its_deadline` | `apps/mcbtodo/src/mcp/tests.rs::deadline_proposals_are_idempotent_revision_checked_and_leave_vault_unchanged` | MCP proposal must be approved before publication. |
| WorkUnit dependency graph | `crates/application/src/lib.rs` dependency preview and command tests; `crates/vault/tests/persistence.rs` graph validation tests | `apps/mcbtodo/tests/cli.rs::vault_commands_create_and_schedule_work_with_dependency_checks` | `apps/mcbtodo/src/tui/calendar_tests.rs::graph_dependency_edit_previews_then_confirms_or_cancels` | `apps/mcbtodo/src/mcp/tests.rs::graph_diff_previews_parallel_units_and_rejects_cycles_without_writes` | MCP Graph Diff submission is separately reviewed; preview alone does not write. |
| Workflow snapshot review and publication | `crates/application/src/workflow.rs::tests::validation_publication_and_revision_access_share_one_use_case`; `crates/workflow/tests/publication.rs` | Workflow CLI flows in `apps/mcbtodo/src/workflow_runtime_cli.rs` | `apps/mcbtodo/src/tui/proposal_review.rs` workflow proposal tests | `apps/mcbtodo/src/mcp/tests.rs::workflow_draft_tool_captures_exact_base64_sources_idempotently_for_review`; `xtask/src/smoke.rs` | MCP submission remains unpublished until TUI approval; publication does not grant script authorization or enqueue execution. |

The owner-layer tests establish shared validation and persistence behavior;
adapter tests establish how each exposed entry point applies those rules and
preserves its own safety contract. Continue extending this matrix when an
adapter gains a shared scenario. Do not treat intentional differences in
parsing, proposal approval, snapshot ownership, or presentation as parity
failures.
