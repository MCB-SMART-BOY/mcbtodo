# Documentation Workflow

Use this workflow when adding or changing user documentation, architecture
records, examples, agent instructions, or AI workflows.

## Establish Facts

1. Read `AGENTS.md`, `.agents/instructions.md`, `docs/README.md`, and the
   relevant source, tests, and existing documentation. Inventory repository
   Markdown with `rg --files -g '*.md'`; include user guides, examples,
   deployment docs, every harness instruction, every registered workflow,
   `CONTRIBUTING.md`, the GitHub PR template, and rights/contribution guidance.
   For a repository-wide documentation request, inspect every project document
   in that inventory; do not treat an index update as a substitute for
   updating or verifying its sources. Example Vault records are user data,
   not project documentation, and should only be changed when fixture content
   itself needs to change. Inspect `git status` first and preserve all existing
   files and changes.
2. Separate product intent, implemented behavior, and verified acceptance.
   Use `docs/design.md` for goals, source and tests for implementation facts,
   and `docs/architecture-assessment.md` for dated verification evidence.
   Never upgrade a design statement or a single example into a claim that a
   feature is complete.
3. Keep path, command, package, and binary claims aligned with implementation.
   Configuration belongs under `~/.config/mcbtodo/`; mutable runtime data,
   cache, sockets, locks, and the default Vault belong under `~/.mcbtodo/`.
   Crate and package names use the `mcbtodo-` prefix; the user-facing binaries
   are `mcbtodo` and `mcbtodo-daemon`. The default Vault identity is the exact
   canonical-path match against the configured default root: its symlink alias
   preserves default proposal binding and backup inclusion, while symlinks
   within its contents remain rejected. Document and test that distinction.
   For execution architecture, distinguish Daemon coordination, in-process
   scheduling, sandboxed child processes, and a separately supervised Worker.
   The frozen target is a single-host Daemon plus independent local Worker;
   distributed Worker clusters are deferred. The Daemon now starts and
   supervises the local Worker in production. The process-group termination
   helper has a real parent/descendant test, full Daemon cancellation with a
   live workload and Worker SIGKILL after Started pass with no-replay recovery,
   and sandbox smoke verifies state recovery/no replay after force-killing a
   Daemon with a live script. It samples the observable `/proc` descendant
   tree before Daemon termination and waits for those sampled processes to exit
   or become zombies. This does not establish containment of processes that
   were not sampled, nor undo or reconcile external side effects. Malformed
   readiness output has a production child-process test, and a mismatched
   Result frame emitted by a fixture subprocess through a pipe and passed
   through the production protocol reader and Started `poll_worker` path
   verifies `outcome_unknown` and restart no-replay. The fixture is not the
   packaged Worker binary. The extracted release archive sandbox smoke covers
   a packaged Worker's nonzero script exit and durable failed-step trace;
   malformed-frame and combined-fault acceptance for the packaged binary
   remain gaps.
   Keep design targets, current behavior, and acceptance evidence separate.

## Make and Synchronize Changes

4. Write for the document's owner: `README.md` for user operations,
   `docs/architecture.md` for current boundaries, `docs/design.md` for goals,
   `docs/architecture-assessment.md` for dated evidence, and
   `docs/naming.md` for package and publication rules.
5. Update every affected index, example, workflow, and harness instruction.
   Keep each harness guide linked from `.agents/README.md`; keep registered
   workflows linked from both `.agents/README.md` and `docs/README.md`.
   Document the shared instructions in `docs/README.md`. When adding or
   removing a document or workflow, update its index and, for workflows,
   `.agents/registry.toml`. Keep links relative and verify targets.
   Review the entire repository Markdown inventory, including each registered
   workflow and harness file; do not assume updating the main README alone
   synchronizes the project documentation. `cargo xtask agent validate`
   checks the required root `AGENTS.md`, registered sources, and registered
   workflow links and local source paths in both `.agents/README.md` and
   `docs/README.md`. It does not validate arbitrary Markdown links or heading
   fragments elsewhere in the repository; check those across the full project
   Markdown inventory separately.
6. The maintainer identifies this as original software distributed under
   Apache-2.0; keep license and package metadata consistent. Do not invent a
   specific author identity or canonical repository URL. For externally
   sourced material, check its source, license compatibility, attribution, and
   notices under `docs/licensing.md`.
   When changing contribution intake or publication scope, synchronize
   `CONTRIBUTING.md`, `.github/pull_request_template.md`, the contribution
   workflow, `docs/licensing.md`, and the release workflow as applicable.
7. Keep changes focused. Do not reformat or rewrite unrelated documents just
   for stylistic uniformity. Remove generated diagram/export noise if present,
   without changing the intended content.

## Verify

8. Manually check every local Markdown link target and fragment across the
   complete project Markdown inventory; do not treat registry validation as
   general link validation. Also check package/binary names,
   filesystem paths, dates, and implementation claims against source and
   tests. Check both directions of the documentation index: every project
   documentation file is discoverable where appropriate, and every registered
   workflow is indexed. Search the complete documentation inventory, registered
   workflows, and harness guidance for stale terminology, incorrect rights
   claims, and contradictions. Keep current implementation, target design,
   and verified acceptance clearly distinct in each owning document. Do not
   mechanically rewrite fixture content that is not project documentation.
9. Run `cargo xtask agent validate`, `cargo xtask check`, and the example Vault
   validation command from `.agents/instructions.md` when available. Report
   any check that cannot run and why.
10. Inspect the final diff and `git status`. Do not stage, commit, publish, or
    discard changes unless explicitly requested.
