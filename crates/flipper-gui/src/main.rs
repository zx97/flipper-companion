//! Desktop interface built with Slint.
//!
//! The UI thread never performs I/O.  A dedicated worker thread owns a Tokio
//! runtime and the application services; both sides exchange plain messages.

mod logging;
mod worker;

use std::{
    cell::RefCell,
    env, fs,
    path::{Path, PathBuf},
    rc::Rc,
    time::Duration,
};

use anyhow::{Context, Result};
use slint::{Image, ModelRc, SharedPixelBuffer, VecModel};

use flipper_companion_protocol::{HEIGHT as SCREEN_HEIGHT, WIDTH as SCREEN_WIDTH};
use logging::{LogLevel, LogRecord};
use tokio::sync::mpsc::UnboundedSender;
use worker::{Command, DeviceView, Event, FileView, Worker};

slint::include_modules!();

fn main() -> Result<()> {
    let (log_receiver, log_path) = logging::init();
    tracing::info!(
        "flipper-companion {} starting ({} {} {})",
        env!("CARGO_PKG_VERSION"),
        std::env::consts::OS,
        std::env::consts::ARCH,
        env!("CARGO_PKG_NAME")
    );

    if let Some(path) = &log_path {
        tracing::debug!("log: writing to {}", path.display());
    }

    let options = GuiOptions::from_env();
    let mut worker = Worker::spawn();
    let ui = AppWindow::new().context("cannot create the Slint window")?;
    ui.set_current_tab(options.tab);
    ui.set_channel_index(load_channel());
    ui.set_local_path(downloads_dir().into());
    ui.set_log_file(
        log_path
            .as_deref()
            .map_or_else(String::new, |path| path.display().to_string())
            .into(),
    );

    let state = Rc::new(RefCell::new(UiState {
        log_verbosity: 1,
        sort_ascending: true,
        ..UiState::default()
    }));

    install_callbacks(&ui, &worker, &state);
    install_timer(&ui, worker.receiver(), log_receiver, Rc::clone(&state));

    // First scan runs in the background once the window is visible.
    let _ = worker.sender().send(Command::Refresh);

    if options.stream {
        let _ = worker.sender().send(Command::StartStream { serial: None });
    }

    if options.tab == 4 {
        let _ = worker.sender().send(Command::LoadApps {
            serial: None,
            query: String::new(),
            source: None,
        });
        let _ = worker
            .sender()
            .send(Command::CheckAppUpdates { serial: None });
    }

    let result = ui.run();
    let _ = worker.sender().send(Command::Shutdown);

    result.context("Slint event loop failed")?;

    Ok(())
}

struct GuiOptions {
    tab: i32,
    stream: bool,
}

impl GuiOptions {
    fn from_env() -> Self {
        let mut options = Self {
            tab: 0,
            stream: false,
        };

        for argument in std::env::args().skip(1) {
            if let Some(value) = argument.strip_prefix("--tab=") {
                options.tab = value.parse().unwrap_or(0).clamp(0, 5);
            } else if argument == "--stream" {
                options.stream = true;
            }
        }

        options
    }
}

#[derive(Default)]
struct UiState {
    log: Vec<LogRecord>,
    log_verbosity: u8,
    devices: Vec<DeviceView>,
    files: Vec<FileView>,
    selected_remote: Option<String>,
    entries: Vec<worker::AppEntryView>,
    visible: Vec<worker::AppEntryView>,
    filter: i32,
    query: String,
    sort_column: i32,
    sort_ascending: bool,
    selected: std::collections::HashSet<String>,
    opened: bool,
    sources: Vec<worker::SourceView>,
    source_index: i32,
}

/// Shows an operation in the status bar as soon as the user triggers it.
///
/// The worker reports its own progress, but the first message may take a few
/// milliseconds; announcing it immediately makes every button (and the Enter
/// key) visibly acknowledged.
fn announce(ui: &slint::Weak<AppWindow>, message: impl Into<slint::SharedString>) {
    if let Some(ui) = ui.upgrade() {
        ui.set_status(message.into());
    }
}

fn install_callbacks(ui: &AppWindow, worker: &Worker, state: &Rc<RefCell<UiState>>) {
    let sender = worker.sender();

    ui.on_refresh({
        let sender = sender.clone();
        let weak = ui.as_weak();
        move || {
            announce(&weak, "Scanning USB devices...");
            let _ = sender.send(Command::Refresh);
        }
    });

    ui.on_flash({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |path| {
            announce(&weak, format!("Flashing {path}..."));
            let serial = weak.upgrade().and_then(|ui| selected_serial(&ui, &state));
            let _ = sender.send(Command::Flash {
                path: path.as_str().into(),
                serial,
            });
        }
    });

    ui.on_check_updates({
        let sender = sender.clone();
        let weak = ui.as_weak();
        move |index| {
            announce(
                &weak,
                format!("Checking the `{}` channel...", channel_name(index)),
            );
            let _ = sender.send(Command::CheckUpdates {
                channel: channel_name(index),
            });
        }
    });

    ui.on_channel_changed(|index| {
        save_channel(index);
    });

    ui.on_install_update({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |index| {
            announce(
                &weak,
                format!("Installing the `{}` update...", channel_name(index)),
            );
            let serial = weak.upgrade().and_then(|ui| selected_serial(&ui, &state));
            let _ = sender.send(Command::Install {
                channel: channel_name(index),
                serial,
            });
        }
    });

    ui.on_backup({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |path| {
            announce(&weak, format!("Backing up internal memory to {path}..."));
            let serial = weak.upgrade().and_then(|ui| selected_serial(&ui, &state));
            let _ = sender.send(Command::Backup {
                serial,
                archive: Path::new(path.as_str()).to_path_buf(),
            });
        }
    });

    ui.on_restore({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |path| {
            announce(&weak, format!("Restoring {path}..."));
            let serial = weak.upgrade().and_then(|ui| selected_serial(&ui, &state));
            let _ = sender.send(Command::Restore {
                serial,
                archive: Path::new(path.as_str()).to_path_buf(),
            });
        }
    });

    ui.on_factory_reset({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move || {
            announce(&weak, "Sending the factory reset...");
            let serial = weak.upgrade().and_then(|ui| selected_serial(&ui, &state));
            let _ = sender.send(Command::FactoryReset { serial });
        }
    });

    ui.on_play_alert({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move || {
            announce(&weak, "Playing the audiovisual alert...");
            let serial = weak.upgrade().and_then(|ui| selected_serial(&ui, &state));
            let _ = sender.send(Command::Alert { serial });
        }
    });

    ui.on_refresh_apps({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move || {
            announce(&weak, "Loading apps and installed applications...");
            let serial = weak.upgrade().and_then(|ui| selected_serial(&ui, &state));
            let _ = sender.send(Command::LoadApps {
                serial,
                query: String::new(),
                source: current_source(&state),
            });
        }
    });

    ui.on_filter_apps({
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |filter| {
            state.borrow_mut().filter = filter;
            if let Some(ui) = weak.upgrade() {
                ui.set_selected_app(-1);
                render_entries(&ui, &state);
            }
        }
    });

    ui.on_search_apps({
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |text| {
            state.borrow_mut().query = text.as_str().to_owned();
            if let Some(ui) = weak.upgrade() {
                render_entries(&ui, &state);
            }
        }
    });

    ui.on_install_entry({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |index| {
            let serial = weak.upgrade().and_then(|ui| selected_serial(&ui, &state));
            let Some(entry) = entry_at(&state, index) else {
                return;
            };
            let Some(id) = Some(entry.id).filter(|id| !id.is_empty()) else {
                announce(&weak, "This app is not in the selected repository");
                return;
            };

            announce(&weak, format!("Installing {}...", entry.name));
            let _ = sender.send(Command::InstallApp { serial, app_id: id });
        }
    });

    ui.on_update_entry({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |index| {
            let serial = weak.upgrade().and_then(|ui| selected_serial(&ui, &state));
            let Some(entry) = entry_at(&state, index) else {
                return;
            };

            if entry.id.is_empty() {
                announce(&weak, "This app is not in the selected repository");
                return;
            }

            announce(&weak, format!("Updating {}...", entry.name));
            let _ = sender.send(Command::InstallApp {
                serial,
                app_id: entry.id,
            });
        }
    });

    ui.on_reinstall_entry({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |index| {
            let serial = weak.upgrade().and_then(|ui| selected_serial(&ui, &state));
            let Some(entry) = entry_at(&state, index) else {
                return;
            };
            let (Some(path), false) = (entry.remote_path.clone(), entry.id.is_empty()) else {
                announce(&weak, "This app is not in the selected repository");
                return;
            };

            announce(&weak, format!("Reinstalling {}...", entry.name));
            let _ = sender.send(Command::ReinstallApp {
                serial,
                remote: path,
                app_id: entry.id,
            });
        }
    });

    ui.on_toggle_app({
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |index| {
            let Some(entry) = entry_at(&state, index) else {
                return;
            };
            let key = entry_key(&entry);

            {
                let mut state = state.borrow_mut();
                if !state.selected.remove(&key) {
                    state.selected.insert(key);
                }
            }

            if let Some(ui) = weak.upgrade() {
                render_entries(&ui, &state);
            }
        }
    });

    ui.on_toggle_select_all({
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move || {
            {
                let mut state = state.borrow_mut();
                let visible = state.visible.clone();
                let keys = visible
                    .iter()
                    .filter(|entry| entry.state == worker::AppStateView::UpdateAvailable)
                    .map(entry_key)
                    .collect::<Vec<_>>();

                let all = !keys.is_empty() && keys.iter().all(|key| state.selected.contains(key));

                if all {
                    for key in keys {
                        state.selected.remove(&key);
                    }
                } else {
                    state.selected.extend(keys);
                }
            }

            if let Some(ui) = weak.upgrade() {
                render_entries(&ui, &state);
            }
        }
    });

    ui.on_update_selected({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move || {
            let ids = {
                let mut state = state.borrow_mut();
                let ids = state
                    .entries
                    .iter()
                    .filter(|entry| state.selected.contains(&entry_key(entry)))
                    .map(|entry| entry.id.clone())
                    .filter(|id| !id.is_empty())
                    .collect::<Vec<_>>();

                state.selected.clear();
                ids
            };

            if ids.is_empty() {
                return;
            }

            announce(&weak, format!("Updating {} app(s)...", ids.len()));
            let serial = weak.upgrade().and_then(|ui| selected_serial(&ui, &state));
            let _ = sender.send(Command::UpdateApps {
                serial,
                app_ids: ids,
            });

            if let Some(ui) = weak.upgrade() {
                render_entries(&ui, &state);
            }
        }
    });

    ui.on_sort_apps({
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |column| {
            {
                let mut state = state.borrow_mut();

                if state.sort_column == column {
                    state.sort_ascending = !state.sort_ascending;
                } else {
                    state.sort_column = column;
                    state.sort_ascending = true;
                }
            }

            if let Some(ui) = weak.upgrade() {
                render_entries(&ui, &state);
            }
        }
    });

    ui.on_apps_opened({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move || {
            let serial = weak.upgrade().and_then(|ui| selected_serial(&ui, &state));

            // First open: load the repository and compare the installed apps.
            if !state.borrow().opened {
                state.borrow_mut().opened = true;
                let _ = sender.send(Command::LoadApps {
                    serial: serial.clone(),
                    query: String::new(),
                    source: current_source(&state),
                });
                let _ = sender.send(Command::CheckAppUpdates { serial });
            }
        }
    });

    ui.on_verify_entry({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |index| {
            let serial = weak.upgrade().and_then(|ui| selected_serial(&ui, &state));
            let Some(entry) = entry_at(&state, index) else {
                return;
            };
            let (Some(path), false) = (entry.remote_path.clone(), entry.id.is_empty()) else {
                announce(&weak, "This app is not in the selected repository");
                return;
            };

            announce(&weak, format!("Verifying {}...", entry.name));
            let _ = sender.send(Command::VerifyApp {
                serial,
                remote: path,
                app_id: entry.id,
            });
        }
    });

    ui.on_uninstall_entry({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |index| {
            let serial = weak.upgrade().and_then(|ui| selected_serial(&ui, &state));
            let Some(entry) = entry_at(&state, index) else {
                return;
            };
            let Some(path) = entry.remote_path else {
                return;
            };

            announce(&weak, format!("Uninstalling {}...", entry.name));
            let _ = sender.send(Command::UninstallApp { serial, path });
        }
    });

    ui.on_check_app_updates({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move || {
            announce(&weak, "Comparing installed apps with the catalog...");
            let serial = weak.upgrade().and_then(|ui| selected_serial(&ui, &state));
            let _ = sender.send(Command::CheckAppUpdates { serial });
        }
    });

    ui.on_app_source_changed({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |label| {
            let index = state
                .borrow()
                .sources
                .iter()
                .position(|source| source.label == label.as_str());

            let Some(index) = index else {
                return;
            };

            state.borrow_mut().source_index = index as i32;
            announce(&weak, format!("Loading apps from {label}..."));
            let serial = weak.upgrade().and_then(|ui| selected_serial(&ui, &state));
            let _ = sender.send(Command::LoadApps {
                serial,
                query: String::new(),
                source: current_source(&state),
            });
        }
    });

    ui.on_remove_app_source({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move || {
            let source = {
                let state = state.borrow();
                state
                    .sources
                    .get(state.source_index.max(0) as usize)
                    .cloned()
            };

            let Some(source) =
                source.filter(|source| !source.id.is_empty() && source.id != "official")
            else {
                return;
            };

            announce(
                &weak,
                format!("Removing the repository {}...", source.label),
            );
            let _ = sender.send(Command::RemoveAppSource { url: source.id });
        }
    });

    ui.on_add_app_source({
        let sender = sender.clone();
        let weak = ui.as_weak();
        move |url| {
            announce(&weak, format!("Adding the repository {url}..."));
            let _ = sender.send(Command::AddAppSource {
                url: url.as_str().to_owned(),
            });
        }
    });

    ui.on_clear_log({
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move || {
            state.borrow_mut().log.clear();
            if let Some(ui) = weak.upgrade() {
                ui.set_log_lines(ModelRc::new(VecModel::from(
                    Vec::<slint::SharedString>::new(),
                )));
            }
        }
    });

    ui.on_set_log_level({
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |level| {
            let level = level.clamp(0, 2) as u8;
            state.borrow_mut().log_verbosity = level;
            logging::set_verbosity(level);

            if let Some(ui) = weak.upgrade() {
                refresh_log_text(&ui, &state);
            }
        }
    });

    ui.on_prepare_download({
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |index| {
            let Some(file) = file_at(&state, index) else {
                return;
            };
            if let Some(ui) = weak.upgrade() {
                ui.set_local_path(format!("{}/{}", downloads_dir(), file.name).into());
            }
        }
    });

    ui.on_inspect_entry({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |index| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let Some(file) = file_at(&state, index) else {
                return;
            };

            state.borrow_mut().selected_remote = Some(file.remote.clone());
            ui.set_prop_path(file.remote.as_str().into());
            ui.set_prop_kind(file.kind.as_str().into());
            ui.set_prop_size(file.size.as_str().into());
            ui.set_prop_modified("".into());
            ui.set_prop_extra("".into());
            ui.set_prop_md5("".into());
            ui.set_prop_error("".into());
            ui.set_status(format!("Reading properties of {}...", file.remote).into());

            let serial = selected_serial(&ui, &state);
            let _ = sender.send(Command::InspectFile {
                serial,
                remote: file.remote,
            });
        }
    });

    ui.on_md5_entry({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |index| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let Some(file) = file_at(&state, index) else {
                return;
            };

            ui.set_prop_md5("computing…".into());
            ui.set_status(format!("Computing the MD5 of {}...", file.remote).into());
            let serial = selected_serial(&ui, &state);
            let _ = sender.send(Command::FileMd5 {
                serial,
                remote: file.remote,
            });
        }
    });

    ui.on_go_up({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move || {
            let Some(ui) = weak.upgrade() else {
                return;
            };

            let parent = parent_path(ui.get_remote_path().as_str());

            navigate(&ui, &state, &sender, parent);
        }
    });

    ui.on_start_stream({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move || {
            announce(&weak, "Starting the screen stream...");
            let serial = weak.upgrade().and_then(|ui| selected_serial(&ui, &state));
            let _ = sender.send(Command::StartStream { serial });
        }
    });

    ui.on_stop_stream({
        let sender = sender.clone();
        let weak = ui.as_weak();
        move || {
            announce(&weak, "Stopping the screen stream...");
            let _ = sender.send(Command::StopStream);
        }
    });

    ui.on_send_key({
        let sender = sender.clone();
        move |key| {
            let _ = sender.send(Command::Input { key });
        }
    });

    ui.on_list_files({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |path| {
            announce(&weak, format!("Reading {path}..."));
            let serial = weak.upgrade().and_then(|ui| selected_serial(&ui, &state));
            let _ = sender.send(Command::ListFiles {
                serial,
                path: path.as_str().to_owned(),
            });
        }
    });

    ui.on_open_entry({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |index| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let Some(file) = file_at(&state, index) else {
                return;
            };

            if file.is_dir {
                navigate(&ui, &state, &sender, file.remote);
            }
        }
    });

    ui.on_download_entry({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |index, local| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let Some(file) = file_at(&state, index) else {
                return;
            };
            announce(&weak, format!("Downloading {} to {local}...", file.remote));
            let serial = selected_serial(&ui, &state);
            let _ = sender.send(Command::DownloadFile {
                serial,
                remote: file.remote,
                local: Path::new(local.as_str()).to_path_buf(),
            });
        }
    });

    ui.on_upload_file({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |local, remote_dir| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let local = Path::new(local.as_str());
            let Some(name) = local.file_name().and_then(|name| name.to_str()) else {
                return;
            };
            announce(
                &weak,
                format!("Uploading {} to {remote_dir}...", local.display()),
            );
            let serial = selected_serial(&ui, &state);
            let _ = sender.send(Command::UploadFile {
                serial,
                local: local.to_path_buf(),
                remote: join_remote(remote_dir.as_str(), name),
            });
        }
    });

    ui.on_delete_entry({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |index| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let Some(file) = file_at(&state, index) else {
                return;
            };
            announce(&weak, format!("Removing {}...", file.remote));
            let serial = selected_serial(&ui, &state);
            let _ = sender.send(Command::DeleteFile {
                serial,
                remote: file.remote,
            });
        }
    });

    ui.on_make_directory({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |parent, name| {
            announce(
                &weak,
                format!("Creating {}/{name}...", parent.trim_end_matches('/')),
            );
            let serial = weak.upgrade().and_then(|ui| selected_serial(&ui, &state));
            let _ = sender.send(Command::MakeDir {
                serial,
                path: join_remote(parent.as_str(), name.as_str()),
            });
        }
    });

    ui.on_rename_entry({
        let sender = sender.clone();
        let state = Rc::clone(state);
        let weak = ui.as_weak();
        move |index, new_name| {
            let Some(ui) = weak.upgrade() else {
                return;
            };
            let Some(file) = file_at(&state, index) else {
                return;
            };
            let parent = parent_path(&file.remote);
            announce(&weak, format!("Renaming {} to {new_name}...", file.remote));
            let serial = selected_serial(&ui, &state);
            let _ = sender.send(Command::RenameFile {
                serial,
                from: file.remote,
                to: join_remote(parent.as_str(), new_name.as_str()),
            });
        }
    });
}

fn install_timer(
    ui: &AppWindow,
    receiver: tokio::sync::mpsc::UnboundedReceiver<Event>,
    log_receiver: tokio::sync::mpsc::UnboundedReceiver<LogRecord>,
    state: Rc<RefCell<UiState>>,
) {
    let timer = slint::Timer::default();
    let ui = ui.as_weak();
    let mut receiver = receiver;
    let mut log_receiver = log_receiver;

    timer.start(
        slint::TimerMode::Repeated,
        Duration::from_millis(120),
        move || {
            let Some(ui) = ui.upgrade() else {
                return;
            };

            let mut logged = false;

            while let Ok(record) = log_receiver.try_recv() {
                state.borrow_mut().log.push(record);
                logged = true;
            }

            if logged {
                trim_log(&state);
                refresh_log_text(&ui, &state);
            }

            while let Ok(event) = receiver.try_recv() {
                match event {
                    Event::Devices(devices) => apply_devices(&ui, &state, devices),
                    Event::Files(files) => apply_files(&ui, &state, files),
                    Event::AppEntries(entries) => apply_entries(&ui, &state, entries),
                    Event::Sources(sources) => apply_sources(&ui, &state, sources),
                    Event::AppVerified {
                        remote,
                        matches,
                        detail,
                        error,
                    } => log_verification(&ui, &state, &remote, matches, &detail, error),
                    Event::FileProperties(properties) => {
                        apply_properties(&ui, &state, properties);
                    }
                    Event::FileMd5 { remote, md5, error } => {
                        if state.borrow().selected_remote.as_deref() == Some(remote.as_str()) {
                            if let Some(md5) = md5 {
                                ui.set_prop_md5(format!("MD5: {md5}").into());
                            } else if let Some(error) = error {
                                ui.set_prop_md5(format!("MD5 error: {error}").into());
                            }
                        }
                    }
                    Event::Busy(busy) => {
                        ui.set_busy(busy);

                        if !busy {
                            ui.set_progress(0.0);
                            ui.set_progress_text(Default::default());
                        }
                    }
                    Event::Task(task) => ui.set_current_task(task.into()),
                    Event::Progress(value) => {
                        ui.set_progress(value);
                        ui.set_progress_text(format!("{}%", (value * 100.0).round() as i32).into());
                    }
                    Event::Status(text) => ui.set_status(text.into()),
                    Event::Latest(text) => ui.set_latest_firmware(text.into()),
                    Event::Log(message) => {
                        state.borrow_mut().log.push(LogRecord {
                            level: LogLevel::Info,
                            target: String::from("flipper"),
                            message,
                            elapsed: logging::elapsed(),
                        });
                        trim_log(&state);
                        refresh_log_text(&ui, &state);
                    }
                    Event::Screen(rgba) => {
                        let buffer = SharedPixelBuffer::clone_from_slice(
                            &rgba,
                            SCREEN_WIDTH as u32,
                            SCREEN_HEIGHT as u32,
                        );
                        ui.set_screen_image(Image::from_rgba8(buffer));
                    }
                    Event::Streaming(streaming) => ui.set_streaming(streaming),
                }
            }
        },
    );

    // The timer is owned by the closure graph of the UI; keep it alive for the
    // whole event loop.
    std::mem::forget(timer);
}

fn apply_entries(ui: &AppWindow, state: &Rc<RefCell<UiState>>, entries: Vec<worker::AppEntryView>) {
    state.borrow_mut().entries = entries;
    ui.set_selected_app(-1);
    render_entries(ui, state);
}

/// Rebuilds the single app list from the entries and the active filter.
fn render_entries(ui: &AppWindow, state: &Rc<RefCell<UiState>>) {
    let (rows, updates, selected_count, all_selected) = {
        let mut state = state.borrow_mut();
        let filter = state.filter;
        let query = state.query.to_lowercase();

        let visible = state
            .entries
            .iter()
            .filter(|entry| match filter {
                1 => entry.installed_size.is_some(),
                2 => entry.state == worker::AppStateView::UpdateAvailable,
                _ => true,
            })
            .filter(|entry| query.is_empty() || entry.name.to_lowercase().contains(&query))
            .cloned()
            .collect::<Vec<_>>();

        let updates = state
            .entries
            .iter()
            .filter(|entry| entry.state == worker::AppStateView::UpdateAvailable)
            .count() as i32;

        let mut visible = visible;
        sort_entries(&mut visible, state.sort_column, state.sort_ascending);

        let updatable = visible
            .iter()
            .filter(|entry| entry.state == worker::AppStateView::UpdateAvailable)
            .map(entry_key)
            .collect::<Vec<_>>();
        let selected_count = state
            .selected
            .iter()
            .filter(|key| visible.iter().any(|entry| &entry_key(entry) == *key))
            .count() as i32;
        let all_selected =
            !updatable.is_empty() && updatable.iter().all(|key| state.selected.contains(key));

        let rows = visible
            .iter()
            .map(|entry| AppRow {
                checked: state.selected.contains(&entry_key(entry)),
                name: entry.name.clone().into(),
                version: entry
                    .version
                    .clone()
                    .unwrap_or_else(|| String::from("—"))
                    .into(),
                size: entry
                    .installed_size
                    .or(entry.catalog_size)
                    .map_or_else(|| String::from("—"), worker::human_bytes)
                    .into(),
                repository: if entry.source.is_empty() {
                    String::from("—").into()
                } else {
                    entry.source.clone().into()
                },
                update: entry_state_label(entry.state).into(),
                description: entry_description(entry).into(),
                status: entry_status(entry),
            })
            .collect::<Vec<_>>();

        state.visible = visible;
        (rows, updates, selected_count, all_selected)
    };

    ui.set_apps(ModelRc::new(VecModel::from(rows)));
    ui.set_update_count(updates);
    ui.set_selected_count(selected_count);
    ui.set_all_selected(all_selected);
}

fn entry_state_label(state: worker::AppStateView) -> &'static str {
    match state {
        worker::AppStateView::NotInstalled => "not installed",
        worker::AppStateView::Installed => "installed",
        worker::AppStateView::UpdateAvailable => "update available",
        worker::AppStateView::UpToDate => "up to date",
    }
}

/// Sorts the visible entries by the selected column.
fn sort_entries(entries: &mut [worker::AppEntryView], column: i32, ascending: bool) {
    entries.sort_by(|left, right| {
        let order = match column {
            1 => version_key(left.version.as_deref().unwrap_or(""))
                .cmp(&version_key(right.version.as_deref().unwrap_or(""))),
            2 => left
                .installed_size
                .or(left.catalog_size)
                .unwrap_or(0)
                .cmp(&right.installed_size.or(right.catalog_size).unwrap_or(0)),
            3 => left.source.to_lowercase().cmp(&right.source.to_lowercase()),
            4 => entry_status(left).cmp(&entry_status(right)),
            _ => left.name.to_lowercase().cmp(&right.name.to_lowercase()),
        };

        if ascending { order } else { order.reverse() }
    });
}

/// Numeric-aware version key (`1.10` sorts after `1.9`).
fn version_key(version: &str) -> Vec<u64> {
    version
        .split(['.', '-', '+'])
        .map(|part| part.parse::<u64>().unwrap_or(0))
        .collect()
}

fn entry_description(entry: &worker::AppEntryView) -> String {
    match &entry.remote_path {
        Some(path) => format!("{}\n\n{path}", entry.description),
        None => entry.description.clone(),
    }
}

fn entry_status(entry: &worker::AppEntryView) -> i32 {
    match entry.state {
        worker::AppStateView::NotInstalled => 0,
        worker::AppStateView::Installed => 1,
        worker::AppStateView::UpdateAvailable => 2,
        worker::AppStateView::UpToDate => 3,
    }
}

/// Stable key of an entry in the selection set.
fn entry_key(entry: &worker::AppEntryView) -> String {
    if entry.id.is_empty() {
        entry.remote_path.clone().unwrap_or_default()
    } else {
        entry.id.clone()
    }
}

fn entry_at(state: &Rc<RefCell<UiState>>, index: i32) -> Option<worker::AppEntryView> {
    if index < 0 {
        return None;
    }

    state.borrow().visible.get(index as usize).cloned()
}

fn apply_sources(ui: &AppWindow, state: &Rc<RefCell<UiState>>, sources: Vec<worker::SourceView>) {
    let labels = sources
        .iter()
        .map(|source| slint::SharedString::from(source.label.clone()))
        .collect::<Vec<_>>();

    {
        let mut state = state.borrow_mut();
        state.source_index = 0;
        state.sources = sources;
    }

    ui.set_app_source_labels(ModelRc::new(VecModel::from(labels)));
    ui.set_app_source_index(0);
}

fn current_source(state: &Rc<RefCell<UiState>>) -> Option<String> {
    let state = state.borrow();
    let source = state.sources.get(state.source_index.max(0) as usize)?;

    if source.id.is_empty() {
        None
    } else {
        Some(source.id.clone())
    }
}

/// Reports an exact verification in the log and the status bar.
fn log_verification(
    ui: &AppWindow,
    state: &Rc<RefCell<UiState>>,
    remote: &str,
    matches: bool,
    detail: &str,
    error: Option<String>,
) {
    let message = if let Some(error) = error {
        format!("Verification of {remote} failed: {error}")
    } else if matches {
        format!("{remote} matches the catalog build ({detail})")
    } else {
        format!("{remote} differs from the catalog build — update available")
    };

    state.borrow_mut().log.push(LogRecord {
        level: LogLevel::Info,
        target: String::from("flipper"),
        message: message.clone(),
        elapsed: logging::elapsed(),
    });
    trim_log(state);
    refresh_log_text(ui, state);

    ui.set_status(message.into());
}

fn apply_devices(ui: &AppWindow, state: &Rc<RefCell<UiState>>, devices: Vec<DeviceView>) {
    let count = devices.len();
    state.borrow_mut().devices = devices.clone();

    let rows = devices
        .into_iter()
        .map(|device| DeviceRow {
            title: device.title.into(),
            subtitle: device.subtitle.into(),
            detail: device.detail.into(),
        })
        .collect::<Vec<_>>();

    ui.set_devices(ModelRc::new(VecModel::from(rows)));
    ui.set_selected_index(0);
    ui.set_status(
        match count {
            0 => String::from("No device connected"),
            1 => String::from("1 device connected"),
            count => format!("{count} devices connected"),
        }
        .into(),
    );
}

fn apply_files(ui: &AppWindow, state: &Rc<RefCell<UiState>>, files: Vec<FileView>) {
    state.borrow_mut().files = files.clone();
    state.borrow_mut().selected_remote = None;

    let rows = files
        .into_iter()
        .map(|file| FileRow {
            name: file.name.into(),
            kind: file.kind.into(),
            size: file.size.into(),
        })
        .collect::<Vec<_>>();

    ui.set_files(ModelRc::new(VecModel::from(rows)));
    ui.set_selected_file(-1);
    ui.set_delete_armed(false);
}

fn apply_properties(
    ui: &AppWindow,
    state: &Rc<RefCell<UiState>>,
    properties: worker::FilePropertiesView,
) {
    if state.borrow().selected_remote.as_deref() != Some(properties.remote.as_str()) {
        return;
    }

    ui.set_prop_path(properties.remote.as_str().into());
    ui.set_prop_kind(properties.kind.as_str().into());
    ui.set_prop_size(
        if properties.kind == "directory" {
            String::from("-")
        } else {
            format!(
                "{} ({} bytes)",
                properties.size_human, properties.size_bytes
            )
        }
        .into(),
    );
    ui.set_prop_modified(
        properties
            .modified
            .map_or_else(
                || String::from("modified: unknown"),
                |value| format!("modified: {value}"),
            )
            .into(),
    );
    ui.set_prop_extra(
        properties
            .entries
            .map(|count| format!("entries: {count}"))
            .unwrap_or_default()
            .into(),
    );
    ui.set_prop_md5("".into());
    ui.set_prop_error(properties.error.unwrap_or_default().into());
}

fn selected_serial(ui: &AppWindow, state: &Rc<RefCell<UiState>>) -> Option<String> {
    let index = ui.get_selected_index().max(0) as usize;
    state
        .borrow()
        .devices
        .get(index)
        .and_then(|device| device.serial.clone())
}

fn file_at(state: &Rc<RefCell<UiState>>, index: i32) -> Option<FileView> {
    if index < 0 {
        return None;
    }

    state.borrow().files.get(index as usize).cloned()
}

fn navigate(
    ui: &AppWindow,
    state: &Rc<RefCell<UiState>>,
    sender: &UnboundedSender<Command>,
    path: String,
) {
    // Drop the old listing immediately: otherwise a fast click could combine
    // a stale entry with the new path and produce a non-existent path.
    state.borrow_mut().files.clear();
    state.borrow_mut().selected_remote = None;
    ui.set_files(ModelRc::new(VecModel::from(Vec::<FileRow>::new())));
    ui.set_selected_file(-1);
    ui.set_delete_armed(false);
    ui.set_remote_path(path.as_str().into());
    ui.set_prop_path("".into());
    ui.set_prop_kind("".into());
    ui.set_prop_size("".into());
    ui.set_prop_modified("".into());
    ui.set_prop_extra("".into());
    ui.set_prop_md5("".into());
    ui.set_prop_error("".into());

    let serial = selected_serial(ui, state);
    let _ = sender.send(Command::ListFiles { serial, path });
}

fn parent_path(current: &str) -> String {
    let current = current.trim_end_matches('/');

    if current.is_empty() || current == "/" {
        return String::from("/");
    }

    current
        .rsplit_once('/')
        .map(|(parent, _)| if parent.is_empty() { "/" } else { parent })
        .unwrap_or("/")
        .to_owned()
}

fn join_remote(base: &str, name: &str) -> String {
    if base.ends_with('/') {
        format!("{base}{name}")
    } else {
        format!("{base}/{name}")
    }
}

/// Default download directory (`~/Downloads`).
fn downloads_dir() -> String {
    std::env::var_os("HOME").map_or_else(String::new, |home| {
        PathBuf::from(home).join("Downloads").display().to_string()
    })
}

fn channel_name(index: i32) -> String {
    match index {
        1 => String::from("release-candidate"),
        2 => String::from("development"),
        _ => String::from("release"),
    }
}

fn config_path() -> Option<PathBuf> {
    let base = env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("HOME").map(|home| PathBuf::from(home).join(".config")))?;

    Some(base.join("flipper-companion").join("config.toml"))
}

fn load_channel() -> i32 {
    let Some(path) = config_path() else {
        return 0;
    };

    let Ok(contents) = fs::read_to_string(path) else {
        return 0;
    };

    for line in contents.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };

        if key.trim() == "channel" {
            let value = value.trim().trim_matches('"');

            return match value {
                "release-candidate" => 1,
                "development" => 2,
                _ => 0,
            };
        }
    }

    0
}

fn save_channel(index: i32) {
    let Some(path) = config_path() else {
        return;
    };

    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    let channel = channel_name(index);
    let _ = fs::write(path, format!("channel = \"{channel}\"\n"));
}

/// Keeps the in-memory buffer bounded so long sessions stay responsive.
fn trim_log(state: &Rc<RefCell<UiState>>) {
    let mut state = state.borrow_mut();
    let excess = state.log.len().saturating_sub(2000);

    if excess > 0 {
        state.log.drain(..excess);
    }
}

/// Rebuilds the visible lines from the buffer and the selected verbosity.
fn refresh_log_text(ui: &AppWindow, state: &Rc<RefCell<UiState>>) {
    let state = state.borrow();
    let highest = LogLevel::from_verbosity(state.log_verbosity);

    let lines = state
        .log
        .iter()
        .filter(|record| record.level <= highest)
        .map(|record| slint::SharedString::from(record.line()))
        .collect::<Vec<_>>();

    ui.set_log_lines(ModelRc::new(VecModel::from(lines)));
}

#[cfg(test)]
mod tests {
    use super::{join_remote, parent_path, version_key};

    #[test]
    fn sorts_versions_numerically() {
        let mut versions = vec![version_key("1.10"), version_key("1.9"), version_key("1.2")];
        versions.sort();

        assert_eq!(
            versions,
            vec![version_key("1.2"), version_key("1.9"), version_key("1.10")]
        );
    }

    #[test]
    fn computes_parent_paths() {
        assert_eq!(parent_path("/ext/apps"), "/ext");
        assert_eq!(parent_path("/ext/"), "/");
        assert_eq!(parent_path("/"), "/");
        assert_eq!(parent_path(""), "/");
    }

    #[test]
    fn joins_remote_paths() {
        assert_eq!(join_remote("/ext", "apps"), "/ext/apps");
        assert_eq!(join_remote("/ext/", "apps"), "/ext/apps");
        assert_eq!(join_remote("/", "ext"), "/ext");
    }
}
