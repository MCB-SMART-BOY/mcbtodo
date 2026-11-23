# Distribution

Distribution recipes and release helpers live here. `examples/` contains
user walkthroughs and `tests/fixtures/vault/` holds shared test inputs. Keep
personal Vaults, local configuration, credentials, and runtime state out of
version control and distribution inputs.

## Release source

From a checkout at an approved release tag with no tracked changes, run the
source archive helper with the matching version:

```sh
distro/release/create-source-archive.sh 0.1.0
```

The helper archives the tag's `Cargo.toml`, `Cargo.lock`, `LICENSE`,
`README.md`, `.cargo/`, `apps/`, `crates/`, and `xtask/` and writes a SHA-256
sidecar under `target/dist/`. The custom archive omits `examples/`, `tests/`,
and `distro/`. AUR and upstream Nix recipes should pin this exact attached
source asset. GitHub generates its automatic archives from the tagged
tree, which includes the tracked examples and fixtures.

The release-preparation workflow stages the three Linux executables, the
systemd user unit, and the Apache-2.0 license in binary archives for x86_64
and aarch64. It retains them and their checksums as CI artifacts. Uploading
the reviewed source and binary assets to the matching GitHub Release is a
separate, authorized step.

## crates.io

The repository's [release-preparation workflow](../.github/workflows/release-preparation.yml)
validates a version tag; it does not publish crates or create a GitHub Release.
Before publishing, require that workflow and the normal CI checks to have
completed successfully for the exact commit named by the release tag. A
successful manual workflow dispatch against another ref is not evidence for
the tag.

The publisher requires a clean Git worktree, including no untracked files, and
requires `HEAD` to be exactly the `v<workspace-version>` tag. Review all
published package archives, third-party notices, and the dependency order
before running it. To inspect the guarded order without uploading:

```sh
distro/crates/publish.sh --plan
```

The first crates.io release is sequential: a crate that depends on another
`mcbtodo` crate cannot be published until the dependency is available in the
registry index. The script orders all thirteen public workspace packages by
their Cargo dependencies and waits between dependency groups for index
propagation. `mcbtodo-xtask` is repository tooling and is not published.

Each successful upload is permanent for that crate version. If a command
fails, stop and check crates.io for the exact version before retrying; the
client may have received an error after the upload was accepted. Once the
registry state is confirmed, resume with `--from <package>` at the first
package that is not yet published. The operator is responsible for confirming
that every earlier dependency version is available. The script intentionally
does not guess registry state or automatically retry an uncertain upload.

Publishing requires an authorized crates.io login/token and explicit
confirmation at the prompt. Do not publish until the release tag's validation
workflow and CI are green, every package version matches the tag, and the
package-content review is complete.

## Linux distributions

- [AUR](aur/): Arch Linux source package built from the versioned source
  archive. Run `distro/aur/check.sh --static` before submission. The final
  recipe needs the real source archive checksum and the maintainer's AUR
  identity. `--build` also requires an Arch environment with `makepkg`.
- [Nix](nix/): shared Nix expression plus NUR and nixpkgs entry points. NUR
  requires a dedicated package repository and a submission to the NUR
  registry. nixpkgs requires a package pull request. The local entry points
  filter the checkout to the same top-level paths as the custom archive.
  They can still pick up local additions within those paths. Upstream
  recipes need the verified hash of the custom source archive attached to
  the matching GitHub Release.

Never invent or reuse a checksum from a different archive. Publish or submit
only after the matching release exists. A nixpkgs pull request is a submission
for review, not evidence that the package has been accepted or merged.

## Release status

`v0.1.0` was published on 2026-11-23 with the custom source archive and
CI-built Linux x86_64 and aarch64 archives, each with a checksum sidecar.
The custom source archive SHA-256 is
`fb15e9fae29a69b8b6852bb2c05926e718ac01195833ee65be52e8e2fc2ef037`.
See the [v0.1.0 GitHub Release](https://github.com/MCB-SMART-BOY/mcbtodo/releases/tag/v0.1.0).

Distribution channel checks for `v0.1.0` are dated 2026-11-23.

- The AUR package record lists `mcbtodo 0.1.0-1`. The recorded build
  verification covers AArch64. See the [AUR package](https://aur.archlinux.org/packages/mcbtodo)
  and [build record](aur/README.md#v010-status).
- The crates.io index lists `0.1.0` for all 13 publishable workspace
  packages, including `mcbtodo` and `mcbtodo-daemon`.
- The recorded Nix evaluation passed and the build failed Cargo's
  `rust-version` check. NUR and nixpkgs remained pending before submission.
  See [Nix status](nix/README.md#v010-status).
