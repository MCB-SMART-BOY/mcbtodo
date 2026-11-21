# mcbtodo engineering instructions

- The design is in `docs/design.md`. The implemented Vault format and commands are documented in `README.md` and demonstrated in `examples/vault/`.
- Use `docs/README.md` as the documentation index. Update user docs, architecture evidence, and the applicable `.agents/workflows/` file with behavior changes; keep goals distinct from verified implementation.
- For documentation-only changes, follow `.agents/workflows/documentation.md`. Treat source and executable tests as evidence for implementation claims; date and scope any assessment rather than implying unverified completeness.
- Preserve project naming: Cargo crates and packages use the `mcbtodo-` prefix (except the root workspace if it is not a package); user-facing executables are `mcbtodo` and `mcbtodo-daemon`. See `docs/naming.md`.
- Preserve the dependency direction from the application to Vault to domain. Keep calendar semantics and graph invariants in the domain. Keep raw Markdown loading in Vault.
- A business object is identified by its UUID. An activity period, an event, a time block and a deadline have distinct calendar semantics. Task Graph edges point from prerequisite to dependent WorkUnit.
- Use `cargo xtask check` before delivering code. Use `cargo run --locked -- --vault examples/vault --timezone Asia/Shanghai validate` to verify the fixture.
- Use `uv run` and `uvx` for Python. Manage Python dependencies through `uv add` or `uv sync`. Prefer Bun for JavaScript and TypeScript, honoring an explicitly required package manager.
- The project is licensed under Apache-2.0; see the root `LICENSE` and Cargo workspace metadata. Do not invent copyright notices, authorship, or repository URLs.
- The maintainer identifies the project as original software they authored; no ownership certificate, rights attestation, or private ownership evidence is required.
- Before including externally sourced code, documentation, fixtures, assets, or other material, follow `.agents/workflows/contribution.md` and `CONTRIBUTING.md` to check its source, license, attribution, and notices.
- Follow [the license and third-party material guidance](../docs/licensing.md) for Apache-2.0 distribution and external-material requirements.

## Language Tooling

- All Python operations must go through `uv`. Run scripts with `uv run
  <script>` and one-off tools with `uvx <tool>`. Never invoke `python`,
  `python3`, another Python interpreter directly, `pip`, or `uv pip`. Declare
  dependencies in `pyproject.toml` and manage them with `uv add` or `uv sync`;
  do not create virtual environments manually.
- Prefer `bun` for JavaScript and TypeScript runtimes, package management,
  scripts, tests, and builds. Use `bun install`, `bun add`, and `bun run`.
  Do not use `npm`, `npx`, `yarn`, or `pnpm` when Bun is available. Respect an
  existing package manager when the project explicitly requires another one
  for compatibility or reproducible builds.
