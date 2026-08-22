# Contributing

Thanks for your interest in Odori. The project moves quickly; for a substantial
change, open an issue before writing code so its scope and durable-execution
semantics can be agreed first.

## Ground Rules

- [AGENTS.md](AGENTS.md) is the repository's engineering contract for human
  and AI contributors. It defines the frozen provider boundary, documentation
  standard, dependency discipline, and concurrent-agent Git protocol.
- Changes to durable behaviour are spec-driven. Read the relevant material in
  [`.kiro/specs`](.kiro/specs) before changing a workflow, activity, bridge, or
  provider contract.
- The provider trait is frozen. If it cannot express a provider requirement,
  stop and raise the gap instead of reshaping it as part of another change.
- Do not commit credentials, harness session files, tokens, or `.env` files.

## Development Setup

Install the Rust toolchain declared by `rust-version`, nightly rustfmt,
`cargo-nextest`, and `cargo-deny`. The examples and provider-specific setup are
documented in the [framework guides](docs/README.md).

## Quality Bar

Run the complete local finish bar before every push or pull request:

```bash
cargo +nightly fmt --all
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked
cargo test --workspace --doc --locked
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
```

Run `cargo deny check bans licenses sources` whenever a change moves a
dependency. Keep `--locked` on build and test commands: dependency movement is
a reviewed change, not a build side effect.

Live subscription-provider tests consume quota and remain ignored by default.
State explicitly in the pull request whether any live test ran; never substitute
a live test for the unguarded scripted test suite.

## Pull Requests

- Branch from current `main` and keep each pull request to one coherent change.
- Explain what changed and why. List the validation commands actually run,
  anything skipped and why, the base and head commits, dependency or lockfile
  movement, and known risks.
- Rebase once onto `origin/main` at the pull-request boundary, rerunning the
  finish bar if the base changed. Do not merge `main` into a task branch.
- A maintainer reviews and merges pull requests serially with merge commits so
  branch ancestry is preserved.
- Credit genuine AI authorship or assistance with the exact `Co-authored-by:`
  or `Assisted-by:` trailers required by [AGENTS.md](AGENTS.md).

By participating, you agree to follow the [Code of Conduct](CODE_OF_CONDUCT.md).
Report security issues through the private channel in [SECURITY.md](SECURITY.md),
not through a public issue.
