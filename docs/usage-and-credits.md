# Usage and credits

Every run's accounting is typed, recorded, and rolled up — and every
figure a backend reports about its own limits and credits is surfaced.
Enforcement is unchanged: [budgets](budgets-and-handoffs.md) remain the
mechanism that ends a run, and this surface widens what an operator can
see, not how the guardrail acts.

## The typed usage surface

Three levels, one discipline — an unknown figure is absent, never zero:

- **Per turn** — `TurnUsage`, recorded on each transcript entry
  (`TurnRecord::usage`): provider-reported dollar cost, input and output
  tokens, cache-served input tokens, cache-written input tokens,
  reasoning output tokens, and wall-clock duration. Failed-attempt spend
  is folded in through heartbeat carryover exactly as before, extended
  to the new figures: a retried attempt's aggregate carries everything
  the failed attempt reported.
- **Per run** — `RunUsage` on `RunOutput`: sums of every reported
  figure, plus the unknown-turn counters. Cache and reasoning figures
  are provider-dependent detail — the matrix below says who reports
  them — so absent values contribute nothing and are not counted as
  unknown turns. Handoff children's usage is absorbed into the parent,
  unchanged.
- **Per session** — `RunUsage::from_turns` rolls any set of recorded
  turns up client-side: one conversation's transcript, or several runs'
  transcripts concatenated. Pure arithmetic over recorded history; no
  new queries, nothing new stored.

## Credits and limits

Providers report account limit and credit state as they observe it, and
the run records it: the seam's `TurnEvent::LimitObserved` carries a
sparse `ProviderLimitStatus`, and the latest observation of a turn is
recorded with that turn (`TurnRecord::limit`). Fields are filled per
backend and left `None` where a backend has no such concept; the credit
balance stays the vendor's decimal string — money is never coerced
through a float. Odori calls no vendor billing APIs: everything here
arrives in-band on sessions the operator already runs.

## Capability matrix

Measured 2026-08-30, per surface: the Claude harness against the pinned
2.1.220 binary's own stream output; Codex against a live app-server
session at codex-cli 0.149.0 (provider pin 0.148.0-alpha.15 — the
fields read are drift-tolerant `Option`s either way); the API tiers
against the vendors' documented response schemas. Gaps are rows, not
omissions.

| Figure | Claude Code harness | Codex app-server | Anthropic Messages API | OpenAI Responses API |
| --- | --- | --- | --- | --- |
| Input / output tokens | ✓ `result.usage` | ✓ `thread/tokenUsage/updated` | ✓ stream usage frames | ✓ `response.completed` usage |
| Cache-served input | ✓ `cache_read_input_tokens` | ✓ `cachedInputTokens` | ✓ `cache_read_input_tokens` | ✓ `input_tokens_details.cached_tokens` |
| Cache-written input | ✓ `cache_creation_input_tokens` | ✓ `cacheWriteInputTokens` | ✓ `cache_creation_input_tokens` | — no such figure |
| Reasoning output | — not reported at the pin (2.1.237+ adds `output_tokens_details.thinking_tokens`; read when present) | ✓ `reasoningOutputTokens` | — folded into output tokens, not distinct | ✓ `output_tokens_details.reasoning_tokens` |
| Provider-reported cost | ✓ `total_cost_usd` (a per-model `modelUsage` map with `costUSD` also exists at the pin; recorded figure is the total) | — subscription surface, no cost | — none; deriving cost needs an operator-supplied price table, deliberately out of scope | — same as Anthropic |
| Limits / credits | ✓ `rate_limit_event`: status, window kind, reset time, overage state | ✓ `account/rateLimits/updated`: used percent, window length, reset time, plan, **credit balance** | ✓ remaining requests/tokens response headers | ✓ remaining requests/tokens response headers |
| Duration | ✓ `duration_ms` | ✓ `turn/completed.durationMs` | ✓ measured wall-clock | ✓ measured wall-clock |

The API tiers' window-reset headers are deliberately not parsed: the two
vendors encode them in different non-numeric formats, and remaining
requests/tokens carry the actionable signal.

## Replay and history

The new usage fields and the per-turn limit observation are additive,
serde-defaulted extensions of shapes already recorded in workflow
history: transcripts and heartbeats written before this surface replay
with the new fields absent, and an unobserved limit serializes to
nothing. Compatibility is pinned by tests
(`new_accounting_fields_default_when_replaying_older_history`,
`pre_extension_usage_wire_shapes_still_deserialize`).
