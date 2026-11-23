# Development Workflow

## Before Editing

1. Read `AGENTS.md`, `.agents/instructions.md`, the relevant design sections,
   and the current implementation. Check `git status` first; preserve existing
   user changes and generated files.
   When accepting work supplied by another contributor, first follow
   `.agents/workflows/contribution.md`; technical review does not establish
   provenance, rights, or permission.
2. State the behavior and owner of the invariant being changed. Keep domain
   rules in `mcbtodo-domain`, Markdown and filesystem durability in
   `mcbtodo-vault`, cross-adapter business use cases in
   `mcbtodo-application`, OS/config boundaries in `mcbtodo-platform`, and
   transport and presentation in the apps.
   Reusable business-shaped provider contracts belong to `mcbtodo-ports`;
   reusable Agent and sandbox execution adapters belong to
   `mcbtodo-execution`; the Daemon owns scheduling and durable run results.
3. Prefer the existing workspace APIs and `cargo xtask` commands. Do not add
   compatibility paths or migration code unless the design explicitly
   requires support for a released format.
   For crate-boundary, persistence-ownership, or runtime-layout changes, also
   follow `.agents/workflows/architecture.md`. Start from a concrete use case;
   do not add a generalized service or store without a demonstrated need.

## Implementation

4. Add focused tests at the layer that owns the changed invariant, including
   stale revisions, interruption, malformed input, and permission boundaries
   when applicable. Use temporary homes for filesystem behavior.
   For calendar focus changes, test object identity separately from occurrence
   identity: recurring or multi-day rows may share a source UUID, and switching
   views or reloading must not silently select a different occurrence.
5. Use `WorkspacePaths` and shared platform file helpers for config and runtime
   locations. Config belongs under `~/.config/mcbtodo/`; mutable state, cache,
   sockets, locks, and the default Vault belong under `~/.mcbtodo/`. Exercise
   `WorkspacePaths::read_config_file` for ordinary user config such as config
   and TUI keymap files. Test symlink, ownership, permissions, hard-link, and
   bounded-read behavior at the layer that opens or publishes the path; use
   atomic same-directory replacement for small mutable preference state. Do
   not assume a private leaf makes its ancestors safe. Use the same preferences
   form for startup repair and in-session edits. Route in-session Settings
   commands through the same active-form dispatcher. Verify that F5 reloads only
   the current page and keeps the draft if the file cannot be read. During a
   running session, an unreadable `config.toml` leaves Keyboard and Agents
   accessible. An unreadable `keymap.toml` opens Keyboard in info mode with
   its own F5 retry and preserves the preferences draft. Codex editing uses the
   same save and reload flow. Reuse the Worker's field validation and keep
   installation diagnostics in `doctor`. Keep the Vault closed until
   startup repair publishes a valid configuration. For agent cache access,
   use `WorkspacePaths::prepare_cache_directory`, `read_cache_file`, and
   `write_cache_file`; on Unix these traverse and operate relative to verified
   directory file descriptors. Test nested and final symlinks, directory
   replacement, ownership, permissions, hard links, and bounded reads. The
   non-Unix fallback uses path-based checks. Preferences and Agent settings on
   Unix share descriptor-relative reads. Preserve their file-specific permissions,
   parsing, and byte limits when changing that boundary.
6. For Inbox conversion, derive retry identity from the source UUID, publish
   the Task before changing its source record, and test interruption recovery
   plus conflicting destination edits. Never describe two Markdown file writes
   as one atomic transaction.
   When changing a port, keep the interface independent of SQLite, Unix socket,
   Bubblewrap, Codex, and other adapter types. Add a real adapter parity test
   in the same change. Do not add a registry, scheduler, or plugin protocol
   until a second real provider or a concrete coordination requirement exists.
7. For interrupted external side effects, preserve `outcome_unknown` as a
   distinct fact. Any operator reconciliation must be durable and auditable,
   must not fabricate execution evidence or trigger success-dependent work, and
   must never silently replay the uncertain operation. If cancellation races
   active work, do not commit a late result or continue downstream nodes unless
   the execution protocol can atomically record both the result and terminal
   cancellation; otherwise persist `outcome_unknown`. When result persistence
   fails after execution, test transaction rollback and restart recovery to
   `outcome_unknown`; do not retry the external operation as part of result
   recovery.
   Keep execution boundaries explicit: the frozen target is a Daemon and an
   independently supervised local Worker, and the production path now launches
   that Worker for script and Agent attempts. Linux tests cover production-path
   cancellation of a live sandbox script and SIGKILL of a Worker after Started,
   with `outcome_unknown`, no step commit, Worker reaping, stopped descendants,
   and no replay after reopening the RunStore. Worker startup arms Linux
   `PDEATHSIG` and checks the expected Daemon PID; the sandbox smoke force-kills
   the Daemon after a script side effect, verifies sampled Worker descendants
   exit, then checks restart state/no replay. A malformed-readiness subprocess
   test and a Started-state mismatched-Result frame passed through the
   production protocol reader and `poll_worker` cover fail-closed supervision,
   process reaping, and no replay. A fixture subprocess emits the Result frame
   through a pipe; it is not the packaged Worker binary. The extracted release
   archive sandbox smoke covers a packaged Worker's nonzero script exit and
   durable failed-step trace. The Worker process test and Linux smoke also send
   malformed Prepare input directly to the Worker executable and require
   rejection before readiness. Packaged-Daemon smoke uses development-only
   protocol fixtures to test malformed readiness and malformed post-Start
   output; it verifies fail-closed behavior, zero step commits, and process
   reaping without adding fault modes to release binaries. These fixtures test
   Daemon supervision, not malformed output emitted by the packaged Worker.
   Combined packaged-Worker fault acceptance remains open.
   These checks do not undo or reconcile external
   effects. Worker
   protocol, attempt identity, RunStore fencing, result commits, and
   `outcome_unknown` recovery must be scenario-backed and tested across process
   failures before claiming the target complete. Store-local epochs are not
   expiring Worker leases or protection against external effects from a
   disconnected process. Distributed Worker clusters remain out of scope.
   When adding RunStore attempt APIs, wire them into the production execution
   path in the same change; do not leave production persistence code unused.
   Bind each claim to run ID, immutable revision, node, epoch, and expected
   step count. Test one-time start, stale/mismatched result rejection,
   monotonic epochs, restart invalidation, atomic attempt/result rollback, and
   no replay after an uncertain side effect. State explicitly that store-local
   fencing is not cross-process Worker fencing.
8. Update user and engineering documentation alongside behavior. Keep design
   goals, implemented behavior, and verified acceptance evidence distinct.
   When a business query is exposed by multiple adapters, add or extend parity
   coverage that exercises the shared application use case through each
   adapter; keep environment and presentation diagnostics in their owning app.
   Follow `docs/README.md` to identify related documents and workflow files.
9. For release-facing changes, preserve `mcbtodo-` crate/package naming and
   the `mcbtodo`, `mcbtodo-daemon`, and `mcbtodo-worker` binary names. The
   project is original software identified by its maintainer and distributed
   under Apache-2.0. Do not invent a specific author identity or canonical
   repository URL; use maintainer-provided values when available. Review
   externally sourced material under `docs/licensing.md` and
   `.agents/workflows/contribution.md`.
10. Follow the repository language-tooling rules in `.agents/instructions.md`:
    Python commands go through `uv`, and JavaScript/TypeScript commands use
    Bun unless the project explicitly requires another package manager.

## Verify

11. Run `cargo xtask agent validate`, `cargo xtask check`, and
    `cargo run --locked -- --vault examples/vault --timezone Asia/Shanghai validate`.
    Use `examples/vault` only as an input fixture. Copy it into an isolated
    temporary workspace for tests that write. Linux CI checks that the example
    tree remains clean, including files normally ignored by Git.
    If a test fails with an intermittent process/lock collision, rerun that
    exact test in isolation and rerun its full test target with normal
    parallelism. A passing retry shows only that the failure did not
    reproduce; investigate and report it if it recurs.
    When changing the harness/workflow registry or context assembly, run
    `cargo test --locked -p mcbtodo-xtask --test agents` and verify the complete
    registered matrix remains covered.
    When changing platform gates, use the package and target list in the
    `cross-check` job of `.github/workflows/ci.yml`. Keep compiler and Cargo
    warnings visible and record any native verification separately.
12. For scheduling, script execution, Agent decisions, or recovery changes,
    also run `cargo xtask smoke --bin-dir target/debug --sandbox` on Linux with
    Bubblewrap and Nushell. Record unavailable checks explicitly.
    For Settings input changes, check both the interactive editor and startup
    repair with the existing PTY smoke. For backup validation changes, use a
    staged bundle with a matching manifest and verify rejection before
    publication.
    For changes to the systemd user unit or its smoke runner, run
    `cargo xtask smoke-systemd --bin-dir target/debug` from a Linux user
    session with Bubblewrap and a reachable user manager.
13. Inspect the final diff and `git status`; do not stage, commit, publish, or
    discard changes unless explicitly requested.
