#!/usr/bin/env bash
set -euo pipefail

cd "$(dirname "$0")"
mode="${1:---static}"

case "$mode" in
  --static)
    bash -n PKGBUILD
    command -v makepkg >/dev/null || {
      printf 'makepkg is required for .SRCINFO validation\n' >&2
      exit 1
    }
    generated="$(mktemp)"
    trap 'rm -f "$generated"' EXIT
    makepkg --printsrcinfo > "$generated"
    diff -u .SRCINFO "$generated"
    if grep -q 'REPLACE_WITH_RELEASE_SHA256' PKGBUILD; then
      printf 'Recipe syntax and .SRCINFO are valid; release checksum is not set yet.\n'
      exit 0
    fi
    printf 'Recipe syntax, .SRCINFO, and release checksum placeholder check passed.\n'
    ;;
  --build)
    bash -n PKGBUILD
    command -v makepkg >/dev/null || {
      printf 'makepkg is required for an Arch package build\n' >&2
      exit 1
    }
    if grep -q 'REPLACE_WITH_RELEASE_SHA256' PKGBUILD; then
      printf 'A real release source archive and its SHA-256 are required before building.\n' >&2
      exit 2
    fi
    makepkg --verifysource
    makepkg --cleanbuild --nodeps --noconfirm
    ;;
  *)
    printf 'usage: %s [--static|--build]\n' "$0" >&2
    exit 2
    ;;
esac
