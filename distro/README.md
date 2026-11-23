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

Review the package archives and dependency graph before invoking
`distro/crates/publish.sh`. The script publishes all public workspace crates
in dependency order. `mcbtodo-xtask` is repository tooling and is not
published.

## Linux distributions

Arch Linux recipes belong under `distro/aur/`. Nix expressions shared by NUR
and nixpkgs belong under `distro/nix/`; channel-specific entry points remain
under their respective subdirectories. Stable package metadata must use the
source archive and checksum attached to the same GitHub Release.

Publishing to AUR, NUR, or nixpkgs is a separate repository operation. A
nixpkgs pull request is a submission for review, not evidence that the package
has been accepted.
