//! The durable-execution substrate: worker bootstrap over an embedded
//! tokeira engine (or any Temporal-contract endpoint).
//!
//! This crate owns everything between "the user calls run" and "a workflow
//! task executes": connecting the Temporal Rust SDK 0.7 client, building
//! the worker with `odori-agents`' run-loop workflow and turn activities
//! registered, and running it to shutdown.
//!
//! ## The embedded transport
//!
//! The embedded tokeira engine (`tokeira-engine` in the engine repo)
//! exposes an in-process RPC endpoint consumed through
//! `ConnectionOptions::service_override` — an in-memory duplex with no TCP
//! listener and no port. `service_override` accepts the **SDK's own**
//! callback-service type, so this crate depends only on SDK types: the
//! application constructs the engine and hands its service override to
//! [`ConnectTarget::ServiceOverride`]. The same bootstrap drives an
//! external tokeirad or Temporal server via [`ConnectTarget::Url`], which
//! is also how the integration harness proves both paths.
//!
//! ```rust,ignore
//! // Application side, with the engine repo's tokeira-engine:
//! let engine = tokeira_engine::Engine::start().await?;
//! let odori = OdoriRuntime::builder("my-app")
//!     .connect(ConnectTarget::service_override(engine.service_override()))
//!     .agents(registry)
//!     .providers(providers)
//!     .start()
//!     .await?;
//! let result: String = odori.runner().run("assistant", "hello", "run-1").await?;
//! ```

use std::{fmt, sync::Arc};

use anyhow::Context as _;
use odori_agents::{AgentRegistry, Providers, Runner, TurnActivities, register_odori};
use temporalio_client::{
    Client, ClientOptions, Connection, ConnectionOptions, Url,
    callback_based::CallbackBasedGrpcService,
};
use temporalio_sdk::{Runtime, Worker, WorkerOptions};

/// Where the worker and client connect.
#[derive(Clone)]
pub enum ConnectTarget {
    /// In-process duplex to an embedded engine: the callback service from
    /// `tokeira_engine::Engine::service_override()` (or any implementation
    /// of the SDK's callback service).
    ServiceOverride(CallbackBasedGrpcService),
    /// A network endpoint speaking the Temporal contract (tokeirad, or a
    /// Temporal server).
    Url(Url),
}

impl ConnectTarget {
    /// Wrap an engine's service override.
    pub fn service_override(service: CallbackBasedGrpcService) -> Self {
        ConnectTarget::ServiceOverride(service)
    }
}

impl fmt::Debug for ConnectTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ConnectTarget::ServiceOverride(_) => f.write_str("ConnectTarget::ServiceOverride"),
            ConnectTarget::Url(url) => f.debug_tuple("ConnectTarget::Url").field(url).finish(),
        }
    }
}

/// Builder for [`OdoriRuntime`].
#[derive(Debug)]
pub struct OdoriRuntimeBuilder {
    task_queue: String,
    namespace: String,
    target: Option<ConnectTarget>,
    registry: AgentRegistry,
    providers: Option<Providers>,
}

impl OdoriRuntimeBuilder {
    /// The registered agents, replacing any prior set.
    pub fn agents(mut self, registry: AgentRegistry) -> Self {
        self.registry = registry;
        self
    }

    /// The provider set turns execute through.
    pub fn providers(mut self, providers: Providers) -> Self {
        self.providers = Some(providers);
        self
    }

    /// Where to connect (required).
    pub fn connect(mut self, target: ConnectTarget) -> Self {
        self.target = Some(target);
        self
    }

    /// Override the namespace (default: `"default"`).
    pub fn namespace(mut self, namespace: impl Into<String>) -> Self {
        self.namespace = namespace.into();
        self
    }

    /// Connect, assemble the worker, and start it on the current tokio
    /// runtime.
    pub async fn start(self) -> anyhow::Result<OdoriRuntime> {
        let target = self.target.context("ConnectTarget is required")?;
        let providers = self.providers.context("a provider set is required")?;

        // The URL is only an SDK configuration value when service_override
        // is present; no DNS lookup or listener is involved (engine-repo
        // tokeira-engine README).
        let connection_options = match target {
            ConnectTarget::ServiceOverride(service) => ConnectionOptions::new(
                Url::parse("http://odori-embedded.invalid:7233")
                    .context("static embedded URL must parse")?,
            )
            .service_override(service)
            .dns_load_balancing(None)
            .build(),
            ConnectTarget::Url(url) => ConnectionOptions::new(url).build(),
        };
        let connection = Connection::connect(connection_options)
            .await
            .context("connect the Temporal Rust SDK client")?;
        let client = Client::new(connection, ClientOptions::new(self.namespace).build())
            .context("construct the SDK client")?;

        let registry = Arc::new(self.registry);
        let activities = TurnActivities::new(registry, providers);

        // Workflow futures are deliberately !Send, so the worker cannot run
        // on a multi-threaded tokio executor. It gets its own OS thread with
        // a current-thread runtime; the client (Send) is shared with it, and
        // the shutdown handle comes back over a oneshot.
        let (ready_sender, ready_receiver) = tokio::sync::oneshot::channel();
        let worker_client = client.clone();
        let worker_queue = self.task_queue.clone();
        let worker_thread = std::thread::Builder::new()
            .name("odori-worker".to_owned())
            .spawn(move || -> anyhow::Result<()> {
                let local = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .context("build the worker thread's runtime")?;
                local.block_on(async move {
                    let runtime = Runtime::new_assume_tokio(Default::default())
                        .context("assemble the SDK runtime")?;
                    let worker_options =
                        register_odori(WorkerOptions::new(worker_queue), activities)
                            .context("register the Odori workflow and activities")?
                            .build();
                    let mut worker = Worker::new(&runtime, worker_client, worker_options)
                        .context("construct the SDK worker")?;
                    let shutdown: Box<dyn Fn() + Send + Sync> = Box::new(worker.shutdown_handle());
                    if ready_sender.send(shutdown).is_err() {
                        // The starter gave up; nothing to serve.
                        return Ok(());
                    }
                    worker.run().await.context("run the SDK worker")
                })
            })
            .context("spawn the worker thread")?;
        let shutdown = ready_receiver
            .await
            .context("worker thread ended before signalling readiness")?;

        Ok(OdoriRuntime {
            client,
            task_queue: self.task_queue,
            shutdown: Some(shutdown),
            worker_thread: Some(worker_thread),
        })
    }
}

/// A running Odori worker plus the client surface against it.
pub struct OdoriRuntime {
    client: Client,
    task_queue: String,
    shutdown: Option<Box<dyn Fn() + Send + Sync>>,
    worker_thread: Option<std::thread::JoinHandle<anyhow::Result<()>>>,
}

impl fmt::Debug for OdoriRuntime {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OdoriRuntime")
            .field("task_queue", &self.task_queue)
            .finish_non_exhaustive()
    }
}

impl OdoriRuntime {
    /// Start assembling a runtime submitting to `task_queue`.
    pub fn builder(task_queue: impl Into<String>) -> OdoriRuntimeBuilder {
        OdoriRuntimeBuilder {
            task_queue: task_queue.into(),
            namespace: "default".to_owned(),
            target: None,
            registry: AgentRegistry::new(),
            providers: None,
        }
    }

    /// A runner bound to this runtime's task queue.
    pub fn runner(&self) -> Runner {
        Runner::new(self.client.clone(), self.task_queue.clone())
    }

    /// The underlying SDK client, for surfaces the runner does not wrap.
    pub fn client(&self) -> Client {
        self.client.clone()
    }

    /// Stop the worker and await its drain.
    pub async fn shutdown(mut self) -> anyhow::Result<()> {
        if let Some(shutdown) = self.shutdown.take() {
            shutdown();
        }
        if let Some(thread) = self.worker_thread.take() {
            tokio::task::spawn_blocking(move || {
                thread
                    .join()
                    .map_err(|_| anyhow::anyhow!("worker thread panicked"))?
            })
            .await
            .context("join the worker thread")??;
        }
        Ok(())
    }
}
