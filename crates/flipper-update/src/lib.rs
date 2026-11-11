//! Firmware update directory, downloads and bundle extraction.

pub mod region;

use std::{
    collections::HashSet,
    path::{Component, Path, PathBuf},
};

use flipper_companion_core::{CoreError, Progress, Version};
use futures::StreamExt;
use serde::Deserialize;
use sha2::{Digest, Sha256};
use tar::EntryType;
use tokio::io::AsyncWriteExt;

/// Default directory served by the official update infrastructure.
pub const DEFAULT_DIRECTORY_URL: &str = "https://update.flipperzero.one/firmware/directory.json";

/// The update directory: a list of release channels.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Directory {
    pub channels: Vec<Channel>,
}

/// One release channel (`release`, `release-candidate`, `development`, ...).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Channel {
    pub id: String,
    pub title: String,
    pub description: String,
    pub versions: Vec<Release>,
}

/// One firmware release within a channel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Release {
    pub version: String,
    pub changelog: String,
    pub timestamp: u64,
    pub files: Vec<FirmwareFile>,
}

/// One downloadable artifact of a release.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareFile {
    pub target: String,
    pub kind: String,
    pub url: String,
    pub sha256: String,
}

impl Directory {
    /// Parses the JSON served at the update directory URL.
    pub fn parse(json: &str) -> Result<Self, UpdateError> {
        let wire: WireDirectory = serde_json::from_str(json)
            .map_err(|error| UpdateError::InvalidDirectory(error.to_string()))?;

        if wire.channels.is_empty() {
            return Err(UpdateError::InvalidDirectory(String::from(
                "directory has no channels",
            )));
        }

        let mut seen = HashSet::new();
        let mut channels = Vec::with_capacity(wire.channels.len());

        for wire_channel in wire.channels {
            if wire_channel.id.is_empty() {
                return Err(UpdateError::InvalidDirectory(String::from(
                    "channel without an id",
                )));
            }

            if !seen.insert(wire_channel.id.clone()) {
                return Err(UpdateError::InvalidDirectory(format!(
                    "duplicate channel `{}`",
                    wire_channel.id
                )));
            }

            let mut versions: Vec<Release> = wire_channel
                .versions
                .into_iter()
                .map(Release::from_wire)
                .collect();

            versions.sort_by_key(|release| std::cmp::Reverse(release.sort_key()));

            channels.push(Channel {
                id: wire_channel.id,
                title: wire_channel.title,
                description: wire_channel.description,
                versions,
            });
        }

        Ok(Self { channels })
    }

    /// Returns a channel by id.
    #[must_use]
    pub fn channel(&self, id: &str) -> Option<&Channel> {
        self.channels.iter().find(|channel| channel.id == id)
    }

    /// Returns the newest release of a channel.
    #[must_use]
    pub fn latest(&self, id: &str) -> Option<&Release> {
        self.channel(id)?.versions.first()
    }

    /// Returns the channel ids in directory order.
    #[must_use]
    pub fn channel_ids(&self) -> Vec<&str> {
        self.channels
            .iter()
            .map(|channel| channel.id.as_str())
            .collect()
    }
}

impl Release {
    fn from_wire(wire: WireRelease) -> Self {
        Self {
            version: wire.version,
            changelog: wire.changelog,
            timestamp: wire.timestamp,
            files: wire
                .files
                .into_iter()
                .map(FirmwareFile::from_wire)
                .collect(),
        }
    }

    /// Finds an artifact by type (`full_dfu`, `update_tgz`, ...) and target
    /// (`f18`, `any`, ...).
    #[must_use]
    pub fn file(&self, kind: &str, target: &str) -> Option<&FirmwareFile> {
        self.files
            .iter()
            .find(|file| file.kind == kind && file.target == target)
    }

    /// Parsed semantic version, when the release is not a development build.
    #[must_use]
    pub fn parsed_version(&self) -> Option<Version> {
        self.version.parse().ok()
    }

    fn sort_key(&self) -> ReleaseSortKey {
        match self.parsed_version() {
            Some(version) => {
                let (release, rc) = match version.pre {
                    None => (1, 0),
                    Some(flipper_companion_core::PreRelease::Rc(number)) => {
                        (0, number.unwrap_or(0))
                    }
                };

                (
                    1,
                    version.major.into(),
                    version.minor.into(),
                    version.patch.into(),
                    release,
                    rc,
                )
            }
            None => (0, 0, 0, 0, 0, 0),
        }
    }
}

impl FirmwareFile {
    fn from_wire(wire: WireFile) -> Self {
        Self {
            target: wire.target,
            kind: wire.kind,
            url: wire.url,
            sha256: wire.sha256,
        }
    }

    /// A safe local file name derived from the URL.
    pub fn file_name(&self) -> Result<String, UpdateError> {
        let name = self
            .url
            .rsplit('/')
            .next()
            .filter(|name| !name.is_empty() && !name.contains('?'))
            .ok_or_else(|| UpdateError::InvalidUrl(self.url.clone()))?;

        Ok(name.to_owned())
    }

    /// Parses the expected SHA-256 digest, if the directory provides one.
    pub fn expected_digest(&self) -> Result<Option<[u8; 32]>, UpdateError> {
        if self.sha256.is_empty() {
            return Ok(None);
        }

        let bytes = hex::decode(&self.sha256)
            .map_err(|_| UpdateError::InvalidDigest(self.sha256.clone()))?;

        let digest = bytes
            .try_into()
            .map_err(|_| UpdateError::InvalidDigest(self.sha256.clone()))?;

        Ok(Some(digest))
    }
}

type ReleaseSortKey = (u8, u64, u64, u64, u8, u32);

/// Client for the update directory and its artifacts.
pub struct DirectoryClient {
    url: String,
    client: reqwest::Client,
}

impl DirectoryClient {
    #[must_use]
    pub fn new(url: impl Into<String>) -> Self {
        Self {
            url: url.into(),
            client: reqwest::Client::new(),
        }
    }

    #[must_use]
    pub fn with_default_url() -> Self {
        Self::new(DEFAULT_DIRECTORY_URL)
    }

    /// Fetches and parses the directory.
    pub async fn load(&self) -> Result<Directory, UpdateError> {
        let response = self
            .client
            .get(&self.url)
            .send()
            .await?
            .error_for_status()?;
        let body = response.text().await?;

        Directory::parse(&body)
    }

    /// Downloads an artifact, verifies its SHA-256 and reports progress.
    ///
    /// The destination is removed when the checksum does not match, so a
    /// caller can never flash a partially-written or corrupted image.
    pub async fn download(
        &self,
        file: &FirmwareFile,
        destination: &Path,
        on_progress: impl FnMut(Progress),
    ) -> Result<(), UpdateError> {
        let result = self.download_inner(file, destination, on_progress).await;

        if result.is_err() {
            // Never leave a partial or corrupt image behind.
            let _ = tokio::fs::remove_file(destination).await;
        }

        result
    }

    async fn download_inner(
        &self,
        file: &FirmwareFile,
        destination: &Path,
        mut on_progress: impl FnMut(Progress),
    ) -> Result<(), UpdateError> {
        let response = self
            .client
            .get(&file.url)
            .send()
            .await?
            .error_for_status()?;

        let total = response.content_length().unwrap_or(0);
        let mut stream = response.bytes_stream();
        let mut output = tokio::fs::File::create(destination).await?;
        let mut hasher = Sha256::new();
        let mut downloaded = 0u64;

        while let Some(chunk) = stream.next().await {
            let chunk = chunk?;
            output.write_all(&chunk).await?;
            hasher.update(&chunk);
            downloaded += chunk.len() as u64;
            on_progress(Progress::ratio(downloaded, total));
        }

        output.flush().await?;

        if let Some(expected) = file.expected_digest()? {
            let actual: [u8; 32] = hasher.finalize().into();

            if actual != expected {
                return Err(UpdateError::ChecksumMismatch {
                    file: file.url.clone(),
                    expected: hex::encode(expected),
                    actual: hex::encode(actual),
                });
            }
        }

        Ok(())
    }

    /// Downloads an artifact into a directory, using its URL file name.
    pub async fn download_to(
        &self,
        file: &FirmwareFile,
        directory: &Path,
        on_progress: impl FnMut(Progress),
    ) -> Result<PathBuf, UpdateError> {
        let destination = directory.join(file.file_name()?);
        self.download(file, &destination, on_progress).await?;
        Ok(destination)
    }
}

/// A `.tgz` update bundle extracted on disk.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FirmwareBundle {
    /// Directory containing `update.fuf`.
    pub root: PathBuf,
    /// Path of the update manifest expected by `System_update`.
    pub manifest: PathBuf,
}

impl FirmwareBundle {
    /// Extracts a `.tgz` bundle into `destination`, rejecting unsafe paths.
    pub fn extract(archive: &Path, destination: &Path) -> Result<Self, UpdateError> {
        extract_tar_gz(archive, destination)?;

        let manifest = find_file(destination, "update.fuf").ok_or_else(|| {
            UpdateError::Archive(String::from("archive does not contain update.fuf"))
        })?;

        let root = manifest
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| UpdateError::Archive(String::from("invalid update.fuf path")))?;

        Ok(Self { root, manifest })
    }

    /// Lists files of the bundle as paths relative to [`Self::root`].
    pub fn files(&self) -> Result<Vec<PathBuf>, UpdateError> {
        let mut files = Vec::new();
        collect_files(&self.root, &self.root, &mut files)?;
        files.sort();
        Ok(files)
    }
}

/// Extracts a gzip-compressed tar archive, rejecting path traversal and
/// non-regular entries.
pub fn extract_tar_gz(archive: &Path, destination: &Path) -> Result<(), UpdateError> {
    let file = std::fs::File::open(archive)?;
    let decoder = flate2::read::GzDecoder::new(file);
    let mut tar = tar::Archive::new(decoder);

    std::fs::create_dir_all(destination)?;

    for entry in tar.entries().map_err(archive_error)? {
        let mut entry = entry.map_err(archive_error)?;
        let path = entry.path().map_err(archive_error)?.to_path_buf();

        if path.components().any(|component| {
            matches!(
                component,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        }) {
            return Err(UpdateError::UnsafeArchivePath(path.display().to_string()));
        }

        // Update and backup archives only contain regular files and
        // directories. Reject symlinks, hard links and device nodes instead of
        // relying on unpack semantics for them.
        if !matches!(
            entry.header().entry_type(),
            EntryType::Regular | EntryType::Directory
        ) {
            return Err(UpdateError::UnsafeArchivePath(path.display().to_string()));
        }

        if !entry.unpack_in(destination).map_err(archive_error)? {
            return Err(UpdateError::UnsafeArchivePath(path.display().to_string()));
        }
    }

    Ok(())
}

fn find_file(root: &Path, name: &str) -> Option<PathBuf> {
    let entries = std::fs::read_dir(root).ok()?;

    for entry in entries.flatten() {
        let path = entry.path();

        if path.is_dir() {
            if let Some(found) = find_file(&path, name) {
                return Some(found);
            }
        } else if path.file_name().is_some_and(|file_name| file_name == name) {
            return Some(path);
        }
    }

    None
}

fn collect_files(
    root: &Path,
    directory: &Path,
    files: &mut Vec<PathBuf>,
) -> Result<(), UpdateError> {
    for entry in std::fs::read_dir(directory)? {
        let path = entry?.path();

        if path.is_dir() {
            collect_files(root, &path, files)?;
        } else if let Ok(relative) = path.strip_prefix(root) {
            files.push(relative.to_path_buf());
        }
    }

    Ok(())
}

/// Errors raised by the update layer.
#[derive(Debug, thiserror::Error)]
pub enum UpdateError {
    #[error("HTTP error: {0}")]
    Http(#[from] reqwest::Error),

    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("invalid update directory: {0}")]
    InvalidDirectory(String),

    #[error("invalid URL: {0}")]
    InvalidUrl(String),

    #[error("invalid SHA-256 digest: {0}")]
    InvalidDigest(String),

    #[error("checksum mismatch for {file}: expected {expected}, got {actual}")]
    ChecksumMismatch {
        file: String,
        expected: String,
        actual: String,
    },

    #[error("archive error: {0}")]
    Archive(String),

    #[error("archive contains an unsafe path: {0}")]
    UnsafeArchivePath(String),

    #[error("invalid region bundle: {0}")]
    InvalidRegion(String),

    #[error("region service rejected the request ({code}): {text}")]
    RegionRejected { code: i32, text: String },

    #[error(transparent)]
    Domain(#[from] CoreError),
}

fn archive_error(error: std::io::Error) -> UpdateError {
    UpdateError::Archive(error.to_string())
}

// ---------------------------------------------------------------------------
// Wire format
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct WireDirectory {
    channels: Vec<WireChannel>,
}

#[derive(Deserialize)]
struct WireChannel {
    id: String,
    #[serde(default)]
    title: String,
    #[serde(default)]
    description: String,
    #[serde(default)]
    versions: Vec<WireRelease>,
}

#[derive(Deserialize)]
struct WireRelease {
    version: String,
    #[serde(default)]
    changelog: String,
    #[serde(default, deserialize_with = "deserialize_timestamp")]
    timestamp: u64,
    #[serde(default)]
    files: Vec<WireFile>,
}

#[derive(Deserialize)]
struct WireFile {
    #[serde(default)]
    target: String,
    #[serde(rename = "type", default)]
    kind: String,
    url: String,
    #[serde(default)]
    sha256: String,
}

fn deserialize_timestamp<'de, D>(deserializer: D) -> Result<u64, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error;

    #[derive(Deserialize)]
    #[serde(untagged)]
    enum Timestamp {
        Number(u64),
        Text(String),
    }

    match Timestamp::deserialize(deserializer)? {
        Timestamp::Number(value) => Ok(value),
        Timestamp::Text(value) => value.parse().map_err(D::Error::custom),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIRECTORY: &str = r#"{
        "channels": [
            {
                "id": "release",
                "title": "Release",
                "description": "Stable",
                "versions": [
                    {
                        "version": "1.4.3",
                        "changelog": "new",
                        "timestamp": 200,
                        "files": [
                            {"target": "f18", "type": "full_dfu", "url": "https://example.com/1.4.3.dfu", "sha256": "aa"},
                            {"target": "f18", "type": "update_tgz", "url": "https://example.com/1.4.3.tgz", "sha256": "bb"}
                        ]
                    },
                    {
                        "version": "1.4.3-rc2",
                        "changelog": "rc",
                        "timestamp": 100,
                        "files": []
                    },
                    {
                        "version": "1.4.2",
                        "changelog": "old",
                        "timestamp": 50,
                        "files": []
                    }
                ]
            },
            {
                "id": "development",
                "title": "Development",
                "description": "Bleeding edge",
                "versions": [
                    {
                        "version": "3060cee9",
                        "changelog": "dev",
                        "timestamp": 300,
                        "files": []
                    }
                ]
            }
        ]
    }"#;

    #[test]
    fn parses_and_sorts_releases() {
        let directory = Directory::parse(DIRECTORY).unwrap();

        assert_eq!(directory.channel_ids(), vec!["release", "development"]);

        let release = directory.channel("release").unwrap();
        assert_eq!(
            release
                .versions
                .iter()
                .map(|version| version.version.as_str())
                .collect::<Vec<_>>(),
            vec!["1.4.3", "1.4.3-rc2", "1.4.2"]
        );
    }

    #[test]
    fn finds_artifacts_by_type_and_target() {
        let directory = Directory::parse(DIRECTORY).unwrap();
        let latest = directory.latest("release").unwrap();

        let file = latest.file("full_dfu", "f18").unwrap();
        assert_eq!(file.url, "https://example.com/1.4.3.dfu");
        assert_eq!(file.file_name().unwrap(), "1.4.3.dfu");
        assert!(latest.file("full_dfu", "f7").is_none());
    }

    #[test]
    fn rejects_unknown_channel_shapes() {
        assert!(Directory::parse("{}").is_err());
        assert!(Directory::parse(r#"{"channels": []}"#).is_err());
        assert!(Directory::parse(r#"{"channels": [{"id": ""}]}"#).is_err());
    }

    #[test]
    fn accepts_numeric_and_textual_timestamps() {
        let json = r#"{"channels":[{"id":"release","versions":[{"version":"1.0.0","timestamp":"123","files":[]}]}]}"#;
        let directory = Directory::parse(json).unwrap();
        assert_eq!(directory.latest("release").unwrap().timestamp, 123);
    }

    #[test]
    fn validates_digests() {
        let file = FirmwareFile {
            target: String::new(),
            kind: String::new(),
            url: String::new(),
            sha256: "zz".into(),
        };

        assert!(file.expected_digest().is_err());

        let file = FirmwareFile {
            sha256: "00".repeat(32),
            ..file
        };
        assert!(file.expected_digest().unwrap().is_some());
    }

    #[test]
    fn rejects_symlinks_in_archives() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("update.tgz");

        {
            let file = std::fs::File::create(&archive).unwrap();
            let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
            let mut builder = tar::Builder::new(encoder);

            let mut header = tar::Header::new_gnu();
            header.set_entry_type(EntryType::Symlink);
            header.set_size(0);
            header.set_mode(0o777);
            header.set_link_name("/etc/passwd").unwrap();
            header.set_cksum();
            builder
                .append_data(&mut header, "f18/link", &[][..])
                .unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }

        let destination = temp.path().join("out");
        let error = FirmwareBundle::extract(&archive, &destination).unwrap_err();

        assert!(matches!(error, UpdateError::UnsafeArchivePath(_)));
    }

    #[test]
    fn extracts_bundles_and_rejects_traversal() {
        let temp = tempfile::tempdir().unwrap();
        let archive = temp.path().join("update.tgz");

        {
            let file = std::fs::File::create(&archive).unwrap();
            let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::fast());
            let mut builder = tar::Builder::new(encoder);

            let mut header = tar::Header::new_gnu();
            header.set_size(4);
            header.set_mode(0o644);
            header.set_cksum();
            builder
                .append_data(&mut header, "f18/update.fuf", &b"test"[..])
                .unwrap();
            builder.into_inner().unwrap().finish().unwrap();
        }

        let destination = temp.path().join("out");
        let bundle = FirmwareBundle::extract(&archive, &destination).unwrap();

        assert_eq!(bundle.root, destination.join("f18"));
        assert_eq!(bundle.manifest, destination.join("f18/update.fuf"));
        assert_eq!(bundle.files().unwrap(), vec![PathBuf::from("update.fuf")]);
    }
}
