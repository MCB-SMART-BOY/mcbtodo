# Repository Audit Workflow

Use this workflow before a repository-wide refactor, architecture review, release
readiness review, or any change whose effect is not yet bounded. The audit is an
engineering activity. It must produce evidence and a scoped implementation plan,
not a restatement of the product goal.

## Scope and Evidence

1. Read `AGENTS.md`, `.agents/instructions.md`, `docs/README.md`,
   `CONTRIBUTING.md`, `docs/design.md`, `docs/architecture.md`, and
   `docs/architecture-assessment.md`.
2. Record `git status --short`, the current branch, remotes, staged changes, and
   the complete diff. Preserve unrelated user work. Never use `git reset --hard`,
   remove user data, clear a database, or rewrite history as part of an audit.
3. Inspect the Cargo workspace, package names, binary names, feature flags,
   target-specific modules, dependency direction, build scripts, examples, CI,
   release workflows, and `cargo xtask` commands.
4. Trace the real paths for CLI, TUI, MCP, Application, Domain, Vault, Daemon,
   Worker, Workflow, SQLite, Agent, process supervision, notifications, backup,
   and recovery. Identify who owns each invariant and where raw external types
   cross a boundary.
5. Search every external tool invocation, including Bubblewrap, Bash, Nushell,
   jq, systemd, `notify-send`, Codex, Unix sockets, named pipes, and tar. Record
   whether each tool is required for startup, optional for one capability, or
   required by a fail-closed security policy.
6. Inspect Markdown loading and writing, snapshots, revisions, locks, journals,
   SQLite schema and migrations, attempt transitions, completion events, proposal
   queues, agent policy, peer credentials, timeouts, cancellation, output limits,
   and result authorization.
7. Inspect TUI state, keymaps, screen models, presenters, renderers, themes,
   layout metrics, Unicode width handling, narrow-terminal behavior, and tests.
8. Inspect configuration discovery, secrets, runtime files, logs, cache, socket
   paths, user policy, and platform-specific paths. Runtime files are not logical
   sync data.
9. Inspect Linux x86_64 and AArch64 evidence separately from macOS and Windows
   evidence. Distinguish compile checks, startup checks, basic runtime checks,
   adapter tests, and security verification.

## Classification

For every requested capability, classify it as one of the following and attach
an exact file, test, command, or external constraint.

- `Implemented` means source exists and is used by a real caller.
- `Verified` means the relevant behavior passed a focused or integration check.
- `Partially Implemented` means a real path exists but important behavior or
  platform coverage is missing.
- `Planned` means the design is documented without a production caller.
- `Blocked` means a required dependency, credential, or external state prevents
  verification.
- `Unsupported` means the project deliberately rejects the capability.

Do not promote a design statement, type declaration, compile success, or fixture
subprocess into runtime support.

## Required Audit Output

Write or update the dated architecture assessment with:

- current workspace and dependency graph
- real module responsibilities
- preserved business invariants
- duplicate orchestration and redundant abstractions
- boundary leaks and missing adapters
- storage, Agent, process, TUI, and platform evidence
- documentation claims that disagree with source or tests
- proposed files and interfaces for the smallest useful next slice
- focused tests and commands for that slice
- explicit deferred and unsupported capabilities

The report must use the A–G delivery format from `completion.md` when the audit
also includes implementation work. A documentation index update does not replace
this evidence report.

## Audit Commands

Run the commands that apply and record their output rather than claiming a check
was run from a previous session.

```sh
git status --short
git diff --check
cargo metadata --no-deps --format-version 1
cargo xtask agent validate
cargo xtask check
cargo run --locked -- --vault examples/vault --timezone Asia/Shanghai validate
```

For platform or process claims, also run the relevant focused package tests,
`cargo xtask smoke --bin-dir target/debug --sandbox`, and
`cargo xtask smoke-systemd --bin-dir target/debug` when their prerequisites are
available. State unavailable prerequisites and the exact unrun check.

## Stop Conditions

Stop the requested refactor slice when the real caller, invariant owner, or
migration path cannot be identified. Record the blocker and leave a minimal
verified plan. Do not create empty crates, unused traits, speculative plugin
protocols, or TODO-only implementations to make the architecture appear complete.
