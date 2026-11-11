//! Application services shared by the CLI and the GUI.
//!
//! This crate is the seam between the pure protocol/USB layers and the user
//! interfaces.  It owns the "what" (scan devices, probe them, flash an image,
//! pick a firmware release) while the interfaces only own presentation.

use std::{
    path::{Path, PathBuf},
    time::Duration,
};

use flipper_companion_core::{DeviceInfo, ProtobufVersion, StorageInfo, UsbDevice, UsbMode};
use flipper_companion_dfu::{DfuDevice, DfuError, DfuseFile, FlashProgress, NusbTransport, list_devices};
use flipper_companion_protocol::pb::CommandStatus;
use flipper_companion_protocol::pb_system;
use flipper_companion_protocol::{
    ProtocolError, RpcSession, ScreenFrame, SessionEvent, find_flipper_port, open_serial,
    start_session,
};
use flipper_companion_update::{
    Directory, DirectoryClient, FirmwareBundle, FirmwareFile, Release, UpdateError,
};
use tokio::{sync::broadcast, time::timeout};

mod apps;
mod backup;
mod recovery;
mod region;

pub use apps::{
    AppInfo, AppInstallReport, AppVerification, InstallPhase, InstalledApp, OFFICIAL_CATALOG_URL,
    add_custom_source, app_sources_path, load_custom_sources, remove_custom_source,
};
pub use backup::{BackupReport, RestoreReport};
pub use recovery::{FusAction, RecoveryInfo};
pub use region::RegionReport;

/// How long device probing may take before it is reported as unavailable.
pub const PROBE_TIMEOUT: Duration = Duration::from_secs(5);

/// Default time to wait for a full screen frame.
pub const SCREEN_FRAME_TIMEOUT: Duration = Duration::from_secs(5);

/// Reboot destinations supported by the firmware.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebootTarget {
    Os,
    Dfu,
    Update,
}

impl From<RebootTarget> for pb_system::reboot_request::RebootMode {
    fn from(target: RebootTarget) -> Self {
        match target {
            RebootTarget::Os => Self::Os,
            RebootTarget::Dfu => Self::Dfu,
            RebootTarget::Update => Self::Update,
        }
    }
}

/// A device as presented in a device list.
#[derive(Debug, Clone)]
pub struct DeviceSummary {
    pub usb: UsbDevice,
    pub info: Option<DeviceInfo>,
    pub protobuf: Option<ProtobufVersion>,
    pub storage: Option<StorageInfo>,
    /// Raw `System_power_info` key/value pairs (battery level, charging, ...).
    pub power: Option<Vec<(String, String)>>,
    pub error: Option<String>,
}

impl DeviceSummary {
    #[must_use]
    pub fn display_name(&self) -> String {
        match &self.info {
            Some(info) => info.name.clone(),
            None => match self.usb.mode {
                UsbMode::Dfu => String::from("Flipper Zero (DFU)"),
                UsbMode::VirtualComPort => String::from("Flipper Zero"),
            },
        }
    }

    #[must_use]
    pub fn firmware_label(&self) -> String {
        self.info
            .as_ref()
            .map_or_else(|| String::from("unknown"), DeviceInfo::firmware_label)
    }

    #[must_use]
    pub fn target(&self) -> Option<&str> {
        self.info
            .as_ref()
            .map(|info| info.hardware.target.as_str())
            .filter(|target| !target.is_empty())
    }

    #[must_use]
    pub const fn is_dfu(&self) -> bool {
        matches!(self.usb.mode, UsbMode::Dfu)
    }

    #[must_use]
    pub fn is_probed(&self) -> bool {
        self.info.is_some()
    }
}

/// High-level entry point for applications.
pub struct Companion {
    directory: DirectoryClient,
}

impl Default for Companion {
    fn default() -> Self {
        Self::new()
    }
}

impl Companion {
    #[must_use]
    pub fn new() -> Self {
        Self {
            directory: DirectoryClient::with_default_url(),
        }
    }

    #[must_use]
    pub fn with_directory_url(url: impl Into<String>) -> Self {
        Self {
            directory: DirectoryClient::new(url),
        }
    }

    /// Lists every connected Flipper and probes the ones exposing RPC.
    ///
    /// Probing is best-effort: a device whose serial port is busy is still
    /// listed, with an explanatory error.
    pub async fn scan(&self) -> Result<Vec<DeviceSummary>, AppError> {
        let devices = tokio::task::spawn_blocking(list_devices)
            .await
            .map_err(|error| AppError::Join(error.to_string()))??;

        tracing::info!("usb: {} device(s) found", devices.len());

        let mut summaries = Vec::with_capacity(devices.len());

        for device in devices {
            if device.mode == UsbMode::Dfu {
                tracing::info!(
                    "usb: {} is in DFU mode",
                    device.serial_number.as_deref().unwrap_or("unknown device")
                );
                summaries.push(DeviceSummary {
                    usb: device,
                    info: None,
                    protobuf: None,
                    storage: None,
                    power: None,
                    error: None,
                });
                continue;
            }

            match self.probe(&device).await {
                Ok((info, protobuf, storage, power)) => summaries.push(DeviceSummary {
                    usb: device,
                    info: Some(info),
                    protobuf: Some(protobuf),
                    storage,
                    power,
                    error: None,
                }),
                Err(error) => summaries.push(DeviceSummary {
                    usb: device,
                    info: None,
                    protobuf: None,
                    storage: None,
                    power: None,
                    error: Some(error.to_string()),
                }),
            }
        }

        Ok(summaries)
    }

    async fn probe(
        &self,
        device: &UsbDevice,
    ) -> Result<
        (
            DeviceInfo,
            ProtobufVersion,
            Option<StorageInfo>,
            Option<Vec<(String, String)>>,
        ),
        AppError,
    > {
        let probe = async {
            let session = self.connect(device.serial_number.as_deref()).await?;
            let protobuf = session.protobuf_version().await?;
            let info = if protobuf.has_property_device_info() {
                session.device_info().await?
            } else {
                session.device_info_legacy().await?
            };

            tracing::info!(
                "device: {} target={} api={} protobuf={}.{}",
                info.name,
                info.hardware.target,
                info.firmware
                    .api_version
                    .map_or_else(|| String::from("unknown"), |version| version.to_string()),
                protobuf.major,
                protobuf.minor
            );

            let storage = session.storage_info("/ext").await.ok();
            let power = session.system_power_info().await.ok();
            session.stop().await;

            Ok((info, protobuf, storage, power))
        };

        match timeout(PROBE_TIMEOUT, probe).await {
            Ok(result) => result,
            Err(_) => Err(AppError::ProbeTimeout),
        }
    }

    /// Opens an RPC session on the first Flipper or on a specific serial.
    pub async fn connect(&self, serial: Option<&str>) -> Result<RpcSession, AppError> {
        let port = find_flipper_port(serial)?;
        tracing::debug!("session: opening {}", port.path);

        let stream = open_serial(&port.path)?;
        let session = start_session(stream).await?;

        tracing::debug!("session: started on {}", port.path);

        Ok(session)
    }

    /// Reboots a device into the requested mode.
    pub async fn reboot(&self, serial: Option<&str>, target: RebootTarget) -> Result<(), AppError> {
        let session = self.connect(serial).await?;
        session.reboot(target.into()).await?;
        Ok(())
    }

    /// Flashes a `.dfu` image over USB.
    ///
    /// The blocking USB work runs on a dedicated thread, so callers can keep
    /// their UI or async runtime responsive.
    pub async fn flash_dfu<F>(
        &self,
        serial: Option<String>,
        image: PathBuf,
        on_progress: F,
    ) -> Result<(), AppError>
    where
        F: FnMut(FlashProgress) + Send + 'static,
    {
        tokio::task::spawn_blocking(move || {
            tracing::info!("dfu: flashing {}", image.display());
            let image = DfuseFile::from_path(&image)?;
            let devices = list_devices()?;

            let device = devices
                .into_iter()
                .find(|device| {
                    device.is_dfu()
                        && serial
                            .as_deref()
                            .is_none_or(|serial| device.serial_number.as_deref() == Some(serial))
                })
                .ok_or(AppError::NoDfuDevice)?;

            let mut transport = NusbTransport::open(&device)?;
            DfuDevice::new(&mut transport).flash(&image, on_progress)?;

            Ok::<_, AppError>(())
        })
        .await
        .map_err(|error| AppError::Join(error.to_string()))?
    }

    /// Fetches the firmware update directory.
    pub async fn firmware_directory(&self) -> Result<Directory, AppError> {
        Ok(self.directory.load().await?)
    }

    /// Downloads the newest artifact of a given kind for a device target.
    pub async fn download_latest(
        &self,
        channel: &str,
        kind: &str,
        target: &str,
        destination: &Path,
        on_progress: impl FnMut(flipper_companion_core::Progress),
    ) -> Result<(Release, PathBuf), AppError> {
        let directory = self.firmware_directory().await?;
        let release = select_release(&directory, channel, kind, target)?.clone();
        let file = release
            .file(kind, target)
            .ok_or_else(|| AppError::MissingArtifact {
                channel: channel.to_owned(),
                kind: kind.to_owned(),
                target: target.to_owned(),
            })?;

        std::fs::create_dir_all(destination)?;
        let path = self
            .directory
            .download_to(file, destination, on_progress)
            .await?;

        Ok((release, path))
    }

    /// Downloads one specific artifact into a directory.
    pub async fn download_artifact(
        &self,
        file: &FirmwareFile,
        directory: &Path,
        on_progress: impl FnMut(flipper_companion_core::Progress),
    ) -> Result<PathBuf, AppError> {
        std::fs::create_dir_all(directory)?;
        Ok(self
            .directory
            .download_to(file, directory, on_progress)
            .await?)
    }

    /// Uploads an extracted update bundle and asks the firmware to install it.
    ///
    /// The bundle files are stored under `/ext/update/<bundle>/`, existing
    /// files with a matching MD5 are skipped, and uploaded files are verified
    /// before `System_update` is called.
    pub async fn install_bundle<F>(
        &self,
        serial: Option<&str>,
        bundle: &FirmwareBundle,
        mut on_progress: F,
    ) -> Result<InstallReport, AppError>
    where
        F: FnMut(flipper_companion_core::Progress),
    {
        let session = self.connect(serial).await?;

        let result = async {
            let directory_name = bundle
                .root
                .file_name()
                .and_then(|name| name.to_str())
                .ok_or_else(|| AppError::Bundle(String::from("bundle root has no usable name")))?;
            let remote_root = format!("/ext/update/{directory_name}");

            ensure_remote_directory(&session, &remote_root).await?;

            let files = bundle.files()?;
            let total_bytes = files.iter().try_fold(0u64, |total, relative| {
                let metadata = std::fs::metadata(bundle.root.join(relative))?;
                Ok::<_, std::io::Error>(total + metadata.len())
            })?;

            let mut done = 0u64;
            let mut uploaded = 0usize;

            for relative in &files {
                let local = bundle.root.join(relative);
                let relative_remote = relative.to_string_lossy().replace('\\', "/");
                let remote = format!("{remote_root}/{relative_remote}");

                if let Some(parent) = relative
                    .parent()
                    .filter(|parent| !parent.as_os_str().is_empty())
                {
                    let parent_remote = format!(
                        "{remote_root}/{}",
                        parent.to_string_lossy().replace('\\', "/")
                    );
                    ensure_remote_directory(&session, &parent_remote).await?;
                }

                let data = tokio::fs::read(&local).await?;
                let local_md5 = md5_hex(&data);
                let remote_md5 = session.storage_md5sum(&remote).await.ok();

                if remote_md5.as_deref() != Some(local_md5.as_str()) {
                    session.storage_write(&remote, &data).await?;

                    let verified = session.storage_md5sum(&remote).await?;
                    if verified != local_md5 {
                        return Err(AppError::UploadVerification {
                            path: remote,
                            expected: local_md5,
                            actual: verified,
                        });
                    }

                    uploaded += 1;
                }

                done += data.len() as u64;
                on_progress(flipper_companion_core::Progress::ratio(done, total_bytes));
            }

            let manifest = format!("{remote_root}/update.fuf");
            let status = session.system_update(&manifest).await?;

            if status != pb_system::update_response::UpdateResultCode::Ok {
                return Err(AppError::UpdateRejected(status.as_str_name().to_owned()));
            }

            Ok(InstallReport {
                files_total: files.len(),
                files_uploaded: uploaded,
                bytes_uploaded: total_bytes,
            })
        }
        .await;

        let _ = session.stop().await;
        result
    }

    /// Captures one screen frame from a running device.
    pub async fn screen_frame(
        &self,
        serial: Option<&str>,
        wait: Duration,
    ) -> Result<ScreenFrame, AppError> {
        let session = self.connect(serial).await?;
        let mut events = session.subscribe();

        session.start_screen_stream().await?;

        let frame = timeout(wait, async {
            loop {
                match events.recv().await {
                    Ok(SessionEvent::Broadcast(message)) => {
                        if let Some(frame) = ScreenFrame::from_message(&message) {
                            return Ok(frame);
                        }
                    }
                    Ok(SessionEvent::Closed(reason)) => {
                        return Err(AppError::Protocol(ProtocolError::Closed(
                            reason.unwrap_or_else(|| String::from("device disconnected")),
                        )));
                    }
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => {
                        return Err(AppError::ScreenTimeout);
                    }
                }
            }
        })
        .await
        .map_err(|_| AppError::ScreenTimeout)??;

        let _ = session.stop_screen_stream().await;
        session.stop().await;

        Ok(frame)
    }
}

/// Selects the newest release that actually ships `kind` for `target`.
pub fn select_release<'a>(
    directory: &'a Directory,
    channel: &str,
    kind: &str,
    target: &str,
) -> Result<&'a Release, AppError> {
    directory
        .channel(channel)
        .and_then(|channel| {
            channel
                .versions
                .iter()
                .find(|release| release.file(kind, target).is_some())
        })
        .ok_or_else(|| AppError::MissingArtifact {
            channel: channel.to_owned(),
            kind: kind.to_owned(),
            target: target.to_owned(),
        })
}

/// Picks the artifact matching a device, preferring its hardware target.
#[must_use]
pub fn artifact_for_target<'a>(
    release: &'a Release,
    kind: &str,
    target: &str,
) -> Option<&'a FirmwareFile> {
    release
        .file(kind, target)
        .or_else(|| release.file(kind, "any"))
}

/// Result of installing an update bundle on the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct InstallReport {
    pub files_total: usize,
    pub files_uploaded: usize,
    pub bytes_uploaded: u64,
}

/// Errors raised by application services.
#[derive(Debug, thiserror::Error)]
pub enum AppError {
    #[error(transparent)]
    Protocol(#[from] ProtocolError),

    #[error(transparent)]
    Dfu(#[from] DfuError),

    #[error(transparent)]
    Update(#[from] UpdateError),

    #[error(transparent)]
    Io(#[from] std::io::Error),

    #[error("no Flipper Zero in DFU mode is connected")]
    NoDfuDevice,

    #[error("device probing timed out")]
    ProbeTimeout,

    #[error("no screen frame arrived in time")]
    ScreenTimeout,

    #[error("no `{kind}` artifact for target `{target}` in channel `{channel}`")]
    MissingArtifact {
        channel: String,
        kind: String,
        target: String,
    },

    #[error("network error: {0}")]
    Network(#[from] reqwest::Error),

    #[error("app catalog error: {0}")]
    App(String),

    #[error("update bundle error: {0}")]
    Bundle(String),

    #[error("backup entry has an unsafe name: {0}")]
    InvalidBackupEntry(String),

    #[error("backup is too large or too deep")]
    BackupTooLarge,

    #[error("backup does not contain an `int/` directory")]
    MissingBackupRoot,

    #[error("remote path is not a directory: {0}")]
    NotADirectory(String),

    #[error("device rejected the update: {0}")]
    UpdateRejected(String),

    #[error("checksum mismatch after uploading `{path}`: expected {expected}, got {actual}")]
    UploadVerification {
        path: String,
        expected: String,
        actual: String,
    },

    #[error("background task failed: {0}")]
    Join(String),
}

/// Creates a directory hierarchy on the device, tolerating existing paths.
async fn ensure_remote_directory(session: &RpcSession, path: &str) -> Result<(), AppError> {
    let mut current = String::new();

    for component in path.split('/').filter(|component| !component.is_empty()) {
        current.push('/');
        current.push_str(component);

        // `/ext` is the SD mount point and is always present when an SD card
        // is inserted; probing it would only add latency.
        if current == "/ext" {
            continue;
        }

        match session.storage_stat(&current).await {
            Ok(Some(entry)) if entry.is_directory() => {}
            Ok(Some(_)) => return Err(AppError::NotADirectory(current)),
            Ok(None) => create_remote_directory(session, &current).await?,
            Err(ProtocolError::Device {
                status: CommandStatus::ErrorStorageNotExist,
                ..
            }) => create_remote_directory(session, &current).await?,
            Err(error) => return Err(error.into()),
        }
    }

    Ok(())
}

async fn create_remote_directory(session: &RpcSession, path: &str) -> Result<(), AppError> {
    if let Err(error) = session.storage_mkdir(path).await {
        // Another client (or a previous run) may have created it between the
        // stat and the mkdir; accept the path if it is now a directory.
        match session.storage_stat(path).await {
            Ok(Some(entry)) if entry.is_directory() => return Ok(()),
            _ => return Err(error.into()),
        }
    }

    Ok(())
}

fn md5_hex(data: &[u8]) -> String {
    use md5::{Digest, Md5};

    let mut hasher = Md5::new();
    hasher.update(data);
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    const DIRECTORY: &str = r#"{
        "channels": [{
            "id": "release",
            "versions": [
                {"version": "1.4.3", "timestamp": 300, "files": [
                    {"target": "f18", "type": "full_dfu", "url": "https://example.com/f18.dfu"}
                ]},
                {"version": "1.4.2", "timestamp": 200, "files": [
                    {"target": "f7", "type": "full_dfu", "url": "https://example.com/f7.dfu"}
                ]}
            ]
        }]
    }"#;

    #[test]
    fn selects_the_newest_release_with_a_matching_artifact() {
        let directory = Directory::parse(DIRECTORY).unwrap();

        let release = select_release(&directory, "release", "full_dfu", "f18").unwrap();
        assert_eq!(release.version, "1.4.3");

        let release = select_release(&directory, "release", "full_dfu", "f7").unwrap();
        assert_eq!(release.version, "1.4.2");

        assert!(select_release(&directory, "release", "full_dfu", "f0").is_err());
        assert!(select_release(&directory, "unknown", "full_dfu", "f18").is_err());
    }

    #[test]
    fn computes_md5_digests() {
        assert_eq!(md5_hex(b"hello"), "5d41402abc4b2a76b9719d911017c592");
        assert_eq!(md5_hex(b""), "d41d8cd98f00b204e9800998ecf8427e");
    }

    #[test]
    fn prefers_a_target_specific_artifact() {
        let directory = Directory::parse(DIRECTORY).unwrap();
        let release = directory.latest("release").unwrap();

        let file = artifact_for_target(release, "full_dfu", "f18").unwrap();
        assert_eq!(file.target, "f18");
    }
}
