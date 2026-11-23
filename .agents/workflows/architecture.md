# Architecture Workflow

Use this workflow when changing crate boundaries, adding an adapter or service,
moving business behavior between layers, changing persistence ownership, or
revising the runtime/configuration layout.

For the staged workspace evolution, also follow
[Architecture Evolution](architecture-evolution.md) for scope and acceptance.

## Frame the Change

1. Read `AGENTS.md`, `.agents/instructions.md`, `docs/design.md`,
   `docs/architecture.md`, and `docs/architecture-assessment.md`. Inspect the
   working tree and preserve changes already present.
2. Start from a concrete user scenario and its observable result. Trace the
   current path through CLI, TUI, MCP, daemon, application, domain, Vault, and
   platform as applicable. Identify the invariant owner and the duplicated
   orchestration or boundary failure the change addresses.
3. Separate the target boundary from the implementation baseline. The
   `application` crate is the intended shared business-use-case boundary, but
   Vault owns Markdown parsing, writer locking, event journals, recovery, and
   storage-specific atomic publication. Do not move storage mechanisms into
   application merely to make the dependency graph look uniform.
   For execution changes, distinguish an in-process coordinator, a spawned
   restricted child process, and an independently supervised Worker process.
   Keep one queue and authoritative run state in the local Daemon. Use the
   attempt envelope and failure contract in `docs/design.md` §11.3 as the
   protocol baseline. Update both the design and acceptance tests when changing
   that contract. Separate tests of the packaged Worker from tests that use a
   subprocess substitute. Consult `docs/architecture-assessment.md` for dated
   evidence and outstanding failure cases.
   `mcbtodo-protocol::worker` defines the wire contract, `mcbtodo-worker`
   provides the worker entry point, and `mcbtodo-execution` provides the
   shared Agent and sandbox implementations.
4. Keep dependencies directed inward: apps may depend on application and
   infrastructure; application may compose domain and infrastructure APIs;
   domain must not depend on adapters, filesystem paths, or transport types.
   Protocol owns wire contracts, platform owns OS/config boundaries, and
   `mcbtodo-execution` owns reusable execution adapters but not queue state or
   result persistence. `mcbtodo-ports` owns only business-shaped contracts
   already required by a real caller. Apps own presentation and transport
   parsing.

## Choose and Implement a Slice

5. Prefer one end-to-end scenario and the smallest boundary change that
   removes real duplication or prevents a concrete invariant violation. Do not
   introduce a general daemon query/index service, abstraction layer, or new
   persistence store without measured need and an explicit design decision.
6. Define typed inputs, outputs, errors, and revision semantics at the owning
   layer. Keep transport DTOs and rendering in adapters. Preserve UUID identity,
   occurrence identity, timezone meaning, idempotency, stale-write rejection,
   and `outcome_unknown` semantics where relevant. When a use case combines
   metadata from multiple records, obtain and validate that metadata from one
   Vault snapshot unless the design explicitly permits mixed revisions.
7. Add focused tests at the owner layer and adapter parity tests for each
   shared scenario that an adapter exposes. Parity means matching business
   rules and revision behavior, not identical command sets or interaction
   flows. Keep a scenario-to-adapter matrix when adapter coverage changes.
   For a new port, require one real adapter and one focused contract test
   before adding another provider, registry, scheduler, or plugin protocol.
   Provider metadata describes capability and health for discovery, but it
   never grants authorization. Keep policy, budget, idempotency, revision
   authorization, result submission, and `outcome_unknown` in the host.
   Include malformed input, stale revisions, interrupted operations, and
   restart behavior when the affected use case requires them. Reuse an
   adapter's already-validated snapshot instead of reloading it just to route
   through a shared API.
   For the required Worker boundary, retain the production-path process-tree
   cancellation/reaping regression and test stale-result rejection,
   Daemon-death cleanup of the sampled Worker process tree, external
   side-effect reconciliation after Daemon death, immutable revision and credential
   binding, durable result commits, and uncertain external outcomes before
   claiming recoverable isolation. RunStore epoch checks are not lease expiry
   or cross-process fencing. Existing cancellation coverage does not replace
   remaining independent crash/restart cases or packaged-binary failure
   acceptance.
   For side-effecting execution, inject a RunStore result-transaction failure
   and verify atomic rollback, restart to `outcome_unknown`, and no replay.
8. Preserve the filesystem contract: config under `~/.config/mcbtodo/`;
   mutable runtime state, sockets, locks, cache, and default Vault under
   `~/.mcbtodo/`. Use `WorkspacePaths` and its checked file helpers. When
   canonicalizing a Vault path, define the default Vault by exact equality
   between the canonical selected root and canonical configured default root.
   A symlink alias to the default root keeps default proposal binding and is
   backed up from its canonical real directory after checking the opened
   directory identity against the resolved source; restore materializes
   ordinary directory contents. External roots stay outside default-only
   backup, and symlinks within the Vault content tree remain rejected. Test
   binding, backup inclusion, replacement detection, restore, and `--vault`
   behavior together.

## Synchronize and Verify

9. Update `docs/architecture.md` for current boundaries and data ownership,
   `docs/architecture-assessment.md` for dated evidence and remaining gaps,
   `docs/design.md` only when goals or decisions change, and `README.md` when
   user-visible behavior changes. Update `.agents/workflows/` when the
   engineering procedure itself changes.
10. Follow `.agents/workflows/develop.md` for implementation checks and
    `.agents/workflows/documentation.md` for documentation consistency. Run
    `cargo xtask check`, example Vault validation, and the relevant smoke tests.
    Record unavailable platform checks rather than implying acceptance.
11. Inspect the final diff and working tree. Do not stage, commit, publish, or
    discard changes unless explicitly requested.

The maintainer identifies this as original software distributed under
Apache-2.0. Architecture changes do not replace the third-party material
review required for externally sourced content. For licensing and release
scope, follow `docs/licensing.md` and `.agents/workflows/release.md`.
