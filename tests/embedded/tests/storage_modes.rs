//! Storage-mode contract tests for Odori's E1 engine wrapper.
//!
//! The in-memory legs are ordinary unguarded tests. The two DSQL legs are
//! ignored and separately environment-authorized because they use live AWS.

use std::{
    collections::BTreeMap,
    net::TcpListener,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicU64, AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result, ensure};
use async_trait::async_trait;
use odori_agents::{
    Agent, AgentRegistry, Providers,
    provider::{Provider, TurnError, TurnEvent, TurnEventSink, TurnOutcome, TurnRequest},
};
use odori_engine::{
    ConnectTarget, DsqlMigrationPolicy, EmbeddedDsqlLimits, EmbeddedEngineConfig,
    EmbeddedEngineStartError, EmbeddedStorageConfig, EmbeddedStorageMode, Engine,
    ExistingEmbeddedDsqlConfig, ManagedClusterIntent, ManagedEmbeddedDsqlConfig, OdoriRuntime,
    SnapshotPolicyConfig, TokeiraConfig,
};
use tokeira_managed_dsql::{
    AdminDeadline, AwsDsqlControlPlane, ClusterDescriptorState, ClusterDescriptorStore,
    DestroyOutcome, LocalClusterDescriptorStore, ManagedDsqlAdmin,
};

const LIVE_TIMEOUT: Duration = Duration::from_secs(30 * 60);
static TEMP_SEQUENCE: AtomicU64 = AtomicU64::new(0);

#[derive(Debug)]
struct TemporaryDirectory(PathBuf);

impl TemporaryDirectory {
    fn new(label: &str) -> Result<Self> {
        let sequence = TEMP_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let path =
            std::env::temp_dir().join(format!("odori-{label}-{}-{sequence}", std::process::id()));
        std::fs::create_dir(&path)
            .with_context(|| format!("create temporary directory {}", path.display()))?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TemporaryDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Debug, Default)]
struct RestartProvider {
    calls: AtomicUsize,
}

impl RestartProvider {
    fn calls(&self) -> usize {
        self.calls.load(Ordering::SeqCst)
    }
}

#[async_trait]
impl Provider for RestartProvider {
    fn name(&self) -> &str {
        "restart-scripted"
    }

    async fn execute_turn(
        &self,
        request: TurnRequest,
        events: TurnEventSink,
    ) -> Result<TurnOutcome, TurnError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        let session_id = format!("restart-session-{}", request.identity.turn);
        events.emit(TurnEvent::SessionStarted {
            session_id: session_id.clone(),
        });
        Ok(TurnOutcome::new(session_id, "durable-before-restart"))
    }
}

fn registry() -> AgentRegistry {
    let mut registry = AgentRegistry::new();
    registry.register(
        Agent::new("restart-agent", "Record one deterministic turn.")
            .with_provider("restart-scripted"),
    );
    registry
}

fn config_with_unique_ports(storage: EmbeddedStorageConfig) -> Result<EmbeddedEngineConfig> {
    let grpc_guard = TcpListener::bind("127.0.0.1:0")?;
    let nexus_guard = TcpListener::bind("127.0.0.1:0")?;
    let mut server = TokeiraConfig::default();
    server.infrastructure.network.grpc_addr = grpc_guard.local_addr()?.to_string();
    server.policy.nexus_completion.http_addr = nexus_guard.local_addr()?.to_string();
    Ok(EmbeddedEngineConfig {
        server,
        storage,
        ..EmbeddedEngineConfig::default()
    })
}

async fn start_runtime(
    engine: &Engine,
    task_queue: &str,
    provider: Arc<RestartProvider>,
) -> Result<OdoriRuntime> {
    OdoriRuntime::builder(task_queue)
        .connect(ConnectTarget::service_override(engine.service_override()))
        .agents(registry())
        .providers(Providers::new(provider))
        .start()
        .await
}

fn unique_id(prefix: &str) -> String {
    let epoch = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis();
    format!("{prefix}-{}-{epoch}", std::process::id())
}

async fn wait_for_turn(runtime: &OdoriRuntime, run_id: &str) -> Result<()> {
    let conversation = runtime.runner().resume_conversation(run_id);
    tokio::time::timeout(Duration::from_secs(30), async {
        loop {
            if conversation.transcript().await?.len() == 1 {
                return Ok::<_, anyhow::Error>(());
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .context("the durable turn was not queryable before restart")??;
    Ok(())
}

async fn exercise_durable_restart(
    storage: EmbeddedStorageConfig,
    expected_mode: EmbeddedStorageMode,
    label: &str,
) -> Result<()> {
    let task_queue = unique_id(&format!("odori-{label}-queue"));
    let run_id = unique_id(&format!("odori-{label}-run"));
    let first_engine =
        Engine::start_with_embedded_config(config_with_unique_ports(storage.clone())?).await?;
    ensure!(first_engine.startup_report().storage_mode == expected_mode);
    ensure!(first_engine.startup_report().cluster.is_some());
    ensure!(first_engine.startup_report().schema.is_some());
    let first_report = first_engine.startup_report().clone();
    println!(
        "{label} first startup: {:?}; elapsed={:?}",
        first_engine.startup_report(),
        first_engine.startup_elapsed()
    );
    let first_provider = Arc::new(RestartProvider::default());
    let first_runtime = start_runtime(&first_engine, &task_queue, first_provider.clone()).await?;
    let conversation = first_runtime
        .runner()
        .start_conversation("restart-agent", "record the durable marker", &run_id)
        .await?;
    wait_for_turn(&first_runtime, &run_id).await?;
    ensure!(first_provider.calls() == 1);
    drop(conversation);
    first_runtime.shutdown().await?;
    first_engine.shutdown().await?;

    let restarted = Engine::start_with_embedded_config(config_with_unique_ports(storage)?).await?;
    ensure!(restarted.startup_report().storage_mode == expected_mode);
    let restart_report = restarted.startup_report();
    let first_cluster = first_report
        .cluster
        .as_ref()
        .context("first DSQL startup must report its cluster")?;
    let restart_cluster = restart_report
        .cluster
        .as_ref()
        .context("restarted DSQL engine must report its cluster")?;
    ensure!(restart_cluster.cluster_id == first_cluster.cluster_id);
    ensure!(restart_cluster.cluster_arn == first_cluster.cluster_arn);
    let first_fence = first_report
        .ownership
        .context("first DSQL startup must report ownership")?
        .fence_token;
    let restart_fence = restart_report
        .ownership
        .context("restarted DSQL engine must report ownership")?
        .fence_token;
    ensure!(restart_fence > first_fence);
    println!(
        "{label} restart: {:?}; elapsed={:?}",
        restarted.startup_report(),
        restarted.startup_elapsed()
    );
    let replacement_provider = Arc::new(RestartProvider::default());
    let replacement = start_runtime(&restarted, &task_queue, replacement_provider.clone()).await?;
    let restored = replacement.runner().resume_conversation(&run_id);
    let transcript = restored.transcript().await?;
    ensure!(transcript.len() == 1);
    ensure!(transcript[0].text == "durable-before-restart");
    ensure!(replacement_provider.calls() == 0);
    let output = restored.end().await?;
    ensure!(output.text == "durable-before-restart");
    ensure!(replacement_provider.calls() == 0);
    replacement.shutdown().await?;
    restarted.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn in_memory_report_and_invalid_dsql_never_fallback() -> Result<()> {
    let engine = Engine::start_with_embedded_config(EmbeddedEngineConfig::default()).await?;
    assert_eq!(
        engine.startup_report().storage_mode,
        EmbeddedStorageMode::InMemory
    );
    assert!(engine.startup_report().cluster.is_none());
    assert!(engine.startup_report().schema.is_none());
    engine.shutdown().await?;

    let error = Engine::start_with_embedded_config(EmbeddedEngineConfig {
        storage: EmbeddedStorageConfig::ExistingDsql(ExistingEmbeddedDsqlConfig {
            region: String::new(),
            cluster_id: String::new(),
            cluster_arn: String::new(),
            endpoint: String::new(),
            migration_policy: DsqlMigrationPolicy::ValidateOnly,
            limits: EmbeddedDsqlLimits::default(),
        }),
        ..EmbeddedEngineConfig::default()
    })
    .await
    .expect_err("invalid existing-DSQL intent must not start an in-memory engine");
    assert!(matches!(
        error,
        EmbeddedEngineStartError::InvalidConfiguration(_)
    ));
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn in_memory_snapshot_preserves_a_live_run_across_engine_restart() -> Result<()> {
    let state = TemporaryDirectory::new("in-memory-snapshot")?;
    let snapshot = state.path().join("engine.snapshot");
    let storage = EmbeddedStorageConfig::InMemory;
    let mut first_config = config_with_unique_ports(storage.clone())?;
    first_config.server.policy.snapshot = Some(SnapshotPolicyConfig {
        location: snapshot.clone(),
        interval_ms: 3_600_000,
    });
    let first_engine = Engine::start_with_embedded_config(first_config).await?;
    let task_queue = unique_id("odori-snapshot-queue");
    let run_id = unique_id("odori-snapshot-run");
    let first_provider = Arc::new(RestartProvider::default());
    let first_runtime = start_runtime(&first_engine, &task_queue, first_provider.clone()).await?;
    let conversation = first_runtime
        .runner()
        .start_conversation("restart-agent", "record the snapshot marker", &run_id)
        .await?;
    wait_for_turn(&first_runtime, &run_id).await?;
    let joined = first_runtime
        .runner()
        .start_conversation("restart-agent", "record the snapshot marker", &run_id)
        .await?;
    ensure!(joined.transcript().await?.len() == 1);
    ensure!(first_provider.calls() == 1);
    drop(joined);
    drop(conversation);
    first_runtime.shutdown().await?;
    first_engine.shutdown().await?;
    ensure!(snapshot.is_file());

    let mut restart_config = config_with_unique_ports(storage)?;
    restart_config.server.policy.snapshot = Some(SnapshotPolicyConfig {
        location: snapshot,
        interval_ms: 3_600_000,
    });
    let restarted = Engine::start_with_embedded_config(restart_config).await?;
    let replacement_provider = Arc::new(RestartProvider::default());
    let replacement = start_runtime(&restarted, &task_queue, replacement_provider.clone()).await?;
    let restored = replacement.runner().resume_conversation(&run_id);
    let transcript = restored.transcript().await?;
    ensure!(transcript.len() == 1);
    ensure!(transcript[0].text == "durable-before-restart");
    ensure!(replacement_provider.calls() == 0);
    restored.end().await?;
    replacement.shutdown().await?;
    restarted.shutdown().await?;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "creates and deletes one live Aurora DSQL cluster; set ODORI_LIVE_MANAGED_DSQL_ACK=CREATE_AND_DELETE"]
async fn managed_dsql_preserves_a_live_run_across_engine_restart() -> Result<()> {
    if std::env::var("ODORI_LIVE_MANAGED_DSQL_ACK").as_deref() != Ok("CREATE_AND_DELETE") {
        println!("skipped: set ODORI_LIVE_MANAGED_DSQL_ACK=CREATE_AND_DELETE");
        return Ok(());
    }
    let region =
        std::env::var("ODORI_LIVE_DSQL_REGION").context("ODORI_LIVE_DSQL_REGION must be set")?;
    let descriptor_path = PathBuf::from(
        std::env::var_os("ODORI_LIVE_DSQL_DESCRIPTOR_PATH")
            .context("ODORI_LIVE_DSQL_DESCRIPTOR_PATH must be set")?,
    );
    let storage = EmbeddedStorageConfig::ManagedDsql(ManagedEmbeddedDsqlConfig {
        intent: ManagedClusterIntent::CreateOrRecover,
        descriptor_path: descriptor_path.clone(),
        region: region.clone(),
        migration_policy: None,
        limits: EmbeddedDsqlLimits::default(),
        tags: BTreeMap::from([("tokeira:test".to_owned(), "odori-e2".to_owned())]),
    });
    let exercise =
        exercise_durable_restart(storage, EmbeddedStorageMode::ManagedDsql, "managed-dsql").await;

    let store = LocalClusterDescriptorStore::new(descriptor_path);
    let control = AwsDsqlControlPlane::from_region(region).await;
    let admin = ManagedDsqlAdmin::new(control, store.clone());
    let plan = admin
        .plan_destroy(AdminDeadline::after(LIVE_TIMEOUT))
        .await?;
    let confirmation = plan.confirm();
    let destroyed = admin
        .apply_destroy(&plan, confirmation, AdminDeadline::after(LIVE_TIMEOUT))
        .await?;
    ensure!(destroyed.outcome == DestroyOutcome::Destroyed);
    let descriptor = store
        .load()
        .await?
        .context("destroy must leave a descriptor tombstone")?
        .into_v1();
    ensure!(matches!(
        descriptor.state,
        ClusterDescriptorState::Destroyed { .. }
    ));
    println!("managed-dsql teardown: one cluster created/recovered, then explicitly destroyed");
    exercise
}

#[tokio::test(flavor = "multi_thread")]
#[ignore = "uses an operator-owned live Aurora DSQL endpoint; set ODORI_LIVE_EXISTING_DSQL_ACK=USE_EXISTING"]
async fn adopted_endpoint_preserves_a_live_run_across_engine_restart() -> Result<()> {
    if std::env::var("ODORI_LIVE_EXISTING_DSQL_ACK").as_deref() != Ok("USE_EXISTING") {
        println!("skipped: set ODORI_LIVE_EXISTING_DSQL_ACK=USE_EXISTING");
        return Ok(());
    }
    let migration_policy = match std::env::var("ODORI_LIVE_DSQL_MIGRATION_POLICY")
        .context("ODORI_LIVE_DSQL_MIGRATION_POLICY must be set")?
        .as_str()
    {
        "automatic" => DsqlMigrationPolicy::Automatic,
        "validate-only" => DsqlMigrationPolicy::ValidateOnly,
        other => anyhow::bail!(
            "ODORI_LIVE_DSQL_MIGRATION_POLICY must be automatic or validate-only, not {other:?}"
        ),
    };
    let storage = EmbeddedStorageConfig::ExistingDsql(ExistingEmbeddedDsqlConfig {
        region: std::env::var("ODORI_LIVE_DSQL_REGION")
            .context("ODORI_LIVE_DSQL_REGION must be set")?,
        cluster_id: std::env::var("ODORI_LIVE_DSQL_CLUSTER_ID")
            .context("ODORI_LIVE_DSQL_CLUSTER_ID must be set")?,
        cluster_arn: std::env::var("ODORI_LIVE_DSQL_CLUSTER_ARN")
            .context("ODORI_LIVE_DSQL_CLUSTER_ARN must be set")?,
        endpoint: std::env::var("ODORI_LIVE_DSQL_ENDPOINT")
            .context("ODORI_LIVE_DSQL_ENDPOINT must be set")?,
        migration_policy,
        limits: EmbeddedDsqlLimits::default(),
    });
    exercise_durable_restart(
        storage,
        EmbeddedStorageMode::ExistingDsql,
        "adopted-endpoint",
    )
    .await?;
    println!("adopted-endpoint teardown: no cluster lifecycle mutation attempted");
    Ok(())
}
