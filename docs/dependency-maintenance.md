# Dependency Maintenance

Dependabot checks the Cargo workspace and GitHub Actions dependencies weekly.
It opens update pull requests but does not merge them automatically.

For each update:

1. Review the upstream change and any security advisory relevant to the
   dependency.
2. Run `cargo xtask check` and
   `cargo run --locked -- --vault examples/vault --timezone Asia/Shanghai validate`.
3. For package or build-tool updates, run the package and smoke checks from
   `.agents/workflows/release.md` that apply to the changed dependency.
4. Preserve immutable GitHub Action references. Review both the updated commit
   SHA and its version comment.
5. Merge only through the repository's configured review and status-check
   policy. Do not enable automatic merging solely because the checks pass.

Dependabot configuration does not establish branch protection or reviewer
ownership. Those controls must be configured and verified in the GitHub
repository settings once the canonical repository and maintainer policy are
established.
