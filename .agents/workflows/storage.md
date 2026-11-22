# Storage and Persistence Workflow

Use this workflow when changing Vault persistence, SQLite state, business
repositories, event logs, snapshots, migrations, cursors, or recovery. Storage
interfaces must preserve business semantics and transaction guarantees rather
than merely hide SQL behind a generic trait.

## Data Authority

Keep the following boundary explicit.

- Markdown Vault is the user-editable authority for primary task data.
- SQLite stores local derived state, execution state, and other data whose
  ownership is defined by the application.
- Event Log stores logical, exportable facts when their identity and merge rules
  are defined.
- Runtime files include sockets, locks, journals, caches, and temporary files.
  They are operational recovery material and are not logical cross-device data.

Vault snapshots must bind to an explicit revision. Stale writes must fail using
the existing lock, conflict, and atomic publication semantics.

## Business Repositories

Prefer business repositories such as `FocusStore`, `TimerStore`,
`WorkflowRunStore`, `CompletionEventStore`, `ProposalStore`, `AgentBudgetStore`,
and `NotificationStore`. Combine operations only when their transaction scope
and invariant owner are the same.

The business layer must not accept arbitrary SQL, `rusqlite::Connection`, SQL
rows, statements, or SQLite transaction types. The SQLite adapter may use those
internally and must preserve the existing schema and state meanings.

`WorkflowRunStore` must preserve atomic claim, start fencing, result commit,
unknown-result marking, budget reservation, and restart recovery. A claim must
not grant duplicate external execution. A late or mismatched result must not
commit. A result lost after an external side effect must become
`outcome_unknown` and must not be replayed automatically.

Keep completion journal SQL, schema migration and SQLite snapshot validation
in `mcbtodo-storage-sqlite`. Vault owns the writer lock, Markdown publication,
revision comparison and the recovery decision. Use the established
prepare, publish, commit or abort sequence and retain recovery tests in
Vault. The Daemon reads completion facts through `CompletionEventStore` and
commits each page through `CompletionDeliveryStore`. Recheck the live rules and
commit runs, receipts and cursor advances together. Verify both atomic rollback
in SQLite and the poller's retry from the durable cursor.

Use Workflow's `Trigger::clock_schedule` for both registration and recovery of
clock rules. Keep the trigger and missed-policy JSON stable. The SQLite
adapter owns the transaction that enqueues occurrences and advances the
schedule cursor.

Use `WorkflowQueueStore` for the serialized Daemon loop. The host supplies
wall time and whether it can advance a pending run. Clock rules can wake
the loop with an occupied Worker slot. Pending candidates are ordered by
eligibility and insertion order, then fenced by the existing state
transitions and Attempt claim. Exercise batch rollback after one occurrence
has been inserted and retry the same boundary after reopening the store.

Use `WorkflowHistoryStore` for traversal history, step inspection, and artifact
reads. Decode persisted traces inside the adapter and report malformed traces
as errors. Keep committed steps immutable. Verify that an artifact read can
resume at the returned offset after reopening the database. When a step
depends on a waiting review, fence the prior revision and exact recorded
detail in the same transaction as the append.

Use `WorkflowRunCatalog` for run lookup, pagination, and manual enqueue.
Validate the publication in the host before creating a run. The repository
creates at most one row for a UUID and returns its durable binding. The host
compares revision, due time, and scheduled origin to classify duplicates.

Use `WorkflowScheduleStore` for schedule registration, inspection, and
deactivation. Register the schedule and its trigger rule in one transaction.
The host obtains the completion watermark and validates the publication.
The background completion reader reports writer-lock contention as `Busy`.
Verify a retry preserves the durable cursor and delivers the eligible event
once after the lock is released. CLI exports wait for the writer lock.
Disable by UUID and read current delivery state under the write transaction
to preserve counters advanced since a caller's prior inspection.

Use `WorkflowProgressStore` for host-driven transitions, step appends and
operator recordings of unknown outcomes. Check for active Worker Attempts
inside the same write transaction. Only the same-node transition from
Running to Cancelling is allowed through the host transition path while an
Attempt is active. Worker result commits own Agent budget settlement and
publish the trace, artifact and run progress with the Attempt fence.
Use `ScriptAuthorizationStore` for exact revision and node grants. The
host validates the published node before changing a grant.

Use `NotificationStore` for Inbox reads and reminder deliveries. Keep
terminal workflow notifications in the state transition transaction.
Publish the reminder occurrence and its outbox row together and deduplicate
by reminder UUID and scheduled instant. Recover in-flight deliveries
before accepting new claims under exclusive Daemon ownership. Desktop
delivery can repeat if the sink accepted a notification before the Daemon
recorded its acknowledgement and the Daemon restarted. Retry a failed
acknowledgement write in the live process without contacting the sink
again. End exhausted interrupted deliveries and retain the Inbox entry.

Wire one composite workflow repository at the Daemon startup boundary so
the run, trigger, attempt and notification operations share the existing
transaction and recovery scope. Inject `FocusSessionStore` independently.
Its durable checkpoint must succeed before the timer publishes the new
state. Recovery marks an interrupted running session for review. The
daily report groups sessions by their start date in the selected timezone.

## Adapter Capabilities

Record the guarantees of each backend separately.

- transactions
- atomic batch
- durable cursor
- snapshot export and validation
- concurrent reads
- watches

If a workflow requires a guarantee the backend cannot provide, reject that
workflow or backend with a diagnostic. Never silently degrade an atomic commit
to multiple non-atomic writes.

At Daemon construction, check the composed `WorkflowRepository` for atomic
batches and a durable completion cursor before starting the runtime. Include
the backend ID and the missing guarantee in startup errors. Verify capability
claims with the adapter's rollback, cursor and restart tests. Snapshot and
watch support have separate owners and do not gate this runtime.

## Migrations and Recovery

1. Identify the current schema version and all migration entry points.
2. Test reading existing user data before changing schema or crate boundaries.
3. Make migration steps explicit, ordered, and recoverable.
4. Do not rebuild or clear user state to make a test pass.
5. Validate backup and restored snapshots before publication.
6. Preserve event and proposal idempotency, pending and rejected states, and
   journal recovery behavior.
7. Keep staged validation free of lock creation and staged-byte mutation. If a
   rollback journal needs recovery, use an isolated disposable copy and reject
   unsafe super-journal references before opening it.
8. Preserve byte-exact event/database round trips where the backup contract
   requires them.

Validate staged Vault Markdown and completion events together. Reuse the
validated Markdown revision for pending events. Test invalid Markdown
without a completion store as well as pending-event digest conflicts.

For focus and workflow backups, use `mcbtodo-storage-sqlite` for SQLite
snapshot capture and typed validation. The CLI owns locks, permission and
sidecar checks, staging and publication. The adapter includes committed WAL
data when making a standalone snapshot. Reject WAL-mode staged files before
SQLite opens them. Verify byte preservation, absence of staged sidecars and
rejection of the wrong store even when the backup manifest matches.
Use the Daemon's version checks for snapshot validation. Verify that capture
and restore reject unsupported schema versions before publication. Preserve
version 0 Workflow snapshots for startup migration.
Decode every Focus session using the startup checkpoint decoder. Test a
malformed historical timestamp in a structurally valid database with a
matching manifest. Check that capture and restore reject it before publication
and preserve the input bytes. Verify that a running checkpoint survives backup
and restore so Daemon startup can mark it for review.

## Required Tests

Storage changes should cover the applicable cases below.

- atomic commit and rollback
- idempotent insert or result submission
- cursor resume and bounded page reads
- concurrent reader and writer behavior
- restart and journal recovery
- snapshot export, staged validation, restore, and failure cleanup
- stale revision and conflict rejection
- old data and schema migration
- unknown external result and no-replay recovery

Run focused package tests, then the full workspace check and example Vault
validation. Record database and platform prerequisites. Do not claim a remote or
cross-device backend exists because a local interface can theoretically support
it.
