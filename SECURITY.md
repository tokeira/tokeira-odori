# Security Policy

## Reporting a Vulnerability

Please do not report suspected vulnerabilities through public GitHub issues.
Use GitHub's private vulnerability reporting instead: on
[github.com/tokeira/tokeira-odori](https://github.com/tokeira/tokeira-odori),
open **Security → Report a vulnerability**. Include reproduction steps, the
provider and harness versions involved, and the relevant deployment
configuration. Do not include live credentials, tokens, or session files.
Reports are acknowledged and triaged as quickly as we can manage.

## Supported Versions

Security fixes land on `main` and ship with the next release. Please report
against the most recent release.

## Posture

- **Credentials remain outside Odori.** Subscription providers use the vendor
  harness's authenticated credential store; API-provider keys come from the
  process environment. Neither belongs in source, fixtures, prompts, or logs.
- **History is durable.** Workflow inputs, activity results, and tool results
  may be retained in engine history. Do not put a secret in agent input or tool
  output unless that retention is explicitly acceptable.
- **Durable tools are locally fenced.** The preview MCP bridge listens on
  loopback, mints a bearer token per attempt, fences stale attempts, and evicts
  a run's tokens after terminal state is confirmed.
- **Unsafe Rust is denied workspace-wide** (`unsafe_code = "deny"`).
- **Dependency policy is enforced by `cargo-deny`** for licenses, bans, and
  sources on every pull request.
