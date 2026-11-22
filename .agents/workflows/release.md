# Release Preparation Workflow

This workflow prepares and verifies release inputs. It does not publish
packages or create a public GitHub Release. The project license is Apache-2.0;
see the root `LICENSE`.

1. Confirm the intended version, release scope, target platforms, and package
   set. Every publishable package must follow the `mcbtodo-` naming convention;
   exclude repository-only `mcbtodo-xtask`.
2. Follow `.agents/workflows/contribution.md` and
   `docs/licensing.md` to review externally sourced material for
   license compatibility, attribution, and notice obligations. Exclude
   unresolved or incompatible third-party material.
3. Run `cargo xtask check`, example Vault validation,
   `cargo package --workspace --exclude mcbtodo-xtask --locked`,
   `cargo xtask verify-crate-packages`,
   and `cargo publish --dry-run --workspace --locked --no-verify`.
   The workspace `cargo package` command compiles each extracted crate,
   resolving unpublished workspace dependencies from Cargo's temporary
   registry. On a developer machine, use a fresh `CARGO_TARGET_DIR` for that
   command to keep old packages with the same version out of verification.
   The dry-run checks the publication path without uploading. A first-release
   dry-run of a single dependent crate may require its dependencies to be
   available on crates.io. Publish crates in dependency order, then rerun
   isolated dry-runs for dependents.
4. Inspect every publishable package with
   `cargo package --list --package <crate-name>`. The xtask verifies the
   `Apache-2.0` metadata and `LICENSE` archive entry; also confirm accurate
   descriptions, canonical repository metadata, required sources, and no
   secrets, private files, or local state. Review third-party notices and
   dependency licensing.
5. Review CI artifacts, architecture labels, checksums, package extraction,
   sandbox smoke results, and installation/recovery evidence. Compare the
   extracted `usr/share/licenses/mcbtodo/LICENSE` with the repository copy.
   Staged archive tests cover the extracted binaries. System installation and
   long-term reliability require separate evidence.
6. For the first crates.io release, publish workspace crates in dependency
   order so each dependent crate can resolve the already-published versions.
   Obtain maintainer approval before any non-dry-run `cargo publish`, public
   GitHub Release creation, or public asset upload. The current repository
   workflow does not perform these actions.
7. Record exact commands, package versions, artifact checksums, test evidence,
   and remaining blockers.

Do not invent an author or repository URL to make packaging appear complete.
A successful dry-run is not approval to publish.
