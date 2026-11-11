//! Project-side configuration: `flip.toml`, sources and the fetch plan.
//!
//! This module is a clean-room adaptation of the shipped `flippy` CLI
//! behaviour (MIT, (c) 2025 elijah629).  It keeps the file format and the
//! validation rules compatible so an existing project can be opened with
//! either tool.

use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result, bail};

use crate::{
    config::{CONFIG_FILE_NAME, FlipConfig, RepositoryConfig},
    sources::{FirmwareSource, RepositoryRemote, RepositorySource},
};

pub const STORE_DIR_NAME: &str = "store";
pub const FETCH_PLAN_FILE_NAME: &str = "fetch-plan.txt";

/// Paths created by [`create_project`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CreatedProject {
    pub directory: PathBuf,
    pub config: PathBuf,
    pub store: PathBuf,
}

/// Creates a project directory with `flip.toml` and `store/`.
pub fn create_project(path: &Path) -> Result<CreatedProject> {
    fs::create_dir_all(path)
        .with_context(|| format!("failed to create project directory at '{}'", path.display()))?;

    let config_path = path.join(CONFIG_FILE_NAME);
    if config_path.exists() {
        bail!("project already contains {}", config_path.display());
    }

    let name = path
        .file_name()
        .and_then(std::ffi::OsStr::to_str)
        .filter(|name| !name.is_empty())
        .unwrap_or("flippy");

    let config = FlipConfig::new(name.to_owned());
    config.save_to_dir(path)?;
    let store = ensure_store_dir(path)?;

    Ok(CreatedProject {
        directory: path.to_path_buf(),
        config: config_path,
        store,
    })
}

/// Adds a repository to `flip.toml`, deriving a unique name when none is
/// requested.
pub fn add_repository(
    cwd: &Path,
    source: RepositorySource,
    requested_name: Option<String>,
) -> Result<(String, RepositorySource)> {
    let mut config = FlipConfig::load_from_dir(cwd)?;

    let name = requested_name
        .map(|name| validate_repository_name(name.trim()))
        .transpose()?
        .unwrap_or_else(|| config.next_repository_name(suggest_repository_name(&source)));

    if config.repositories.contains_key(&name) {
        bail!(
            "repository '{name}' already exists in {}",
            cwd.join(CONFIG_FILE_NAME).display()
        );
    }

    config.repositories.insert(
        name.clone(),
        RepositoryConfig {
            source: source.clone(),
        },
    );
    config.save_to_dir(cwd)?;

    Ok((name, source))
}

/// Sets the firmware source in `flip.toml`.
pub fn set_firmware(cwd: &Path, source: FirmwareSource) -> Result<()> {
    let mut config = FlipConfig::load_from_dir(cwd)?;
    config.firmware = source;
    config.save_to_dir(cwd)
}

/// Ensures `store/` exists and returns its path.
pub fn create_store(cwd: &Path) -> Result<PathBuf> {
    ensure_store_dir(cwd)
}

/// Validates local sources and writes `store/fetch-plan.txt`.
pub fn fetch_store(cwd: &Path, optimize: bool) -> Result<PathBuf> {
    let config = FlipConfig::load_from_dir(cwd)?;
    let store_dir = ensure_store_dir(cwd)?;
    validate_sources(cwd, &config)?;

    let plan = render_fetch_plan(&config, optimize);
    let plan_path = store_dir.join(FETCH_PLAN_FILE_NAME);
    fs::write(&plan_path, plan)
        .with_context(|| format!("failed to write fetch plan to '{}'", plan_path.display()))?;

    Ok(plan_path)
}

/// Loads `flip.toml` from a project directory.
pub fn load_config(cwd: &Path) -> Result<FlipConfig> {
    FlipConfig::load_from_dir(cwd)
}

fn ensure_store_dir(cwd: &Path) -> Result<PathBuf> {
    let store_dir = cwd.join(STORE_DIR_NAME);
    fs::create_dir_all(&store_dir)
        .with_context(|| format!("failed to create store directory '{}'", store_dir.display()))?;
    Ok(store_dir)
}

fn validate_sources(cwd: &Path, config: &FlipConfig) -> Result<()> {
    if let FirmwareSource::Local(path) = &config.firmware {
        validate_local_path(cwd, path, "firmware source")?;
    }

    for (name, repository) in &config.repositories {
        if let RepositorySource::Local(path) = &repository.source {
            validate_local_path(cwd, path, &format!("repository '{name}'"))?;
        }
    }

    Ok(())
}

fn validate_local_path(cwd: &Path, path: &Path, kind: &str) -> Result<()> {
    let candidate = if path.is_absolute() {
        path.to_path_buf()
    } else {
        cwd.join(path)
    };

    if candidate.exists() {
        Ok(())
    } else {
        bail!("{kind} '{}' does not exist", candidate.display())
    }
}

/// Renders the human-readable fetch plan written by [`fetch_store`].
#[must_use]
pub fn render_fetch_plan(config: &FlipConfig, optimize: bool) -> String {
    let mut plan = String::new();
    plan.push_str("flippy fetch plan\n");
    plan.push_str("=================\n");
    plan.push_str(&format!("name: {}\n", config.name));
    plan.push_str(&format!("optimize: {optimize}\n"));
    plan.push_str(&format!("firmware: {}\n", config.firmware));
    plan.push_str("repositories:\n");

    if config.repositories.is_empty() {
        plan.push_str("- none\n");
    } else {
        for (name, repository) in &config.repositories {
            plan.push_str(&format!("- {name}: {}\n", repository.source));
        }
    }

    plan
}

fn validate_repository_name(name: &str) -> Result<String> {
    if name.is_empty() {
        bail!("repository name cannot be empty");
    }

    Ok(name.to_owned())
}

fn suggest_repository_name(source: &RepositorySource) -> String {
    match source {
        RepositorySource::Local(path) => path
            .file_name()
            .and_then(std::ffi::OsStr::to_str)
            .map(normalize_repository_name)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "repository".to_owned()),
        RepositorySource::Remote(RepositoryRemote::Url(url)) => url
            .path_segments()
            .and_then(Iterator::last)
            .map(normalize_repository_name)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "repository".to_owned()),
        RepositorySource::Remote(RepositoryRemote::ScpLike(remote)) => remote
            .rsplit(['/', ':'])
            .next()
            .map(normalize_repository_name)
            .filter(|name| !name.is_empty())
            .unwrap_or_else(|| "repository".to_owned()),
    }
}

fn normalize_repository_name(value: &str) -> String {
    let trimmed = value.trim_end_matches(".git");
    let mut normalized = String::with_capacity(trimmed.len());
    let mut last_was_separator = false;

    for character in trimmed.chars() {
        if character.is_ascii_alphanumeric() {
            normalized.push(character.to_ascii_lowercase());
            last_was_separator = false;
        } else if !last_was_separator {
            normalized.push('-');
            last_was_separator = true;
        }
    }

    normalized.trim_matches('-').to_owned()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use tempfile::tempdir;

    use super::{add_repository, create_project, fetch_store, load_config, set_firmware};
    use crate::sources::{FirmwarePreset, FirmwareSource, RepositoryRemote, RepositorySource};

    #[test]
    fn new_creates_project_files() {
        let temp_dir = tempdir().unwrap();
        let project_dir = temp_dir.path().join("demo");

        create_project(&project_dir).unwrap();

        assert!(project_dir.join("flip.toml").exists());
        assert!(project_dir.join("store").exists());
    }

    #[test]
    fn new_rejects_an_existing_project() {
        let temp_dir = tempdir().unwrap();

        create_project(temp_dir.path()).unwrap();
        let error = create_project(temp_dir.path()).unwrap_err();

        assert!(error.to_string().contains("already contains"));
    }

    #[test]
    fn map_commands_update_flip_toml() {
        let temp_dir = tempdir().unwrap();

        create_project(temp_dir.path()).unwrap();
        set_firmware(
            temp_dir.path(),
            FirmwareSource::Preset(FirmwarePreset::RogueFree),
        )
        .unwrap();
        add_repository(
            temp_dir.path(),
            RepositorySource::Remote(RepositoryRemote::ScpLike(
                "git@github.com:flipperdevices/flipperzero-firmware.git".to_owned(),
            )),
            None,
        )
        .unwrap();

        let config = load_config(temp_dir.path()).unwrap();
        assert_eq!(
            config.firmware,
            FirmwareSource::Preset(FirmwarePreset::RogueFree)
        );
        assert_eq!(config.repositories.len(), 1);
        assert_eq!(
            config.repositories["flipperzero-firmware"].source,
            RepositorySource::Remote(RepositoryRemote::ScpLike(
                "git@github.com:flipperdevices/flipperzero-firmware.git".to_owned(),
            ))
        );
    }

    #[test]
    fn fetch_validates_local_sources_and_writes_plan() {
        let temp_dir = tempdir().unwrap();
        let firmware_path = temp_dir.path().join("fw.tgz");
        fs::write(&firmware_path, b"firmware").unwrap();
        let repository_dir = temp_dir.path().join("Flipper-IRDB");
        fs::create_dir_all(&repository_dir).unwrap();

        create_project(temp_dir.path()).unwrap();
        set_firmware(temp_dir.path(), "path://fw.tgz".parse().unwrap()).unwrap();
        add_repository(
            temp_dir.path(),
            "path://Flipper-IRDB".parse().unwrap(),
            Some("irdb".to_owned()),
        )
        .unwrap();

        let plan_path = fetch_store(temp_dir.path(), true).unwrap();
        let plan = fs::read_to_string(plan_path).unwrap();

        assert!(plan.contains("optimize: true"));
        assert!(plan.contains("firmware: path://fw.tgz"));
        assert!(plan.contains("- irdb: path://Flipper-IRDB"));
    }

    #[test]
    fn fetch_fails_for_missing_local_sources() {
        let temp_dir = tempdir().unwrap();

        create_project(temp_dir.path()).unwrap();
        set_firmware(temp_dir.path(), "path://missing.tgz".parse().unwrap()).unwrap();

        let error = fetch_store(temp_dir.path(), false).unwrap_err();

        assert!(error.to_string().contains("firmware source"));
    }
}
