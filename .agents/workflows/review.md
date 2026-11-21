# Review Workflow

1. Read repository instructions and inspect the working tree before review.
   Review only the requested change; do not overwrite or clean unrelated files.
   For externally supplied contributions, also follow
   `.agents/workflows/contribution.md`; do not infer rights from authorship or
   repository access.
2. Compare the change with design invariants and inspect all affected callers,
   storage boundaries, and error paths. Treat identity, revisions, time zones,
   event ordering, and idempotency as explicit contracts.
3. Check external-edit behavior, graph references, date boundaries, malformed
   input, and crash/restart behavior wherever those apply.
   For calendar and TUI navigation changes, check whether row identity is an
   object UUID or a specific occurrence; repeated rows must not restore focus
   to the wrong date/time occurrence.
   For scheduler/executor changes, verify that claims about process isolation
   match both the frozen target and actual topology. Production Daemon starts
   and supervises an independent local Worker for script and Agent attempts.
   Check cancellation, process-tree reaping, Daemon/Worker crash recovery,
   stale-result rejection, and `outcome_unknown` behavior. Distinguish fixture
   subprocess coverage from tests of the packaged Worker binary; packaged
   Worker failure combinations and external-side-effect reconciliation remain
   acceptance gaps. Store epochs are not expiring leases and do not fence
   external effects. Distributed Worker clusters remain deferred.
4. Verify the application boundary: adapters should not independently compose
   shared business operations; storage-specific locking and recovery may remain
   in Vault or the daemon.
   For architecture changes, follow `.agents/workflows/architecture.md` and
   check that the change solves a traced user scenario without moving
   storage-specific responsibilities into application.
5. Run focused tests, `cargo xtask check`, and example Vault validation as
   appropriate. Report checks not run and the reason.
6. Check documentation and workflows for agreement with actual behavior.
   Design goals must not be presented as implementation or acceptance evidence.
7. For release-facing changes, inspect workspace membership, package names,
   metadata, lockfile consistency, package contents, and CI/release behavior.
   A dry-run is not publication approval.
8. For externally sourced material, verify its source and compatibility with
   the project's Apache-2.0 license. Follow `docs/licensing.md` to check
   attribution and notice obligations.
9. For Inbox conversion, verify deterministic Task identity, exact-content
   retry validation, revision checks, source retention, and behavior when either
   file was edited between attempts.
10. Report actionable findings first, ordered by impact, with file and line
    references. Then list assumptions, unrun checks, and concise change context.
