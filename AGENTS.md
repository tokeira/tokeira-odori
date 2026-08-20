# AGENTS.md — Odori

Odori is a minimal Rust agent framework with durable execution built in: the
OpenAI Agents SDK primitive set (`Agent`, `Runner`, `Tool`, `Handoff`,
`Guardrail`, typed outputs), subscription-first providers that drive vendor
harnesses (headless Claude Code, Codex app-server) as supervised subprocesses,
and an embedded tokeira engine underneath — the run loop is a workflow, one
harness turn is one activity, sessions are history, handoffs are child
workflows. The engine repo (`../tokeira`) is the sibling; mirror its standards,
never run git or cargo mutations there.

Several agents and one human work this repository simultaneously. This file is
the contract.

## The lifecycle

Every task follows one lifecycle:

> **worktree + branch → work → finish green → rebase once → push + PR →
> human approval, serial merge → cleanup**

- **One agent, one worktree, one branch, one task.** Work only inside your own
  worktree; the main checkout is the human's integration seat. Base worktrees
  on fresh `origin/main` (Claude: `worktree.baseRef: "fresh"` is configured).
- Branch naming: **`agent/<provider>/<task-slug>`** — `agent/claude/…`,
  `agent/codex/…`. Rename harness-default branch names to the convention
  before the first push. Never finish agent work on `main`.
- Stay within the crate(s) the task names. No drive-by edits to other crates,
  shared configs, or the workspace `Cargo.toml` unless the task says so.
- At the PR boundary, rebase **once**: `git fetch --prune && git rebase
  origin/main`; if anything changed, re-run the bar. Conflicts outside your
  task's scope: stop before pushing and report a recommendation instead. Never
  merge `main` into a task branch; after a PR exists, updates are additional
  commits.
- The human integration seat processes PRs **one at a time** and approves the
  exact head. Agents never approve their own work; on explicit approval the
  owning agent merges with `gh pr merge --merge --match-head-commit <sha>`.
  PR body: what/why; validation commands actually run (name anything skipped,
  and why); base and head SHAs; dependency/lockfile notes; known risks.

## Finish green — the bar

Run before any push or PR:

```bash
cargo +nightly fmt --all                                  # CI-style check is --check
cargo clippy --workspace --all-targets --locked -- -D warnings
cargo nextest run --workspace --locked
cargo test --workspace --doc --locked                     # nextest does not run doctests
RUSTDOCFLAGS="-D warnings" cargo doc --workspace --no-deps --locked
```

`cargo deny check bans licenses sources` guards dependency movement — run it
whenever a task touches dependencies.

## Dependencies are single-writer

No add/remove/upgrade unless the task explicitly calls for it — assume another
agent holds the lockfile this window. Otherwise build `--locked` so
`Cargo.lock` can never be rewritten by accident. Dependency movement is a
reviewed change, never a build side effect.

## No secrets

Never read or commit secrets (`.env*`, keys, tokens, subscription session
files) — fleet-wide, not just where harness deny rules enforce it. Providers
in this repo drive authenticated harnesses; their credentials live in the
operator's environment, never in the tree, tests, or fixtures.

## Commit attribution (required)

The human operator stays the git `author`; agents are credited with trailers,
one line per agent, after a blank line at the end of the message:

- **`Co-authored-by: <Agent> <email>`** — every agent that authored part of
  the change.
- **`Assisted-by: <Agent> <email>`** — every agent that assisted without
  primary authorship (review, verification, pairing).

Canonical identities (use exactly these):

- `Claude <noreply@anthropic.com>`
- `Codex <codex@openai.com>`

A commit with genuine agent involvement and no attribution trailer is an
incomplete change, the same as a missing test or a failing lint.
