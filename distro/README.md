# Distribution

Distribution recipes and release helpers live here. Package recipes must point
to the same tagged source release and must not include local Vault data.

## Release source

After committing and pushing a release commit, create and push its version tag.
Then run:

```sh
distro/release/create-source-archive.sh 0.1.0
```

The script archives only tracked Cargo build inputs from the exact tag, writes a
SHA-256 sidecar, and places both under `target/dist/`. Upload the archive and
sidecar to the matching GitHub Release. The generated source archive excludes
`distro/`, so its digest does not depend on package recipes that embed it.

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
  registry; nixpkgs requires a package pull request. Both need the verified
  hash of the source archive attached to the same GitHub Release.

Never invent or reuse a checksum from a different archive. Publish or submit
only after the matching release exists. A nixpkgs pull request is a submission
for review, not evidence that the package has been accepted or merged.

## Release status

`v0.1.0` is released at
https://github.com/MCB-SMART-BOY/mcbtodo/releases/tag/v0.1.0. The release
contains the tracked-source archive, checksum sidecar, and CI-built Linux
x86_64 and aarch64 archives. The source archive SHA-256 is
`fb15e9fae29a69b8b6852bb2c05926e718ac01195833ee65be52e8e2fc2ef037`.

- AUR: `mcbtodo 0.1.0-1` published at https://aur.archlinux.org/packages/mcbtodo.
  The source checksum and AArch64 package build were verified.
- crates.io: 12 of 13 packages are published at `0.1.0`. The root
  `mcbtodo@0.1.0` upload received HTTP 429 and the exact-version API confirms
  it is not registered. crates.io permits another attempt after
  2026-11-23 05:36:19 UTC (13:36:19 Asia/Shanghai); do not retry earlier.
- NUR and nixpkgs: not submitted. The real build currently fails because
  nixpkgs unstable's Rust compiler is older than the package's declared
  `rust-version`; see [Nix status](nix/README.md#v010-status).

The release-preparation workflow validates packages and builds artifacts; it
does not publish crates or create a GitHub Release.
