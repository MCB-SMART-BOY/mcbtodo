# Nix Distribution

`package.nix` is the shared Nixpkgs-style derivation for the three Linux
executables shipped by the project: `mcbtodo`, `mcbtodo-daemon`, and
`mcbtodo-worker`. It compiles from Cargo sources, uses the checked-in
`Cargo.lock`, installs the existing systemd user unit and Apache-2.0 license,
and excludes examples and local Vault data from the build source.

The package metadata lists only `x86_64-linux` and `aarch64-linux`, matching
the Linux release artifact targets. macOS and Windows builds or CLI tests do
not establish that the complete packaged daemon/worker workflow is supported
there. Bubblewrap and Nushell are not hard runtime dependencies; workflows
that require them still depend on the user's host capabilities.

## Local Evaluation

With Nixpkgs available through `NIX_PATH`, evaluate the shared expression
against the current checkout:

```sh
nix-instantiate --eval --strict --json --expr '
  let
    pkgs = import <nixpkgs> {};
    package = pkgs.callPackage ./distro/nix/package.nix {
      version = "0.1.0-pre";
      src = ./.;
    };
  in {
    name = package.name;
    mainProgram = package.meta.mainProgram;
    platforms = package.meta.platforms;
  }
'
```

To build it, evaluate the derivation itself:

```sh
nix-build --expr '
  let pkgs = import <nixpkgs> {};
  in pkgs.callPackage ./distro/nix/package.nix {
    version = "0.1.0-pre";
    src = ./.;
  }
'
```

A build needs a usable Nixpkgs installation and network access to fetch locked
Cargo dependencies. The current pre-release version is only a local
evaluation label; it is not a published version.

## NUR Entry

`nur/default.nix` follows the NUR repository function contract and exposes
`mcbtodo`. Its source is the repository checkout and its version is explicitly
marked `0.1.0-pre`; it does not claim to represent a tagged release.

The checked-in entry can be evaluated from this repository. For an upstream
NUR submission, make a dedicated, slim package repository containing the
shared `package.nix` and a root `default.nix` that exposes `mcbtodo`. The
published package recipe should fetch a fixed, tagged source release rather
than package the entire project repository. Add that public repository to
NUR's central `repos.json` (or use its `file` field while preparing the
repository layout); NUR updates and evaluates registered repositories and the
central manifest change is submitted as a reviewed PR.

## Nixpkgs Entry

`nixpkgs/default.nix` is a local evaluation entry for the shared expression.
For a nixpkgs change, submit the shared derivation as a self-contained
`package.nix` in the appropriate nixpkgs package directory, with `src` set to
the exact upstream release source and `version` set to that release version.
Nixpkgs' `pkgs/by-name` package definitions cannot refer to files outside
their own directory, so do not make the upstream package depend on this
repository's relative `../package.nix`. Follow nixpkgs' current package layout
and review requirements at submission time.

Do not merge a made-up fixed-output hash into either channel. Once a GitHub
source release exists, calculate its hash from the exact release archive and
put that verified value in the nixpkgs fetcher. `cargoLock.lockFile` imports
the committed Cargo lockfile without requiring a guessed aggregate Cargo
vendor hash; this repository has no Git-sourced Cargo dependencies.

NUR and nixpkgs are separate upstream contribution paths. A NUR package
repository is registered through a `repos.json` pull request to
`nix-community/NUR`; a nixpkgs package is proposed as a pull request to
`NixOS/nixpkgs`. Neither action is performed by this repository-local
preparation.

## v0.1.0 Status

The tagged source archive is published at the v0.1.0 GitHub Release. Its
verified SHA-256 is
`fb15e9fae29a69b8b6852bb2c05926e718ac01195833ee65be52e8e2fc2ef037`.
Nix evaluation against current nixpkgs unstable succeeds, but a real package
build is blocked because the project declares Rust 1.100 and current nixpkgs
provides Rust 1.99.1. The build correctly fails Cargo's `rust-version` check.

No NUR repository or registration PR, and no nixpkgs PR, has been submitted.
These channels remain blocked until the package can build with the toolchain
available from nixpkgs unstable. The nixpkgs candidate was based on the
current upstream branch and passed formatting and evaluation checks, but it
was not pushed because the actual build failed. Do not report either package
as available or accepted.

No tag, GitHub Release, external NUR/nixpkgs change, or package publication is
created by these files.

## Upstream Requirement Evidence

Checked 2026-11-23:

- NUR's [repository instructions](https://github.com/nix-community/NUR/blob/master/README.md#how-to-add-your-own-repository)
  require a top-level `default.nix` returning derivations from the supplied
  `pkgs`, register repositories through `repos.json`, run `bin/nur update`,
  and open a PR. They also support a `file` path and recommend keeping repos
  slim because evaluators download and hash them.
- Nixpkgs' [by-name package layout](https://github.com/NixOS/nixpkgs/blob/master/pkgs/by-name/README.md)
  defines top-level packages under `pkgs/by-name/<2-char>/<name>/package.nix`
  and disallows references to files outside that package directory.
- Nixpkgs' [contribution guide](https://github.com/NixOS/nixpkgs/blob/master/CONTRIBUTING.md#how-to-create-pull-requests)
  requires testing and asks PR authors to test with sandboxing and report
  only platforms they actually built on.
- The upstream [GitHub Releases page](https://github.com/MCB-SMART-BOY/mcbtodo/releases)
  currently reports no releases. Therefore there is no stable tagged source
  archive or release checksum to pin yet.
