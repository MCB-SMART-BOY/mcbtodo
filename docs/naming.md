# Naming Convention

## Cargo packages

Every Cargo package is identifiable as part of mcbtodo. Library crate names use
the `mcbtodo-` prefix; the primary CLI package and binary are named `mcbtodo`,
and executable names describe their user-facing role:

| Cargo package | Source directory | Binary |
| --- | --- | --- |
| `mcbtodo` | `apps/mcbtodo` | `mcbtodo` |
| `mcbtodo-daemon` | `apps/mcbtodo-daemon` | `mcbtodo-daemon` |
| `mcbtodo-worker` | `apps/mcbtodo-worker` | `mcbtodo-worker` |
| `mcbtodo-agent-codex` | `crates/agent-codex` | Library |
| `mcbtodo-application` | `crates/application` | Library |
| `mcbtodo-domain` | `crates/domain` | Library |
| `mcbtodo-execution` | `crates/execution` | Library |
| `mcbtodo-platform` | `crates/platform` | Library |
| `mcbtodo-ports` | `crates/ports` | Library |
| `mcbtodo-protocol` | `crates/protocol` | Library |
| `mcbtodo-storage-sqlite` | `crates/storage-sqlite` | Library |
| `mcbtodo-vault` | `crates/vault` | Library |
| `mcbtodo-workflow` | `crates/workflow` | Library |
| `mcbtodo-xtask` | `xtask` | `xtask` (repository-only development tool) |

Internal dependencies declare both `path` and `version`, so workspace builds
use local sources while published crates resolve dependencies from crates.io.
The `mcbtodo-xtask` package remains unpublished; `cargo xtask` is the stable
repository command provided by `.cargo/config.toml`.

## Runtime names

The daemon executable is `mcbtodo-daemon`; its systemd unit is
`mcbtodo-daemon.service`, its Unix socket is `mcbtodo-daemon.sock`, and its
state lock is `mcbtodo-daemon.lock`. User-facing commands remain under the
`mcbtodo` executable.

Rust library imports use the corresponding underscore form, for example
`mcbtodo_domain` for the Cargo package `mcbtodo-domain`. Source directories
remain short role-based paths (`crates/domain`, `crates/vault`, and so on);
they are not package names.

## crates.io publication status

Workspace library and application crates are configured for publication,
except `mcbtodo-xtask`, which is repository-only. Each publishable member
inherits `Apache-2.0` and the canonical repository URL from the workspace and
declares a package-specific description; the complete license text is in the
root `LICENSE`. Publication preparation is not publication: every crate must
still pass package-content review.

Before publishing, follow the [license and third-party material guidance](licensing.md) and
[release workflow](../.agents/workflows/release.md): review third-party
licenses and notices, verify package descriptions, repository metadata, and
docs.rs targets, then run
`cargo xtask verify-crate-packages` and inspect each publishable crate with
`cargo package --list`. The xtask checks Apache-2.0 package metadata and
confirms the Cargo archive listing contains `LICENSE`; it does not replace
review of package contents, third-party notices, or metadata accuracy. The
release-preparation workflow uses `--no-verify` for Cargo package and publish
dry-runs because first-release workspace dependencies are not yet available
from crates.io; the full workspace check compiles and tests local sources, but
does not establish standalone builds against published dependencies. The
workspace repository URL is
`https://github.com/MCB-SMART-BOY/mcbtodo`. As of 2026-11-23, all thirteen
publishable packages passed local archive creation, Apache-2.0 archive checks,
and the workspace `cargo publish --dry-run`. Publish dependencies in an order
accepted by crates.io, and exclude `mcbtodo-xtask`. This preparation does not
publish any package.
