# Arch Linux (AUR)

`PKGBUILD` builds the three release executables from the versioned source
archive attached to the matching GitHub Release. It installs:

- `mcbtodo`, `mcbtodo-daemon`, and `mcbtodo-worker` under `/usr/bin`
- `mcbtodo-daemon.service` under `/usr/lib/systemd/user`
- the project Apache-2.0 license under `/usr/share/licenses/mcbtodo`

The service is a user service. Package installation does not enable or start it;
users may manage it with `systemctl --user`.

The repository's Apache-2.0 license covers the package recipe and helper files;
the package installs the same license from the tagged source archive.

## Local checks

Run the metadata and syntax check on any machine with Arch `makepkg`:

```sh
distro/aur/check.sh --static
```

This checks shell syntax and verifies the committed `.SRCINFO` matches
`makepkg --printsrcinfo`. It does not fetch sources or assert that a release
exists.

For a new release, after the matching `v<version>` GitHub Release contains
the upstream source asset `mcbtodo-<version>-source.tar.gz`:

1. Set `pkgver` to the released version.
2. Replace `REPLACE_WITH_RELEASE_SHA256` with the SHA-256 from that exact
   attached source asset. Do not use a checksum for GitHub's automatically
   generated tag archive or for a different release file.
3. Regenerate `.SRCINFO` with `makepkg --printsrcinfo > .SRCINFO`.
4. Run `distro/aur/check.sh --static`.
5. On Arch Linux with the `cargo` build dependency available, run
   `distro/aur/check.sh --build`; then inspect the generated package with
   `namcap`.
6. Before submission, check both the official Arch package database and AUR
   for an existing package with this name. If it is available, follow AUR's
   duplicate/adoption rules instead of creating a duplicate.
7. Run `pkgctl license check` on the final AUR recipe before submission.

## v0.1.0 Status

`mcbtodo 0.1.0-1` is published in the AUR. The recipe pins the source
archive SHA-256
`fb15e9fae29a69b8b6852bb2c05926e718ac01195833ee65be52e8e2fc2ef037`.
The archive sidecar, static metadata check, and an AArch64 `makepkg` build
were verified before submission. `namcap` was unavailable and was not run.
