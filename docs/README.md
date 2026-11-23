# Documentation Index

Use this index to find the document that owns a command, component boundary,
design goal, or acceptance result. Resolve discrepancies against the
implementation and its tests, then update the affected documents together.

## Start Here

- [User guide and command reference](../README.md): supported commands,
  configuration locations, operating-system scope, and release status.
- [Contribution guide](../CONTRIBUTING.md): change scope, testing, and
  third-party material handling.
- [Architecture](architecture.md): crate ownership, CLI/TUI/MCP routes,
  Daemon and Worker responsibilities, and platform capability gates.
- [Design](design.md): product goals and acceptance requirements. Goals are
  not implementation claims.
- [Architecture acceptance assessment](architecture-assessment.md): source
  audit, dated CI and smoke evidence, and outstanding acceptance gaps.
- [Naming and publication](naming.md): Cargo package names, binaries, and
  crates.io readiness.
- [License and third-party material](licensing.md): the project's
  Apache-2.0 license and review requirements for externally sourced material.
- [Apache License 2.0](../LICENSE): project license text.
- [Agent adapter protocol](agent-adapter.md): local adapter authorization and
  socket contract.
- [TUI design system](tui-design.md): presentation boundaries, theme,
  responsive layout, and visual tests.
- [Storage and sync model](storage-and-sync.md): data authority,
  transaction and recovery rules, private backup, and manual completion-event
  exchange.
- [Focus reports](focus-reports.md): timer report semantics and output formats.
- [Systemd deployment](../deploy/systemd/README.md): Linux user-service
  installation and diagnostics.
- [Distribution](../distro/README.md): tagged source archives, crates.io
  publication order, and Linux distribution package locations.
- [Dependency maintenance](dependency-maintenance.md): Dependabot cadence and
  review expectations for Cargo and GitHub Actions updates.
- [Runnable examples](../examples/README.md): release checks in a Vault and
  the page-table check workflow.
- [Documentation workflow](../.agents/workflows/documentation.md):
  documentation ownership, evidence, and synchronization rules.
- [Architecture workflow](../.agents/workflows/architecture.md): scenario-led
  boundary changes, dependency direction, and architecture verification.

## AI Workflows

The canonical workflow registry is
[`../.agents/registry.toml`](../.agents/registry.toml). Its registered
procedures are:

- [`develop`](../.agents/workflows/develop.md): implementation boundaries,
  focused testing, and delivery checks.
- [`contribution`](../.agents/workflows/contribution.md): review of
  externally sourced material and acceptance boundaries.
- [`review`](../.agents/workflows/review.md): risk-first review, invariants, and
  findings.
- [`release`](../.agents/workflows/release.md): third-party license review,
  package validation, and explicit publication gates.
- [`documentation`](../.agents/workflows/documentation.md): evidence,
  consistency, indexing, and documentation verification.
- [`architecture`](../.agents/workflows/architecture.md): scenario-led
  architecture changes and boundary evidence.
- [`architecture-evolution`](../.agents/workflows/architecture-evolution.md):
  staged architecture evolution, invariants, and acceptance gates.
- [`audit`](../.agents/workflows/audit.md): repository audit, evidence
  classification, gap analysis, and scoped implementation slices.
- [`ports`](../.agents/workflows/ports.md): stable business ports, adapter
  contracts, and boundary verification.
- [`storage`](../.agents/workflows/storage.md): business repositories, SQLite
  transactions, migrations, snapshots, and recovery.
- [`agent`](../.agents/workflows/agent.md): Agent providers, policy, transport,
  sessions, supervision, and Codex integration.
- [`platform`](../.agents/workflows/platform.md): platform capability states,
  optional tools, and fail-closed sandbox behavior.
- [`tui`](../.agents/workflows/tui.md): ScreenModel, theme, layout, keymaps,
  terminal fallback, and visual tests.
- [`sync`](../.agents/workflows/sync.md): backup, event identity, restore
  validation, and cross-device sync boundaries.
- [`completion`](../.agents/workflows/completion.md): delivery gates, evidence
  labels, and the A–G completion report.

Harness-specific setup is indexed in
[`../.agents/README.md`](../.agents/README.md); the harness documents and
workflow sources are loaded according to the registry. Update the registry
when adding or removing a workflow, and keep this index synchronized.

## GitHub Actions

These executable CI workflows are separate from the AI workflow registry:

- [Security policy](../SECURITY.md): supported versions and the current
  private vulnerability reporting status.
- [Bug report form](../.github/ISSUE_TEMPLATE/bug_report.yml): structured
  public reports for reproducible non-security defects.
- [Code owners](../.github/CODEOWNERS): review requests for repository
  control-plane files. Required approval still depends on GitHub branch rules.
- [CI](../.github/workflows/ci.yml) runs on push, pull request and manual
  dispatch. The jobs cover both Linux release architectures, native macOS
  and Windows foreground behavior, and separate cross-target Rust checks.
  Linux jobs test the extracted binary archives. Dated results are recorded
  in the assessment.
- [Release preparation](../.github/workflows/release-preparation.yml):
  version-tag push or manual dispatch; validates packages and staged Linux
  binaries and checks that publishable crate archives carry the Apache
  license text, but does not publish crates or create a GitHub Release. Its
  `cargo publish --dry-run --no-verify` step exercises packaging and the
  upload path; the preceding workspace check compiles and tests local
  sources. It does not compile extracted crates against registry-only
  dependencies. Initial publication and isolated verification must follow
  dependency order. A manual dispatch
  validates the selected ref and does not itself assert that the ref is an
  authorized release tag.

## Source of Truth

Use the implementation and its tests to establish what the software
currently does. The user guide owns commands and configuration behavior.
Architecture owns component boundaries and runtime paths. Storage and sync
owns persistence, recovery and exchange behavior. Design owns product
intent and constraints. The assessment owns dated verification evidence.
Agent workflow behavior is defined by `.agents/registry.toml` and its
referenced files.

## Change Documentation Together

For a user-visible behavior change, update the README and the owning detailed
document. For a crate or runtime boundary change, update `architecture.md`.
For a persistence or recovery change, update `storage-and-sync.md`. Record
any new acceptance evidence in `architecture-assessment.md`. For a product
goal or acceptance change, update `design.md` without presenting the goal as
implemented. For release, naming, or licensing changes, update `naming.md`,
`licensing.md`, the `.agents/workflows/release.md` workflow, Cargo
metadata, and the GitHub Actions release-preparation workflow as applicable.

The project-wide engineering rules are in
[`../.agents/instructions.md`](../.agents/instructions.md). Harness setup and
AI workflows are documented in [`../.agents/README.md`](../.agents/README.md).
The `codex`, `claude`, `opencode`, `pi`, and `generic` harness-specific
guides are linked from that registry index and loaded only for the selected
harness.
Use the documentation workflow when changing docs or workflow policy, and
update this index when adding or removing project documentation.

The documentation set includes user and engineering guides, agent harness and
workflow instructions, deployment docs, and executable example guides.
Example Vault Markdown is user-data-shaped fixture content, not project policy
documentation; change it only when the fixture itself needs to change.
