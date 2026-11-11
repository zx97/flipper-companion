//! Flipper app catalog client and FAP installation.
//!
//! The official catalog is queried through its JSON API.  Additional sources
//! can be simple JSON indexes using the format documented in
//! `docs/apps.md`.

use std::{env, fs, path::PathBuf};

use flipper_companion_core::Progress;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{AppError, Companion};

/// Official catalog API root.
pub const OFFICIAL_CATALOG_URL: &str = "https://catalog.flipperzero.one/api";

/// One installable application.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInfo {
    pub id: String,
    pub alias: String,
    pub name: String,
    pub version: String,
    pub category: String,
    pub author: String,
    pub description: String,
    pub target: Option<String>,
    pub api: Option<String>,
    pub size_bytes: Option<u64>,
    pub download_url: String,
    pub sha256: Option<String>,
    /// Stable identifier of the repository ("official" or the index URL).
    pub source_id: String,
    /// Human-readable repository name.
    pub source: String,
    pub icon_url: Option<String>,
}

/// One application installed on the SD card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApp {
    /// File name without the `.fap` extension.
    pub name: String,
    /// Category directory under `/ext/apps`.
    pub category: String,
    /// Full remote path of the `.fap`.
    pub path: String,
    pub size_bytes: u64,
}

/// Outcome of verifying one installed `.fap` against the catalog build.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppVerification {
    pub bytes: u64,
    pub sha256: String,
    pub matches: bool,
}

/// Step of an installation, so the interface can narrate progress.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallPhase {
    Downloading,
    Verifying,
    Uploading,
}

/// Result of installing a FAP.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppInstallReport {
    pub name: String,
    pub version: String,
    pub remote_path: String,
    pub bytes: u64,
}

impl Companion {
    /// Lists applications from the official catalog and from user sources.
    ///
    /// A failing custom source is skipped so that one broken index cannot hide
    /// the official catalog.
    pub async fn catalog_apps(
        &self,
        target: &str,
        api: &str,
        category: Option<&str>,
        query: Option<&str>,
        source: Option<&str>,
    ) -> Result<Vec<AppInfo>, AppError> {
        let catalog = AppCatalog::new();

        let categories = catalog.categories(target, api).await?;

        let category_id = category.and_then(|wanted| {
            categories
                .iter()
                .find(|category| category.name.eq_ignore_ascii_case(wanted))
                .map(|category| category.id.clone())
        });

        let mut apps = catalog
            .official_apps(target, api, category_id.as_deref(), query)
            .await?;

        for category in &categories {
            for app in &mut apps {
                if app.category == category.id {
                    app.category = category.name.clone();
                }
            }
        }

        for source_url in load_custom_sources() {
            if let Ok(mut extra) = catalog.custom_apps(&source_url, target, api).await {
                apps.append(&mut extra);
            }
        }

        if let Some(source) = source {
            apps.retain(|app| app.source_id == source);
        }

        apps.sort_by_key(|app| app.name.to_lowercase());

        Ok(apps)
    }

    /// Lists the FAPs installed under `/ext/apps`.
    ///
    /// Missing category directories are skipped, so a partial SD card still
    /// reports whatever can be read.
    pub async fn installed_apps(
        &self,
        serial: Option<&str>,
    ) -> Result<Vec<InstalledApp>, AppError> {
        let session = self.connect(serial).await?;

        let mut apps = Vec::new();

        if let Ok(categories) = session.storage_list("/ext/apps").await {
            for category in categories.into_iter().filter(|entry| entry.is_directory()) {
                let directory = format!("/ext/apps/{}", category.name);
                let Ok(files) = session.storage_list(&directory).await else {
                    continue;
                };

                for file in files {
                    if file.is_directory() {
                        continue;
                    }

                    let Some(name) = file.name.strip_suffix(".fap") else {
                        continue;
                    };

                    apps.push(InstalledApp {
                        name: name.to_owned(),
                        category: category.name.clone(),
                        path: format!("{directory}/{}", file.name),
                        size_bytes: file.size_bytes,
                    });
                }
            }
        }

        let _ = session.stop().await;

        apps.sort_by(|left, right| {
            left.name
                .to_lowercase()
                .cmp(&right.name.to_lowercase())
                .then_with(|| left.category.cmp(&right.category))
        });

        Ok(apps)
    }

    /// Fetches the published build size of several applications at once.
    ///
    /// The list endpoint omits build metadata, so one detail request per
    /// application is required.  Downloads are limited to eight in flight to
    /// stay polite with the catalog API.
    pub async fn catalog_sizes(
        &self,
        target: &str,
        api: &str,
        ids: &[String],
    ) -> std::collections::HashMap<String, u64> {
        use std::sync::Arc;
        use tokio::sync::Semaphore;

        let catalog = Arc::new(AppCatalog::new());
        let permits = Arc::new(Semaphore::new(8));
        let mut tasks = tokio::task::JoinSet::new();

        for id in ids {
            let catalog = Arc::clone(&catalog);
            let permits = Arc::clone(&permits);
            let id = id.clone();
            let target = target.to_owned();
            let api = api.to_owned();

            tasks.spawn(async move {
                let _permit = permits.acquire_owned().await.ok()?;
                let size = catalog.build_size(&id, &target, &api).await.ok()?;
                Some((id, size))
            });
        }

        let mut sizes = std::collections::HashMap::new();

        while let Some(joined) = tasks.join_next().await {
            if let Ok(Some((id, Some(size)))) = joined {
                sizes.insert(id, size);
            }
        }

        sizes
    }

    /// Compares one installed FAP with the SHA-256 published by the catalog.
    pub async fn verify_installed_app(
        &self,
        serial: Option<&str>,
        remote: &str,
        expected_sha256: &str,
    ) -> Result<AppVerification, AppError> {
        let session = self.connect(serial).await?;
        let data = session.storage_read(remote).await?;
        let _ = session.stop().await;

        let sha256 = hex::encode(Sha256::digest(&data));

        Ok(AppVerification {
            bytes: data.len() as u64,
            matches: sha256.eq_ignore_ascii_case(expected_sha256),
            sha256,
        })
    }

    /// Removes one installed FAP from the SD card.
    pub async fn uninstall_app(&self, serial: Option<&str>, path: &str) -> Result<(), AppError> {
        let session = self.connect(serial).await?;
        let result = session.storage_remove(path, false).await;
        let _ = session.stop().await;

        result?;

        Ok(())
    }

    /// Downloads and installs one FAP on the device.
    pub async fn install_app<F, P>(
        &self,
        serial: Option<&str>,
        app: &AppInfo,
        mut on_progress: F,
        mut on_phase: P,
    ) -> Result<AppInstallReport, AppError>
    where
        F: FnMut(Progress),
        P: FnMut(InstallPhase),
    {
        on_progress(Progress::Indeterminate);
        on_phase(InstallPhase::Downloading);

        let bytes = AppCatalog::new()
            .download(&app.download_url, app.sha256.as_deref())
            .await?;

        if app.sha256.is_some() {
            on_phase(InstallPhase::Verifying);
        }

        let total = bytes.len() as u64;
        on_phase(InstallPhase::Uploading);

        let session = self.connect(serial).await?;
        let category = sanitize_component(&app.category, "Tools");
        let filename = sanitize_component(
            if app.alias.is_empty() {
                &app.name
            } else {
                &app.alias
            },
            "app",
        );

        let directory = format!("/ext/apps/{category}");
        ensure_remote_directory(&session, &directory).await?;

        let remote_path = format!("{directory}/{filename}.fap");
        session.storage_write(&remote_path, &bytes).await?;
        let _ = session.stop().await;

        on_progress(Progress::fraction(flipper_companion_core::Fraction::ONE));

        Ok(AppInstallReport {
            name: app.name.clone(),
            version: app.version.clone(),
            remote_path,
            bytes: total,
        })
    }
}

struct AppCatalog {
    base_url: String,
    client: reqwest::Client,
}

impl AppCatalog {
    fn new() -> Self {
        // The catalog is a convenience: never let a slow network freeze the
        // interface, every request gives up quickly.
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(6))
            .build()
            .unwrap_or_default();

        Self {
            base_url: String::from(OFFICIAL_CATALOG_URL),
            client,
        }
    }

    async fn categories(&self, target: &str, api: &str) -> Result<Vec<Category>, AppError> {
        let response = self
            .client
            .get(format!("{}/v0/0/category", self.base_url))
            .query(&[("api", api), ("target", target)])
            .send()
            .await?
            .error_for_status()?;

        let categories: Vec<WireCategory> = response.json().await?;
        Ok(categories
            .into_iter()
            .map(|category| Category {
                id: category.id,
                name: category.name,
            })
            .collect())
    }

    /// Fetches the official catalog, following pagination.
    ///
    /// The API caps a page at 500 entries; the catalog holds several hundred
    /// applications, so every page is requested until a short one arrives.
    async fn official_apps(
        &self,
        target: &str,
        api: &str,
        category_id: Option<&str>,
        query: Option<&str>,
    ) -> Result<Vec<AppInfo>, AppError> {
        const PAGE: u32 = 500;
        const MAX_PAGES: u32 = 20;

        let mut apps = Vec::new();

        for page in 0..MAX_PAGES {
            let request = WireListRequest {
                limit: PAGE,
                offset: page * PAGE,
                query,
                category_id,
                sort_by: "updated_at",
                sort_order: -1,
                target,
                api,
            };

            let response = self
                .client
                .post(format!("{}/v0/1/application", self.base_url))
                .json(&request)
                .send()
                .await?
                .error_for_status()?;

            let batch: Vec<WireApplication> = response.json().await?;
            let received = batch.len();

            apps.extend(
                batch
                    .into_iter()
                    .filter(|app| app.current_version.status == "READY")
                    .filter_map(|app| self.to_app_info(app, target, api)),
            );

            if (received as u32) < PAGE {
                break;
            }
        }

        Ok(apps)
    }

    fn to_app_info(&self, app: WireApplication, target: &str, api: &str) -> Option<AppInfo> {
        let version = app.current_version;
        let bundle_url = format!(
            "{}/v0/0/application/version/{}/build/compatible?target={target}&api={api}",
            self.base_url, version.id
        );

        Some(AppInfo {
            id: app.id,
            alias: app.alias,
            name: version.name,
            version: version.version,
            category: app.category_id,
            author: app.author.unwrap_or_default(),
            description: version.short_description.unwrap_or_default(),
            target: version.current_build.sdk.target,
            api: version.current_build.sdk.api,
            size_bytes: version
                .current_build
                .metadata
                .and_then(|metadata| metadata.length),
            download_url: bundle_url,
            sha256: version.current_build.fap_hash,
            source_id: String::from("official"),
            source: String::from("official"),
            icon_url: version.icon_uri,
        })
    }

    /// Size of the published build of one application.
    async fn build_size(&self, id: &str, target: &str, api: &str) -> Result<Option<u64>, AppError> {
        let response = self
            .client
            .get(format!("{}/v0/0/application/{id}", self.base_url))
            .query(&[("api", api), ("target", target)])
            .send()
            .await?
            .error_for_status()?;

        let detail: WireDetail = response.json().await?;

        Ok(detail
            .current_version
            .current_build
            .metadata
            .and_then(|metadata| metadata.length))
    }

    async fn custom_apps(
        &self,
        source_url: &str,
        target: &str,
        api: &str,
    ) -> Result<Vec<AppInfo>, AppError> {
        let response = self
            .client
            .get(source_url)
            .send()
            .await?
            .error_for_status()?;
        let index: WireIndex = response.json().await?;
        let source = if index.name.is_empty() {
            source_url.to_owned()
        } else {
            index.name
        };

        Ok(index
            .apps
            .into_iter()
            .filter(|app| {
                app.target
                    .as_deref()
                    .is_none_or(|value| value == "any" || value == target)
                    && app
                        .api
                        .as_deref()
                        .is_none_or(|value| value == "any" || value == api)
            })
            .map(|app| AppInfo {
                id: app.id,
                alias: app.alias.unwrap_or_default(),
                name: app.name,
                version: app.version,
                category: app.category.unwrap_or_else(|| String::from("Tools")),
                author: app.author.unwrap_or_default(),
                description: app.description.unwrap_or_default(),
                target: app.target,
                api: app.api,
                size_bytes: None,
                download_url: app.url,
                sha256: app.sha256,
                source_id: source_url.to_owned(),
                source: source.clone(),
                icon_url: None,
            })
            .collect())
    }

    async fn download(
        &self,
        url: &str,
        expected_sha256: Option<&str>,
    ) -> Result<Vec<u8>, AppError> {
        let response = self.client.get(url).send().await?.error_for_status()?;
        let bytes = response.bytes().await?.to_vec();

        if let Some(expected) = expected_sha256.filter(|value| !value.is_empty()) {
            let actual = hex::encode(Sha256::digest(&bytes));

            if !actual.eq_ignore_ascii_case(expected) {
                return Err(AppError::App(format!(
                    "checksum mismatch: expected {expected}, got {actual}"
                )));
            }
        }

        Ok(bytes)
    }
}

async fn ensure_remote_directory(
    session: &flipper_companion_protocol::RpcSession,
    path: &str,
) -> Result<(), AppError> {
    if matches!(session.storage_stat(path).await, Ok(Some(entry)) if entry.is_directory()) {
        return Ok(());
    }

    if let Err(error) = session.storage_mkdir(path).await
        && !matches!(session.storage_stat(path).await, Ok(Some(entry)) if entry.is_directory())
    {
        return Err(error.into());
    }

    Ok(())
}

fn sanitize_component(value: &str, fallback: &str) -> String {
    let mut result = String::with_capacity(value.len());
    let mut last_separator = false;

    for character in value.chars() {
        if character.is_ascii_alphanumeric() || character == '-' || character == '_' {
            result.push(character);
            last_separator = false;
        } else if !last_separator {
            result.push('_');
            last_separator = true;
        }
    }

    let result = result.trim_matches('_').to_owned();

    if result.is_empty() {
        fallback.to_owned()
    } else {
        result
    }
}

/// Path of the user source list.
#[must_use]
pub fn app_sources_path() -> Option<PathBuf> {
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;

    Some(base.join("flipper-companion").join("app-sources.txt"))
}

/// Loads the custom JSON index URLs.
#[must_use]
pub fn load_custom_sources() -> Vec<String> {
    let Some(path) = app_sources_path() else {
        return Vec::new();
    };

    fs::read_to_string(path)
        .map(|contents| {
            contents
                .lines()
                .map(str::trim)
                .filter(|line| !line.is_empty() && !line.starts_with('#'))
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default()
}

/// Removes a custom JSON index URL.
///
/// Returns whether the URL was configured.
pub fn remove_custom_source(url: &str) -> Result<bool, AppError> {
    let sources = load_custom_sources();

    if !sources.iter().any(|source| source == url) {
        return Ok(false);
    }

    let path = app_sources_path()
        .ok_or_else(|| AppError::App(String::from("cannot determine the config directory")))?;

    let kept = sources
        .into_iter()
        .filter(|source| source != url)
        .collect::<Vec<_>>();

    if kept.is_empty() {
        let _ = fs::remove_file(path);
    } else {
        fs::write(path, kept.join("\n") + "\n")?;
    }

    Ok(true)
}

/// Adds a custom JSON index URL, ignoring duplicates.
pub fn add_custom_source(url: &str) -> Result<(), AppError> {
    let url = url.trim();

    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return Err(AppError::App(String::from(
            "an app source must be an HTTP(S) URL",
        )));
    }

    let mut sources = load_custom_sources();

    if sources.iter().any(|source| source == url) {
        return Ok(());
    }

    let path = app_sources_path()
        .ok_or_else(|| AppError::App(String::from("cannot determine the config directory")))?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    sources.push(url.to_owned());
    fs::write(path, sources.join("\n") + "\n")?;

    Ok(())
}

#[derive(Debug, Clone)]
struct Category {
    id: String,
    name: String,
}

#[derive(Deserialize)]
struct WireDetail {
    current_version: WireVersion,
}

#[derive(Deserialize)]
struct WireCategory {
    #[serde(rename = "_id")]
    id: String,
    name: String,
}

#[derive(Serialize)]
struct WireListRequest<'a> {
    limit: u32,
    offset: u32,
    #[serde(skip_serializing_if = "Option::is_none")]
    query: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    category_id: Option<&'a str>,
    sort_by: &'a str,
    sort_order: i32,
    target: &'a str,
    api: &'a str,
}

#[derive(Deserialize)]
struct WireApplication {
    #[serde(rename = "_id")]
    id: String,
    #[serde(default)]
    alias: String,
    #[serde(default)]
    author: Option<String>,
    category_id: String,
    current_version: WireVersion,
}

#[derive(Deserialize)]
struct WireVersion {
    #[serde(rename = "_id")]
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    short_description: Option<String>,
    #[serde(default)]
    icon_uri: Option<String>,
    #[serde(default)]
    status: String,
    current_build: WireBuild,
}

#[derive(Deserialize)]
struct WireBuild {
    #[serde(default)]
    metadata: Option<WireMetadata>,
    #[serde(default)]
    fap_hash: Option<String>,
    sdk: WireSdk,
}

#[derive(Deserialize)]
struct WireMetadata {
    #[serde(default)]
    length: Option<u64>,
}

#[derive(Deserialize)]
struct WireSdk {
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    api: Option<String>,
}

#[derive(Deserialize)]
struct WireIndex {
    #[serde(default)]
    name: String,
    #[serde(default)]
    apps: Vec<WireCustomApp>,
}

#[derive(Deserialize)]
struct WireCustomApp {
    #[serde(default)]
    id: String,
    name: String,
    version: String,
    #[serde(default)]
    alias: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    category: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    target: Option<String>,
    #[serde(default)]
    api: Option<String>,
    url: String,
    #[serde(default)]
    sha256: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_fap_path_components() {
        assert_eq!(sanitize_component("Sub-GHz", "Tools"), "Sub-GHz");
        assert_eq!(
            sanitize_component("Pokemon Trade Tool", "app"),
            "Pokemon_Trade_Tool"
        );
        assert_eq!(sanitize_component("***", "app"), "app");
    }

    #[test]
    fn rejects_non_http_sources() {
        assert!(add_custom_source("file:///tmp/apps.json").is_err());
    }
}
