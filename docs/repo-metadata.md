# Repository metadata and launch settings

This page is the operator's click-through sheet for GitHub repository settings.
It proposes metadata only; repository settings remain an operator action.

## About panel

- **Description:** A minimal Rust agent framework where every run is durably
  executed.
- **Website:** <https://tokeira.io>
- **Topics:** `rust`, `durable-execution`, `agents`, `ai-agents`, `temporal`,
  `agent-framework`, `durable-workflows`, `mcp`, `claude-code`, `codex`

## Social preview

Prepare a 1280 × 640 PNG, under 1 MB, using the Odori bird from
[`assets/odori.png`](../assets/odori.png). Use the bird at left on its existing
blue field, with **Odori** and “A minimal Rust agent framework where every run
is durably executed.” at right. Keep all type and the bird's silhouette at
least 64 px from the image edge so GitHub's crops retain them. Check legibility
at roughly 320 × 160 before uploading it under **Settings → General → Social
preview**.

## Five-minute settings checklist

### General

- [ ] Under **Settings → General → Default branch**, confirm `main`.
- [ ] Under **Pull Requests**, enable **Allow merge commits**.
- [ ] Disable **Allow squash merging** and **Allow rebase merging** so merged
  branch ancestry is preserved.
- [ ] Enable **Automatically delete head branches**.
- [ ] Keep **Discussions** off at launch; the question issue template is the
  initial support surface.
- [ ] Apply the About-panel description, website, and topics above, then upload
  the social preview.

### Main-branch protection

- [ ] Create a branch ruleset targeting `main` and require a pull request before
  merging with at least one approval.
- [ ] Require conversation resolution and dismiss stale approvals after new
  commits.
- [ ] Require the `Format`, `Supply-chain and licenses`, and `Offline links`
  status checks from the **Stopgap gates** workflow.
- [ ] Require the branch to be up to date before merging; apply the ruleset to
  administrators as well as contributors.
- [ ] Block force pushes and branch deletion. Do **not** require linear history:
  the project deliberately merges with merge commits.

### Security and release

- [ ] Enable **Private vulnerability reporting** so the channel named in
  `SECURITY.md` and `CODE_OF_CONDUCT.md` is available when the repository opens.
- [ ] At launch, create the annotated SemVer tag `v0.1.0` from the exact release
  commit and use `vMAJOR.MINOR.PATCH` for subsequent releases.
