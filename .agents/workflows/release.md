# Release Preparation Workflow

This workflow prepares and verifies release inputs. It does not publish
packages or create a public GitHub Release. The project license is Apache-2.0;
see the root `LICENSE`.

1. Confirm the intended version, release scope, target platforms, and package
   set. Publishable packages use the `mcbtodo-` prefix, with `mcbtodo` as the
   CLI package name. Exclude repository-only `mcbtodo-xtask` from crates.io
   publication. Match CI and release-preparation evidence to the exact commit
   identified by the release tag.
2. Follow `.agents/workflows/contribution.md` and
   `docs/licensing.md` to review externally sourced material for
   license compatibility, attribution, and notice obligations. Exclude
   unresolved or incompatible third-party material. Review the tagged tree
   for public disclosure because GitHub uses it to generate automatic source
   archives. The tracked `examples/vault/` fixture must remain synthetic. Keep
   real Vault data, credentials, local configuration, backups, and runtime
   state out of the tag and all artifacts.
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
   available on crates.io. After authorized dependency releases, rerun
   isolated dry-runs for dependents.
4. Inspect every publishable package with
   `cargo package --list --locked --package <crate-name>` and inspect the
   generated `.crate` archive from the release checkout. Confirm accurate
   descriptions, canonical repository metadata, required build inputs,
   Apache-2.0 license text, third-party notices, and the absence of local
   data. The root Vault fixture sits outside the publishable Cargo members.
   Review additions within `apps/` and `crates/` for unintended inputs.
5. Inspect the custom source tarball produced by
   `distro/release/create-source-archive.sh` and both CI-built Linux binary
   archives. Verify the source tarball's tagged inputs and the binary
   archives' architecture labels, three executables, and systemd user unit.
   Check SHA-256 sidecars and compare the included `LICENSE` files with the
   tagged repository copy. Review sandbox smoke results for the extracted
   binaries. System installation and long-term reliability require
   separate evidence.
6. Check AUR and upstream Nix recipes against the exact custom source asset
   from the matching GitHub Release before submitting changes. Review the
   source pin and hash, package build, and installed file list. Static AUR
   checks and Nix evaluation provide narrower evidence than a package build.
7. Obtain maintainer approval before any non-dry-run `cargo publish`,
   public GitHub Release creation, or public asset upload. For an approved
   crates.io release, publish in dependency order so each dependent crate
   can resolve the already-published versions.
8. Record the release tag and commit, commands, package versions, artifact
   checksums, test evidence, and remaining blockers. Record publication
   separately using the exact registry version or uploaded release asset.
   Record downstream submissions and upstream acceptance as separate events.

Do not invent an author or repository URL to make packaging appear complete.
A successful dry-run is not approval to publish.
