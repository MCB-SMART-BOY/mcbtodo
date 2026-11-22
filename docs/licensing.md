# License and Third-Party Material

The maintainer identifies this project as original software they authored.
No ownership certificate, rights attestation, or private ownership evidence is
required for the project.

The project is distributed under the Apache License, Version 2.0, as stated in
the repository's [`LICENSE`](../LICENSE) and Cargo package metadata. Keep the
license text at the repository root so Cargo packages and source distributions
can include it. The Linux binary archive includes the same text at
`usr/share/licenses/mcbtodo/LICENSE`.

## Third-Party Material

For code, documentation, fixtures, images, generated assets, and dependencies
that come from outside this project:

- Keep the source and applicable license identifiable.
- Preserve required copyright, attribution, and notice text.
- Check that redistribution terms are compatible with the intended package
  and release.
- Do not include material with unclear or incompatible terms until resolved.

These checks apply to third-party material; they are not a request for the
maintainer to prove ownership of their original work. Release checks are
described in the [release preparation workflow](../.agents/workflows/release.md).

The development-only `xtask` uses `vt100` (MIT) from the `vt100-rust`
repository for its PTY smoke tests. Cargo records `vte` (Apache-2.0 OR MIT)
and `arrayvec` (MIT OR Apache-2.0) as transitive dependencies. The distributed
application binaries do not use this parser.
