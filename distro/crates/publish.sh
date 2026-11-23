#!/usr/bin/env bash
set -euo pipefail

usage() {
  cat <<'EOF'
Usage: distro/crates/publish.sh [--plan] [--from PACKAGE]

--plan         Validate the release checkout and print the remaining package
               order without publishing.
--from PACKAGE Resume at PACKAGE after manually confirming all preceding
               versions are already available on crates.io.
EOF
}

plan_only=false
from_package=""
while (($#)); do
  case "$1" in
    --plan)
      plan_only=true
      ;;
    --from)
      (($# >= 2)) || { usage >&2; exit 2; }
      from_package="$2"
      shift
      ;;
    -h|--help)
      usage
      exit 0
      ;;
    *)
      usage >&2
      exit 2
      ;;
  esac
  shift
done

packages=(
  mcbtodo-domain
  mcbtodo-workflow
  mcbtodo-ports
  mcbtodo-platform
  mcbtodo-protocol
  mcbtodo-agent-codex
  mcbtodo-storage-sqlite
  mcbtodo-vault
  mcbtodo-application
  mcbtodo-execution
  mcbtodo-client
  mcbtodo-tui
  mcbtodo-worker
  mcbtodo-daemon
  mcbtodo
)

root="$(git rev-parse --show-toplevel)"
cd "$root"

package_id="$(cargo pkgid --package mcbtodo --locked)"
version="${package_id##*#}"
tag="v${version}"
tag_commit="$(git rev-parse --verify --quiet "refs/tags/${tag}^{commit}")" || {
  printf 'The exact release tag %s must exist before publishing.\n' "$tag" >&2
  exit 1
}
head_commit="$(git rev-parse HEAD)"
test "$tag_commit" = "$head_commit" || {
  printf 'HEAD must point to the exact release tag %s.\n' "$tag" >&2
  exit 1
}

if [[ -n "$(git status --porcelain=v1 --untracked-files=all)" ]]; then
  printf 'The working tree must be clean, including staged, unstaged, and untracked files.\n' >&2
  exit 1
fi

start=0
if [[ -n "$from_package" ]]; then
  found=false
  for index in "${!packages[@]}"; do
    if [[ "${packages[$index]}" == "$from_package" ]]; then
      start="$index"
      found=true
      break
    fi
  done
  if [[ "$found" != true ]]; then
    printf 'Unknown publishable package: %s\n' "$from_package" >&2
    usage >&2
    exit 2
  fi
fi

printf 'Release %s package order:\n' "$version"
for ((index = start; index < ${#packages[@]}; index++)); do
  printf '  %s\n' "${packages[$index]}"
done

if [[ "$plan_only" == true ]]; then
  printf 'Plan only; no packages were published.\n'
  exit 0
fi

wait_seconds="${MCBTODO_CRATES_INDEX_WAIT_SECONDS:-30}"
if [[ ! "$wait_seconds" =~ ^[0-9]+$ ]]; then
  printf 'MCBTODO_CRATES_INDEX_WAIT_SECONDS must be a non-negative integer.\n' >&2
  exit 2
fi

if [[ -n "$from_package" ]]; then
  printf 'Resume only after confirming every preceding %s version is present on crates.io.\n' "$version"
fi
printf 'This uploads public crates and cannot be rolled back. Continue? [y/N] '
read -r answer
test "$answer" = y || exit 1

publish() {
  local package="$1"
  if ! cargo publish --locked --package "$package"; then
    printf '\nPublishing %s@%s failed or returned an uncertain result.\n' "$package" "$version" >&2
    printf 'Check crates.io before retrying; a failed client command may still have uploaded the package.\n' >&2
    printf 'After confirming registry state, resume explicitly with --from PACKAGE.\n' >&2
    return 1
  fi
}

for ((index = start; index < ${#packages[@]}; index++)); do
  package="${packages[$index]}"
  publish "$package"
  case "$package" in
    mcbtodo-workflow|mcbtodo-ports|mcbtodo-platform|mcbtodo-storage-sqlite|mcbtodo-vault|mcbtodo-application|mcbtodo-execution|mcbtodo-client|mcbtodo-tui)
      if ((wait_seconds > 0)); then
        sleep "$wait_seconds"
      fi
      ;;
  esac
done
