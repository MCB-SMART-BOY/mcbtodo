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

After a GitHub Release exists for the matching `v<version>` tag and contains
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

The current repository has no `v0.1.0` release tag or source release asset.
Therefore the checksum is intentionally unset, the package cannot yet pass
source verification/build, and this recipe is not ready for AUR submission.
Do not submit `.SRCINFO` with the placeholder checksum.

Publishing is intentionally outside this helper. It does not log in to or
modify AUR, create Git tags/releases, or push Git changes.
