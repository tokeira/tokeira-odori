//! Shared support for Odori's runnable examples and the integration tests:
//! the `--storage` flag and the bridge endpoint a scripted provider reads out
//! of a turn's tooling. The scenarios themselves live in `odori-examples`.

use std::{collections::BTreeMap, error::Error, fmt, path::PathBuf};

use clap::{Args, ValueEnum};
use odori_agents::provider::{McpTransport, TurnError, TurnTooling};
use odori_engine::{
    DsqlMigrationPolicy, EmbeddedDsqlLimits, EmbeddedStorageConfig, ExistingEmbeddedDsqlConfig,
    ManagedClusterIntent, ManagedEmbeddedDsqlConfig,
};

/// The embedded engine's storage backing, selected by `--storage`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, ValueEnum)]
pub enum StorageMode {
    /// Ephemeral in-process storage. No AWS account, no cleanup.
    #[default]
    InMemory,
    /// A DSQL cluster this run creates or recovers, then owns.
    ManagedDsql,
    /// A DSQL cluster that already exists and is owned elsewhere.
    AdoptExistingEndpoint,
}

/// The `--storage` flag, flattened into every example's parser.
///
/// The mode itself is validated by clap; the credentials each DSQL mode
/// needs come from the environment and are resolved by [`StorageArgs::resolve`].
#[derive(Debug, Clone, Copy, Args)]
pub struct StorageArgs {
    /// Storage backing for the embedded engine.
    #[arg(long = "storage", value_enum, default_value_t = StorageMode::InMemory)]
    pub storage: StorageMode,
}

/// A factual `--storage` environment error for an example.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageArgumentError(String);

impl fmt::Display for StorageArgumentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl Error for StorageArgumentError {}

fn required_environment(name: &str) -> Result<String, StorageArgumentError> {
    std::env::var(name).map_err(|_| {
        StorageArgumentError(format!(
            "{name} must be set for the selected DSQL storage mode"
        ))
    })
}

impl StorageArgs {
    /// Resolve the selected mode into E1's storage configuration, reading the
    /// environment the DSQL modes require.
    ///
    /// `managed-dsql` reads `ODORI_DSQL_REGION` and
    /// `ODORI_DSQL_DESCRIPTOR_PATH`. `adopt-existing-endpoint` additionally
    /// reads `ODORI_DSQL_CLUSTER_ID`, `ODORI_DSQL_CLUSTER_ARN`,
    /// `ODORI_DSQL_ENDPOINT`, and an explicit `ODORI_DSQL_MIGRATION_POLICY`
    /// (`automatic` or `validate-only`).
    pub fn resolve(self) -> Result<EmbeddedStorageConfig, StorageArgumentError> {
        match self.storage {
            StorageMode::InMemory => Ok(EmbeddedStorageConfig::InMemory),
            StorageMode::ManagedDsql => Ok(EmbeddedStorageConfig::ManagedDsql(
                ManagedEmbeddedDsqlConfig {
                    intent: ManagedClusterIntent::CreateOrRecover,
                    descriptor_path: PathBuf::from(required_environment(
                        "ODORI_DSQL_DESCRIPTOR_PATH",
                    )?),
                    region: required_environment("ODORI_DSQL_REGION")?,
                    migration_policy: None,
                    limits: EmbeddedDsqlLimits::default(),
                    tags: BTreeMap::from([(
                        "tokeira:owner".to_owned(),
                        "odori-example".to_owned(),
                    )]),
                },
            )),
            StorageMode::AdoptExistingEndpoint => {
                let migration_policy =
                    match required_environment("ODORI_DSQL_MIGRATION_POLICY")?.as_str() {
                        "automatic" => DsqlMigrationPolicy::Automatic,
                        "validate-only" => DsqlMigrationPolicy::ValidateOnly,
                        _ => {
                            return Err(StorageArgumentError(
                                "ODORI_DSQL_MIGRATION_POLICY must be automatic or validate-only"
                                    .to_owned(),
                            ));
                        }
                    };
                Ok(EmbeddedStorageConfig::ExistingDsql(
                    ExistingEmbeddedDsqlConfig {
                        region: required_environment("ODORI_DSQL_REGION")?,
                        cluster_id: required_environment("ODORI_DSQL_CLUSTER_ID")?,
                        cluster_arn: required_environment("ODORI_DSQL_CLUSTER_ARN")?,
                        endpoint: required_environment("ODORI_DSQL_ENDPOINT")?,
                        migration_policy,
                        limits: EmbeddedDsqlLimits::default(),
                    },
                ))
            }
        }
    }
}

/// The bridge's URL and bearer token, as attached to a turn's tooling.
///
/// Called from inside a turn, so a missing or non-HTTP attachment is a
/// `TurnError::Tooling` rather than a panic.
pub fn endpoint(tooling: &TurnTooling) -> Result<(String, String), TurnError> {
    let fault = |message: &str| TurnError::Tooling {
        message: message.to_owned(),
    };
    let server = tooling
        .mcp_servers
        .first()
        .ok_or_else(|| fault("the durable bridge was not attached"))?;
    let McpTransport::Http { url, headers } = &server.transport else {
        return Err(fault("the durable bridge must be attached over HTTP"));
    };
    let authorization = headers
        .iter()
        .find(|(name, _)| name.eq_ignore_ascii_case("authorization"))
        .map(|(_, value)| value.clone())
        .ok_or_else(|| fault("bridge attachment omitted authorization"))?;
    Ok((url.clone(), authorization))
}

#[cfg(test)]
mod tests {
    use clap::Parser;

    use super::*;

    #[derive(Debug, Parser)]
    struct Harness {
        #[command(flatten)]
        storage: StorageArgs,
    }

    #[test]
    fn storage_defaults_to_in_memory() {
        let parsed = Harness::try_parse_from(["example"]).expect("no flag parses");
        assert_eq!(parsed.storage.storage, StorageMode::InMemory);
        assert_eq!(
            parsed.storage.resolve().expect("in-memory needs no env"),
            EmbeddedStorageConfig::InMemory
        );
    }

    #[test]
    fn unknown_storage_mode_is_rejected_without_fallback() {
        let error = Harness::try_parse_from(["example", "--storage", "mystery"])
            .expect_err("unknown mode is rejected");
        assert_eq!(error.kind(), clap::error::ErrorKind::InvalidValue);
    }

    #[test]
    fn every_mode_is_reachable_by_its_documented_spelling() {
        for (spelling, expected) in [
            ("in-memory", StorageMode::InMemory),
            ("managed-dsql", StorageMode::ManagedDsql),
            (
                "adopt-existing-endpoint",
                StorageMode::AdoptExistingEndpoint,
            ),
        ] {
            let parsed = Harness::try_parse_from(["example", "--storage", spelling])
                .expect("documented spelling parses");
            assert_eq!(parsed.storage.storage, expected);
        }
    }
}
