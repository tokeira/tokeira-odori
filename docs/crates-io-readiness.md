# crates.io readiness

This audit covers every Cargo package in the repository. Five crates form the
public framework closure; the integration harness, protocol spike, and bundled
example fixtures are explicitly non-publishable.

## Publish closure

Publish in dependency order:

1. `odori-agents`
2. `odori-mcp-bridge`
3. `odori-providers` and `odori-engine` (either order after the bridge)
4. `odori`

Exact crates.io registry lookups from outside the workspace found no existing
package under any of these five names on 2026-08-22. This is a point-in-time
availability check, not a reservation; publication claims each name.

Each public crate declares its own description, keywords, categories,
crate-specific README, and docs.rs URL. All inherit the Apache-2.0 licence,
repository (`https://github.com/tokeira/tokeira-odori`), homepage
(`https://tokeira.io`), and Rust 1.97 minimum from the workspace.

| Crate | Keywords | Categories | README |
| --- | --- | --- | --- |
| `odori-agents` | `ai-agents`, `agent-framework`, `durable-execution`, `workflow`, `temporal` | `asynchronous`, `development-tools` | `crates/odori-agents/README.md` |
| `odori-mcp-bridge` | `mcp`, `durable-execution`, `ai-agents`, `tools`, `workflow` | `asynchronous`, `development-tools` | `crates/odori-mcp-bridge/README.md` |
| `odori-providers` | `ai-agents`, `codex`, `claude-code`, `agent-framework`, `llm` | `api-bindings`, `development-tools` | `crates/odori-providers/README.md` |
| `odori-engine` | `durable-execution`, `workflow`, `temporal`, `ai-agents`, `embedded` | `asynchronous`, `development-tools` | `crates/odori-engine/README.md` |
| `odori` | `ai-agents`, `durable-execution`, `agent-framework`, `workflow`, `rust` | `asynchronous`, `development-tools` | `crates/odori/README.md` |

The release-preparation pass removed one dev-dependency publication cycle. The
quota-gated Codex bridge test now lives with the provider integration tests, so
the bridge no longer needs `odori-providers` to package. Test behaviour and its
ignored-by-default quota gate are unchanged.

## Package contents and sizes

The following results came from a per-crate `cargo package --list` pass and
`cargo package --workspace --offline --allow-dirty --locked --no-verify` on
2026-08-22.

| Crate | Files | Source payload | `.crate` archive |
| --- | ---: | ---: | ---: |
| `odori-agents` | 16 | 208.1 KiB | 55,404 bytes (54.1 KiB) |
| `odori-mcp-bridge` | 11 | 123.7 KiB | 34,126 bytes (33.3 KiB) |
| `odori-providers` | 18 | 247.0 KiB | 61,020 bytes (59.6 KiB) |
| `odori-engine` | 6 | 96.8 KiB | 26,597 bytes (26.0 KiB) |
| `odori` | 8 | 96.5 KiB | 26,722 bytes (26.1 KiB) |

No package includes `examples/`, the example fixture projects, `spikes/`, the
embedded integration harness, repository-wide docs, or the bird artwork. Crate
tests are included as expected. The largest archive is `odori-providers`; its
contents are Rust source for the providers, scripted fake harness, and provider
tests rather than binary or captured-response fixture bloat.

## Non-publishable packages

| Package | Location | Reason |
| --- | --- | --- |
| `codex-driver-spike` | `spikes/codex-driver` | Protocol research with an independent dependency graph |
| `odori-embedded-harness` | `tests/embedded` | Integration harness with the sibling engine as a Git dependency |
| `approval-resume-fixture` | `examples/approval-resume/fixture` | Bundled project copied to a temporary directory by the example |
| `fixture-project` | `examples/slice-fleet/fixture` | Bundled project copied to a temporary directory by the example |

All four declare `publish = false`, a description, Apache-2.0 licence,
repository, and Rust 1.97 minimum. Keywords, categories, and a crates.io README
are intentionally not assigned because these packages are outside the publish
closure.
