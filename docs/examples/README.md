# Examples

The examples are executable targets in the workspace-excluded embedded
integration package. They use the real embedded engine; scripted providers keep
the policy and recovery demonstrations deterministic.

| Example | What it proves | Quota |
| --- | --- | --- |
| [hello-durable](hello-durable.md) | One subscription-backed agent, one durable run | Codex quota |
| [slice-fleet](slice-fleet.md) | Typed planning, approvals, scoped durable tools, child workflows, budgets, review, and Raise | None on the scripted path |
| [rewind](rewind.md) | Retry dedupe, default-cache worker replacement, and divergent timelines | None |
| [approval-resume](approval-resume.md) | A human approval boundary persisted to disk and resumed in another process | None |

All commands below run from the repository root.

## Storage mode flag

Each executable accepts `--storage <mode>`:

| Mode | Required environment | Lifecycle fact |
| --- | --- | --- |
| `in-memory` | none | Default; ephemeral unless that scenario configures the snapshot policy |
| `managed-dsql` | `ODORI_DSQL_REGION`, `ODORI_DSQL_DESCRIPTOR_PATH` | Explicitly creates or recovers one descriptor-owned cluster; engine shutdown does not delete it |
| `adopt-existing-endpoint` | `ODORI_DSQL_REGION`, `ODORI_DSQL_CLUSTER_ID`, `ODORI_DSQL_CLUSTER_ARN`, `ODORI_DSQL_ENDPOINT`, `ODORI_DSQL_MIGRATION_POLICY` | Uses the supplied canonical identity without creating or deleting a cluster |

`ODORI_DSQL_MIGRATION_POLICY` is `automatic` or `validate-only`. Invalid or
incomplete DSQL configuration is an error; the examples do not fall back to
in-memory storage. They print the successful startup report and the measured
duration of the complete startup call. See
[Aurora DSQL clusters](../dsql-clusters.md) for creation prerequisites,
descriptor recovery, IAM requirements, and teardown.

For example:

```console
cargo run --manifest-path tests/embedded/Cargo.toml --example rewind -- \
  --storage managed-dsql
```

Managed clusters incur AWS quota and cost until explicitly destroyed. The
live managed regression performs confirmed teardown; the examples retain the
descriptor and cluster for operator-managed teardown.
