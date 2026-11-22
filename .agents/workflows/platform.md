# Platform Capability Workflow

Use this workflow when changing filesystem paths, locks, watchers, atomic writes,
process control, local transport, sandboxing, notifications, service startup,
terminal behavior, or platform-specific dependencies.

## Evidence Levels

Track each adapter and capability separately as `Implemented`, `Tested`,
`Verified`, `Planned`, `Degraded`, or `Unsupported`. Distinguish static support
from runtime availability.

- `Supported` describes a platform implementation target.
- `Available` means the current machine has the required tool or service.
- `Unavailable` means the target is supported but the runtime dependency is
  absent or unhealthy.
- `Degraded` means an explicitly documented reduced capability is active.
- `Unsupported` means the required contract is deliberately rejected.

A successful cross-target compile proves none of startup, basic operation,
security, process cleanup, or equivalent isolation.

## Adapter Surface

Evaluate `WorkspaceLayout`, `FileLock`, `FileWatcher`, `AtomicFileWriter`,
`ProcessLauncher`, `ProcessController`, `LocalTransport`, `Sandbox`,
`NotificationSink`, `ServiceSupervisor`, and terminal capability handling. Keep
platform details out of domain and application logic. Use Unix peer credentials
only where the transport can verify them. Provide the Windows named-pipe or
macOS equivalent only after its security and lifecycle semantics are understood.

The current Linux path is the primary verified target. Record macOS and Windows
compile, startup, CLI, backup/restore, doctor, and security evidence separately.
Do not call platform support complete because a type-check job passed.

## Optional and Required Tools

`cargo run` must start the TUI without Bubblewrap, Nushell, jq, or
`notify-send`. Their absence may disable only the corresponding optional
capability. A required sandbox or permission guarantee must not silently fall
back to an unsandboxed process.

Represent sandbox policy as an explicit choice such as `RejectExecution`,
`AllowUnsandboxed`, `AllowSideEffectFreeOnly`, or `RequireUserConfirmation`.
The default for a workflow that requires isolation is fail-closed.

For Bash, Nushell, Codex, systemd, notify-send, and tar, diagnose missing,
unsupported, unhealthy, and version-incompatible states by provider and
capability. Keep user-facing messages concise and detailed causes in logs.
Use the Linux sandbox adapter's installation preflight for Bubblewrap and its
shells in `doctor`. Keep this check free of process startup and workspace
writes. Record startup and isolation evidence from the sandbox smoke.

Share desktop session detection between the Linux Daemon and `doctor`.
Test absent and empty session variables, a missing or non-executable
`notify-send`, and an executable helper. Installation checks must avoid
starting the helper, contacting the session bus, or recording its address.
Report the current process environment and verify delivery separately.

On macOS and Windows, `doctor` reports `unsupported` for the Linux
Daemon transport, Workflow Agent policy and Codex adapter, sandbox runners,
desktop notification adapter, and systemd. These diagnostics should leave an
otherwise healthy offline workspace usable.

## Local IPC Supervision

On Linux, keep socket frame reads and writes on the Daemon's single admitted
IPC connection thread. The main loop polls for a complete request or channel
closure and keeps ownership of business commands and Worker supervision.
Verify that a partial frame yields the scheduler promptly, a completed
frame receives a response, and the read deadline releases admission without
dispatching an incomplete command. Preserve same-user peer authorization and
the independent request and response deadlines.

## Platform Matrix

Maintain the adapter-by-platform matrix in `docs/architecture-assessment.md`
and `docs/architecture.md`. Initial entries are evidence placeholders only.
Every cell must state the actual status and test scope. Include transactions,
cancellation, process-tree cleanup, notification, local transport, sandbox, and
service lifecycle where applicable.

## Verification

Run the relevant package and integration tests, cross-target checks, and native
smoke commands. On Linux, run:

```sh
cargo xtask check
cargo xtask smoke --bin-dir target/debug --sandbox
cargo xtask smoke-systemd --bin-dir target/debug
```

Run the systemd smoke only in a Linux user session with a reachable user manager.
For macOS and Windows, use the CI jobs or native hosts and record which checks
were actually executed. Missing prerequisites are evidence of an unavailable
runtime capability, not a passing test.

Compile-check all three application entry points and their tests in each native
CLI CI job with warnings denied. Run the in-memory TUI renderer and screen model
tests in the native macOS and Windows jobs. Exercise Settings input and
rendering, config publication and replacement, unchanged saves, and concurrent
saves on the native filesystem. Record interactive terminal startup and
rendering independently. In the portable cross-target
check, keep shared wire DTO dependencies unconditional. Gate verified peer
credentials and other transport implementations by platform.

Keep `cargo xtask agent validate` and offline harness preparation buildable on
macOS and Windows. Gate Linux-only smoke, release packaging, and sandbox
example implementations at the task boundary. The native xtask suite should
exercise all bundled harness/workflow combinations and explicit rejection
without creating output. On Windows verify `.exe` discovery on `PATH`.
