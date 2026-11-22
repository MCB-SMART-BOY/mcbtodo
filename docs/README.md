# Documentation Index

This index distinguishes product intent from current behavior and points to
the operational source of truth. When documents disagree, use the order below
and update the affected documents together.

## Start Here

- [User guide and command reference](../README.md): supported commands,
  configuration locations, operating-system scope, and release status.
- [Contribution guide](../CONTRIBUTING.md): change scope, testing, and
  third-party material handling.
- [Architecture](architecture.md): crate ownership, data authority, paths,
  runtime boundaries, and current integration behavior.
- [Design](design.md): product goals and acceptance requirements. Goals are
  not implementation claims.
- [Architecture acceptance assessment](architecture-assessment.md): dated
  evidence, milestone status, and gaps that remain unverified.
- [Naming and publication](naming.md): Cargo package names, binaries, and
  crates.io readiness.
- [License and third-party material](licensing.md): the project's
  Apache-2.0 license and review requirements for externally sourced material.
- [Apache License 2.0](../LICENSE): project license text.
- [Agent adapter protocol](agent-adapter.md): local adapter authorization and
  socket contract.
- [TUI design system](tui-design.md): presentation boundaries, theme,
  responsive layout, and visual tests.
- [Storage and sync model](storage-and-sync.md): data authority, backup and
  migration behavior, and cross-device event requirements.
- [Focus reports](focus-reports.md): timer report semantics and output formats.
- [Systemd deployment](../deploy/systemd/README.md): Linux user-service
  installation and diagnostics.
- [Dependency maintenance](dependency-maintenance.md): Dependabot cadence and
  review expectations for Cargo and GitHub Actions updates.
- [Workflow example](../examples/workflow/README.md): fixture graph and smoke
  coverage.
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
  dispatch. Linux jobs check both release architectures, validate the
  example Vault and retain binary artifacts. Additional jobs cover
  cross-target Rust checks and native macOS and Windows CLI checks.
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

Use the implementation and its tests to establish what the software currently
does. `README.md` owns user-facing commands and configuration behavior;
`architecture.md` owns current component, data, and path boundaries;
`design.md` owns product intent and constraints; and
`architecture-assessment.md` owns dated acceptance evidence and remaining
gaps. Examples demonstrate only the behavior they actually exercise. Agent
workflow behavior is defined by `.agents/registry.toml` and the referenced
workflow files.

## Change Documentation Together

For a user-visible behavior change, update the README and the owning detailed
document. For a crate boundary, data-path, or recovery change, update
`architecture.md` and reassess `architecture-assessment.md`. For a product
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
