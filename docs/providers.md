# Providers

One provider call executes one complete turn. Subscription providers supervise
the vendor harness, stream liveness into activity heartbeats, record a session
ID as soon as it exists, and classify the terminal process result. A retry
resumes the session recovered from heartbeat details. Later conversation turns
fork from the prior recorded session so a failed attempt cannot contaminate the
accepted lineage.

A `Providers` value has one default provider and any number of named
additional providers. `Agent::with_provider` selects by
`Provider::name()`; an unbound agent uses the default.

## Claude Code subscription

`ClaudeProvider` runs one supervised `claude -p --output-format stream-json`
process per turn. It depends on **Claude Code 2.1.220 or newer**; 2.1.220 is
the conformance baseline.

Install it with the
[official Claude Code quickstart](https://code.claude.com/docs/en/quickstart),
then authenticate the harness in the same user environment that will run the
Odori worker:

```console
claude login
claude --version
```

Use `claude setup-token` instead of `claude login` for a supported headless
authentication flow. The binary must be on `PATH`; a nonstandard installation
can be selected with `ClaudeConfig::with_binary`.

At turn start, the provider removes inherited `ANTHROPIC_BASE_URL`,
`ANTHROPIC_AUTH_TOKEN`, and `ANTHROPIC_API_KEY` so subscription
authentication comes from Claude Code's own store. Explicit
`ClaudeConfig::with_env` values are applied afterward.

The provider warns, but does not refuse to run, when `claude --version`
differs from 2.1.220. Unknown stream events remain liveness. Authentication
failure and subscription usage-cap exhaustion are terminal with a login or
reset-window instruction. API and rate-limit failures are retryable. A missing
session is non-retryable. A process death, timeout, or harness death while MCP
calls are pending is retryable.

Provider name: `claude`.

## Codex subscription

`CodexProvider` supervises `codex app-server` over JSON-RPC. It starts,
resumes, or forks a persisted Codex thread and completes after the
`turn/completed` notification. It depends on **Codex CLI
0.148.0-alpha.15 or newer**; 0.148.0-alpha.15 is the conformance baseline.

Install it with the
[official Codex CLI getting-started guide](https://learn.chatgpt.com/docs/codex/cli#getting-started),
then authenticate the harness in the worker's environment:

```console
codex login
codex login status
codex --version
```

`CodexProvider::new()` resolves `codex` through `PATH`;
`CodexProvider::with_command` selects another executable. Version drift logs
a warning because app-server protocol compatibility is not guaranteed. Missing
CLI and authentication errors are terminal and include the commands above.

The provider removes inherited OpenAI API keys, API endpoints, organization and
project IDs, and `CODEX_API_KEY`; subscription authentication comes from the
Codex credential store. Rate limits, usage limits, quota, server overload, and
stream failures are retryable provider API failures. Bad requests, policy or
context-window failures, and protocol drift are configuration failures.
Missing resumed threads are non-retryable. Sandboxing and MCP failures retain
their separate tooling classification.

Codex's MCP tool timeout at this pin is a fixed wall-clock ceiling; progress
notifications do not extend it. The bridge pins a timeout above its keepalive
cadence, but a real tool must still complete within that ceiling.

Provider name: `codex`.

## Anthropic Messages API

Enable the `api-anthropic` feature and set `ANTHROPIC_API_KEY` in the
environment of the worker process:

```toml
[dependencies]
odori = { version = "0.1.0", features = ["api-anthropic"] }
```

`AnthropicProvider` streams Messages API responses and runs its internal
model/tool loop to quiescence within one turn. It retries transient HTTP
failures at most four times, honors `retry-after` up to 30 seconds, reports
token usage, and calls framework tools only through the `preview` MCP bridge.

Multi-turn here is continuity-within-process: the provider stores message
history in memory. A new worker process cannot resume that Anthropic API
session. Use the subscription harness tier when the model session itself must
survive a process boundary.

Provider name: `anthropic-api`.

## OpenAI Responses API

Enable the `api-openai` feature and set `OPENAI_API_KEY` in the worker
process:

```toml
[dependencies]
odori = { version = "0.1.0", features = ["api-openai"] }
```

`OpenAiProvider` streams Responses API events and runs its internal
model/tool loop to quiescence within one turn. It uses the same bounded
transient retry policy and MCP-only framework-tool path as the Anthropic API
provider.

Each completed response ID becomes the next `previous_response_id`. This
continuity is server-side rather than process-local, but only lasts as long as
OpenAI retains the response chain. A missing resumed response is
`SessionNotFound`.

Provider name: `openai-api`.

## API-tier boundaries

The raw API tier is secondary and feature-gated. Its internal loop makes one
framework turn, not a durable replacement for vendor harness session storage.
Whole-loop activity retry reissues model requests and re-spends their tokens.
Framework tool results with a stable recorded identity replay from workflow
history, so those tool effects do not repeat.

API authentication is environment-only. Missing or rejected keys are
non-retryable configuration errors. Transient network errors, HTTP 429/529, and
server errors retry with bounded backoff and then surface as retryable
`TurnError::Api`.

## Reasoning effort

`Agent::with_effort(Effort)` sets a provider-neutral deliberation level:
`none`, `minimal`, `low`, `medium`, `high`, `xhigh`, or `max` — the
union of the vendor surfaces Odori drives. Unset means the backend's own
default; `none` is the explicit request for no deliberation, where the
backend can be told so.
Every (provider, level) pair either maps onto that backend's lever or
fails **before spawn** as a non-retryable `TurnError::Config` naming the
gap; a level is never silently ignored, clamped, or coerced. Like model
and guardrail selection, effort is worker-side agent configuration:
change it under in-flight runs and the same determinism caveat as any
registry change applies.

| Effort | Claude Code (pinned 2.1.220) | Codex app-server | Anthropic Messages API | OpenAI Responses API |
| --- | --- | --- | --- | --- |
| `none` | **config error** — the pin has no none tier | `model_reasoning_effort = "none"` | thinking explicitly disabled | `reasoning.effort = "none"` |
| `minimal` | **config error** — the pin has no minimal tier | `"minimal"` | thinking budget 1024 (the API's minimum) | `"minimal"` |
| `low` | `--effort low` | `"low"` | thinking budget 2048 | `"low"` |
| `medium` | `--effort medium` | `"medium"` | thinking budget 4096 | `"medium"` |
| `high` | `--effort high` | `"high"` | thinking budget 8192 | `"high"` |
| `xhigh` | `--effort xhigh` | `"xhigh"` | thinking budget 16384 | `"xhigh"` |
| `max` | `--effort max` | **config error** — Codex tops out at `xhigh` | thinking budget 32768 | **config error** — no such level |

Sources, checked 2026-08-30: the Claude column was verified against the
pinned 2.1.220 binary's own usage text (`--effort <level>` accepting
`low, medium, high, xhigh, max`); the Codex column against the Codex
configuration reference's `model_reasoning_effort` set; the API columns
against the vendors' request schemas. Within a mapped set, **model**
support still varies — Codex models and OpenAI's Responses API each
accept subsets of their ladder (`xhigh` is limited to the codex-max
line), and such rejections come back from the vendor as non-retryable
configuration errors rather than being second-guessed in Odori.

The Anthropic budgets are Odori's documented mapping onto the Messages
API's numeric `thinking.budget_tokens` (the API has no levels). A budget
must stay strictly below `max_tokens`; a level that does not fit is a
configuration error naming `AnthropicConfig::with_max_tokens` as the
remedy. Operators who want an exact number instead of the ladder's
points set `AnthropicConfig::with_thinking_budget(n)` — the
provider-level default for turns whose agent sets no effort, validated
on use against the API's 1024-token floor and the `max_tokens` ceiling.
An agent-level effort always overrides it, `Effort::None` included.
With thinking enabled, streamed thinking blocks are assembled verbatim —
text and signature — so the internal tool loop's assistant echo
satisfies the API's thinking contract.

## Error surfaces

Providers map failures to one public taxonomy:

| Class | Retry | Meaning |
| --- | --- | --- |
| `Api` | yes | Vendor API, rate limit, quota, overload, or exhausted transient HTTP retries |
| `SessionNotFound` | no | The requested harness session or API response chain no longer exists |
| `HarnessDied` | yes | The CLI ended without a terminal result |
| `HarnessDiedAwaitingTools` | yes | The CLI died with durable MCP calls still pending |
| `Config` | no | Missing binary, authentication, invalid flags, protocol drift, or rejected request |
| `Timeout` | yes | The provider-side turn deadline elapsed |
| `Tooling` | yes | The MCP attachment or tool transport failed mid-turn |

The turn activity applies its configured retry policy to retryable classes.
None of these classes turns a quota error or dead harness into an unbounded
wait.
