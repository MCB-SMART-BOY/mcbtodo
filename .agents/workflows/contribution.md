# Contribution Workflow

Use this workflow when a change introduces external material or fixture data.
The maintainer identifies the software as their original work; no ownership
certificate, rights attestation, or private ownership evidence is required.

1. Read `CONTRIBUTING.md`, `docs/licensing.md`, and the relevant
   development, architecture, documentation, and review workflows.
2. Identify code, documentation, tests, fixtures, generated assets, or
   dependencies sourced outside the project.
3. For that external material, record its source and check the applicable
   license, attribution, notices, and redistribution conditions. Do not include
   material with unclear or incompatible terms.
4. Review technical quality independently. Apache-2.0 applies to project
   material distributed under the project license and does not replace the
   separate terms of third-party material.
5. Keep changes to the tracked `examples/vault/` fixture synthetic and suitable
   for public use. Use a disposable synthetic copy for write tests and public
   reproductions. Exclude real Vault records, backups, credentials, local
   configuration, and runtime state from commits, PRs, and attachments.
6. Before a public release, follow `.agents/workflows/release.md` to review
   package contents, third-party notices, and publication readiness.

Do not request ownership certificates, private contracts, or identity
documents. Keep licensing guidance in `docs/`.
