use std::{collections::HashMap, path::Path};

use cargo_metadata::{semver::Version, MetadataCommand};
use color_eyre::{eyre::Context, Result};
use serde::Deserialize;

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct WorkspaceMetadata {
    pub versions_replacer: VersionsReplacerMetadata,
}

#[derive(Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct VersionsReplacerMetadata {
    pub external_versions: HashMap<String, String>,
}

pub type VersionMap = HashMap<String, Version>;

pub fn collect_versions_from_cargo_toml(manifest_path: impl AsRef<Path>) -> Result<VersionMap> {
    let metadata = MetadataCommand::new()
        .manifest_path(manifest_path.as_ref())
        .exec()
        .wrap_err("failed to execute 'cargo metadata'")?;
    let version_map = metadata
        .packages
        .iter()
        .map(|package| (package.name.clone(), package.version.clone()))
        .collect::<HashMap<_, _>>();
    Ok(version_map)
}
