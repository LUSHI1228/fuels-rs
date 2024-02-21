use std::path::PathBuf;

use argh::FromArgs;
use color_eyre::{
    eyre::{eyre, Context},
    Result,
};
use regex::Regex;
use versions_manager::{
    metadata::collect_versions_from_cargo_toml, write::replace_versions_in_file,
};
use walkdir::WalkDir;

#[derive(FromArgs)]
/// Manage version-related tasks for CI.
struct VersionsManager {
    /// path to Cargo.toml with versions
    #[argh(option)]
    manifest_path: PathBuf,
    #[argh(subcommand)]
    subcommand: VersionsManagerSubcommandEnum,
}

#[derive(FromArgs)]
#[argh(subcommand)]
enum VersionsManagerSubcommandEnum {
    Replace(Replacer),
}

#[derive(FromArgs)]
/// Replace variables like '{{versions.fuels}}' with correct versions from Cargo.toml.
/// Uses versions from '[workspace.members]' and '[workspace.metadata.versions-manager.external-versions]'.
#[argh(subcommand, name = "replace")]
struct Replacer {
    /// path to directory with files containing variables
    #[argh(positional)]
    path: PathBuf,
    /// regex to filter filenames (example: "\.md$")
    #[argh(option)]
    filename_regex: Option<Regex>,
}

fn main() -> Result<()> {
    use VersionsManagerSubcommandEnum::*;

    let VersionsManager {
        manifest_path,
        subcommand,
    } = argh::from_env();
    match subcommand {
        Replace(Replacer {
            path,
            filename_regex,
        }) => {
            let versions = collect_versions_from_cargo_toml(manifest_path)?;

            let mut total_replacements: Vec<usize> = Vec::new();

            for entry in WalkDir::new(path) {
                let entry = entry.wrap_err("failed to get directory entry")?;

                if entry.path().is_file() {
                    if let Some(filename_regex) = &filename_regex {
                        let file_name = entry
                            .path()
                            .file_name()
                            .ok_or_else(|| eyre!("{:?} has an invalid file name", entry.path()))?
                            .to_str()
                            .ok_or_else(|| eyre!("filename is not valid UTF-8"))?;
                        if !filename_regex.is_match(file_name) {
                            continue;
                        }
                    }

                    let replacement_count = replace_versions_in_file(entry.path(), &versions)
                        .wrap_err_with(|| {
                            format!("failed to replace versions in {:?}", entry.path())
                        })?;
                    if replacement_count > 0 {
                        total_replacements.push(replacement_count);
                    }
                }
            }

            println!(
                "replaced {} variables across {} files",
                total_replacements.iter().sum::<usize>(),
                total_replacements.len()
            );
        }
    }

    Ok(())
}
