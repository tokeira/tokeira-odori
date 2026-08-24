# Aurora DSQL clusters

Odori can either create one single-Region Aurora DSQL cluster or adopt a
cluster whose canonical identity the operator already knows. These are
different authorities:

- `managed-dsql` is explicit permission for the embedded engine to create a
  cluster when its descriptor does not exist, or recover exactly the cluster
  recorded in an existing descriptor.
- `adopt-existing-endpoint` validates and uses the supplied Region, cluster ID,
  cluster ARN, and endpoint. It never creates or deletes a cluster.

Neither mode falls back to in-memory storage. A configuration, AWS, network,
schema, or ownership error is returned to the Odori caller.

Aurora DSQL consumes AWS quota and can incur cost. Stop the engine before
administrative deletion, and account for every cluster created while testing.

## Prerequisites

Use an AWS Region in which Aurora DSQL is available and make sure the process
can reach the cluster endpoint on TCP port 5432. Odori uses the standard AWS
SDK credential chain; it does not read an Odori-specific credential file or
accept a database password. This is a useful credential preflight:

```console
aws sts get-caller-identity
```

The principal running a managed example needs these DSQL actions:

- `dsql:CreateCluster` for the first start;
- `dsql:GetCluster` for readiness and later recovery;
- `dsql:TagResource`, because the examples attach the
  `tokeira:owner=odori-example` tag during creation; and
- `dsql:DbConnectAdmin` for IAM-authenticated database connections and schema
  initialization or migration.

Adopt-existing mode needs `dsql:GetCluster` and `dsql:DbConnectAdmin`, but not
creation permission. Administrative deletion additionally needs
`dsql:UpdateCluster` and `dsql:DeleteCluster`. Scope those permissions to the
account, Region, and cluster resources appropriate for the operator; Odori
does not install IAM policy.

The examples use admin database authentication because embedded startup owns
the release-pinned schema check. They generate short-lived IAM authentication
tokens through the DSQL connector. Those database tokens are distinct from the
idempotent creation token recorded in a managed descriptor; no database token
belongs in an environment variable or descriptor.

## Let Odori create the cluster

Choose a stable, host-owned path for the lifecycle descriptor. Use an absolute
path outside both the repository and temporary directories. The descriptor
file must not exist on the first run; do not create an empty file. Its parent
may be absent, but the process must be able to create and durably write that
directory.

```console
export ODORI_DSQL_REGION=eu-west-2
export ODORI_DSQL_DESCRIPTOR_PATH=/absolute/operator-owned/odori/rewind-cluster.json

cargo run --manifest-path tests/embedded/Cargo.toml --example rewind -- \
  --storage managed-dsql
```

On the first start, the embedded engine:

1. writes a pending descriptor and an idempotent AWS creation token before
   making the create request;
2. creates one single-Region cluster with deletion protection enabled and the
   example tag;
3. records the returned canonical cluster ID, ARN, and endpoint in the
   descriptor;
4. waits for a usable cluster, connects with IAM authentication, and applies
   the automatic schema policy; and
5. opens admission only after cluster, schema, and embedded ownership checks
   succeed.

The example prints `Engine::startup_report()` and the measured startup time.
The cluster report says whether that start created or recovered the cluster and
includes the Region, canonical ID and ARN, and current endpoint.

A later start with the same Region and descriptor recovers that exact cluster.
Recovery never searches by tag or endpoint. A crash between the create request
and the ready descriptor replays the same client token instead of authorizing
a second cluster.

The descriptor and its `.lock` sidecar are lifecycle state, not disposable
cache. Retain them across engine and host restarts, do not commit them, and do
not point two unrelated deployments at the same path. A Region mismatch,
corrupt descriptor, missing AWS cluster, or destroyed tombstone is an error;
Odori will not silently create a replacement.

Engine shutdown releases embedded ownership and drains connections. It does
not disable deletion protection or delete the cluster.

## Create a cluster for adopt-existing mode

Use this path when the operator, rather than Odori, owns cluster creation. The
AWS CLI is not required for managed mode; it is shown here as one way to create
and inspect an operator-owned cluster.

```console
aws dsql create-cluster \
  --region eu-west-2 \
  --deletion-protection-enabled
```

Record the `identifier`, `arn`, and `endpoint` from the response, then wait for
the cluster to become active:

```console
aws dsql wait cluster-active \
  --region eu-west-2 \
  --identifier <cluster-id>
```

Supply the complete canonical identity to the example. For a new, empty
cluster, select `automatic` so the embedded engine can initialize its schema.
`validate-only` checks compatibility without changing the schema and therefore
does not initialize an empty cluster.

```console
export ODORI_DSQL_REGION=eu-west-2
export ODORI_DSQL_CLUSTER_ID=<cluster-id>
export ODORI_DSQL_CLUSTER_ARN=<cluster-arn>
export ODORI_DSQL_ENDPOINT=<cluster-endpoint>
export ODORI_DSQL_MIGRATION_POLICY=automatic

cargo run --manifest-path tests/embedded/Cargo.toml --example rewind -- \
  --storage adopt-existing-endpoint
```

Odori verifies the ID and ARN against `GetCluster`; the endpoint alone is not
resource identity. Both embedded DSQL modes reject a multi-Region cluster.

## Delete a cluster

There is no example `--delete` flag. The ordinary engine deliberately lacks
cluster-destruction authority. After stopping every engine that uses the
cluster, obtain its canonical ID and Region from the successful startup report
or the managed descriptor, then inspect the target:

```console
aws dsql get-cluster \
  --region <region> \
  --identifier <cluster-id>
```

Check the returned ARN as well as the ID. Managed clusters have deletion
protection enabled, so explicit CLI teardown is three steps:

```console
aws dsql update-cluster \
  --region <region> \
  --identifier <cluster-id> \
  --no-deletion-protection-enabled

aws dsql wait cluster-active \
  --region <region> \
  --identifier <cluster-id>

aws dsql delete-cluster \
  --region <region> \
  --identifier <cluster-id>

aws dsql wait cluster-not-exists \
  --region <region> \
  --identifier <cluster-id>
```

The raw AWS CLI path cannot update Odori's managed descriptor to its destroyed
tombstone. After confirmed deletion, archive that descriptor and do not reuse
its path. A future managed cluster must use a new descriptor path. Never remove
or replace a ready descriptor while its cluster still exists: the next managed
start would interpret an absent descriptor as fresh authority to create.

The live managed regression uses the engine's plan-bound administrative API
instead. It confirms the exact descriptor revision and AWS identity, disables
deletion protection, waits for deletion, and writes the destroyed tombstone.
That test path is gated by `ODORI_LIVE_MANAGED_DSQL_ACK=CREATE_AND_DELETE` and
is not an implicit part of example shutdown.

## AWS references

- [CreateCluster CLI reference](https://docs.aws.amazon.com/cli/latest/reference/dsql/create-cluster.html)
- [GetCluster CLI reference](https://docs.aws.amazon.com/cli/latest/reference/dsql/get-cluster.html)
- [UpdateCluster CLI reference](https://docs.aws.amazon.com/cli/latest/reference/dsql/update-cluster.html)
- [DeleteCluster CLI reference](https://docs.aws.amazon.com/cli/latest/reference/dsql/delete-cluster.html)
- [DSQL waiters](https://docs.aws.amazon.com/cli/latest/reference/dsql/wait/index.html)
