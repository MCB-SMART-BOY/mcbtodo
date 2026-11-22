# Storage and Cross-Device Sync Workflow

Use this workflow when changing backup, event export, restore, device identity,
merge behavior, or data ownership boundaries. Backup and cross-device sync have
different contracts and must not be conflated.

## Data Classes

- Markdown Vault is user-editable primary data.
- SQLite is local derived and execution state where the owning feature defines
  that authority.
- Event Log contains logical facts only when identity, payload, and merge rules
  are explicit.
- Runtime files include sockets, locks, rollback journals, caches, and temporary
  files. They are excluded from logical synchronization.

A backup may include a validated SQLite snapshot and recovery material. Direct
SQLite file copying is not a cross-device synchronization protocol.

## Event Identity

A cross-device event model must include a globally unique `event_id`,
`origin_device_id`, `event_type`, `entity_id`, payload, `created_at`,
`observed_revision`, and `idempotency_key`. The local SQLite sequence remains a
local cursor only. A timestamp cannot be the sole identity, ordering, or conflict
winner.

For local export, `vault completion-events --after` exposes the journal's
bounded exclusive sequence cursor, pre-completion document digest, and
original event payload. Test cursor resume, repeated event identity, observed
revision fidelity, and backward compatibility of the bare JSON Lines format.
Keep cursor scope local and leave unavailable origin metadata absent. Generate
the installation UUID once per home in private state. Capture it alongside
the pending completion, including for an explicit external Vault. Verify
concurrent first use, stable attribution after crash recovery, the version 1
journal migration and byte preservation during staged validation. Exclude
the installation identity file from backups. Verify that restore preserves
historical origins and attributes new completions to the restored home.

`vault completion-events --after 0 --logical` exports the versioned completion
envelope through the same page query. Keep `sequence` outside the envelope and
preserve the original event UUID as `event_id` and `idempotency_key`. Confirm
that exporting the same event from another home produces the same envelope
and that omitting `--after` is rejected in logical mode. Audit schema changes
against the documented v1 fields and both existing output shapes.

`vault import-completion-events` processes trusted logical JSON Lines in order
and uses the same per-record intent and recovery path as local completion.
Preserve the imported UUID, origin and observed revision. The receiving
journal assigns the sequence. Test identical and conflicting duplicates,
stale revision, partial import, both pending digests, and unknown origin.
Import preserves existing evidence references and does not deliver Workflow
triggers. Test the production poller with an imported event followed by a
matching local completion. Validate version 1 and 2 migrations without
changing the delivery eligibility of existing records. Device identity and
key management must be explicit before claiming a sync protocol.

## Backup and Restore Validation

Capture and restore must validate all staged files before publication. Validate
agent policy bytes before accepting them and preserve the validated bytes exactly.
Keep ownership, mode, no-follow regular-file, private staging, lock, and path
checks in the backup adapter.

Validate completion events through a private staged Vault. If a hot SQLite
journal is present, recover only in an isolated disposable copy. Reject unsafe
super-journal references before SQLite opens the staged journal. Validation must
not create a lock or mutate staged bytes. Accept both valid pending Markdown
digests when the event contract permits them.

Proposal capture uses the current single schema and fails closed on invalid
records. Historical unreleased absolute-path records must not be silently
published. If a migration is intentionally supported, convert only an exact
canonical default binding, leave the source untouched, and test cross-home
idempotency. External bindings are excluded from default-only backup.

## Verification

Cover byte-exact backup round trips, malformed events, oversized or malformed
policy, hot-journal recovery, unsafe journal rejection, pending digest variants,
proposal validation, default binding conversion, external binding exclusion,
restore failure cleanup, and repeat capture/restore. Run the focused backup and
Vault tests, `cargo xtask check`, and the example Vault validation. Mark future
logical sync as `Planned` until event identity and merge behavior are implemented
and tested end to end.
