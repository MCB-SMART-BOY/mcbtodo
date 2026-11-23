# Storage, recovery and sync model

| Data | Authority and lifecycle |
| --- | --- |
| Default or external Markdown Vault | User-editable projects, tasks, WorkUnits, Inbox captures and calendar documents. CLI, TUI and Daemon writes ultimately use the Vault writer, which checks the observed revision under its write lock. Calendar, Outline and Graph are derived views. |
| `.mcbtodo-events.sqlite3` inside a Vault | Vault-scoped completion facts and recovery intents. Vault owns the publication and recovery decisions. The SQLite adapter implements intent storage and cursor reads. |
| `~/.mcbtodo/state/focus.sqlite3` | Daemon-owned focus checkpoints and recovery state. The timer uses a separate `FocusSessionStore`. |
| `~/.mcbtodo/state/workflow.sqlite3` | Daemon-owned run queue, attempts, approvals, script grants, schedules, delivery cursors, traces, artifacts and notification outbox. The reminder notification Inbox is stored here, while the capture Inbox lives in Markdown. |
| `~/.mcbtodo/state/workflows/revisions/` and `state/proposals/` under `~/.mcbtodo/` | The revision store holds immutable published workflow snapshots. The private proposal queue holds review state and captured draft bytes. Publication, authorization and execution are separate operations. |
| Device-local files | Sockets, locks, caches and sandbox scratch are recreated as needed. `state/last-view` and `state/device-id` are local to the home. A private Vault backup may contain a SQLite rollback journal needed for recovery. |

Storage entry points are in the [Vault writer](../crates/vault/src/persistence.rs),
the [SQLite adapter](../crates/storage-sqlite/src/lib.rs) and the
[Daemon startup](../apps/mcbtodo-daemon/src/server.rs).

## Transactions and compatibility

The workflow adapter reserves Agent budget, transitions the run to Running,
and inserts the claimed Attempt in one write transaction. A failed claim
rolls back the budget charge before the Daemon can launch a Worker. Result
commit groups steps, artifacts, budget settlement and run progress in a
separate transaction. The completion poller reads facts through
`CompletionEventStore` and submits each page through `CompletionDeliveryStore`.
SQLite rechecks active rules and commits the triggered runs, event receipts
and cursor advances in one transaction. A failed delivery retries from the
durable cursor. If a writer holds the Vault lock, the background reader
backs off for one second and retries from the durable cursor. CLI export
waits for the lock. These transaction guarantees apply to
`workflow.sqlite3`. The Markdown completion path uses a separate journal and
reconciles it against the expected old and new Markdown digests. Its SQLite
implementation belongs to `mcbtodo-storage-sqlite`. Vault keeps the write
lock, Markdown publication, revision comparison and recovery decision. The
journal provides durable intent preparation, commit or abort, snapshot
validation and bounded reads. Markdown and the journal recover through the
intent protocol across their separate writes.

Inbox triage also spans separate writes. Vault publishes a Task with a stable
UUID before marking the retained Inbox entry as assigned. A retry after Task
publication checks the exact Task bytes before updating the Inbox entry.
Conflicting edits stop recovery. This applies to foreground, Daemon and
approved proposal writes through the shared Vault path.

`WorkflowHistoryStore` returns typed traversal traces and artifact metadata.
The adapter decodes committed trace JSON in step order. The Daemon still
returns the `trace` and `artifact` fields in the IPC step response. Artifact
reads return at most 8 KiB and a byte offset for resuming an immutable
artifact. Run transitions and step appends compare the stored revision and
detail with the loaded snapshot. A refreshed approval therefore invalidates
an earlier snapshot even when the node and step count are unchanged.

`WorkflowScheduleStore` deactivates rules by UUID in a write transaction.
Disabling prevents future delivery and retains queued runs, completion
receipts, and the latest counters even if delivery advanced after inspection.

`WorkflowProgressStore` serializes host-driven state transitions and step
appends against the active Attempt fence. A host request may move a running
Attempt to same-node Cancelling. Committing a Worker result uses the
Attempt identity and settles Agent budget in the result transaction.
`ScriptAuthorizationStore` persists grants for the exact published revision
and node after host validation.

`NotificationStore` pages workflow and reminder notifications and manages
the reminder delivery outbox. Workflow terminal notifications are written
in the transition transaction. Reminder publication deduplicates on the
reminder UUID and scheduled UTC instant and commits the Inbox entry and
pending delivery together. Restart recovers in-flight deliveries under
exclusive Daemon ownership. The sink may display duplicates if its
acknowledgement is lost on restart. A running Daemon retries a failed
acknowledgement write without sending another notification. Restart
recovery closes exhausted attempts and retains the Inbox entry.

The Daemon startup boundary injects one workflow repository for the
shared transaction scope. Focus persistence uses a separate
`FocusSessionStore`. SQLite atomically recovers interrupted sessions
as `needs_review`. The timer advances its in-memory checkpoint only
after the repository saves it. Daily reports group checkpoints by the
local date of the recorded start time.

The adapter move preserves database paths and schemas. Opening an existing
version 0 `workflow.sqlite3` registers version 1 in the same transaction as
initialization and recovery. Errors roll back that transaction and unknown
versions are rejected. `focus.sqlite3` keeps version 1. The release procedure
uses a stopped Daemon and a private backup as the recovery point. If rollback
across versions is required, restore the pre-upgrade backup to a fresh home
and validate it before use.

## Backup versus multi-device exchange

With the Daemon stopped, `mcbtodo backup` captures the default Vault, its
completion journal, any focus and workflow databases, published revisions,
default-bound proposals, `config.toml` and `agents.toml` if present. `codex.toml`
and `keymap.toml` are outside that inventory. Restore validates a private
staged copy before publishing it into a fresh home. The CLI owns workspace
locks, file permissions, inventory checks and publication. See the
[backup and restore implementation](../apps/mcbtodo/src/backup.rs).

The selected Vault is the default when its canonical path exactly matches
the configured default root. A symlink alias to that root keeps default
proposal binding. Backup copies the real directory and verifies its device
and inode against the opened directory. Restore produces ordinary directory
contents. Backup excludes external Vaults and rejects symlinks inside the
default Vault. The case is covered by the
[backup regressions](../apps/mcbtodo/src/backup_tests.rs).

`mcbtodo-storage-sqlite` copies the focus and
workflow stores with committed WAL data into standalone snapshots. Its
read-only validator checks the SQLite header before opening the file, verifies
structural integrity and declared foreign keys, checks for the expected store
table, and checks compatibility with the version accepted at Daemon startup.
Capture and restore reject a workflow snapshot if a step references a missing
artifact, even when SQLite reports that the file is structurally intact.
Focus snapshots require version 1
and validate every session with the checkpoint decoder used at startup. This
catches malformed historical values that could break reports. A running
checkpoint stays intact in the snapshot and is marked for review when the
restored Daemon starts. Workflow version 0 remains eligible for migration
on first Daemon open. Validation preserves the snapshot bytes. A WAL-mode
snapshot is rejected before SQLite can create sidecars in the staging directory.
Vault staging validates Markdown once and uses that snapshot to check the
before or after digest of any pending completion. SQLite rollback recovery
runs on a disposable copy.
Available evidence and install scope are in the [architecture assessment](architecture-assessment.md).

`mcbtodo vault completion-events --after 0` exports a page of up to 64
committed records as JSON Lines. Each line contains `sequence`, `event`, and
`observed_revision`, the journaled SHA-256 of the exact pre-completion WorkUnit
Markdown bytes. Existing journal entries already contain this digest.
Pass the last processed sequence to `--after` to resume from an exclusive
cursor in the same journal. An empty page means the journal has no newer
committed records at read time. Use the event's original `event_id` to
recognize a repeated export. Running without `--after` keeps the original
whole-journal format with one bare event per line.

Pages also include `origin_device_id` when the original writer supplied an
installation identity. The CLI and reviewed completion proposals share a
random UUID created on demand in `~/.mcbtodo/state/device-id`. An explicit
external Vault uses the same identity as the default Vault for that home.
The journal saves the identity in the same row as the pending completion.
Recovery and export reuse the recorded value. A read-only export leaves the
installation identity file untouched.

Version 1 and 2 journals migrate to version 3 in one SQLite transaction on
open. Earlier events retain their sequence, UUID, payload, revision digest
and eligibility to trigger local workflows. Version 1 events have no
origin metadata. Validation of a captured journal runs the migration on
the disposable copy. To revert to a binary that supports only version 1 or
2, restore a pre-upgrade backup to a fresh home.

Backup preserves event origins in the journal and excludes `device-id`.
Restoring to a fresh home generates a different identity on the first
completion there. Manually cloning the file copies the identity. It is a
home-scoped provenance label, and device authentication requires a
separate mechanism.

`mcbtodo vault completion-events --after 0 --logical` uses the same bounded
page and returns `sequence` and `envelope` on each line. The envelope has
schema `mcbtodo.completion-event/v1` and event type `work_unit.completed`.
`event_id` and `idempotency_key` both carry the original event UUID.
`entity_id` identifies the WorkUnit and `created_at` records the original
completion time. `observed_revision` carries the pre-completion digest.
`payload` contains `project_id`, `parent_task_id`, `previous_status`, and
`evidence`. Historical records with unknown origin omit `origin_device_id`.
The application projects the envelope from the journal record after reading
it. All three output formats use the existing journal schema.

## Manual completion import

`mcbtodo vault import-completion-events` reads logical JSON Lines from
standard input. Use an event file from a trusted source and a destination
Vault that still has the exact pre-completion WorkUnit Markdown. The
import checks the digest, task and project identity, previous status,
prerequisites and evidence already present in the destination. Evidence
references point into local workflow history, so this import only accepts
events that preserve the destination's existing references.

The Vault records the original event UUID, time and origin, including
unknown origins. It assigns a sequence from the receiving journal.
Reimporting an identical event succeeds without a second update. Reusing
a UUID with altered content fails. A different Markdown revision also
fails and leaves the document unchanged. Input is processed in file order
and committed one record at a time. On failure, correct the reported line
and retry the file. The existing completion intent protocol resolves
interrupted publication against the pre- and post-completion digests. The
Daemon triggers workflows only for local completion actions. Imported facts
still appear in exports and backups.

The available exchange is manual and limited to WorkUnit completion facts.
Device authentication, transport and merging divergent edits remain planned.
Timestamps record observation time. The sequence belongs to one journal,
and an origin UUID is provenance supplied by the input. Use database files
for private backup and restore, and use the logical envelope for event
exchange. Import eligibility and local trigger filtering are implemented in
the [Vault writer](../crates/vault/src/persistence.rs) and
[completion journal](../crates/storage-sqlite/src/completion_journal.rs).
