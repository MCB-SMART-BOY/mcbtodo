# Architecture Acceptance Assessment

Assessment baseline: 2026-11-23. This records what the local implementation
can demonstrate, not what the product design intends to provide. The dated
evidence is a snapshot and must be refreshed when acceptance checks change;
documentation ownership and synchronization rules are in
[`docs/README.md`](README.md).

## Defensive code review (2026-11-22)

This review traced Agent dispatch, Worker result admission, backup and
restore, and TUI Settings.

| Production path | Redundancy | Resolution |
| --- | --- | --- |
| [Agent dispatch](../crates/execution/src/agent.rs) and [AgentPolicy](../crates/platform/src/agent_policy.rs) | After resolving an exact grant, the Worker checked fixed platform and invocation metadata, unobserved health, and a capability predicate that Codex already enforced at launch. | Resolved. The Worker constructs the authorized adapter. Codex rejects a capability mismatch before launch and the Unix adapter delegates the authorized capability to its authenticated peer. Runtime availability comes from launch or connection. Failure terminates the call. |
| [Daemon Worker runtime](../apps/mcbtodo-daemon/src/runtime.rs) | `finish_worker` repeated the message variant and Attempt identity checks performed by `accept_worker_message`. | Resolved. Admission constructs a private result for the matching Attempt. Commit still checks cancellation and deadline, validates the result contents, and uses the protected storage transition. |
| [Daemon queue](../apps/mcbtodo-daemon/src/runtime.rs) | The launch path rechecked Worker capacity after claiming an Attempt, although the same loop already required an empty slot before selecting the run. | Resolved. The Daemon uses the slot directly for wake-up and before selecting pending work. Busy runs remain queued with no new Attempt or budget reservation. |
| [Worker framing](../crates/protocol/src/worker.rs) | Validation built an intermediate JSON tree for the entire message, including script and executable bytes. | Resolved. The codec checks the typed message fields directly and bounds serialization before writing the frame. The v2 wire format retains revision and Attempt constraints, size limits, and supervisor identity comparison. |
| [Backup](../apps/mcbtodo/src/backup.rs) and [Vault persistence](../crates/vault/src/persistence.rs) | Staging validation loaded Markdown again if the event database had a pending completion. | Resolved. One validation operation loads Markdown and checks the pending event against that snapshot. SQLite recovery still uses a disposable copy. |
| [Codex configuration](../crates/execution/src/codex_host.rs) | Oversized configuration reached the UTF-8 decoder and TOML parser before being rejected. | Resolved. The size check precedes decoding. Doctor exercises the oversized case without launching a process or connecting to the model endpoint. |
| [Codex launch](../crates/execution/src/codex_host.rs) | The Worker connected to the model endpoint before checking it could prepare the local sandbox. | Resolved. Sandbox preparation succeeds before the model connection. An extracted Worker under `/tmp` is rejected without contacting the endpoint. |
| [Unix Agent](../crates/execution/src/unix_agent.rs) | The stricter `0o077` mask already covers the `0o022` part of `owned_and_protected`. | Resolved. The socket root checks directory type, owner and the strict mask directly. Intermediate-directory checks and socket peer authentication still apply. |
| [Agent port](../crates/ports/src/lib.rs) | Unix Agent parsed workflow `Scalar` values, converted them to JSON values, then had the host decode them as `Scalar` again. The two adapters also had separate scalar output parsers. | Resolved. Both adapters use the port's typed values and its duplicate-key rejecting output decoder. The host still checks the declared workflow output ports before committing. |
| [Doctor](../apps/mcbtodo/src/doctor.rs) | Doctor inspected `config.toml` before the platform opened and verified the same file. | Resolved. Doctor and Settings use the platform's optional configuration reader. The opened file still undergoes regular-file and ownership checks. A dangling symlink is reported as an error. |
| [Agent discovery](../apps/mcbtodo/src/agent_discovery.rs) | Doctor summarized configured providers from the policy and passed a separate Codex-present flag to preflight. Settings later reused the discovery result and performed the installation preflight on every open. | Settings and doctor share a typed policy summary. Settings shows the defaults and grant counts. Doctor performs the local installation diagnosis on request. The Linux Codex editor shares configuration validation with the Worker. |
| [TUI keymap](../apps/mcbtodo/src/tui/keymap.rs) | Default characters, normalization targets, and view scope were maintained separately. | Resolved. One binding table per scope supplies defaults, normalization, and the editable labels. Input and shortcut hints share the saved keymap. |
| [Settings persistence](../crates/platform/src/lib.rs) | The preferences and keyboard pages maintained separate snapshots and decided independently when to encode a replacement. | Both pages use `ConfigSnapshot` to hold the original text and parsed baseline for their own file. The snapshot skips encoding on unchanged saves and uses the shared publisher to detect comment-only edits. |
| [Keyboard configuration](../apps/mcbtodo/src/tui/keymap.rs) | The loader counted and extracted characters from strings. The keymap tests used a separate filesystem reader. | The TOML codec reads and writes `char`, and one keymap validator handles control characters, contextual collisions, and reserved actions. Configuration tests load through the production workspace API. |
| [Configuration reads](../crates/platform/src/lib.rs) | Preferences, key bindings, and Agent configuration maintained different Unix file-opening paths for the same directory and file checks. | One descriptor-relative Unix reader enforces each file's permissions, link checks, and byte limits. Higher layers parse their own configuration. |
| [Settings form](../apps/mcbtodo/src/tui/settings.rs) | Navigation and rendering depended on enum discriminants matching the separately declared display order. Each page also derived its own save status. | Both layers use the form's field order. The config publisher reports whether it wrote replacement bytes for either page. |
| [TUI startup](../apps/mcbtodo/src/main.rs) and [Settings](../apps/mcbtodo/src/tui/settings.rs) | Invalid preferences at startup required a manual repair even though the Settings form already supported replacement. The form also held a second timezone value. | Startup passes the captured file to the shared Settings form before opening the Vault. The form keeps the timezone as editable text and parses it on save. The PTY smoke verifies cancellation, invalid input, and continuation with the repaired timezone. |
| [Settings input and feedback](../apps/mcbtodo/src/tui/settings.rs) | The preferences form and its caller reinterpreted the same key and translated save and reload actions between two enums. Save feedback once depended on the on-disk baseline. | Startup repair and Settings share one mapping for common editor keys. The active page accepts saves only when editable. Save feedback compares the saved timezone with the active Vault timezone. |
| [Settings commands](../apps/mcbtodo/src/tui/mod.rs) | Four separate handlers checked that the Settings overlay was active before opening a subpage, saving, or reloading. | One dispatcher applies Settings commands to the active form. Preferences, keyboard bindings, and passive Agent discovery retain their own read and update paths. |
| [Settings read errors](../apps/mcbtodo/src/tui/settings.rs) | A keymap read error left the preferences page active, where Enter still targeted `config.toml`. | Both editors present read errors on the page that owns the file. Keyboard keeps the preferences draft and retries its own file on F5. |
| [Startup view](../apps/mcbtodo/src/tui/mod.rs) | An unreadable `last-view` blocked TUI startup, and every terminal handoff attempted to record the view. | Startup shows Month with a notice so the restore preference can be changed in Settings. The TUI writes `last-view` on normal exit. The `:view` command, its usage text, and `last-view` share saved view names. |
| [TUI palette](../apps/mcbtodo/src/tui/theme.rs) | `MCBTODO_TUI_THEME` duplicated the saved theme preference. An invalid value blocked startup. | Settings and `config.toml` own palette selection. Startup and settings saves use the same `NO_COLOR` rule for the rendered palette. |
| [Codex handshake](../crates/agent-codex/src/lib.rs) | The client compared platform and home metadata even though the Worker owns process isolation and the scratch environment. | Resolved. The handshake checks the response and the negotiated behavior used by the adapter. The Worker continues to enforce isolation. |
| [Agent execution](../crates/execution/src/agent.rs) | A message substring was used to classify a missing policy. | Resolved. The optional policy reader distinguishes absence from errors. A transport error containing those words keeps its own classification. |
| [Worker Agent execution](../apps/mcbtodo-worker/src/linux.rs) | The execution layer wrapped an uncertain outcome in a generic error, and the Worker downcast the error to recover the state. | Resolved. Execution returns an explicit disposition. The Worker's uncertainty result includes the matching Attempt and the selected host adapter's ID. |

The CLI reads `config.toml` once during TUI startup and opens Settings for
repair if the readable contents are invalid. Settings edits the time
zone, startup view, and theme from a fresh draft on each visit. The platform
save API compares the baseline, skips unchanged writes, and atomically
publishes edited preferences. A successful save updates the palette in the
current session unless `NO_COLOR` is active. Restart applies the
saved timezone and startup view.
On Linux, the same draft edits `worker_slots`. The Daemon reads the saved
limit at startup and gives `--worker-slots` precedence. If preferences cannot
be loaded, the Daemon logs the cause and uses one slot. Workspace validation
still reports an invalid configuration.

F2 opens the Keyboard page. It applies successful saves immediately and
preserves the preferences draft. Both preferences and keyboard bindings use
the platform's configuration publisher and compare file text, including
comment-only intervening edits. An explicit replace action can repair a
readable invalid keymap or a preferences file broken during a running session.
F5 reloads the current page in place. An unsuccessful read preserves the draft
and focus. A conflicting save reports how to reload.

F3 opens the Agents policy summary and default-route editor. On Linux, F6
edits the selected configured Provider's socket path, and F4 opens the Codex
parameter editor. A successful save is read by future
Worker attempts, and returning to preferences restores its focused field
and draft. Agent authorization and runtime dispatch continue through the
existing policy and Worker. During a
running session, a preferences read
error is displayed on the preferences page. Keyboard and Agents use their
own configuration reads. Startup repair uses the preferences editor directly
and handles only preferences commands.

The TUI's second [Vault read at startup](../apps/mcbtodo/src/tui/mod.rs) closes
the gap before watcher registration. Vault rechecks revision after staging
because an external editor can write while mcbtodo stages its update. Worker
and Daemon deadlines cover different points in the execution lifecycle. The
backup path identity checks need a separate review of symlink and rename
handling before consolidation. Manifest integrity, SQLite integrity, and
completion-event validation cover different properties of a restored bundle.

## Resource scheduling review (2026-11-22)

The Daemon owns one shared `WorkerSlot` for Script and Agent execution.
The loop uses the same slot state to compute queue wake-up and to select pending
work before any claim. The launch-time admission check and the public capacity
types were removed. The current Worker supervisor owns the only capacity.

The [Daemon regression](../apps/mcbtodo-daemon/src/agent_budget_tests.rs)
holds a real Agent Worker open, observes that a second run stays pending with
zero Attempts and zero budget usage, and verifies one successful dispatch once
the slot is released. The interrupted run records `outcome_unknown`. Durable
claims, Agent budget reservation, Provider authorization, deadlines, cancellation,
and result admission remain owned by their existing layers. Resource scheduling
beyond the single shared slot is planned.

## Bounded worker concurrency follow-up (2026-11-23)

The Daemon now owns a bounded `VecDeque` of supervised Worker slots. The
default remains one slot. `mcbtodo-daemon --worker-slots` accepts values from
1 through 4. Script and Agent attempts use the same durable queue, policy,
budget, Start fencing and result-admission boundaries across all slots, while
each slot has independent cancellation and process supervision.

Focused regressions cover the capacity boundary and recovery behavior. An
occupied slot leaves the next Agent run pending without an Attempt or budget
reservation. Two occupied slots can execute independently and cancellation of
one does not cancel the other. Restart recovery fences both Started Workers as
`outcome_unknown` while preserving pending work. The workspace check and Linux
sandbox and systemd smoke checks also exercise the resulting runtime.

This verifies local bounded concurrency. A general `ResourceScheduler` with
cross-provider priority, resource quotas, fallback or health-based selection
is still planned.

## Release preparation refresh (2026-11-23)

The current workspace passed the first-release package preparation checks on
Linux AArch64. `cargo package --workspace --exclude mcbtodo-xtask --locked
--no-verify` created archives for all thirteen publishable packages in a fresh
target directory. `cargo xtask verify-crate-packages` confirmed Apache-2.0
metadata and an archived `LICENSE` entry for every package. The online
`cargo publish --dry-run --workspace --locked --no-verify` reached the upload
stage for all thirteen packages and skipped the uploads because it was a dry
run.

This verifies package creation, workspace metadata, and the local publication
path. It does not publish any crate, create a GitHub Release, or establish
that a package can be built from crates.io after its workspace dependencies
have been published. Publication remains a separately authorized operation
that must follow dependency order.

## Native CI verification follow-up (2026-11-23)

GitHub Actions run `45086065249` exposed three environment-specific issues in
the previous `main` revision. The Linux jobs attempted to install `nushell`
from the Ubuntu 24.04 apt sources, which do not provide that package. The
macOS CLI tests revealed that Inbox capture published through the original
Vault path after locking and canonicalizing the workspace, so a temporary
directory whose path was not canonical failed the platform path invariant. The
Windows MCP test expected the Unix tool set even though Windows intentionally
exposes the read-only tool set.

The subsequent fixes addressed those findings. CI installs Bubblewrap
from apt and installs the pinned Nushell `0.107.0` release binary for each
native Linux architecture after verifying its SHA-256 digest. Inbox capture
publishes through the canonical root held by the active Vault lock. The MCP
tool-count assertion follows the platform capability boundary.

Local verification passed after these changes:

- `cargo xtask check`
- `cargo run --locked -- --vault examples/vault --timezone Asia/Shanghai validate`
- the two previously failing macOS CLI tests
- the Windows MCP tool-count regression
- shell syntax and diff checks for the CI installer

GitHub Actions run `45089026032` passed the native macOS ARM64 and Windows
x86_64 CLI, TUI, Settings and Vault jobs, along with both cross-target build
checks. The Linux jobs still failed. AArch64 timed out waiting for the Worker
crash fixture. The x86_64 package smoke reported an unexpected terminal run,
and run `45086966515` recorded Bubblewrap's loopback namespace setup failing
with `Operation not permitted`.

The Worker crash and cancellation fixtures now wait for an observable write
in the sandbox before injecting the fault. Their Bash scripts use built-ins
to keep the attempt open. The old `sleep` loop was reproduced locally failing
at the sandbox's 64 KiB log limit because `sleep` is absent from that sandbox.
The packaged crash fixture also uses built-ins. The tests still require
`outcome_unknown`, process cleanup and the applicable restart checks.
Failure reports include the Daemon history or the smoke run's first trace to
identify the operation that failed.

GitHub Actions run `45090214588` reproduced Bubblewrap's loopback failure on
both Linux architectures. The ARM64 logs also showed that glibc can list
the in-memory vDSO as an address without a library name. The sandbox now
accepts this address-only entry without adding a mount. It still resolves
file-backed dependencies through the trusted system path and ownership checks.
The ARM64 loader regression and `MCBTODO_REQUIRE_SANDBOX=1 cargo xtask check`
passed locally.

Run `45090905556` reproduced the namespace failure before the Rust build
on both hosted Linux architectures. Both runners reported Ubuntu's
AppArmor user namespace restriction enabled. CI now loads a profile for
the system Bubblewrap binary on affected GitHub-hosted runners and reruns
the same unprivileged namespace probe. CI requires the sandbox integration
tests to execute. Run `45091033055` passed both Linux jobs, including sandbox
tests against extracted release archives, both native macOS and Windows CLI
and Vault jobs, and both cross-target checks.

The packaged Worker smoke requires the expected script exit code and
checks that the ungranted Agent run reaches revision authorization without
contacting the Agent provider. The updated smoke passed with freshly
extracted Linux AArch64 binaries locally and on Linux x86_64 in run
`45091408663`. The ARM64 job in that run stopped at a Daemon test fixture
that could break the Prepare pipe before the supervisor observed Worker exit.
The fixture now preserves the pipe on a separate inherited descriptor while
its descendant holds stdout. The exit assertion, attempt invalidation,
empty history, and descendant cleanup assertions still run.

CI run `45091917141` passed both Linux jobs, both native foreground jobs,
and both cross-target checks after the fixture fix. Release preparation run
`45091524686` passed both Linux artifact smoke jobs and crates.io package
validation, including the publication dry-run.

The Worker Settings update passed `MCBTODO_REQUIRE_SANDBOX=1 cargo xtask check`
on Linux AArch64 with 668 tests and no skips. The debug-binary sandbox smoke
edited the slot count in a pseudoterminal and verified the saved configuration.

## Background service and terminal client (2026-11-23)

`mcbtodo-daemon` owns the timer and workflow runtime. The TUI uses the same
authenticated workspace socket for timer and workflow requests. A missing
or refusing socket prompts the user to start the service. Each input action
sends a single request. Offline calendar and task views continue to read the
Markdown Vault using the existing snapshot and revision rules.

On Linux AArch64, `cargo xtask smoke-systemd --bin-dir target/debug` started a
transient user service with a temporary workspace. The timer advanced after
the first TUI process quit, and a second TUI displayed the same session ID.
The existing forced-restart scenario recovered that session at its last
checkpoint and verified the Markdown and JSON focus reports. The sandbox smoke
also verified the TUI startup and the service hint with an empty `PATH` and
no daemon. Workspace verification passed 668 tests.

CI run `45092935586` passed all six jobs for the terminal-client change.
On the Linux AArch64 development host, `cargo install --path` installed the
client, Daemon, and Worker under `~/.cargo/bin`. The user unit started the
installed Daemon and was enabled for login. Two TUI instances opened outside
the checkout, read the timer status, and quit while the same Daemon PID stayed
active. `cargo xtask smoke-systemd --bin-dir "$HOME/.cargo/bin"` verified timer
continuity and forced-restart recovery in an isolated workspace using those
installed binaries.

The single `cargo install --git` command in the systemd guide also installed
all three packages from GitHub commit `54c6bde` into an isolated Cargo root.
The sandbox and systemd smoke commands passed with that root's `bin`
directory. The systemd smoke verified that a second TUI could reopen the
same timer session and that the service recovered it after a forced restart.

On Linux, the TUI now sends default-Vault Task captures to the Daemon. The
IPC regression verifies lock contention, stale Project revision rejection,
and rejection of a second submission using the same Task UUID. The TUI
regressions cover the service-absent hint, an interrupted response without
replay or local publication, and offline capture against an external Vault.
The AArch64 systemd smoke drove the real TUI Task form and read the resulting
Task from the isolated Vault.

CI run `45095220559` passed both Linux jobs, both native foreground jobs,
and both cross-target checks for default-Vault Task capture.

The Cargo-installed Daemon now ships the user-unit template and can install
it from outside the checkout with `mcbtodo-daemon install-service`. The
Linux AArch64 check installed all three programs into a custom Cargo root
containing a space and `%n`. `systemd-analyze --user verify` accepted the
generated unit with the installed Daemon as `ExecStart`. A repeat install
preserved the existing unit. The systemd smoke exercised the installed
binaries in a transient user unit and verified TUI reopen, Task capture,
timer recovery, and reports. Workspace checks passed 672 tests and the
Daemon's Cargo package listing included the template.

On 2026-11-23, Linux AArch64 testing extended this path to default-Vault
all-day events. The Daemon IPC regression covers invalid ranges, writer-lock
contention, duplicate UUID rejection, and inclusive endpoint persistence.
TUI regressions cover form retention without a service and UUID reporting
after a lost response, with no replay or foreground write. The systemd smoke
created an event through the TUI and read the same busy event on November 30
and December 1 and verified its absence on December 2. CI run `45096863697`
tracks the all-day event change.

The Project and Deadline quick-create forms also use the default Vault's
Daemon on Linux. The IPC test verifies invalid active-period rejection,
writer-lock contention, and duplicate UUID rejection. The terminal smoke
creates both records and reads them back as non-busy calendar entries. A
deadline still creates a Project with a date-only deadline. External Vaults
use the same application command in the foreground.

## Latest Review

On 2026-11-22, the Linux AArch64 release packaging follow-up verified all
thirteen extracted crates with `cargo package --workspace --exclude
mcbtodo-xtask --locked --allow-dirty --offline` under a fresh
`CARGO_TARGET_DIR`. Cargo used a temporary registry for workspace
dependencies. The reused target directory had reported missing methods
while compiling those dependencies at the same package version. The
release-preparation workflow now enables extracted-crate builds. All
thirteen crates passed the Apache-2.0 metadata and package license check.
The online workspace publication dry-run reached the upload check for all
thirteen crates and skipped each upload.
The repackaged binary archive includes the repository license. It passed
the byte comparison, extracted-binary sandbox smoke, and systemd
user-service timer recovery smoke. The archive
SHA-256 was `99ed8558937421ec39f1591fd13dccb91da37003e2c87d7964e1169c32384d96`.
The workspace check passed 662 tests. GitHub Actions and crates.io
publication for this working tree remain pending.

On 2026-11-22, the Linux AArch64 Agent endpoint editor passed
`cargo xtask check` with 662 tests, zero failures, and zero ignored.
The focused regression covers the 24-column layout, unchanged saves,
cancellation, invalid paths, comment-only external edits, and reload after
the selected Provider disappears. It checks the other endpoint, the default
route, exact revision grants, timeout and output budget after a save.
The real Codex 0.171.0 offline Workflow probe edited the endpoint via a PTY
while the Daemon was running and received the next authorized attempt on the
new endpoint. The existing failure, cancellation, and restart scenarios also
passed. The debug and extracted AArch64 release archives passed sandbox
smoke, and the debug systemd user-service recovery smoke passed. Formatting,
Agent registry and example Vault validation passed. Warning-denied cross
type checks passed for the foreground application and tests on macOS ARM64
and Windows x64 with host pkg-config SQLite. Native runtime checks are pending.
The Linux AArch64 archive SHA-256 was
`4a9518b117e631ac19aa1f60ddfeffda297b149ac096da006cfef258491d46ca`.

On 2026-11-22, the Linux AArch64 default Agent route editor passed
`cargo xtask check` with 661 tests, zero failures, and zero ignored. The
focused test exercises a conflicting external edit, checks that only the
inherited grant switches route, and verifies the exact revision, explicit
provider, endpoint paths, timeout and output budget. Agent registry and
example Vault validation passed. The debug sandbox smoke used the PTY to
switch the default route and verified that preferences and Codex parameters
were preserved. The production Codex 0.171.0 Workflow probe passed with
the offline model fixture. Warning-denied foreground and test cross type
checks passed for macOS ARM64 and Windows x64 with host pkg-config SQLite.
The debug systemd recovery smoke and the sandbox smoke using the extracted
Linux AArch64 release archive passed. The archive SHA-256 was
`d5575563cf994211c09dc51e666cbf1b7c056d3a254a4e4791f16f44add4082b`.
Package verification checked the Apache-2.0 metadata and license files for
all thirteen publishable crates.

On 2026-11-22, the Linux AArch64 Settings slice passed `cargo xtask check`
with 660 tests. A focused test covered unchanged saves, invalid output types,
comment-only conflicts, reload, and policy preservation. The PTY smoke
replaced an invalid `codex.toml` through the editor and verified the saved
fields and unchanged preferences and policy. The real Codex 0.171.0 Workflow
probe passed again with the offline model fixture. Warning-denied macOS ARM64
and Windows x64 cross type checks passed with host pkg-config SQLite.
The Linux systemd user-service smoke also passed checkpoint, restart, and
focus report checks with the debug binaries.

On 2026-11-22, the shared Settings input and SQLite snapshot foreign-key
changes passed `cargo xtask check` on Linux AArch64 with 659 tests, zero
failures, and zero ignored. Focused runs passed 141 TUI tests, 20 backup tests,
and 15 SQLite adapter tests. Removing a referenced artifact from the test
database left `integrity_check` reporting `ok`. The regression first showed
that capture accepted the broken reference. Both capture and restore now reject
it before publication and preserve the database bytes. The backup tests also
passed the version-0 workflow snapshot round trip. Registry and fixture
validation, formatting, diff checks, the three-binary build, and the debug
sandbox smoke with its Settings PTY scenarios passed.

On 2026-11-22, the Keyboard page read-error change passed `cargo xtask check`
on Linux AArch64 with 659 tests, zero failures, and zero ignored. The
regression first reproduced the fallback to Preferences. It now verifies
that Enter cannot save from an unreadable Keyboard page, F5 can recover in
place, and saving the binding returns to the original preferences draft.
Formatting, Agent registry and example Vault validation, the three-binary
build, and the debug sandbox and PTY smoke passed. Warning-denied cross
type checks for the foreground app and its tests passed for macOS ARM64 and
Windows x64 with host pkg-config for SQLite. Native runtime evidence is pending.

On 2026-11-22, the Settings command and Worker capacity cleanup passed
`cargo xtask check` with 658 tests and 140 focused TUI tests on Linux AArch64.
The occupied-Worker regression passed before and after removing the launch-time
check. Workspace checks, example Vault validation, the three-binary build,
sandbox and PTY smoke, and systemd timer recovery passed. The foreground
application and its test code passed warning-denied cross type checks for
macOS ARM64 and Windows x64 using host pkg-config for SQLite.

On 2026-11-22, the Settings page isolation change passed 659 workspace tests
on Linux AArch64 with zero failures and zero ignored. The regression covers
rendering an unreadable preferences file, saving a key binding, opening
Agents, and reloading preferences after file access is restored. Example
Vault validation, Agent registry validation, and sandbox smoke against the
new debug binaries passed. The foreground application and its test code
passed warning-denied cross type checks for macOS ARM64 and Windows x64
using host pkg-config for SQLite. Native runtime validation remains pending.

On 2026-11-22, the Keyboard settings cleanup passed 658 workspace tests on
Linux AArch64. The Unicode binding round trip uses the production snapshot,
publisher, and reader. Agent registry and example Vault validation passed.
The three-binary build and sandbox smoke passed. The PTY now waits for the
Week grid after leaving the Actions menu. `Week` also appears in the menu's
header and could send the next key before Escape was processed.
The foreground application and its tests passed warning-denied type checks
for macOS ARM64 and Windows x64 using host pkg-config for SQLite. The native
jobs are configured in this worktree and still need results for this source.

On 2026-11-22, `cargo xtask check` passed 657 tests on Linux AArch64 with
zero failures and zero ignored. Agent registry validation, the three-binary
build, example Vault validation, and sandbox and PTY smoke passed. The
settings regressions exercise reloading after a conflicting save and
preserving the draft on a read failure. The PTY exercises reloading the
startup repair form and continuing with the corrected timezone. The
foreground application and its tests passed warning-denied macOS ARM64 and
Windows x64 cross type checks using host pkg-config for SQLite.

The production Codex 0.171.0 Workflow probe passed against an offline model
fixture after removing the Worker's metadata precheck. The Codex adapter
test verifies that a capability mismatch does not launch the process.
The host tests verify that revision authorization, deadlines and cancellation
still apply with an adapter that supplies only identity metadata.

On 2026-11-22, shared Agent discovery and the TUI Agents page passed
`cargo xtask check` with 655 tests on Linux AArch64. Agent registry validation,
example Vault validation, sandbox and PTY smoke, and the Codex 0.171.0
offline production Workflow probe passed. The PTY exercised
opening the read-only page from a modified preferences draft, pressing Enter,
returning with Escape, and then saving the draft. The foreground binary
passed warning-denied macOS ARM64 and Windows x64 cross type checks using
the host pkg-config SQLite path. Native runtime and bundled-SQLite builds
still require target toolchains.

On 2026-11-22, the Settings startup cleanup passed `cargo xtask check`
with 655 tests on Linux AArch64. Agent registry validation, example Vault
validation, the three-binary build, and sandbox and PTY smoke passed. The
PTY exercised startup with an obsolete theme override, immediate theme
application, saving under `NO_COLOR`, and changing the startup preference
when the saved view could not be read. An isolated `cargo run --locked`
session also saved `Asia/Shanghai` and the high-contrast theme.

On 2026-11-22, the unified Settings save path passed `cargo xtask check`
with 655 tests on Linux AArch64. The Linux sandbox and PTY smoke and example
Vault validation also passed. A separate isolated `cargo run --locked`
session saved `Asia/Shanghai`, restore-last-view startup, and the
high-contrast theme through Settings.

On 2026-11-22, `cargo xtask check` passed 654 tests on Linux AArch64 with
zero failures and zero ignored. The Agent registry, three-binary build,
sandbox and PTY smoke, and example Vault validation passed. A separate
isolated `cargo run --locked` session saved all three preferences through
Settings.

The Worker now applies the optional host output byte budget to both Agent
adapters. The Unix Agent integration test and the real Codex 0.171.0
offline Workflow probe accepted a result at the exact UTF-8 byte limit and
recorded `outcome_unknown` when the result exceeded the budget by one byte.
Both checks verified no step was committed and the run was stable on restart.
The Unix Agent test also verified that the over-budget Attempt retained its
budget reservation.

On 2026-11-22, the Settings and Agent diagnostics checkpoint passed 652
workspace tests on Linux AArch64 with zero failures and zero ignored.
Formatting, Agent registry, three-binary build, sandbox and PTY smoke, and
example Vault validation also passed. In a separate terminal session,
`cargo run` opened Settings in an isolated home and saved `Asia/Shanghai`,
restore-last-view startup, and the high-contrast theme. The terminal showed
the restart hint for the changed timezone. Focused tests cover view restore
from disk, preference conflicts, unchanged saves, and theme override parsing.

The same review includes the Codex 0.171.0 offline probe. The real app-server
completed initialization, streaming, and cancellation against the model
fixture. The production Workflow probe exercised revision authorization,
approval, disconnect, timeout, crash, cancellation, and Daemon-death recovery.
Online authentication still requires a separate run.

Workspace and test code passed cross-target type checks for macOS ARM64,
Windows MSVC x86_64, and Linux x86_64. The checks used pkg-config SQLite and
treated Rust warnings as errors. Target SQLite linking and native macOS and
Windows runtime verification remain open.

The startup Settings repair passed the workspace Clippy and test gate on Linux
AArch64. The terminal smoke verified that Escape preserves the broken config,
invalid timezone input keeps the form open, and saving the corrected timezone
opens the calendar in that zone. The example Vault passed validation.

The Settings snapshot and Worker Agent checkpoint passed 649 workspace tests
on Linux AArch64 with zero failures and zero ignored. A regression test
reproduced an external comment edit being overwritten by the old save path.
The new save path rejects it and preserves the file. The compact repair test
verifies the explicit replace action at 24×5, cancel, conflict, and successful
replacement. The Worker process test confirms an uncertain result carries only
its fixed status and Attempt identity after cancellation. The three binaries
built, the workspace terminal smoke passed, and the example Vault validated.
The Codex 0.171.0 offline app-server and production Workflow probe passed with
the rebuilt Worker.

The preceding Agent cleanup checkpoint passed 647 workspace tests on Linux
AArch64. Codex 0.171.0 passed the offline app-server exchange and the
production Workflow probe, including revision grants, approval, disconnect,
timeout, crash, cancellation, and Daemon-death recovery. This probe uses an
offline model fixture.

The Keyboard settings checkpoint passed 644 workspace tests on Linux
AArch64 with zero failures and zero ignored. TUI tests cover the visible
focus row at 24×5, cancellation, a duplicate binding, and a conflicting
external edit. The PTY smoke verifies a saved key in the current session
and after restart and checks the other configuration files byte for byte.
Example Vault validation passed.

The grouped Settings checkpoint passed 642 workspace tests on Linux AArch64
with zero failures and zero ignored. A 64×14 render verifies the General and
Appearance headings and reverse focus order. The 24×5 monochrome render
keeps the focused timezone and validation feedback visible after a failed
save. The default Linux workspace smoke verifies both headings through the
terminal entry point, immediate theme changes, startup preferences after
restart, and byte preservation of the three independent configuration files.
`cargo run --locked` opened Settings through the default TUI entry point.
Example Vault validation passed.

The Settings and Worker framing checkpoint passed 639 workspace tests on
Linux AArch64 with zero failures and zero ignored. The PTY smoke verified
the `--timezone` override notice and applied the saved timezone after
restarting without the flag. Example Vault and Agent registry validation
passed. The extracted release binaries passed PTY and sandbox smoke,
systemd restart smoke, and the offline Codex 0.171.0 production Workflow
probe. The same archive check under a shared writable ancestor showed that
a refused Worker installation no longer opens the model connection.
The delivery rollback and schedule-disable tests wait for the fixture lock
when assembling a page. The daemon's independent contention test still
exercises the production poller's nonblocking read and durable cursor.

The Settings and configuration checkpoint passed 637 workspace tests on
Linux AArch64, with zero failures and zero ignored. Empty and partial
`config.toml` files use the shared preference defaults. Invalid values and
unknown keys remain errors. A lone Agent endpoint supplies the default
provider, and the mixed configuration retains its Unix default. Agent
authorization still resolves an exact capability and revision.

`cargo xtask check`, Agent registry validation, example Vault validation,
the three-binary build, and both Linux smoke tasks passed. The terminal
smoke starts with a partial config and verifies defaults, per-field apply
timing, theme changes in the active session, saved startup preferences after
restart, theme preference saves under `NO_COLOR`, and byte preservation of
`agents.toml`, `codex.toml`, and `keymap.toml`. A separate `cargo run` session
also opened Settings and saved a theme in an isolated home. The Rust
cross-target checks covered the applications,
`xtask`, and their test code on macOS ARM64, Windows x86_64 MSVC, and Linux
x86_64 with warnings denied. These were type checks without target SQLite
linking. Native macOS and Windows runtime results are pending. The Cargo
package audit verified the Apache-2.0 metadata and license entry for all
13 publishable packages. The Linux AArch64 release build was packaged and
extracted. The extracted binaries passed the sandbox and systemd restart
smoke tests.

The Settings checkpoint passed 630 workspace tests with zero failures and zero
ignored on Linux AArch64. Example Vault validation, sandbox workspace smoke,
and systemd restart smoke passed. The PTY verifies saving all three settings, applying the theme
by checking a rendered border before restart, restoring the saved view and
timezone on restart, and preserving the bytes of the independent configuration
files. The Agent smoke now uses the same `[providers]` schema as the Worker.
The offline Codex 0.171.0 probe passed both the app-server exchanges and
the production Workflow scenario with Unix and Codex routes. An executable
that exits before answering `initialize` produces no model request and
does not fall back to the other route.
The native macOS ARM64 and Windows x86_64 CI jobs now include Settings
interaction, rendering, and file persistence tests. Their selected suites
passed locally on Linux. Rust cross-target checks passed for those targets and
Linux x86_64, with native macOS and Windows results still pending.

The Codex Worker starts app-server directly. It does not generate or compare a
protocol schema hash or gate the handshake on an exact Codex release. On Linux
AArch64, the available Codex installation passed the offline protocol and
production Workflow probes. The probes exercised structured streaming,
interruption, revision grants, timeouts, cancellation, crash handling, and
Daemon-death recovery. `cargo xtask check` passed 619 workspace tests with
zero failures and zero ignored. The example Vault validated.

The timed calendar recurrence checkpoint passed 615 Linux AArch64 workspace
tests. A rule uses the template zone and preserves elapsed duration,
including when the display zone shows the continuation on another date.
Tests cover both fold choices, gap handling, the explicit offset of the
first instance, and the per-rule projection limit. The Vault rejects a
date anchor mismatch and identifies the source document. The Day and Agenda
test retains the exact occurrence through reload and displays both offsets
when a clock change puts the local end label before the start label.
The example Vault, three binaries, PTY and sandbox smoke, and systemd
restart smoke passed.

The preceding SQLite backup checkpoint passed 610 workspace tests and
17 focused backup tests on Linux AArch64. The CLI stages and publishes
the files. The SQLite adapter captures WAL commits in a standalone
snapshot and validates the expected store without altering its bytes.
Tests reject WAL-mode snapshots without creating sidecars and reject a
wrong store even when the manifest matches. PTY, sandbox, and systemd
restart smoke passed.

The portability checkpoint passed 608 tests on Linux AArch64 and checked
the contents of 13 Cargo packages. The three applications and `xtask`,
including their tests, passed cross-target type checks with warnings treated
as errors for Darwin ARM64, Windows x64 MSVC, and Linux x86_64. Linux-only
operations return an explicit unsupported diagnostic on other platforms.
Native macOS and Windows startup and security behavior still require
native runner evidence.

The desktop notification checkpoint passed 607 Linux AArch64 workspace tests
with zero failures and zero ignored. The Linux `notification_desktop` doctor
check shares the Daemon's session detection and passively checks the current
process `PATH` for an executable `notify-send`. It reports `skipped` without
a configured session, `warning` without an executable helper, and `ok` when
the preflight checks pass. The fixture verifies that no helper is launched,
no session address is disclosed, and no runtime files are created.
Formatting, diff, registry, and example Vault validation passed alongside the
three-binary build, sandbox and PTY smoke, and systemd restart smoke.

The grapheme anchor checkpoint passed 606 Linux AArch64 workspace tests
with zero failures and zero ignored. The test first reproduced lost
position on resize within a long line. The viewport now keeps its
place when widths remove wrapping or heights show the entire preview.
The Proposal regression verified 24×5, 28×5, 88×5, and 24×12 buffers,
return to 24×5, and navigation to the final line. Registry and example
Vault validation, three-binary build, and sandbox and PTY smoke passed.

The review viewport checkpoint passed 605 Linux AArch64 workspace tests
with zero failures and zero ignored. All 122 focused TUI tests passed.
The shared Proposal and Workflow trace viewport restores the visible source
line when the terminal width changes or wrapped mode resumes after vertical
navigation. The mixed-language Proposal regression exercised 24×5 and
70×5 buffers. Formatting, diff, registry, example Vault, three-binary build,
and sandbox and PTY smoke checks passed.

The completion event contention checkpoint passed 604 Linux AArch64
workspace tests with zero failures and zero ignored. Nine Daemon completion
trigger tests and nineteen Vault completion tests passed. The Daemon's event
page reports `Busy` when the Vault writer holds the lock. The background
poller waits one second, retains its durable cursor, and delivers the
completion once after the lock is released. CLI export waits for the lock.
Registry and example Vault validation, the three-binary build, sandbox and
PTY smoke, systemd restart, and the real offline Codex probe also passed.

The Worker Start deadline and Codex installation checkpoint passed 603
Linux AArch64 workspace tests with zero failures and zero ignored. Eight
focused Worker tests passed. Start delivery uses the earliest of the
startup, control transfer, and execution deadlines. Started is durable
before delivery, and unsuccessful delivery records `outcome_unknown`.
Nine `doctor` tests passed. The Linux `agent_codex` check shares the
Worker's configuration validation and executable trust checks. The fixture
verified no model socket connection or runtime files created, and no
instructions disclosed in the report.

Policy and Vault fixture validation, the three-binary build, sandbox and PTY
smoke, systemd restart, and the real offline Codex production Workflow probe
passed. The adapter validates the protocol messages it consumes and does not
depend on a generated schema hash.
All three application entry points and their tests passed warning-free type
checks for Darwin ARM64, Windows x64 MSVC, and Linux x86_64.

The Daemon IPC checkpoint passed 602 Linux AArch64 workspace tests with
zero failures and zero ignored. The twelve focused server tests verify that
the main loop yields on a partial frame, a completed frame gets a response,
and an expired partial Start releases admission without starting the timer.
The socket thread handles one accepted connection at a time. Only the main
loop dispatches complete commands. The request and response retain separate
one-second transfer deadlines and same-user authorization.

Policy validation, example Vault validation, the three-binary build, sandbox
and PTY smoke, systemd restart smoke, and the offline Codex production
Workflow probe passed. The current adapter has no schema SHA-256 gate.
The three application entry points and their tests also passed warning-free
cross-target type checks for Darwin ARM64, Windows x64 MSVC, and Linux x86_64.
Native runtime evidence for this checkpoint is Linux AArch64.

The sandbox installation checkpoint passed 600 Linux AArch64 workspace
tests with zero failures and zero ignored. `doctor` and production execution
share the Bubblewrap, Bash, and Nushell installation checks. Preflight checks
the fixed paths, file trust, and execution access without launching a tool.
Sandbox smoke separately exercised actual process startup and isolation.

The Worker budget and local transport checkpoint passed 599 Linux AArch64
workspace tests with zero failures and zero ignored. The Worker budget starts
just before the Daemon first attempts to write Start, after Started is durable.
The regression accounts for time spent sending Start. Linux clients and the
Daemon now share the Unix socket peer identity and bounded I/O adapter. The
Daemon retains same-user authorization and distinct one-second request and
response budgets. Registry and example Vault validation, the three-binary
build, sandbox smoke, and systemd restart smoke passed. The real offline Codex
0.171.0 probe passed app-server initialization, interruption, streamed
completion, and production Workflow tests.

The compact TUI checkpoint passed 597 Linux AArch64 workspace tests with
zero failures and zero ignored. Registry and fixture Vault validation,
the three-binary build, and the sandbox smoke passed. The smoke includes
compact PTY interaction and startup with an empty `PATH`. Graph uses the
full width for its list when there is insufficient height for dependency
details. The 96×5 buffer preserves the selected mixed-language title.
At 24×5, Agenda preserves its selected row, mode, feedback, shortcuts,
and Vault warning. Workflow distinguishes an empty queue from an unavailable
queue. Native macOS and Windows CI now request the in-memory renderer and
ScreenModel suites. Both Actions definitions parse after quoting the Windows
MCP test filter as a YAML block. Native runner results are pending.

The recurring calendar and Worker lifecycle checkpoint passed 594 tests
on Linux AArch64 with zero failures and zero ignored. Agent registry and
example Vault validation, the three-binary build, sandbox smoke, and systemd
restart smoke passed.

Month, Week, Day, Agenda, and Timeline now use the same bounded occurrence
query. A date event that begins in the previous month retains its full span
and original occurrence identity. Tests cover 1024/1025 overlaps, sparse
rules with long spans, an actionable error in a 24×5 buffer, and refresh
after a recurring source file changes.

The Linux Worker exit probe signals remaining group members before reaping
the leader and preserves its original status. The new regression first
observed a live descendant after a successful Worker exit. It now verifies
descendant termination, a cached successful exit, and repeat termination.
During startup, the Daemon also checks Worker liveness before publishing
Started. A fixture exits while a descendant holds both protocol pipes.
The next supervisor tick terminates the group, invalidates the claim,
records Failed with zero steps, and releases the slot.
The bounded final-frame drain and Started attempt fencing retain their
existing behavior. The same checkpoint passed the real offline Codex
`0.171.0` app-server and production Workflow probe.

The Week compact checkpoint passed 587 Linux AArch64 workspace tests
with zero failures and zero ignored. At 42×18 the daily counts and focused
preview remain visible. At 24×5 in monochrome the selected date and its
occurrence preview share one line, with Chinese alignment and an intact
right border. Enter opens the same occurrence in Day.

The Linux AArch64 workspace check passed 586 tests with zero failures and
zero ignored. Registry and example Vault validation, three-binary build,
sandbox smoke, and systemd restart smoke also passed. A separate tmux
session in an isolated home launched Month with plain `cargo run`. At
28×6, Space opened Actions, End and Home reached both ends of the menu,
Escape restored the selected date, and `q` exited with status zero.

Month now projects the title, date labels, focus, and busy and deadline
markers before rendering. The footer fits complete shortcut groups by
display width, including remapped Chinese keys. Actions shares the
scrolling viewport used by the view and creation menus. Buffer tests and
the 28×6 smoke check cover the compact layout and menu interaction.

The Workflow Runtime checks for atomic batches and durable cursors before
starting its Vault watcher and completion poller. Daemon assembly has
already bound its endpoint and opened the repository at this point.
Tests reject each missing capability and accept the SQLite repository.

The protocol crate exposes its shared Timer and Notification DTOs on
every target, so `mcbtodo-ports` is a common dependency. The Linux
transport dependency stays target-specific. Rust checks with warnings
denied passed for the applications and their tests on Darwin ARM64,
Windows x64 MSVC, and Linux x86_64 using cross pkg-config for SQLite.
Native CI now requests application test builds on macOS and Windows.
Native linking, execution, and platform-specific security are still
awaiting evidence on those two systems.

The Linux AArch64 Codex offline probe passed real app-server initialization,
thread start, interruption, and streamed completion. The production Workflow
probe passed configuration, protocol capability checks, revision grants,
approval, disconnect, timeout, crash, cancellation, and Daemon-death
recovery. No generated schema hash is part of the current acceptance gate.

The Daemon's wake calculation includes the deadlines for Worker startup,
execution, cancellation grace, and final-frame drain. The regression first
observed a 50 ms sleep at an elapsed startup deadline. It now verifies
immediate wake at deadlines and normal polling during the cancellation grace
period. The Agent and Script paths share this calculation. Attempt fencing,
budget reservation, and result commits retain their existing transactions.

Proposal details and Workflow traces share the `PannableReview` viewport.
It wraps by default and retains the first visible source line when toggling
to unwrapped inspection. In-memory 24×5 tests check mixed Chinese and Latin
text, borders, and navigation through the production key handlers. The
583-test Linux AArch64 workspace checkpoint passed with zero failures and
zero ignored. Example Vault and Agent registry validation passed. Workspace
smoke exercises a real 28×6 pseudoterminal with `NO_COLOR=1` and an empty
`PATH`. It checks Help at the start and end, sends Home and Escape, and
verifies that Month returns to the selected date. The supervised Worker
checkpoint also passed the systemd restart smoke and the offline Codex
`0.171.0` production probe.

The Daemon uses `NotificationStore` for Inbox, reminder scanning, and
desktop-delivery persistence, and `FocusSessionStore` for timer checkpoints
and reports. Daemon assembly supplies the Timer and Workflow Runtime with
separate SQLite connections while retaining a shared transaction boundary
for operations in the workflow repository. IPC re-exports the existing
notification and timer DTO names. JSON fixtures pin the notification wire
shapes. An in-memory focus store verifies that a failed Pause leaves the
timer running and that retry includes the elapsed interval.

Reminder delivery retains a popup acknowledgement in memory when saving the
result fails. The next poll retries the acknowledgement before claiming any
new work. Restart recovery marks interrupted deliveries with exhausted
budgets as failed and keeps the Inbox entry available. The SQLite schema is
unchanged. The Linux AArch64 reminder checkpoint passed 580 workspace tests
with zero failures and zero ignored, Agent registry and sample Vault
validation, sandbox smoke, and systemd restart smoke. The preceding
578-test focus checkpoint also passed the real offline Codex production probe.

The Daemon uses `WorkflowProgressStore` for host transitions, step appends
and operator resolution records, and `ScriptAuthorizationStore` for exact
revision and node grants. A two-connection SQLite regression reproduced a
host step append consuming an active Worker step. The immediate writer
transaction now rejects this append and host terminal transitions while an
Attempt is active. It permits same-node cancellation and still accepts the
fenced Worker result. The schema stayed at version 1. The workspace check
passed 576 tests and the sample Vault, sandbox smoke, systemd restart smoke
and Codex 0.171.0 offline workflow probe passed.

Workflow approval now binds the decision to the UUID, revision, waiting node,
step count and exact stored review text. The Daemon recomputes the pending
review and rejects missing or stale identifiers. SQLite compares the loaded
revision and detail when committing the step. The regression uses separate
connections to prove that an intervening refresh prevents a stale append.
The TUI confirmation projects the reviewed snapshot and configured key hints.
In a 24×5 monochrome terminal the remapped `y` still scrolls and a fresh
`Enter` submits. With the default keymap a fresh `y` submits.
CLI automation must retain `approval_review` alongside the `detail` it shows
to operators and pass the identifier with `--review`. Upgrade the client
and Daemon together. The step history schema and rejection request are
compatible with existing data and clients respectively.

The Daemon reads traversal history, step metadata and bounded artifact chunks
through `WorkflowHistoryStore`. The SQLite contract test reopens the database,
decodes an existing JSON trace, resumes an artifact at its returned byte
offset, and rejects invalid stored traces. The Daemon exercises the existing
step and artifact response shapes through its command path.

The Daemon uses `WorkflowRunCatalog` for lookup, reverse-insertion-order
pagination and manual enqueue. SQLite binds the run UUID in one write
transaction and returns the persisted row. The host validates publication
and distinguishes matching retries from collisions. Two SQLite connections
submitting different parameters for the same UUID observed the same winning
binding. Reopening the database found a single pending run.

The Daemon registers, inspects, and disables rules through
`WorkflowScheduleStore`. Registration retains atomic installation of the
schedule and its trigger rule. Disabling now reads the schedule by UUID
inside the writer transaction. A two-connection regression reproduced the old
path overwriting a missed-occurrence count and verifies preservation across
disable, repeat disable, and restart. Schedule summaries now belong to
`mcbtodo-workflow`. The IPC re-export and a JSON fixture preserve the stored
and wire shapes.

On 2026-11-22, all seven priority ports had production callers. The stable
interfaces cover Agent requests, local transport, Worker process control,
script sandboxing, Workflow Attempt persistence, completion-event reads, and
desktop notifications. The Worker applies the exact capability and revision
grant and caps the deadline before the Unix provider executes. A recording
provider test exercises this authorization boundary independently of socket
traffic. `worker_release_fences_start_and_preserves_uncertainty_with_a_substitute_store`
verifies that Started is persisted before Start is sent, an unsuccessful
durable write sends no Start bytes, and a failed pipe write records an
uncertain outcome without a second write from `BufWriter::drop`. CLI and
Worker clients now share the Linux `LocalTransport` connector. The CLI
rejects an unexpected peer UID before framing the request.

Daemon workflow and focus connections moved to `mcbtodo-storage-sqlite`.
The workflow adapter registers the existing version 0 schema as version 1
inside its initialization and recovery transaction. Tests verify preservation
of history, notifications, and cursors and rejection of unknown schema versions.
The completion journal now uses the same SQLite adapter crate. Vault still
owns the writer lock and decides completion recovery from the old and new
Markdown digests. The database path, schema and recovery protocol are
unchanged. The Vault package depends on `rusqlite` only in tests that
construct fixtures and verify persisted state.
The completion poller reads source facts through `CompletionEventStore` and
consumes pages through `CompletionDeliveryStore`. Its substitute-store test
verifies that a failed commit backs off and resumes from the durable cursor.
The SQLite regression verifies rollback of the run, receipt and cursor, then
deduplication after reopening the store. The production adapter retains its
existing transaction and schema.

Clock trigger definitions and validation now live in `mcbtodo-workflow`.
Daemon registration and SQLite delivery compile the same trigger. The IPC
module re-exports the original type names and a fixture checks the stored
Cron/fold and replay policy JSON. Tests cover the replay bounds, input
length and timezone validation, the DST fold, and restart delivery for
interval and Cron rules. The SQLite schema and delivery transaction stayed
the same.

The Daemon uses `WorkflowQueueStore` for wake timing and clock delivery.
The SQLite adapter selects the next wake in a coherent read snapshot and
keeps clock rules eligible while the Worker slot is occupied. Reading a due
run grants no execution rights. Occurrence inserts, counters and the rule
cursor share the existing transaction. A regression fails the second insert,
checks rollback, then reopens SQLite and verifies exact replay of the due
occurrences and deduplication on the next tick. Run summary types now belong
to `mcbtodo-workflow`. The protocol re-exports them with the same wire shapes.
The Linux AArch64 queue checkpoint passed 566 workspace tests with zero
failures and zero ignored, example Vault validation, sandbox and systemd
smoke, and both real offline Codex probes with Codex `0.171.0`. Thirteen
publishable packages passed archive creation and Apache-2.0 verification.
The macOS ARM64, Windows x64 and Linux x86_64 cross-checks type-checked
Rust tests with warnings denied and pkg-config SQLite.

The TUI view selector uses one scrollable row per view and displays its
configured direct key. The 24×5 monochrome regression scrolls through all
nine views, checks the current-view marker and returns to the main menu on
Escape. The Linux AArch64 checkpoint passed 564 workspace tests with zero
failures and zero ignored. The example Vault, sandbox smoke, and Apache-2.0
archive checks for all thirteen publishable crates also passed.

The Linux AArch64 clock-rule slice passed 563 workspace tests with zero
failures and zero ignored. Agent registry validation, example Vault
validation, sandbox and systemd smoke, and the real Codex 0.171.0 offline
probe passed. All thirteen publishable packages passed dirty-tree
`cargo package --no-verify` and archive license checks.

The Linux AArch64 offline Codex probe verified the app-server handshake and
restrictive ephemeral `thread/start` settings through the protocol messages
the adapter consumes. It also started a turn and correlated its interruption
response with the terminal `interrupted` notification. An isolated loopback
Responses fixture verified streamed Chinese text and typed result decoding
with the real app-server. The Worker selects its configured provider
after exact capability and revision authorization. The optional Workflow probe configures
`provider = "codex"` and exercises the production CLI, Daemon and Worker in a
temporary home against a same-user Unix model fixture. It verifies revision
and malformed configuration rejection before connection, unsupported
configuration rejection before model traffic, and approval before downstream
execution.
Model disconnect, stalled response, app-server SIGKILL, Workflow cancellation
and Daemon SIGKILL each leave a durable uncertain run. The probe observes
connection closure and exit of the sampled process tree after Daemon death.
It also verifies late-result fencing and same-ID resubmission without replay.
The Worker reports observed Agent uncertainty using a fixed, attempt-bound
status. The Codex probe verifies that disconnect and app-server SIGKILL
leave an Agent diagnostic and zero steps, including after restart. The
status carries no Provider error text. The Daemon retains its reservation
and continues to own the durable outcome.
The Worker fences results returned after the deadline or cancellation. The
adapter bounds frame size, cumulative response bytes, stream messages, and
assistant text.

The Worker routes both the Unix socket and Codex implementations through the
exact route resolved by `AgentPolicy`. Policy tests reject duplicate grants,
unsafe endpoints, and unconfigured providers. The selected adapter checks its
execution prerequisites. Policy,
revision authorization, budget, deadline, cancellation, idempotency, and final
result submission remain host-owned.

Linux `doctor --json` reports whether `agents.toml` is absent, usable, or
rejected and identifies the selected backend for a usable policy. The
Worker and doctor use the same bounded, no-follow policy loader.
The Unix and Codex adapters report `Unchecked` health before actual
dispatch. Tests cover private path validation, malformed
and oversized policy, diagnostics without policy contents, and inspection
without any connection to the configured endpoint. The 554-test Linux
AArch64 workspace check, sandbox smoke, and real offline Codex probe passed
with these changes.

The Linux AArch64 workspace check passed with 553 tests, zero failures and
zero ignored after the Outline and Inbox presentation changes. Agent registry
validation, example Vault validation and the sandbox smoke passed for both.
The completion origin attribution slice passed separately with 548 tests.
The preceding 544-test workspace check and sandbox smoke passed after the
Agent outcome protocol change. The Codex `0.171.0` probe also passed with
the three workspace executables.
The first-run smoke rendered Month and exited on `q` with an empty `PATH`.
Live upstream authentication, managed distribution, shared resource
scheduling, and the public plugin protocol remain pending.

The Workflow repository commits the Agent budget reservation, run transition
and Attempt claim together. A forced Attempt insert failure rolls back the
budget and run state. The test verifies no Worker activation and one claim
after reopening SQLite. A separate production Worker regression induces a
sandbox setup failure before script release and verifies a failed step with
its diagnostic, no artifact, Worker reaping and the same trace after
reopening SQLite. A release or post-release infrastructure failure with
uncertain execution still takes the `outcome_unknown` path.

The Daemon moves Prepare writes and Ready waits to a background thread.
Process ownership and Started, Start, and result submission remain in the
Daemon loop. A stalled-child regression checks that the runtime tick returns
quickly, the slot is reaped on expiry, and queued work proceeds. An additional
test cancels the run during the handshake and verifies that reopening the
database does not replay the cancelled Attempt. The packaged sandbox smoke
uses the CLI to start and pause the timer while the Worker holds back its
Ready message. It then sends malformed Ready and observes a failed run with
no committed steps and a reaped Worker. Process launch, reaping, and the
short bounded Start write remain synchronous.

After reaping a Worker, the Daemon polls for the final protocol frame over a
bounded 100 ms window and keeps the execution slot occupied. The main loop
can service requests while the reader catches up. The Daemon tests verify
delivery across ticks, expiry and cancellation fencing, and durable outcomes
after reopening SQLite.

`vault completion-events --after 0` exports bounded JSON Lines pages of
committed events with a local sequence and the pre-completion Markdown
digest. The regression verifies cursor resume, rejection of negative cursors,
and exact SHA-256 output. The existing bare event output retains its shape.
New CLI completions and applied completion proposals record the installation
UUID in the pending row. The Linux AArch64 tests verify first-use races,
export from another home, historical version 1 migration, origin validation,
recovery, a byte-exact backup and restoration to a new identity. Logical
export with `--logical` uses that same bounded page and projects the facts
into a `mcbtodo.completion-event/v1` envelope. The CLI test checks the full
envelope against the original event, cross-home stability, and rejection
without a cursor. Manual logical import uses the existing intent protocol
and a durable UUID index. The CLI regression verifies partial-file retry,
identity conflicts and export from a second home. Vault regressions verify
versions 1 and 2 migrating to version 3, recovery before and after Markdown
publication, stale revisions and preservation of local evidence. The
Daemon regression verifies that a matching rule fires only for the local
completion when both a historical import and a local completion are
present. Device authentication, transport and merging divergent edits
remain planned.

The TUI projects a `ScreenModel` before drawing. Shortcut presentation is now
also projected from the loaded `Keymap`, so the main footer, view menu, help
overlay, and Workflow footer use the same configured keys as input handling.
The creation-menu projection uses the shared scroll viewport. A 24×5
rendering regression exercises scrolling to the last action, the right border,
returning to the previous menu, and opening the time-block picker.
The regression covers custom movement, menu, help, reload, quit, picker and
view bindings. The new tests verify narrow
and short Month layouts, Chinese and Latin column alignment, and
high-contrast help and error placement. The 27×5 no-color Agenda case checks
Chinese column alignment and the placement of the mode, border and contextual
footer. Week presentation now supplies rendered cell labels, compact
summary counts and a focused occurrence preview from `WeekScreenModel`.
The in-memory fixture verifies the start, continuation and preview of a
mixed-language overnight event. Existing buffer and navigation tests cover
wide and narrow Week layouts and opening the same occurrence in Day view.
The creation form projects its labels, values, focus and primary action
without accessing Vault in the renderer. Two in-memory rendering regressions
cover a focused date and footer in a six-row monochrome terminal and the
cursor after a long mixed Chinese and English title in high contrast.
`WorkflowScreenModel` projects the run list, details, timezone, trace title
and footer. In-memory tests exercise a 30×6 high-contrast mixed-language
view and check that a run with zero steps has no trace shortcut. Deadline
and time-block regressions check the focused input and dismissal in 28×5.
The shared picker keeps Chinese document and WorkUnit matches visible at
that size. A 28×4 monochrome fixture checks the active time, focus modifier,
validation message, and dismissal. Existing time-block tests cover DST and
stale revision fencing.
Outline checks cover a blocked WorkUnit in 23×5 monochrome, source selection,
mixed-language alignment, and an in-memory high-contrast hierarchy and empty
state. Inbox checks cover a title-first row and its right border in 24×5,
project picker focus and dismissal, assignment using the observed revision,
and footers that reflect the actions available in empty lists.
Day and Agenda now project focus and the renderer uses projected menu and
footer data. The 24×5 timer menu keeps Escape and supported shortcuts visible
while preserving the calendar row and its border.
An end-to-end smoke failure exposed
that raw transcript searches missed text split by Ratatui cursor updates.
The smoke now evaluates the rendered PTY screen through `vt100`, and the
Linux AArch64 sandbox smoke passed after that change. The visual stage still
needs broader fixtures and more extraction of view formatting.

On 2026-11-22, the application suite gained
`serialized_vault_calendar_projects_business_objects_across_display_zones`.
It loads serialized Project, Task (including a dated deadline), WorkUnit,
all-day event, and zoned event Markdown through `Vault::load`, then checks
owned calendar projections and stable dates for all-day events and date-only
deadlines. The Tokyo event and timed deadline appear on November 25 in Shanghai
and November 24 in Los Angeles, with the deadline's UTC instant preserved.
`serialized_calendar_preserves_the_dst_fold_and_rejects_a_gap_endpoint`
projects a New York event across the repeated hour with both offsets and a
20-minute elapsed duration. Its spring-forward fixture fails validation with
the source document and timezone in the diagnostic. Recurring exact-time
calendar rules across transitions remain outside this acceptance.

Review performed on 2026-11-22 against the visible GitHub Actions history and
the current local worktree. The hosted Linux CI failures include two distinct
causes: `sha2` 0.11.1 no longer formats its digest with `LowerHex`, and Linux
Worker tests failed to observe sandbox startup under the existing test
environment and timing. The local changes encode digests with `hex::encode`,
and start the Agent budget after the Worker Start handshake. Linux CI installs
Bubblewrap because workspace tests and packaged smoke execute scripts in the
Linux namespace sandbox; it installs Nushell because the supported Nushell
runner has an integration test. These are CI test prerequisites, not
application runtime dependencies or instructions to install them for TUI use.
The optional `cargo xtask smoke-systemd` now runs directly in Rust. On Linux
AArch64 the built `xtask` passed with a PATH containing only Bubblewrap and
the systemd tools. It verified recovery of the same session and the checkpoint
duration after a forced service restart, plus both report formats. The fixes
still require a hosted Linux CI rerun.

On 2026-11-22, the default TUI startup path was changed to initialize the
standard workspace only when no explicit `--vault` is supplied. This lets a
new user run `cargo run` without first invoking `mcbtodo init`, while preserving
existing configuration and avoiding hidden initialization for external Vault
use. The two focused CLI tests passed, and an interactive PTY run with a
temporary `HOME` rendered the Month view and exited cleanly on `q`. The full
`cargo xtask check`, Agent registry validation, example Vault validation, and
Linux sandbox smoke also passed on this worktree. The test suite verifies
layout creation and external-Vault isolation; a real clean-home graphical
session and ongoing daily-use reliability remain outside this check.

On 2026-11-22, `cargo check --locked --tests` passed on the Linux AArch64
host for `aarch64-apple-darwin`, `x86_64-pc-windows-msvc`, and
`x86_64-unknown-linux-gnu`, with Rust warnings denied and no Cargo manifest
warnings. The checked packages were `mcbtodo-domain`, `mcbtodo-ports`,
`mcbtodo-agent-codex`, `mcbtodo-platform`, `mcbtodo-protocol`,
`mcbtodo-execution`, and `mcbtodo-worker`. Worker protocol process tests
remain Linux-scoped because they start the real Linux executor. On macOS
and Windows the cross-check includes the unsupported Worker entry point,
with Codex and execution modules gated out. CI now has a job to repeat the
macOS and Windows checks. Hosted results for this change and native
macOS/Windows runtime behavior remain pending.

On 2026-11-22, a fresh local `cargo check --locked --tests` with
`RUSTFLAGS='-D warnings'` also passed for `aarch64-apple-darwin`,
`x86_64-pc-windows-msvc`, and `x86_64-unknown-linux-gnu` for the three
executables, Vault, SQLite, Workflow, and Protocol. This used
`LIBSQLITE3_SYS_USE_PKG_CONFIG=1` and `PKG_CONFIG_ALLOW_CROSS=1` to type-check
against the available pkg-config SQLite path. Bundled-SQLite cross checks
stopped at the C toolchain step. Native launch, terminal behavior, file
locking, process supervision, and sandbox guarantees still require target
runtime evidence.

On 2026-11-22, `cargo xtask check` exited successfully, the example Vault
validated, all thirteen publishable crates passed `cargo xtask verify-crate-packages`
and dirty-tree package creation, and an extracted native AArch64 release
archive passed the sandbox smoke. The first packaged smoke attempt failed when
the Worker-crash recovery scenario tried to restart the Daemon and received
`mcbtodo-daemon already holds the state directory lock`; an immediate complete
rerun passed. Review then found that the smoke helper accepted a successful CLI
socket request as proof that its newly spawned Daemon was ready, even if that
child had exited and another listener answered. The helper now also verifies
that the spawned child remains alive after the readiness request. The packaged
sandbox smoke passed after this change. This closes that false-readiness path,
and the Daemon now retries the state lock for up to 250 ms on the same file
descriptor. Old-socket inspection starts only after lock acquisition.
Tests cover a lock retained by a duplicate descriptor, a live endpoint that
must remain untouched, and explicit unlock on normal drop. The latest local
workspace and restart smoke checks passed. The owner of the lock in the
earlier failure remains unconfirmed. The latest visible successful GitHub runs are
Dependabot checks for commit `cbd4928`, not this dirty local worktree, so they
do not validate these changes.

The extracted-package smoke now also launches the packaged Daemon against a
development-only `xtask` Worker fixture. The fixture completes the real
Prepare/Ready/Start exchange and then emits an invalid protocol frame. The
smoke requires `outcome_unknown`, zero step traces, and termination/reaping of
the Worker process. This adds coverage for the packaged Daemon's post-start
protocol-failure path without adding fault-injection behavior to the shipped
Worker. It does not establish that the packaged Worker itself emits malformed
output, nor does it cover combined process faults or external-side-effect
reconciliation. The Linux smoke also runs the packaged Worker until its script
publishes a deterministic marker, kills that Worker, and verifies the marker
persists, the run remains `outcome_unknown` with no committed step, and
reopening the Daemon does not replay the attempt. This is a simulated
side-effect boundary, not reconciliation of a real external system. On
2026-11-22, `cargo xtask smoke --bin-dir target/debug --sandbox` passed with
both scenarios included.

The reminder path constraint now belongs at workspace-aware adapters: CLI and
TUI validate the active Vault through `WorkspacePaths`, and proposal apply
revalidates before publication. `mcbtodo-application` no longer resolves
home-relative paths to enforce an operating-system layout rule. This preserves
the dependency direction and keeps the default-Vault policy out of the
business-use-case API.

The Agent deadline supervisor also preserves the budget-exceeded diagnosis
when cancellation cannot be delivered to a Worker that has already exited;
the run remains `outcome_unknown`, and the transport error is retained as
additional detail. The deadline regression passed five isolated runs before
this branch correction, and the full workspace check passed after it.
The Worker protocol expresses timeouts in whole seconds while Daemon Agent
budgets retain millisecond precision. The Daemon now rounds the Worker timeout
up, so the Worker cannot independently time out before the Daemon's precise
budget deadline; the Daemon remains responsible for enforcing that deadline.
After this correction, `cargo xtask check` passed with all 69 Daemon tests,
including the new rounding boundary test; the example Vault validation, Agent
registry validation, and sandbox workspace smoke also passed locally.

On 2026-11-21, GitHub Actions run `44909569523` failed during workspace tests
on both Linux architectures. ARM64 had two failing production Worker tests;
x86_64 had those same two plus the Agent deadline test. The job installed
Bubblewrap and Nushell only after `cargo xtask check`, so the script tests
could reach sandbox startup without the declared Bubblewrap environment and
failed before observing a workload. The x86_64 Agent test also exposed a
deadline race: its execution budget began before Worker startup and the
Ready/Start handshake, so startup time could consume the Agent's execution
allowance. CI and
release-preparation now install sandbox dependencies before workspace checks.
The Daemon now starts the Agent execution deadline only after Start is
successfully delivered. After that change, `cargo xtask check`, example Vault
validation, `cargo xtask smoke --bin-dir target/debug --sandbox`, and
`cargo package --workspace --exclude mcbtodo-xtask --locked --no-verify
--allow-dirty` passed locally. A new GitHub-hosted run on both architectures
is still required; local passes do not establish hosted-runner behavior.

The related Dependabot runs on 2026-11-21 were also inspected. The `sha2`
0.11.1 update independently failed to compile because its digest output no
longer implements `LowerHex`; the workspace now uses `hex::encode`. The
`serde-saphyr` 2.2.0, `rusqlite` 0.43.0, `actions/checkout` 7.0.1, and
`actions/upload-artifact` 7.0.2 runs failed in the same workspace-check step
on the Worker startup/cancellation tests, so those runs do not establish
dependency-specific regressions. In that November 21 review, the full
`cargo xtask check` passed, and the daemon's 68-test target passed three
consecutive runs. Agent registry validation, example Vault validation,
sandbox smoke, package/license verification, and `git diff --check` also
passed. These local results do not replace a hosted rerun.

The repository governance audit found missing ownership, issue-intake, and
security-policy files. `.github/CODEOWNERS`, a structured non-security bug
form, and `SECURITY.md` have since been added and indexed. The live GitHub
repository currently has private vulnerability reporting disabled and no
branch protection on `main`; those remote settings require maintainer action.
Consequently the policy explicitly does not claim an operational private
disclosure channel, and CODEOWNERS review requests are not enforced as
required approvals until branch rules are configured.

On 2026-11-21, `cargo xtask verify-crate-packages` was added to Linux CI and
release preparation. It reads publishable workspace members from Cargo
metadata, requires the `mcbtodo` package naming convention, `Apache-2.0`
metadata, then checks each Cargo package listing for a `LICENSE` archive
entry. This automates the license-presence gate only;
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
does not test malformed output emitted by the packaged Worker binary. A
separate smoke sends malformed Prepare input to the packaged Worker and
requires rejection before readiness. Combined packaged-Worker fault cases
remain unverified.

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
| M0: domain and temporal rules | Partial | `crates/domain/src/{time,recurrence,graph}.rs`; CLI and MCP share `time::parse_date`. Application tests load serialized Markdown and verify deadlines, daily and weekly calendar projection, DST rules, cross-zone overlap, and elapsed duration. Day and Agenda tests verify focus and explicit offsets during a fold. | RFC 5545 recurring-event import and full timed-event navigation across all calendar views remain open. |
| M1: Markdown Vault and CLI | Partial | `crates/vault/src/{lib.rs,persistence.rs,watch.rs}`; `crates/application/src/lib.rs`; `apps/mcbtodo/tests/cli.rs`; `apps/mcbtodo/src/tui/watch_tests.rs` | Loading, validation, atomic writes, CLI commands, and shared debounced Vault watching for TUI and daemon exist. The daemon retains its last valid Vault snapshot, derives owned reminder definitions from it, and rejects invalid external edits without discarding either snapshot or reminder definitions. Project creation, Task/Inbox capture, deadline updates, time-block/reminder creation, WorkUnit creation/completion, workflow binding, Inbox-to-Task conversion, revision-checked recurring Task materialization, and atomic iCalendar event import use application commands. Inbox conversion publishes a deterministic Task before marking the retained Inbox source; retries verify exact content and reject conflicting edits. Focus report metadata is now read from one application snapshot. A general daemon query/index service is not implemented; other reads still load Markdown directly. |
| M2: daemon, IPC, and time engine | Partial | `apps/mcbtodo-daemon/src/`; `crates/protocol/src/`; systemd deployment and smoke task | Focus persistence/recovery, notifications, bounded completion event reads, IPC, and daemon-owned Vault change validation exist. Focused coverage verifies valid external changes replace the cached snapshot and invalid Markdown preserves the last valid snapshot. Workflow Task creation obtains its Project revision through the shared application query and writes through the shared application command; Vault rechecks that token under the writer lock. Reminder Inbox publication and desktop delivery use a durable outbox with bounded exponential retry and restart recovery; delivery is at-least-once across a crash after the helper side effect, and occurrences older than the 24-hour scan horizon are intentionally not backfilled. Extended-running and multi-session delivery acceptance remains open. The completion poller's direct Vault event reads are storage-facing infrastructure, not unshared business writes. |
| M3: calendar-first TUI | Partial | `apps/mcbtodo/src/tui/`; `crates/platform/tests/workspace_paths.rs`; calendar, Inbox triage, and interaction tests | Calendar projection, key-driven creation, Inbox-to-Task conversion, graph/timeline views, and configurable Month/restore-last-view startup exist. Config validation covers the policy; private state storage tests cover bounded atomic writes, path confinement, symlink rejection, ownership and permissions. `views_menu_covers_calendar_and_planning_views_without_losing_date_cursor` exercises all nine `Space u` menu destinations and asserts the date cursor is retained; `week_continuation_hour_opens_the_same_multiday_occurrence_in_day_view` verifies a multi-day timed occurrence remains focused when opened from its continuation hour; `agenda_focus_preserves_a_multiday_all_day_occurrence_from_a_continuation_day` verifies a multi-day all-day occurrence remains focused from a non-start day through Day view and Vault reload; `recurring_occurrence_focus_survives_month_and_week_intermediates_for_multiday_agenda` verifies exact recurring-occurrence restoration through Month and Week while retaining the selected Agenda day; `calendar_views_preserve_recurring_focus_across_every_direct_transition` exercises all 16 directed transitions among Month, Week, Day, and Agenda with a three-day recurring all-day event, asserting occurrence focus and date range; `month_selection_creates_an_inclusive_all_day_event_across_month_boundary` verifies selecting November 30 through December 1, creating an event, reloading the Vault, and excluding December 2. Saved-view names are allowlisted. Full calendar interaction acceptance and an interactive end-user session remain unverified, including timed multi-day events, timezone boundaries, and narrow-terminal rendering. |
| M4: task graph and workflow compiler | Partial | `crates/domain/src/graph.rs`; `crates/workflow/src/`; `apps/mcbtodo/src/tui/{graph.rs,render.rs,calendar_tests.rs}`; `xtask/src/smoke.rs` | DAG validation, typed compilation, publication, and simulation exist. The Linux sandbox smoke exercises a four-node Graph Diff and a published workflow end to end. The TUI caches its Graph projection for the lifetime of the validated Vault snapshot, invalidates it on snapshot installation, and builds list rows only for the visible terminal window; `graph_projection_cache_refreshes_with_the_validated_vault_snapshot` covers invalidation after a document change. This removes repeated full-projection construction during navigation/redraw and full-list text allocation, but large-Vault latency, memory use, and graph editing behavior remain unmeasured. |
| M5: durable scheduler and script runner | Partial | `crates/storage-sqlite/src/run_store.rs`; `apps/mcbtodo-daemon/src/runtime.rs`; `apps/mcbtodo-daemon/src/runtime_tests.rs::{daemon_supervises_worker_process_and_persists_its_result,worker_spawn_failure_fails_the_claimed_run_and_invalidates_its_attempt,malformed_worker_readiness_frame_fails_closed_in_production_supervisor,mismatched_worker_result_after_start_becomes_unknown_without_replay,terminating_worker_kills_its_process_group_and_reaps_the_worker,cancelling_running_script_through_daemon_reaps_worker_and_records_unknown,worker_crash_after_start_becomes_unknown_and_is_not_replayed_after_runtime_restart,result_commit_failure_becomes_unknown_without_replay_after_restart}`; `crates/platform/src/process.rs`; `apps/mcbtodo-worker/tests/process_protocol.rs`; RunStore attempt tests; `apps/mcbtodo/src/workflow_runtime_cli.rs`; `crates/workflow/src/schedule.rs`; `xtask/src/smoke.rs`; Worker v2 contract in `docs/design.md` §11.3 | Durable scheduling, recovery, authorization, sandbox execution, and one-time operator resolution of `outcome_unknown` runs exist. The production Daemon starts one `mcbtodo-worker` process for script/Agent attempts, sends revision-bound source bytes, persists Started before Start, checks returned attempt identity, waits for successful child exit, and commits results through the Daemon-owned RunStore transaction. Process startup failures invalidate the claimed attempt and fail the current run; focused regressions cover process-spawn failure and a malformed readiness frame through the production supervisor, with no step commit. The mismatched Result regression emits a valid but mismatched frame from a fixture subprocess, passes it through the production `read_worker_messages` decoder and Started `poll_worker` path, then verifies process-group termination/reap, `outcome_unknown`, no step commit, and no replay after reopening the RunStore. A freshly extracted release archive now passes a sandbox smoke where an `exit 23` script is run through its packaged Worker; the Daemon persists one failed step and a trace reason containing the exit status. Separately, smoke sends malformed Prepare input to the packaged Worker and requires rejection before readiness. A development-only fixture also makes the packaged Daemon reject malformed post-Start Worker output. These tests verify Daemon protocol supervision but do not establish that the packaged Worker emits malformed output or cover combined process/protocol faults. Linux coverage exercises production Daemon cancellation of a live sandbox script and SIGKILL of a Worker after Started. Worker startup arms a Linux parent-death signal and validates the expected Daemon PID; the sandbox smoke force-kills the Daemon after a script side effect and verifies sampled Worker descendants exit, then verifies restart recovery to `outcome_unknown` without replay. A separate packaged-Worker smoke writes a deterministic output marker, kills the Worker, and verifies marker persistence, no committed step, and no replay after Daemon restart; this is a simulated side effect, not external-system reconciliation. This process cleanup does not undo or determine the outcome of real external side effects. Non-ESRCH process-group kill errors are surfaced. The store persists monotonic epochs and exact run/revision/node/step binding, and restart invalidates unresolved attempts. A database epoch is not an expiring Worker lease and cannot undo external effects. M5 remains partial: packaged-Worker outbound protocol/fault combinations and external side-effect reconciliation are not accepted; reconciliation remains integration-specific. Result transaction failure and no replay, uncertain outcomes, and interruption state recovery have focused coverage. Distributed Worker clusters remain deferred. |
| M6: MCP and AI operations | Partial | `apps/mcbtodo/src/mcp/`; `proposal.rs`; `crates/platform/src/agent_policy.rs`; `crates/ports/src/{lib.rs,transport.rs}`; `crates/execution/src/agent.rs`; `crates/application/src/lib.rs`; `xtask/src/smoke.rs`; representative adapter matrix below | Typed tools, Vault-bound proposal approval, revisions, policy parsing, the business-shaped `AgentProvider` port, the shared host Worker-capacity admission path, and the production AgentPolicy routing path exist. Unix socket and Codex adapters are selected by explicit capability/revision grants, and the selected adapter retains host-side policy, revision, timeout, cancellation, and response validation. Policy tests cover duplicate grants, unsafe endpoints, and unconfigured providers. CLI and MCP share application query projections; TUI uses the same projection rules over its already-loaded snapshot. Exposed writes share `VaultCommands`; MCP routes writes through human-reviewed proposals, unlike direct local CLI/TUI actions. The smoke submits an MCP-authored workflow graph/source snapshot, verifies it remains unpublished until TUI approval, then confirms publication does not authorize or enqueue execution. Host-selected Agent routing and business storage ports have production callers. The Codex app-server adapter is selectable in production and the offline end-to-end probe verifies that route. Online authentication and managed distribution remain pending. Provider-specific scheduling budgets, an independent plugin protocol, and cross-device sync remain planned. Representative adapter scenarios are mapped to owner-layer and adapter tests below; the matrix is not exhaustive. Feature-set identity across adapters is not a product requirement. |
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

On 2026-11-21, the sandbox smoke added a packaged-Worker crash-after-effect
scenario: it kills the real `mcbtodo-worker` after the sandbox writes an
external marker, confirms the Daemon remains alive, verifies
`outcome_unknown` with no committed step, then restarts the Daemon and confirms
the uncertain run and marker are not replayed. The same smoke now walks the
bounded workflow-notification pages when checking backup continuity; the prior
assertion inspected only the newest page and falsely reported a missing older
notification after sandbox scenarios generated more than one page. The updated
Linux sandbox smoke passed. This adds one packaged-Worker fault case but does
not establish combined malformed-protocol/process-fault acceptance or
integration-specific reconciliation of external effects.

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
manifests provide descriptions and docs.rs documentation targets. The package
verification command checks both SPDX metadata and the full Apache license in
each nested crate archive, also requiring each package-local `LICENSE` to
match the workspace copy byte for byte. This avoids redundant Cargo
`license-file` metadata while keeping each published archive self-contained.
The canonical repository URL is configured in workspace metadata.
Third-party license compatibility and package-content reviews remain release
preparation work. On 2026-11-22, `cargo publish --dry-run --workspace --locked
--no-verify --allow-dirty` packaged all thirteen publishable crates and Cargo
aborted each upload as expected. `cargo xtask verify-crate-packages` verified
the Apache-2.0 metadata and license entries for all thirteen archives.
`--no-verify` skips isolated builds of the extracted packages against
registry dependencies. An earlier verification attempt stopped at
`mcbtodo-vault` when the registry-resolved `mcbtodo-domain` lacked
`InboxEntry::task_id`. Initial publication and isolated verification must
proceed in dependency order, with maintainer authorization before any real
publication.

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

On 2026-11-21, default-Vault identity enforcement was moved out of the shared
`mcbtodo-application` command: whether a selected Vault is the configured
default is a workspace/path policy, not an application business rule.
`WorkspacePaths::is_default_vault()` compares canonical directory identities
and accepts symlink aliases to the configured default. CLI, TUI, proposal
intake, and proposal application enforce this policy at their workspace-aware
boundaries before creating or publishing reminder writes; application commands
remain reusable without home-directory assumptions. This prevents a
successful-looking reminder write that the background Daemon would never scan.
Other foreground edits to external Vaults remain supported; operating a
background service for them remains unimplemented. Platform path tests cover
external-Vault rejection and symlink-alias identity, while adapter tests cover
the affected write flows.

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
