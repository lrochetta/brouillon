# Contributing to Brouillon

Issues and pull requests are welcome. This is an alpha; open an issue before starting a large change.

## Build and test

Rust stable with `clippy` and `rustfmt`. From `_bmad-output/build/poc-floem`:

```
cargo check --workspace
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test --workspace
cargo build --release
```

If certificate revocation checks fail on your network, set `CARGO_HTTP_CHECK_REVOKE=false` for that shell only.

## Commits

Conventional Commits: `feat`, `fix`, `perf`, `refactor`, `style`, `docs`, `test`, `build`, `ci`, `chore`, `security`, `revert`. Imperative lowercase subject, no period, scope such as `core`, `editor-engine`, `ui`. Performance changes include before/after numbers. See [docs/VERSIONING.md](docs/VERSIONING.md).

## Pull request checklist

- [ ] Commit messages follow Conventional Commits
- [ ] `cargo fmt`, `clippy -D warnings` and `cargo test` pass
- [ ] User-visible change added to the changelog (Unreleased)
- [ ] No unrelated refactor
- [ ] New dependency: its licence is MIT-compatible and stated in the PR

## Clean-room rules

1. Write your own code and text. Do not paste code, text, icons or data files from other products; do not use disassembled, decompiled or leaked material.
2. Do not name other editors in code, UI, docs, commits or issue titles, except for the names of file formats that the importer reads. Describe the behaviour you want.
3. File-format and protocol behaviour comes from public specifications or from files you may legitimately use.
4. By contributing you agree your work is released under the MIT licence.

## Security

Never open a public issue for a vulnerability: see [SECURITY.md](SECURITY.md).
