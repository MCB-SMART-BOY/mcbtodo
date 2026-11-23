#!/usr/bin/env bash
set -euo pipefail

version="${1:?usage: create-source-archive.sh VERSION}"
tag="v${version}"
root="$(git rev-parse --show-toplevel)"
cd "$root"

test "$(git describe --exact-match --tags HEAD)" = "$tag" || {
  printf 'HEAD must be the exact release tag %s\n' "$tag" >&2
  exit 1
}
git diff-index --quiet HEAD -- || {
  printf 'tracked working-tree changes must be committed before archiving\n' >&2
  exit 1
}

out="${2:-target/dist}"
mkdir -p "$out"
archive="${out%/}/mcbtodo-${version}-source.tar.gz"
git archive \
  --format=tar \
  --prefix="mcbtodo-${version}/" \
  "$tag" \
  Cargo.toml Cargo.lock LICENSE README.md .cargo apps crates xtask |
  gzip -n > "$archive"
(cd "$(dirname "$archive")" && sha256sum "$(basename "$archive")" > "$(basename "$archive").sha256")
printf 'Created %s\n' "$archive"
