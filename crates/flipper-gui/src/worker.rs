//! Background worker: owns the Tokio runtime and all I/O.

use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
    thread::JoinHandle,
    time::{Duration, Instant},
};

use flipper_companion_app::{AppInfo, Companion, DeviceSummary, add_custom_source, select_release};
use flipper_companion_core::{BuildDate, StorageInfo, UsbMode};
use flipper_companion_protocol::{ScreenFrame, SessionEvent, pb_gui};
use flipper_companion_update::FirmwareBundle;
use tokio::{
    sync::{
        mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel},
        oneshot,
    },
    task::JoinHandle as TokioJoinHandle,
};

/// Commands sent from the UI to the worker.
#[derive(Debug, Clone)]
pub enum Command {
    Refresh,
    Flash {
        path: PathBuf,
        serial: Option<String>,
    },
    CheckUpdates {
        channel: String,
    },
    Install {
        channel: String,
        serial: Option<String>,
    },
    Backup {
        serial: Option<String>,
        archive: PathBuf,
    },
    Restore {
        serial: Option<String>,
        archive: PathBuf,
    },
    FactoryReset {
        serial: Option<String>,
    },
    Alert {
        serial: Option<String>,
    },
    StartStream {
        serial: Option<String>,
    },
    StopStream,
    Input {
        key: i32,
    },
    ListFiles {
        serial: Option<String>,
        path: String,
    },
    InspectFile {
        serial: Option<String>,
        remote: String,
    },
    FileMd5 {
        serial: Option<String>,
        remote: String,
    },
    LoadApps {
        serial: Option<String>,
        query: String,
        source: Option<String>,
    },
    RemoveAppSource {
        url: String,
    },
    CheckAppUpdates {
        serial: Option<String>,
    },
    VerifyApp {
        serial: Option<String>,
        remote: String,
        app_id: String,
    },
    ReinstallApp {
        serial: Option<String>,
        remote: String,
        app_id: String,
    },
    UpdateApps {
        serial: Option<String>,
        app_ids: Vec<String>,
    },
    InstallApp {
        serial: Option<String>,
        app_id: String,
    },
    UninstallApp {
        serial: Option<String>,
        path: String,
    },
    AddAppSource {
        url: String,
    },
    DownloadFile {
        serial: Option<String>,
        remote: String,
        local: PathBuf,
    },
    UploadFile {
        serial: Option<String>,
        local: PathBuf,
        remote: String,
    },
    DeleteFile {
        serial: Option<String>,
        remote: String,
    },
    MakeDir {
        serial: Option<String>,
        path: String,
    },
    RenameFile {
        serial: Option<String>,
        from: String,
        to: String,
    },
    Shutdown,
}

/// Events sent from the worker back to the UI.
#[derive(Debug, Clone)]
pub enum Event {
    Devices(Vec<DeviceView>),
    Files(Vec<FileView>),
    FileProperties(FilePropertiesView),
    FileMd5 {
        remote: String,
        md5: Option<String>,
        error: Option<String>,
    },
    AppEntries(Vec<AppEntryView>),
    Sources(Vec<SourceView>),

    AppVerified {
        remote: String,
        matches: bool,
        detail: String,
        error: Option<String>,
    },
    Busy(bool),
    Progress(f32),
    /// Operation currently driving the progress value ("firmware", "apps", ...).
    Task(String),
    Status(String),
    Latest(String),
    Log(String),
    Screen(Vec<u8>),
    Streaming(bool),
}

/// Rendering-friendly snapshot of one device.
#[derive(Debug, Clone)]
pub struct DeviceView {
    pub title: String,
    pub subtitle: String,
    pub detail: String,
    pub serial: Option<String>,
}

/// Detailed metadata for the selected file or directory.
#[derive(Debug, Clone)]
pub struct FilePropertiesView {
    pub remote: String,
    pub kind: String,
    pub size_human: String,
    pub size_bytes: u64,
    pub modified: Option<String>,
    pub entries: Option<usize>,
    pub error: Option<String>,
}

/// One selectable application repository.
#[derive(Debug, Clone)]
pub struct SourceView {
    pub id: String,
    pub label: String,
}

/// State of one application, shared by the catalog and installed views.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AppStateView {
    /// Present in the repository only.
    NotInstalled,
    /// On the SD card, freshness not checked yet.
    Installed,
    /// The installed file differs from the published build.
    UpdateAvailable,
    /// The installed file matches the published build.
    UpToDate,
}

/// One application, built by merging the repository and the SD card.
///
/// Both list views render this same data, so they always agree.
#[derive(Debug, Clone)]
pub struct AppEntryView {
    /// Catalog id, empty when the app is not in the selected repository.
    pub id: String,
    pub name: String,
    /// Version published by the repository.
    pub version: Option<String>,
    pub catalog_size: Option<u64>,
    pub installed_size: Option<u64>,
    pub remote_path: Option<String>,
    pub source: String,
    pub description: String,
    pub state: AppStateView,
}

/// Everything the app views are built from.
#[derive(Debug, Default)]
struct AppsState {
    catalog: Vec<AppInfo>,
    installed: Vec<flipper_companion_app::InstalledApp>,
    /// Catalog ids whose published size differs from the installed file.
    updates: std::collections::HashSet<String>,
    /// Catalog ids verified by hash: `true` = up to date.
    verified: std::collections::HashMap<String, bool>,
}

impl AppsState {
    /// Merges the repository and the SD card into a single ordered list.
    fn entries(&self) -> Vec<AppEntryView> {
        let mut entries = Vec::new();
        let mut matched = vec![false; self.installed.len()];

        for app in &self.catalog {
            let index = self.installed.iter().position(|candidate| {
                candidate.name.eq_ignore_ascii_case(&app.alias)
                    && candidate.category.eq_ignore_ascii_case(&app.category)
            });

            let (installed_size, remote_path) = match index {
                Some(index) => {
                    matched[index] = true;
                    (
                        Some(self.installed[index].size_bytes),
                        Some(self.installed[index].path.clone()),
                    )
                }
                None => (None, None),
            };

            let state = match installed_size {
                None => AppStateView::NotInstalled,
                Some(_) => match self.verified.get(&app.id) {
                    Some(true) => AppStateView::UpToDate,
                    Some(false) => AppStateView::UpdateAvailable,
                    None if self.updates.contains(&app.id) => AppStateView::UpdateAvailable,
                    None => AppStateView::Installed,
                },
            };

            entries.push(AppEntryView {
                id: app.id.clone(),
                name: app.name.clone(),
                version: Some(app.version.clone()),
                catalog_size: app.size_bytes,
                installed_size,
                remote_path,
                source: app.source.clone(),
                description: format!(
                    "{} — by {}\n\n{}",
                    app.name,
                    if app.author.is_empty() {
                        "unknown author"
                    } else {
                        &app.author
                    },
                    app.description
                ),
                state,
            });
        }

        for (index, app) in self.installed.iter().enumerate() {
            if matched[index] {
                continue;
            }

            entries.push(AppEntryView {
                id: String::new(),
                name: app.name.clone(),
                version: None,
                catalog_size: None,
                installed_size: Some(app.size_bytes),
                remote_path: Some(app.path.clone()),
                source: String::new(),
                description: format!("{}\n\nnot present in the selected repository", app.path),
                state: AppStateView::Installed,
            });
        }

        entries.sort_by_key(|entry| entry.name.to_lowercase());
        entries
    }
}

/// Rendering-friendly snapshot of one remote file.
#[derive(Debug, Clone)]
pub struct FileView {
    pub name: String,
    pub kind: String,
    pub size: String,
    pub is_dir: bool,
    /// Full device path of this entry, captured when the listing was made.
    pub remote: String,
}

/// Handle to the worker thread.
pub struct Worker {
    commands: UnboundedSender<Command>,
    events: Option<UnboundedReceiver<Event>>,
    _thread: JoinHandle<()>,
}

impl Worker {
    pub fn spawn() -> Self {
        let (commands, command_rx) = unbounded_channel();
        let (event_tx, events) = unbounded_channel();

        let thread = std::thread::Builder::new()
            .name(String::from("flipper-worker"))
            .spawn(move || run(command_rx, event_tx))
            .expect("cannot spawn the worker thread");

        Self {
            commands,
            events: Some(events),
            _thread: thread,
        }
    }

    pub fn sender(&self) -> UnboundedSender<Command> {
        self.commands.clone()
    }

    pub fn receiver(&mut self) -> UnboundedReceiver<Event> {
        self.events
            .take()
            .expect("the event receiver can only be taken once")
    }
}

struct StreamHandle {
    cancel: Option<oneshot::Sender<()>>,
    inputs: UnboundedSender<i32>,
    task: TokioJoinHandle<()>,
}

impl StreamHandle {
    async fn stop(mut self) {
        if let Some(cancel) = self.cancel.take() {
            let _ = cancel.send(());
        }

        let _ = tokio::time::timeout(Duration::from_secs(2), self.task).await;
    }
}

fn run(mut commands: UnboundedReceiver<Command>, events: UnboundedSender<Event>) {
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("cannot create the Tokio runtime");

    runtime.block_on(async move {
        let companion = Companion::new();
        let mut stream: Option<StreamHandle> = None;
        let mut apps = AppsState::default();

        let _ = events.send(Event::Sources(source_views()));

        while let Some(command) = commands.recv().await {
            match command {
                Command::Refresh => refresh(&companion, &events).await,
                Command::Flash { path, serial } => {
                    flash(&companion, path, serial, &events).await;
                }
                Command::CheckUpdates { channel } => {
                    check_updates(&companion, &channel, &events).await;
                }
                Command::Install { channel, serial } => {
                    install_update(&companion, &channel, serial, &events).await;
                }
                Command::Backup { serial, archive } => {
                    backup(&companion, serial, archive, &events).await;
                }
                Command::Restore { serial, archive } => {
                    restore(&companion, serial, archive, &events).await;
                }
                Command::FactoryReset { serial } => {
                    factory_reset(&companion, serial, &events).await;
                }
                Command::Alert { serial } => {
                    alert(&companion, serial, &events).await;
                }
                Command::StartStream { serial } => {
                    if let Some(handle) = stream.take() {
                        handle.stop().await;
                    }
                    stream = Some(start_stream(serial, events.clone()));
                }
                Command::StopStream => {
                    if let Some(handle) = stream.take() {
                        handle.stop().await;
                    }
                }
                Command::Input { key } => {
                    if let Some(handle) = &stream {
                        let _ = handle.inputs.send(key);
                    }
                }
                Command::ListFiles { serial, path } => {
                    list_files(&companion, serial, &path, &events).await;
                }
                Command::InspectFile { serial, remote } => {
                    inspect_file(&companion, serial, &remote, &events).await;
                }
                Command::FileMd5 { serial, remote } => {
                    file_md5(&companion, serial, &remote, &events).await;
                }
                Command::LoadApps {
                    serial,
                    query,
                    source,
                } => {
                    let _ = events.send(Event::Busy(true));
                    let _ = events.send(Event::Status(String::from("Loading the app catalog...")));

                    match load_catalog(&companion, serial.clone(), &query, source.as_deref()).await
                    {
                        Ok(catalog) => {
                            apps.catalog = catalog;

                            // The installed list is part of the same model.
                            if let Ok(installed) = companion.installed_apps(serial.as_deref()).await
                            {
                                apps.installed = installed;
                            }

                            publish_entries(&events, &apps);
                            let _ = events.send(Event::Status(format!(
                                "{} app(s) available",
                                apps.catalog.len()
                            )));
                        }
                        Err(error) => {
                            let _ = events.send(Event::Log(format!("apps: {error}")));
                            let _ = events.send(Event::Status(String::from("App catalog failed")));
                        }
                    }

                    let _ = events.send(Event::Busy(false));
                }
                Command::InstallApp { serial, app_id } => {
                    if let Some(app) = apps.catalog.iter().find(|app| app.id == app_id).cloned() {
                        install_app(&companion, serial, &app, &events, true).await;

                        apps.verified.remove(&app_id);
                        apps.updates.remove(&app_id);
                        if let Ok(installed) = companion.installed_apps(None).await {
                            apps.installed = installed;
                        }
                        publish_entries(&events, &apps);
                    }
                }
                Command::UninstallApp { serial, path } => {
                    uninstall_app(&companion, serial.clone(), &path, &events).await;

                    if let Ok(installed) = companion.installed_apps(serial.as_deref()).await {
                        apps.installed = installed;
                    }
                    publish_entries(&events, &apps);
                }
                Command::AddAppSource { url } => match add_custom_source(&url) {
                    Ok(()) => {
                        let _ = events.send(Event::Log(format!("added app source {url}")));
                        let _ = events.send(Event::Sources(source_views()));
                        reload_apps(&companion, &mut apps, &events).await;
                    }
                    Err(error) => {
                        let _ = events.send(Event::Log(format!("app source: {error}")));
                    }
                },
                Command::CheckAppUpdates { serial } => {
                    check_app_updates(&companion, serial, &mut apps, &events).await;
                }
                Command::VerifyApp {
                    serial,
                    remote,
                    app_id,
                } => {
                    verify_app(&companion, serial, &remote, &app_id, &mut apps, &events).await;
                }
                Command::UpdateApps { serial, app_ids } => {
                    let total = app_ids.len();
                    let _ = events.send(Event::Busy(true));

                    for (index, id) in app_ids.iter().enumerate() {
                        let Some(app) = apps.catalog.iter().find(|app| &app.id == id).cloned()
                        else {
                            continue;
                        };

                        let _ = events.send(Event::Status(format!(
                            "Updating {}/{}: {}",
                            index + 1,
                            total,
                            app.name
                        )));

                        install_app(&companion, serial.clone(), &app, &events, false).await;

                        apps.verified.remove(id);
                        apps.updates.remove(id);
                    }

                    if let Ok(installed) = companion.installed_apps(serial.as_deref()).await {
                        apps.installed = installed;
                    }

                    publish_entries(&events, &apps);
                    let _ = events.send(Event::Status(format!("{total} app(s) updated")));
                    let _ = events.send(Event::Busy(false));
                }
                Command::ReinstallApp {
                    serial,
                    remote,
                    app_id,
                } => {
                    if let Some(app) = apps.catalog.iter().find(|app| app.id == app_id).cloned() {
                        let _ = events.send(Event::Busy(true));
                        let _ = events.send(Event::Status(format!("Reinstalling {}...", app.name)));

                        if let Err(error) =
                            companion.uninstall_app(serial.as_deref(), &remote).await
                        {
                            let _ = events.send(Event::Log(format!("reinstall: {error}")));
                        }

                        install_app(&companion, serial.clone(), &app, &events, true).await;

                        apps.verified.remove(&app_id);
                        apps.updates.remove(&app_id);

                        if let Ok(installed) = companion.installed_apps(serial.as_deref()).await {
                            apps.installed = installed;
                        }

                        publish_entries(&events, &apps);
                        let _ = events.send(Event::Busy(false));
                    }
                }
                Command::RemoveAppSource { url } => {
                    match flipper_companion_app::remove_custom_source(&url) {
                        Ok(true) => {
                            let _ = events.send(Event::Log(format!("removed app source {url}")));
                            let _ = events.send(Event::Status(String::from("App source removed")));
                        }
                        Ok(false) => {
                            let _ = events
                                .send(Event::Status(String::from("This source is not configured")));
                        }
                        Err(error) => {
                            let _ = events.send(Event::Log(format!("app source: {error}")));
                        }
                    }

                    let _ = events.send(Event::Sources(source_views()));
                    reload_apps(&companion, &mut apps, &events).await;
                }
                Command::DownloadFile {
                    serial,
                    remote,
                    local,
                } => {
                    download_file(&companion, serial, &remote, &local, &events).await;
                }
                Command::UploadFile {
                    serial,
                    local,
                    remote,
                } => {
                    upload_file(&companion, serial, &local, &remote, &events).await;
                }
                Command::DeleteFile { serial, remote } => {
                    delete_file(&companion, serial, &remote, &events).await;
                }
                Command::MakeDir { serial, path } => {
                    make_dir(&companion, serial, &path, &events).await;
                }
                Command::RenameFile { serial, from, to } => {
                    rename_file(&companion, serial, &from, &to, &events).await;
                }
                Command::Shutdown => {
                    if let Some(handle) = stream.take() {
                        handle.stop().await;
                    }
                    break;
                }
            }
        }
    });
}

async fn refresh(companion: &Companion, events: &UnboundedSender<Event>) {
    let _ = events.send(Event::Busy(true));
    let _ = events.send(Event::Status(String::from("Scanning USB devices...")));

    match companion.scan().await {
        Ok(devices) => {
            let serial = devices
                .iter()
                .find(|device| !device.is_dfu())
                .and_then(|device| device.usb.serial_number.clone());

            let views = devices.into_iter().map(device_view).collect();
            let _ = events.send(Event::Devices(views));

            if let Some(serial) = serial {
                list_files(companion, Some(serial), "/ext", events).await;
            }
        }
        Err(error) => {
            let _ = events.send(Event::Log(format!("scan failed: {error}")));
            let _ = events.send(Event::Status(String::from("USB scan failed")));
        }
    }

    let _ = events.send(Event::Busy(false));
}

async fn flash(
    companion: &Companion,
    path: PathBuf,
    serial: Option<String>,
    events: &UnboundedSender<Event>,
) {
    let _ = events.send(Event::Task(String::from("firmware")));
    if !path.is_file() {
        let _ = events.send(Event::Log(format!(
            "flash: `{}` does not exist",
            path.display()
        )));
        return;
    }

    let devices = match tokio::task::spawn_blocking(flipper_companion_dfu::list_devices).await {
        Ok(Ok(devices)) => devices,
        Ok(Err(error)) => {
            let _ = events.send(Event::Log(format!("USB discovery failed: {error}")));
            return;
        }
        Err(error) => {
            let _ = events.send(Event::Log(format!("USB task failed: {error}")));
            return;
        }
    };

    let device = devices
        .iter()
        .find(|device| {
            device.is_dfu()
                && serial
                    .as_deref()
                    .is_none_or(|serial| device.serial_number.as_deref() == Some(serial))
        })
        .or_else(|| devices.iter().find(|device| device.is_dfu()));

    let Some(device) = device else {
        let _ = events.send(Event::Log(String::from(
            "flash: no Flipper in DFU mode; reboot it into DFU first",
        )));
        let _ = events.send(Event::Status(String::from(
            "No DFU device — reboot into DFU mode",
        )));
        return;
    };

    let serial = device.serial_number.clone();
    let _ = events.send(Event::Busy(true));
    let _ = events.send(Event::Progress(0.0));
    let _ = events.send(Event::Status(format!("Flashing {}", path.display())));

    let sender = events.clone();
    let mut last_percent = -1;

    let result = companion
        .flash_dfu(serial, path, move |progress| {
            let fraction = match progress.stage {
                flipper_companion_dfu::FlashStage::Erasing => progress.fraction() * 0.5,
                flipper_companion_dfu::FlashStage::Writing => 0.5 + progress.fraction() * 0.5,
            };

            let percent = (fraction * 100.0) as i32;

            if percent != last_percent {
                last_percent = percent;
                let _ = sender.send(Event::Progress(fraction));
            }
        })
        .await;

    match result {
        Ok(()) => {
            let _ = events.send(Event::Log(String::from(
                "flash: firmware written successfully",
            )));
            let _ = events.send(Event::Status(String::from("Firmware flashed successfully")));
        }
        Err(error) => {
            let _ = events.send(Event::Log(format!("flash failed: {error}")));
            let _ = events.send(Event::Status(String::from("Firmware flashing failed")));
        }
    }

    let _ = events.send(Event::Busy(false));
}

async fn backup(
    companion: &Companion,
    serial: Option<String>,
    archive: PathBuf,
    events: &UnboundedSender<Event>,
) {
    let _ = events.send(Event::Task(String::from("backup")));
    let _ = events.send(Event::Busy(true));
    let _ = events.send(Event::Status(String::from(
        "Backing up internal storage...",
    )));

    let sender = events.clone();
    let mut last_percent = -1;

    let result = companion
        .backup_internal_storage(serial.as_deref(), &archive, move |progress| {
            if let Some(fraction) = progress.fraction_value() {
                let percent = (fraction * 100.0) as i32;
                if percent != last_percent {
                    last_percent = percent;
                    let _ = sender.send(Event::Progress(fraction));
                }
            }
        })
        .await;

    match result {
        Ok(report) => {
            let _ = events.send(Event::Log(format!(
                "backup: {} file(s), {} directorie(s), {} -> {}",
                report.files,
                report.directories,
                human_bytes(report.bytes),
                archive.display()
            )));
            let _ = events.send(Event::Status(String::from("Backup complete")));
        }
        Err(error) => {
            let _ = events.send(Event::Log(format!("backup failed: {error}")));
            let _ = events.send(Event::Status(String::from("Backup failed")));
        }
    }

    let _ = events.send(Event::Busy(false));
}

async fn restore(
    companion: &Companion,
    serial: Option<String>,
    archive: PathBuf,
    events: &UnboundedSender<Event>,
) {
    let _ = events.send(Event::Task(String::from("restore")));
    let _ = events.send(Event::Busy(true));
    let _ = events.send(Event::Status(String::from("Restoring internal storage...")));

    let sender = events.clone();
    let mut last_percent = -1;

    let result = companion
        .restore_internal_storage(serial.as_deref(), &archive, move |progress| {
            if let Some(fraction) = progress.fraction_value() {
                let percent = (fraction * 100.0) as i32;
                if percent != last_percent {
                    last_percent = percent;
                    let _ = sender.send(Event::Progress(fraction));
                }
            }
        })
        .await;

    match result {
        Ok(report) => {
            let _ = events.send(Event::Log(format!(
                "restore: {} file(s), {} directorie(s), {}",
                report.files,
                report.directories,
                human_bytes(report.bytes)
            )));
            let _ = events.send(Event::Status(String::from("Restore complete")));
        }
        Err(error) => {
            let _ = events.send(Event::Log(format!("restore failed: {error}")));
            let _ = events.send(Event::Status(String::from("Restore failed")));
        }
    }

    let _ = events.send(Event::Busy(false));
}

async fn factory_reset(
    companion: &Companion,
    serial: Option<String>,
    events: &UnboundedSender<Event>,
) {
    let _ = events.send(Event::Busy(true));
    let _ = events.send(Event::Status(String::from("Factory resetting...")));

    match companion.factory_reset(serial.as_deref()).await {
        Ok(()) => {
            let _ = events.send(Event::Log(String::from(
                "factory reset sent; the device is rebooting",
            )));
            let _ = events.send(Event::Status(String::from("Factory reset sent")));
        }
        Err(error) => {
            let _ = events.send(Event::Log(format!("factory reset failed: {error}")));
            let _ = events.send(Event::Status(String::from("Factory reset failed")));
        }
    }

    let _ = events.send(Event::Busy(false));
}

async fn alert(companion: &Companion, serial: Option<String>, events: &UnboundedSender<Event>) {
    match companion.connect(serial.as_deref()).await {
        Ok(session) => match session.system_play_audiovisual_alert().await {
            Ok(()) => {
                let _ = events.send(Event::Log(String::from("alert played")));
            }
            Err(error) => {
                let _ = events.send(Event::Log(format!("alert failed: {error}")));
            }
        },
        Err(error) => {
            let _ = events.send(Event::Log(format!("alert failed: {error}")));
        }
    }
}

async fn check_updates(companion: &Companion, channel: &str, events: &UnboundedSender<Event>) {
    let _ = events.send(Event::Busy(true));
    let _ = events.send(Event::Status(format!(
        "Checking the `{channel}` channel..."
    )));

    match companion.firmware_directory().await {
        Ok(directory) => match directory.latest(channel) {
            Some(release) => {
                let _ = events.send(Event::Latest(format!(
                    "Latest {} release: {}",
                    channel, release.version
                )));
                let _ = events.send(Event::Log(format!(
                    "release {}: {}",
                    release.version,
                    release.changelog.lines().next().unwrap_or_default()
                )));
                let _ = events.send(Event::Status(format!(
                    "Latest {} release is {}",
                    channel, release.version
                )));
            }
            None => {
                let _ = events.send(Event::Log(format!(
                    "the update directory has no `{channel}` channel"
                )));
            }
        },
        Err(error) => {
            let _ = events.send(Event::Log(format!(
                "cannot read the update directory: {error}"
            )));
            let _ = events.send(Event::Status(String::from("Update check failed")));
        }
    }

    let _ = events.send(Event::Busy(false));
}

async fn install_update(
    companion: &Companion,
    channel: &str,
    serial: Option<String>,
    events: &UnboundedSender<Event>,
) {
    let _ = events.send(Event::Task(String::from("firmware")));
    let _ = events.send(Event::Busy(true));
    let _ = events.send(Event::Status(String::from("Preparing update...")));

    let result = async {
        let devices = companion.scan().await?;
        let device = devices
            .iter()
            .find(|device| {
                !device.is_dfu()
                    && serial
                        .as_deref()
                        .is_none_or(|serial| device.usb.serial_number.as_deref() == Some(serial))
            })
            .or_else(|| devices.iter().find(|device| !device.is_dfu()))
            .ok_or_else(|| flipper_companion_app::AppError::Bundle(String::from("no running Flipper")))?;

        let target = device.target().unwrap_or("f18").to_owned();
        let directory = companion.firmware_directory().await?;
        let release = select_release(&directory, channel, "update_tgz", &target)?;
        let file = release.file("update_tgz", &target).ok_or_else(|| {
            flipper_companion_app::AppError::Bundle(format!(
                "release {} has no update bundle for `{target}`",
                release.version
            ))
        })?;

        let _ = events.send(Event::Log(format!(
            "installing {} for target {target}",
            release.version
        )));

        let temporary =
            tempfile::tempdir().map_err(|error| flipper_companion_app::AppError::Join(error.to_string()))?;

        let sender = events.clone();
        let mut last_percent = -1;
        let archive = companion
            .download_artifact(file, temporary.path(), move |progress| {
                if let Some(fraction) = progress.fraction_value() {
                    let percent = (fraction * 100.0) as i32;
                    if percent != last_percent {
                        last_percent = percent;
                        let _ = sender.send(Event::Progress(fraction));
                    }
                }
            })
            .await?;

        let bundle = FirmwareBundle::extract(&archive, &temporary.path().join("bundle"))?;

        let sender = events.clone();
        let mut last_percent = -1;
        let report = companion
            .install_bundle(
                device.usb.serial_number.as_deref(),
                &bundle,
                move |progress| {
                    if let Some(fraction) = progress.fraction_value() {
                        let percent = (fraction * 100.0) as i32;
                        if percent != last_percent {
                            last_percent = percent;
                            let _ = sender.send(Event::Progress(fraction));
                        }
                    }
                },
            )
            .await?;

        Ok::<_, flipper_companion_app::AppError>((release.version.clone(), report))
    }
    .await;

    match result {
        Ok((version, report)) => {
            let _ = events.send(Event::Log(format!(
                "update {version} started: {} file(s), {} uploaded",
                report.files_total, report.files_uploaded
            )));
            let _ = events.send(Event::Status(format!(
                "Update {version} started — follow the device screen"
            )));
        }
        Err(error) => {
            let _ = events.send(Event::Log(format!("update failed: {error}")));
            let _ = events.send(Event::Status(String::from("Update failed")));
        }
    }

    let _ = events.send(Event::Busy(false));
}

fn start_stream(serial: Option<String>, events: UnboundedSender<Event>) -> StreamHandle {
    let (cancel, cancel_rx) = oneshot::channel();
    let (inputs, input_rx) = unbounded_channel();

    let task = tokio::spawn(stream_task(serial, events, input_rx, cancel_rx));

    StreamHandle {
        cancel: Some(cancel),
        inputs,
        task,
    }
}

async fn stream_task(
    serial: Option<String>,
    events: UnboundedSender<Event>,
    mut inputs: UnboundedReceiver<i32>,
    mut cancel: oneshot::Receiver<()>,
) {
    let companion = Companion::new();
    let session = match companion.connect(serial.as_deref()).await {
        Ok(session) => session,
        Err(error) => {
            let _ = events.send(Event::Log(format!("stream: {error}")));
            return;
        }
    };

    let mut broadcasts = session.subscribe();

    if let Err(error) = session.start_screen_stream().await {
        let _ = events.send(Event::Log(format!("stream: {error}")));
        let _ = session.stop().await;
        return;
    }

    let _ = events.send(Event::Streaming(true));
    let _ = events.send(Event::Log(String::from("screen stream started")));

    let mut last_frame = Instant::now() - Duration::from_secs(1);

    loop {
        tokio::select! {
            _ = &mut cancel => break,
            Some(key) = inputs.recv() => {
                if let Ok(key) = pb_gui::InputKey::try_from(key) {
                    // The firmware's input sequence tracker requires a Press
                    // event before Short/Release; a bare Short is ignored.
                    let _ = session.send_input(key, pb_gui::InputType::Press).await;
                    let _ = session.send_input(key, pb_gui::InputType::Short).await;
                    tokio::time::sleep(Duration::from_millis(1)).await;
                    let _ = session.send_input(key, pb_gui::InputType::Release).await;
                }
            }
            event = broadcasts.recv() => match event {
                Ok(SessionEvent::Broadcast(message)) => {
                    if let Some(frame) = ScreenFrame::from_message(&message)
                        && last_frame.elapsed() >= Duration::from_millis(50)
                        && let Ok(rgba) = frame.to_rgba()
                    {
                        last_frame = Instant::now();
                        let _ = events.send(Event::Screen(rgba));
                    }
                }
                Ok(SessionEvent::Closed(_)) => break,
                Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => continue,
                Err(tokio::sync::broadcast::error::RecvError::Closed) => break,
            },
        }
    }

    let _ = session.stop_screen_stream().await;
    session.stop().await;
    let _ = events.send(Event::Streaming(false));
    let _ = events.send(Event::Log(String::from("screen stream stopped")));
}

async fn list_files(
    companion: &Companion,
    serial: Option<String>,
    path: &str,
    events: &UnboundedSender<Event>,
) {
    let _ = events.send(Event::Status(format!("Reading {path}...")));

    let session = match companion.connect(serial.as_deref()).await {
        Ok(session) => session,
        Err(error) => {
            let _ = events.send(Event::Log(format!("files: {error}")));
            return;
        }
    };

    match session.storage_list(path).await {
        Ok(mut entries) => {
            entries.sort_by(|left, right| {
                right
                    .is_directory()
                    .cmp(&left.is_directory())
                    .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
            });

            let base = path.trim_end_matches('/');
            let views = entries
                .into_iter()
                .map(|entry| {
                    let is_dir = entry.is_directory();

                    FileView {
                        remote: format!("{base}/{}", entry.name),
                        name: entry.name,
                        kind: if is_dir {
                            String::from("dir")
                        } else {
                            String::from("file")
                        },
                        size: if is_dir {
                            String::from("-")
                        } else {
                            human_bytes(entry.size_bytes)
                        },
                        is_dir,
                    }
                })
                .collect::<Vec<_>>();

            let _ = events.send(Event::Status(format!("{} entries in {path}", views.len())));
            let _ = events.send(Event::Files(views));
        }
        Err(error) => {
            let _ = events.send(Event::Log(format!("files: {error}")));
            let _ = events.send(Event::Status(format!("Cannot read {path}")));
        }
    }

    let _ = session.stop().await;
}

async fn inspect_file(
    companion: &Companion,
    serial: Option<String>,
    remote: &str,
    events: &UnboundedSender<Event>,
) {
    let _ = events.send(Event::Status(format!("Reading properties of {remote}...")));

    let session = match companion.connect(serial.as_deref()).await {
        Ok(session) => session,
        Err(error) => {
            let _ = events.send(Event::FileProperties(FilePropertiesView {
                remote: remote.to_owned(),
                kind: String::new(),
                size_human: String::new(),
                size_bytes: 0,
                modified: None,
                entries: None,
                error: Some(error.to_string()),
            }));
            return;
        }
    };

    let stat = session.storage_stat(remote).await;
    let timestamp = session.storage_timestamp(remote).await.ok();

    let properties = match stat {
        Ok(Some(entry)) => {
            let is_dir = entry.is_directory();
            let entries = if is_dir {
                session
                    .storage_list(remote)
                    .await
                    .ok()
                    .map(|list| list.len())
            } else {
                None
            };

            FilePropertiesView {
                remote: remote.to_owned(),
                kind: if is_dir {
                    String::from("directory")
                } else {
                    String::from("file")
                },
                size_human: if is_dir {
                    String::from("-")
                } else {
                    human_bytes(entry.size_bytes)
                },
                size_bytes: entry.size_bytes,
                modified: timestamp.map(format_timestamp),
                entries,
                error: None,
            }
        }
        Ok(None) => FilePropertiesView {
            remote: remote.to_owned(),
            kind: String::new(),
            size_human: String::new(),
            size_bytes: 0,
            modified: None,
            entries: None,
            error: Some(String::from("entry does not exist")),
        },
        Err(error) => FilePropertiesView {
            remote: remote.to_owned(),
            kind: String::new(),
            size_human: String::new(),
            size_bytes: 0,
            modified: None,
            entries: None,
            error: Some(error.to_string()),
        },
    };

    let _ = events.send(Event::FileProperties(properties));
    let _ = session.stop().await;
}

async fn file_md5(
    companion: &Companion,
    serial: Option<String>,
    remote: &str,
    events: &UnboundedSender<Event>,
) {
    let _ = events.send(Event::Status(format!("Computing MD5 of {remote}...")));

    let session = match companion.connect(serial.as_deref()).await {
        Ok(session) => session,
        Err(error) => {
            let _ = events.send(Event::FileMd5 {
                remote: remote.to_owned(),
                md5: None,
                error: Some(error.to_string()),
            });
            return;
        }
    };

    let result = session.storage_md5sum(remote).await;

    let _ = events.send(Event::FileMd5 {
        remote: remote.to_owned(),
        md5: result.as_ref().ok().cloned(),
        error: result.err().map(|error| error.to_string()),
    });

    let _ = session.stop().await;
}

fn format_timestamp(timestamp: u32) -> String {
    let date = BuildDate::from_unix_timestamp(u64::from(timestamp));
    let seconds = timestamp % 86_400;

    format!(
        "{date} {:02}:{:02}:{:02} UTC",
        seconds / 3600,
        (seconds % 3600) / 60,
        seconds % 60
    )
}

async fn download_file(
    companion: &Companion,
    serial: Option<String>,
    remote: &str,
    local: &Path,
    events: &UnboundedSender<Event>,
) {
    let session = match companion.connect(serial.as_deref()).await {
        Ok(session) => session,
        Err(error) => {
            let _ = events.send(Event::Log(format!("download: {error}")));
            return;
        }
    };

    match session.storage_read(remote).await {
        Ok(data) => match tokio::fs::write(local, &data).await {
            Ok(()) => {
                let _ = events.send(Event::Log(format!(
                    "downloaded {remote} -> {} ({})",
                    local.display(),
                    human_bytes(data.len() as u64)
                )));
                let _ = events.send(Event::Status(String::from("Download complete")));
            }
            Err(error) => {
                let _ = events.send(Event::Log(format!("download: {error}")));
                let _ = events.send(Event::Status(String::from("Download failed")));
            }
        },
        Err(error) => {
            let _ = events.send(Event::Log(format!("download: {error}")));
            let _ = events.send(Event::Status(String::from("Download failed")));
        }
    }

    let _ = session.stop().await;
}

async fn upload_file(
    companion: &Companion,
    serial: Option<String>,
    local: &Path,
    remote: &str,
    events: &UnboundedSender<Event>,
) {
    let data = match tokio::fs::read(local).await {
        Ok(data) => data,
        Err(error) => {
            let _ = events.send(Event::Log(format!("upload: {error}")));
            let _ = events.send(Event::Status(String::from("Upload failed")));
            return;
        }
    };

    let session = match companion.connect(serial.as_deref()).await {
        Ok(session) => session,
        Err(error) => {
            let _ = events.send(Event::Log(format!("upload: {error}")));
            let _ = events.send(Event::Status(String::from("Upload failed")));
            return;
        }
    };

    match session.storage_write(remote, &data).await {
        Ok(()) => {
            let _ = events.send(Event::Log(format!(
                "uploaded {} -> {remote} ({})",
                local.display(),
                human_bytes(data.len() as u64)
            )));
            let _ = events.send(Event::Status(String::from("Upload complete")));
        }
        Err(error) => {
            let _ = events.send(Event::Log(format!("upload: {error}")));
            let _ = events.send(Event::Status(String::from("Upload failed")));
        }
    }

    let _ = session.stop().await;
}

async fn delete_file(
    companion: &Companion,
    serial: Option<String>,
    remote: &str,
    events: &UnboundedSender<Event>,
) {
    let session = match companion.connect(serial.as_deref()).await {
        Ok(session) => session,
        Err(error) => {
            let _ = events.send(Event::Log(format!("delete: {error}")));
            return;
        }
    };

    match session.storage_remove(remote, true).await {
        Ok(()) => {
            let _ = events.send(Event::Log(format!("removed {remote}")));
            let _ = events.send(Event::Status(String::from("Deleted")));
        }
        Err(error) => {
            let _ = events.send(Event::Log(format!("delete: {error}")));
            let _ = events.send(Event::Status(String::from("Delete failed")));
        }
    }

    let _ = session.stop().await;
}

async fn make_dir(
    companion: &Companion,
    serial: Option<String>,
    path: &str,
    events: &UnboundedSender<Event>,
) {
    let session = match companion.connect(serial.as_deref()).await {
        Ok(session) => session,
        Err(error) => {
            let _ = events.send(Event::Log(format!("mkdir: {error}")));
            return;
        }
    };

    match session.storage_mkdir(path).await {
        Ok(()) => {
            let _ = events.send(Event::Log(format!("created {path}")));
            let _ = events.send(Event::Status(String::from("Directory created")));
        }
        Err(error) => {
            let _ = events.send(Event::Log(format!("mkdir: {error}")));
            let _ = events.send(Event::Status(String::from("mkdir failed")));
        }
    }

    let _ = session.stop().await;
}

async fn rename_file(
    companion: &Companion,
    serial: Option<String>,
    from: &str,
    to: &str,
    events: &UnboundedSender<Event>,
) {
    let session = match companion.connect(serial.as_deref()).await {
        Ok(session) => session,
        Err(error) => {
            let _ = events.send(Event::Log(format!("rename: {error}")));
            return;
        }
    };

    match session.storage_rename(from, to).await {
        Ok(()) => {
            let _ = events.send(Event::Log(format!("renamed {from} -> {to}")));
            let _ = events.send(Event::Status(String::from("Renamed")));
        }
        Err(error) => {
            let _ = events.send(Event::Log(format!("rename: {error}")));
            let _ = events.send(Event::Status(String::from("Rename failed")));
        }
    }

    let _ = session.stop().await;
}

async fn device_context(
    companion: &Companion,
    serial: Option<&str>,
) -> Result<(String, String), flipper_companion_app::AppError> {
    let devices = companion.scan().await?;
    let device = devices
        .iter()
        .find(|device| {
            !device.is_dfu()
                && serial.is_none_or(|serial| device.usb.serial_number.as_deref() == Some(serial))
        })
        .or_else(|| devices.iter().find(|device| !device.is_dfu()))
        .ok_or_else(|| {
            flipper_companion_app::AppError::App(String::from("no running Flipper is connected"))
        })?;

    let info = device
        .info
        .as_ref()
        .ok_or_else(|| flipper_companion_app::AppError::App(String::from("device info unavailable")))?;

    let api = info
        .firmware
        .api_version
        .ok_or_else(|| {
            flipper_companion_app::AppError::App(String::from(
                "firmware API version is not reported by this firmware",
            ))
        })?
        .to_string();

    Ok((info.hardware.target.clone(), api))
}

async fn load_catalog(
    companion: &Companion,
    serial: Option<String>,
    query: &str,
    source: Option<&str>,
) -> Result<Vec<AppInfo>, flipper_companion_app::AppError> {
    let (target, api) = device_context(companion, serial.as_deref()).await?;

    companion
        .catalog_apps(
            &target,
            &api,
            None,
            Some(query).filter(|query| !query.is_empty()),
            source,
        )
        .await
}

async fn install_app(
    companion: &Companion,
    serial: Option<String>,
    app: &AppInfo,
    events: &UnboundedSender<Event>,
    manage_busy: bool,
) {
    if manage_busy {
        let _ = events.send(Event::Busy(true));
    }
    let _ = events.send(Event::Task(String::from("apps")));
    let _ = events.send(Event::Status(format!(
        "Downloading {} {}...",
        app.name, app.version
    )));

    let sender = events.clone();
    let mut last_percent = -1;
    let phase_sender = events.clone();
    let name = app.name.clone();
    let version = app.version.clone();

    let result = companion
        .install_app(
            serial.as_deref(),
            app,
            move |progress| {
                if let Some(fraction) = progress.fraction_value() {
                    let percent = (fraction * 100.0) as i32;
                    if percent != last_percent {
                        last_percent = percent;
                        let _ = sender.send(Event::Progress(fraction));
                    }
                }
            },
            move |phase| {
                let text = match phase {
                    flipper_companion_app::InstallPhase::Downloading => {
                        format!("Downloading {name} {version}...")
                    }
                    flipper_companion_app::InstallPhase::Verifying => {
                        String::from("Verifying the checksum...")
                    }
                    flipper_companion_app::InstallPhase::Uploading => {
                        format!("Uploading {name} to the SD card...")
                    }
                };

                let _ = phase_sender.send(Event::Status(text));
            },
        )
        .await;

    match result {
        Ok(report) => {
            let _ = events.send(Event::Log(format!(
                "installed {} {} -> {} ({})",
                report.name,
                report.version,
                report.remote_path,
                human_bytes(report.bytes)
            )));
            let _ = events.send(Event::Status(format!("Installed {}", report.name)));
        }
        Err(error) => {
            let _ = events.send(Event::Log(format!("install failed: {error}")));
            let _ = events.send(Event::Status(String::from("App installation failed")));
        }
    }

    let _ = events.send(Event::Busy(false));
}

async fn uninstall_app(
    companion: &Companion,
    serial: Option<String>,
    path: &str,
    events: &UnboundedSender<Event>,
) {
    let _ = events.send(Event::Busy(true));
    let _ = events.send(Event::Status(format!("Uninstalling {path}...")));

    match companion.uninstall_app(serial.as_deref(), path).await {
        Ok(()) => {
            let _ = events.send(Event::Log(format!("uninstalled {path}")));
            let _ = events.send(Event::Status(String::from("App uninstalled")));
        }
        Err(error) => {
            let _ = events.send(Event::Log(format!("uninstall: {error}")));
            let _ = events.send(Event::Status(String::from("Uninstall failed")));
        }
    }

    let _ = events.send(Event::Busy(false));
}

/// Checks every installed FAP against the catalog by computing its SHA-256.
///
/// One session is kept open for the whole run; the merged list is published
/// after each application so the interface fills in as the comparisons finish.
async fn check_app_updates(
    companion: &Companion,
    serial: Option<String>,
    apps: &mut AppsState,
    events: &UnboundedSender<Event>,
) {
    let _ = events.send(Event::Busy(true));
    let _ = events.send(Event::Task(String::from("apps")));
    let _ = events.send(Event::Status(String::from(
        "Calculating checksum for evaluating updates...",
    )));

    match companion.installed_apps(serial.as_deref()).await {
        Ok(installed) => apps.installed = installed,
        Err(error) => {
            let _ = events.send(Event::Log(format!("updates: {error}")));
            let _ = events.send(Event::Status(String::from(
                "Cannot read the installed apps",
            )));
            let _ = events.send(Event::Busy(false));
            return;
        }
    }

    if apps.catalog.is_empty() {
        apps.catalog = load_catalog(companion, serial.clone(), "", None)
            .await
            .unwrap_or_default();
    }

    let pairs = apps
        .installed
        .iter()
        .filter_map(|installed| {
            let app = apps.catalog.iter().find(|app| {
                app.alias.eq_ignore_ascii_case(&installed.name)
                    && app.category.eq_ignore_ascii_case(&installed.category)
            })?;

            Some((installed.clone(), app.clone()))
        })
        .collect::<Vec<_>>();

    let total = pairs.len();
    apps.updates.clear();
    apps.verified.clear();

    let session = match companion.connect(serial.as_deref()).await {
        Ok(session) => session,
        Err(error) => {
            let _ = events.send(Event::Log(format!("checksum: {error}")));
            let _ = events.send(Event::Status(String::from(
                "Cannot open the device to compute checksums",
            )));
            let _ = events.send(Event::Busy(false));
            return;
        }
    };

    for (index, (installed, app)) in pairs.iter().enumerate() {
        let _ = events.send(Event::Status(format!(
            "Calculating checksum for evaluating updates — {}/{}: {}",
            index + 1,
            total,
            installed.name
        )));
        let _ = events.send(Event::Progress(index as f32 / total as f32));

        let matches = match session.storage_read(&installed.path).await {
            Ok(data) => {
                let digest = hex::encode(<sha2::Sha256 as sha2::Digest>::digest(&data));
                app.sha256
                    .as_deref()
                    .is_some_and(|expected| digest.eq_ignore_ascii_case(expected))
            }
            Err(error) => {
                let _ = events.send(Event::Log(format!("checksum {}: {error}", installed.name)));
                continue;
            }
        };

        apps.verified.insert(app.id.clone(), matches);

        if matches {
            apps.updates.remove(&app.id);
        } else {
            apps.updates.insert(app.id.clone());
            let _ = events.send(Event::Log(format!(
                "update: {} differs from the catalog build",
                installed.name
            )));
        }

        // Publish progressively so the rows update as checksums complete.
        publish_entries(events, apps);
    }

    let _ = session.stop().await;

    let count = apps.updates.len();
    let _ = events.send(Event::Status(if count == 0 {
        String::from("Everything is up to date")
    } else {
        format!("{count} update(s) available")
    }));
    let _ = events.send(Event::Progress(1.0));

    publish_entries(events, apps);
    let _ = events.send(Event::Busy(false));
}

/// Verifies one installed FAP against the catalog SHA-256.
async fn verify_app(
    companion: &Companion,
    serial: Option<String>,
    remote: &str,
    app_id: &str,
    apps: &mut AppsState,
    events: &UnboundedSender<Event>,
) {
    let _ = events.send(Event::Busy(true));
    let _ = events.send(Event::Status(format!("Verifying {remote}...")));

    if apps.catalog.is_empty() {
        apps.catalog = load_catalog(companion, serial.clone(), "", None)
            .await
            .unwrap_or_default();
    }

    let expected = apps
        .catalog
        .iter()
        .find(|app| app.id == app_id)
        .and_then(|app| app.sha256.clone());

    let Some(expected) = expected else {
        let _ = events.send(Event::AppVerified {
            remote: remote.to_owned(),
            matches: false,
            detail: String::new(),
            error: Some(String::from(
                "the catalog does not publish a checksum for this app",
            )),
        });
        let _ = events.send(Event::Busy(false));
        return;
    };

    match companion
        .verify_installed_app(serial.as_deref(), remote, &expected)
        .await
    {
        Ok(report) => {
            apps.verified.insert(app_id.to_owned(), report.matches);

            if report.matches {
                apps.updates.remove(app_id);
            } else {
                apps.updates.insert(app_id.to_owned());
            }

            let _ = events.send(Event::AppVerified {
                remote: remote.to_owned(),
                matches: report.matches,
                detail: human_bytes(report.bytes),
                error: None,
            });
            let _ = events.send(Event::Status(if report.matches {
                String::from("This app is up to date")
            } else {
                String::from("Update available for this app")
            }));
        }
        Err(error) => {
            let _ = events.send(Event::Log(format!("verify: {error}")));
            let _ = events.send(Event::AppVerified {
                remote: remote.to_owned(),
                matches: false,
                detail: String::new(),
                error: Some(error.to_string()),
            });
            let _ = events.send(Event::Status(String::from("Verification failed")));
        }
    }

    publish_entries(events, apps);
    let _ = events.send(Event::Busy(false));
}

/// Reloads the catalog and the installed apps, then publishes the result.
async fn reload_apps(companion: &Companion, apps: &mut AppsState, events: &UnboundedSender<Event>) {
    if let Ok(catalog) = load_catalog(companion, None, "", None).await {
        apps.catalog = catalog;
    }

    if let Ok(installed) = companion.installed_apps(None).await {
        apps.installed = installed;
    }

    publish_entries(events, apps);
}

/// Sends the merged list to the interface.
fn publish_entries(events: &UnboundedSender<Event>, apps: &AppsState) {
    let _ = events.send(Event::AppEntries(apps.entries()));
}

fn source_views() -> Vec<SourceView> {
    let mut sources = vec![
        SourceView {
            id: String::new(),
            label: String::from("All sources"),
        },
        SourceView {
            id: String::from("official"),
            label: String::from("Official catalog"),
        },
    ];

    sources.extend(
        flipper_companion_app::load_custom_sources()
            .into_iter()
            .map(|url| SourceView {
                id: url.clone(),
                label: url,
            }),
    );

    sources
}

fn device_view(device: DeviceSummary) -> DeviceView {
    let title = match device.usb.mode {
        UsbMode::Dfu => format!("⚡ {} (DFU)", device.display_name()),
        UsbMode::VirtualComPort => device.display_name().to_string(),
    };

    let serial = device.usb.serial_number.clone();

    let subtitle = format!(
        "{} · {}",
        serial.clone().unwrap_or_else(|| device.usb.key()),
        device.usb.mode.label()
    );

    let detail = if let Some(error) = &device.error {
        format!("Could not read device information:\n{error}")
    } else if let Some(info) = &device.info {
        let mut lines = vec![
            format!("Firmware:   {}", info.firmware.label()),
            format!("Channel:    {}", info.firmware.channel),
            format!(
                "Firmware API: {}",
                info.firmware
                    .api_version
                    .map_or_else(|| String::from("unknown"), |version| version.to_string())
            ),
            format!(
                "Hardware:   {} (version {})",
                info.hardware.target, info.hardware.version
            ),
        ];

        if let Some(radio) = &info.radio {
            lines.push(format!(
                "Radio:      FUS {}, stack {}",
                radio.fus_version, radio.stack_version
            ));
        }

        if let Some(protobuf) = device.protobuf {
            lines.push(format!("Protobuf:   {protobuf}"));
        }

        if let Some(storage) = device.storage {
            lines.push(storage_line(storage));
        }

        if let Some(power) = &device.power {
            let map: BTreeMap<&str, &str> = power
                .iter()
                .map(|(key, value)| (key.as_str(), value.as_str()))
                .collect();

            if let Some(level) = map.get("charge_level") {
                lines.push(format!(
                    "▮ Battery:    {level}% ({})",
                    map.get("charge_state").copied().unwrap_or("unknown")
                ));
            }

            if let Some(temperature) = map.get("battery_temp") {
                lines.push(format!("▮ Battery:    {temperature} °C"));
            }
        }

        lines.join("\n")
    } else {
        String::from(
            "DFU bootloader mode.\n\nSelect a .dfu image below and press Flash.\n\
             To leave DFU mode, unplug and replug the device.",
        )
    };

    DeviceView {
        title,
        subtitle,
        detail,
        serial,
    }
}

fn storage_line(storage: StorageInfo) -> String {
    if storage.is_present() {
        format!(
            "Storage:    {} free of {}",
            human_bytes(storage.free_bytes),
            human_bytes(storage.total_bytes)
        )
    } else {
        String::from("Storage:    no SD card")
    }
}

/// Formats a byte count for the interface.
pub fn human_bytes(value: u64) -> String {
    const UNITS: [&str; 6] = ["B", "KiB", "MiB", "GiB", "TiB", "PiB"];

    if value < 1024 {
        return format!("{value} B");
    }

    let mut scaled = value as f64;
    let mut unit = 0;

    while scaled >= 1024.0 && unit + 1 < UNITS.len() {
        scaled /= 1024.0;
        unit += 1;
    }

    format!("{scaled:.1} {}", UNITS[unit])
}
