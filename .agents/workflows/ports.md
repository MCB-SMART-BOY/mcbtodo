# Stable Ports Workflow

Use this workflow when introducing or refining a business-facing port or moving
an external capability behind an adapter. The goal is a small contract with a
real caller and a real implementation. Rust traits, Cargo crates, and external
process plugins solve different extension problems and must remain distinct.

## Boundary Rules

1. Read the audit evidence and trace one concrete caller before editing an
   interface. Reuse an existing equivalent boundary.
2. Keep domain and application code independent of SQLite connections, SQL,
   Unix socket types, Bubblewrap arguments, Codex protocol types, subprocess
   handles, and terminal coordinates.
3. Express operations in business terms. Do not expose `rusqlite::Connection`,
   SQL statements, SQLite rows or transactions, raw IPC frames, or a vendor
   Agent request in a stable port.
4. Keep ownership of correctness in the host. A Provider capability declaration
   is discoverable metadata and does not grant permission to write data or use a
   path.
5. Define synchronous or asynchronous behavior, lifecycle, concurrency, timeout,
   cancellation, idempotency, error classification, and unsupported capability
   behavior in the contract.
6. Add a mock or in-memory test double only when a real caller uses it. Add at
   least one contract test against the real adapter before marking the port
   verified.

## Initial Port Set

Evaluate these in dependency order and add only ports with real consumers.

- `AgentProvider`
- `LocalTransport`
- `ProcessLauncher`
- `Sandbox`
- `WorkflowRunStore`
- `CompletionEventStore`
- `NotificationSink`

Evaluate later as needed: `StorageRepository`, `ProcessController`, `Clock`,
`Scheduler`, `FileSystem`, `FileWatcher`, `FileLock`, `AtomicFileWriter`,
`ServiceSupervisor`, `WorkspaceLayout`, `CredentialProvider`, `SecretStore`, and
`AuthSession`.

The `mcbtodo-ports` crate is the intended home for business-facing contracts and
small capability metadata. Keep platform helpers and storage recovery in their
own adapters when that preserves a clearer invariant owner.

## Contract Checklist

For each port, document:

- business operation and result semantics
- stable identifiers and revision requirements
- error categories and whether retry is safe
- cancellation and deadline behavior
- concurrency and lock ownership
- idempotency key behavior
- side-effect and `outcome_unknown` behavior
- capability and permission requirements
- platform support and unavailable behavior
- mock or in-memory test strategy
- real adapter contract test

For execution ports, a timeout or disconnected transport does not prove that an
external side effect did not happen. Preserve `outcome_unknown` and prevent an
uncertain operation from being silently replayed.

## Provider Routing Boundary

`AgentPolicy` resolves one provider for each authorized capability and
revision. The Worker constructs that adapter and uses its metadata to identify
an uncertain result. The adapter checks its execution prerequisites when
dispatching. An adapter that binds a specific capability rejects a mismatch
before launch. The authenticated Unix socket peer handles the capability
authorized by the host. A selected provider's failure terminates the call.
Budget reservation, revision authorization, final result submission, and
business state transitions remain with the host and the owning
application/store. Use `Unchecked` when health has yet to be observed.

The Daemon coordinates Script and Agent work through a bounded set of
supervised Worker slots. The default is one slot, and the Linux daemon CLI
accepts an explicit limit from 1 through 4. All slots share the Daemon's
queue, persistence, policy and result-admission boundaries. Each slot has
independent supervision, cancellation and result admission. A full set of
slots leaves later work pending without claiming an Attempt or reserving Agent
budget. This is local bounded concurrency, not a general ResourceScheduler.
Derive broader scheduling interfaces only from a concrete resource or
priority requirement. Require `WorkerInput` to enforce the host's absolute
frame delivery deadline. Keep state ownership and classification of partial
delivery in the Daemon.

Anchor the Agent and Script execution budgets after durable Started and before
the first Start write. The Worker can consume Start while a transport finishes
the transfer. Apply the earliest of the handshake deadline, the execution
deadline, and the control-frame delivery deadline to Start. Test both the
delivery time charged against the budget and a budget shorter than the
ordinary control-frame delivery allowance.

Use the Linux Unix stream adapter on both sides of local connections for peer
authentication and bounded I/O. The caller authorizes the authenticated peer
before sending or parsing frames. The Daemon owns the listener and gives each
request read and response write its own deadline. Test expiry with buffered
data, renewed deadlines, cancellation, and IPC request framing.

`WorkflowQueueStore` supplies coherent wake and pending-run snapshots and
transactional clock delivery to the Daemon loop. Slot availability is host
input to wake selection. The clock-rule wake remains eligible during Worker
execution. Eligibility reads grant no Attempt or resource allocation.

The Linux process controller observes exit without reaping the Worker.
It signals the owned group while the Worker still holds the group ID,
then reaps the Worker and retains its original exit status. Cover both
explicit termination and a Worker that exits while a group member is alive.
Check liveness on startup ticks and before releasing a Ready Worker.
Verify that a descendant holding the pipes cannot delay a known pre-Start
exit until the handshake deadline. Invalidate the claim with zero steps.

After reaping a Worker, poll for the final protocol frame within the bounded
drain window. Keep the slot occupied while the reader catches up. Exercise
successful delivery, drain expiry, cancellation, and restart recovery. A
missing or fenced frame must leave the attempt uncertain.
Validate the message kind and Attempt identity when admitting the final
frame. Pass the admitted result to the commit path, which still checks
cancellation, deadline and result contents.

Wake the Daemon at the earlier of the next Worker poll and the deadline for
its current supervision stage. After sending Cancel to a live Worker, use
the cancellation grace deadline. While draining a reaped Worker, honor both
the execution and drain deadlines. Keep clock and completion wakes eligible.

## Verification

After each port slice, run:

```sh
cargo fmt --all -- --check
git diff --check
cargo test --locked -p mcbtodo-ports
cargo xtask agent validate
cargo xtask check
cargo run --locked -- --vault examples/vault --timezone Asia/Shanghai validate
```

Also run the real adapter's focused tests and the relevant daemon, worker, or
application integration test. Inspect the diff for leaked implementation types,
unused public interfaces, duplicated orchestration, and claims that exceed the
actual caller path.
