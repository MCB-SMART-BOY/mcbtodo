#!/usr/bin/env bash
set -euo pipefail

root="$(git rev-parse --show-toplevel)"
cd "$root"
tag="$(git describe --exact-match --tags HEAD)"
version="${tag#v}"
test "$version" != "$tag" || {
  printf 'HEAD must be checked out at a v<version> release tag\n' >&2
  exit 1
}
git diff-index --quiet HEAD -- || {
  printf 'tracked working-tree changes must be committed before publishing\n' >&2
  exit 1
}

printf 'Publishing the mcbtodo %s workspace to crates.io in dependency order.\n' "$version"
printf 'This uploads public crates and cannot be rolled back. Continue? [y/N] '
read -r answer
test "$answer" = y || exit 1

publish() {
  cargo publish --locked --package "$1"
}

publish mcbtodo-domain
publish mcbtodo-workflow
sleep "${MCBTODO_CRATES_INDEX_WAIT_SECONDS:-30}"
publish mcbtodo-ports
sleep "${MCBTODO_CRATES_INDEX_WAIT_SECONDS:-30}"
publish mcbtodo-platform
sleep "${MCBTODO_CRATES_INDEX_WAIT_SECONDS:-30}"
publish mcbtodo-protocol
publish mcbtodo-agent-codex
publish mcbtodo-storage-sqlite
sleep "${MCBTODO_CRATES_INDEX_WAIT_SECONDS:-30}"
publish mcbtodo-vault
sleep "${MCBTODO_CRATES_INDEX_WAIT_SECONDS:-30}"
publish mcbtodo-application
publish mcbtodo-execution
sleep "${MCBTODO_CRATES_INDEX_WAIT_SECONDS:-30}"
publish mcbtodo-worker
publish mcbtodo-daemon
sleep "${MCBTODO_CRATES_INDEX_WAIT_SECONDS:-30}"
publish mcbtodo
