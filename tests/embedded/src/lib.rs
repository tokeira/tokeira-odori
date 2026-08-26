//! Integration harness for the Odori primitives against embedded tokeira.
//!
//! Scenario implementations live beside their example entry points. This
//! crate only owns the shared storage-mode CLI boundary.

use std::{collections::BTreeMap, error::Error, fmt, path::PathBuf};

use odori_engine::{
    DsqlMigrationPolicy, EmbeddedDsqlLimits, EmbeddedStorageConfig, ExistingEmbeddedDsqlConfig,
    ManagedClusterIntent, ManagedEmbeddedDsqlConfig,
};

/// A factual `--storage` argument or environment error for an example.
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

/// Remove `--storage <mode>` from an example's arguments and return E1's
/// storage configuration unchanged.
///
/// The default is `in-memory`. `managed-dsql` reads `ODORI_DSQL_REGION` and
/// `ODORI_DSQL_DESCRIPTOR_PATH`. `adopt-existing-endpoint` additionally reads
/// `ODORI_DSQL_CLUSTER_ID`, `ODORI_DSQL_CLUSTER_ARN`, `ODORI_DSQL_ENDPOINT`,
/// and an explicit `ODORI_DSQL_MIGRATION_POLICY` (`automatic` or
/// `validate-only`).
pub fn take_storage_flag(
    arguments: &mut Vec<String>,
) -> Result<EmbeddedStorageConfig, StorageArgumentError> {
    let positions = arguments
        .iter()
        .enumerate()
        .filter_map(|(index, argument)| (argument == "--storage").then_some(index))
        .collect::<Vec<_>>();
    if positions.len() > 1 {
        return Err(StorageArgumentError(
            "--storage may be supplied only once".to_owned(),
        ));
    }
    let mode = match positions.first().copied() {
        Some(index) => {
            if index + 1 >= arguments.len() {
                return Err(StorageArgumentError(
                    "--storage requires in-memory, managed-dsql, or adopt-existing-endpoint"
                        .to_owned(),
                ));
            }
            let mode = arguments.remove(index + 1);
            arguments.remove(index);
            mode
        }
        None => "in-memory".to_owned(),
    };

    match mode.as_str() {
        "in-memory" => Ok(EmbeddedStorageConfig::InMemory),
        "managed-dsql" => Ok(EmbeddedStorageConfig::ManagedDsql(
            ManagedEmbeddedDsqlConfig {
                intent: ManagedClusterIntent::CreateOrRecover,
                descriptor_path: PathBuf::from(required_environment("ODORI_DSQL_DESCRIPTOR_PATH")?),
                region: required_environment("ODORI_DSQL_REGION")?,
                migration_policy: None,
                limits: EmbeddedDsqlLimits::default(),
                tags: BTreeMap::from([("tokeira:owner".to_owned(), "odori-example".to_owned())]),
            },
        )),
        "adopt-existing-endpoint" => {
            let migration_policy = match required_environment("ODORI_DSQL_MIGRATION_POLICY")?
                .as_str()
            {
                "automatic" => DsqlMigrationPolicy::Automatic,
                "validate-only" => DsqlMigrationPolicy::ValidateOnly,
                _ => {
                    return Err(StorageArgumentError(
                        "ODORI_DSQL_MIGRATION_POLICY must be automatic or validate-only".to_owned(),
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
        _ => Err(StorageArgumentError(format!(
            "unknown storage mode {mode:?}; use in-memory, managed-dsql, or adopt-existing-endpoint"
        ))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn storage_defaults_to_in_memory_without_consuming_arguments() {
        let mut arguments = vec!["prepare".to_owned()];
        assert_eq!(
            take_storage_flag(&mut arguments).unwrap(),
            EmbeddedStorageConfig::InMemory
        );
        assert_eq!(arguments, ["prepare"]);
    }

    #[test]
    fn unknown_storage_mode_is_rejected_without_fallback() {
        let mut arguments = vec!["--storage".to_owned(), "mystery".to_owned()];
        let error = take_storage_flag(&mut arguments).unwrap_err();
        assert!(error.to_string().contains("unknown storage mode"));
    }
}
