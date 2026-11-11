//! Command-line grammar.  Only types live here; behaviour is in `commands`.

use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

#[derive(Debug, Parser)]
#[command(
    name = "flipper",
    version,
    about = "Memory-safe companion for the Flipper Zero",
    long_about = None,
    propagate_version = true
)]
pub struct Cli {
    /// Log level (error, warn, info, debug, trace)
    #[arg(long, global = true, default_value = "warn")]
    pub log_level: String,

    /// Emit machine-readable JSON where supported
    #[arg(long, global = true)]
    pub json: bool,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// List connected Flipper Zero devices
    Devices,

    /// Show detailed information about a device
    Info {
        /// Select a device by USB serial number
        #[arg(long)]
        serial: Option<String>,
    },

    /// Validate a DfuSe image without touching hardware
    Validate {
        /// Path to the .dfu file
        file: PathBuf,
    },

    /// Flash a DfuSe image (the device must be in DFU mode)
    Flash {
        /// Path to the .dfu file
        file: PathBuf,

        /// Select the DFU device by serial number
        #[arg(long)]
        serial: Option<String>,

        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
    },

    /// Back up internal storage to a `.tgz` archive
    Backup {
        /// Destination archive
        archive: PathBuf,

        /// Select a device by USB serial number
        #[arg(long)]
        serial: Option<String>,
    },

    /// Restore internal storage from a backup
    Restore {
        /// Source archive
        archive: PathBuf,

        /// Select a device by USB serial number
        #[arg(long)]
        serial: Option<String>,

        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
    },

    /// Erase internal storage and reset settings (factory reset)
    #[command(alias = "erase")]
    FactoryReset {
        /// Select a device by USB serial number
        #[arg(long)]
        serial: Option<String>,

        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
    },

    /// Provision radio region data (`/int/.region_data`)
    Region {
        /// Override the detected country (ISO 3166-1 alpha-2)
        #[arg(long)]
        country: Option<String>,

        /// Select a device by USB serial number
        #[arg(long)]
        serial: Option<String>,

        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
    },

    /// Send a virtual key press while the screen stream is active
    Input {
        /// Key to send
        #[arg(value_enum)]
        key: InputKeyArg,

        /// Select a device by USB serial number
        #[arg(long)]
        serial: Option<String>,
    },

    /// Show battery and power information
    Power {
        /// Select a device by USB serial number
        #[arg(long)]
        serial: Option<String>,
    },

    /// Play the device alert (sound and vibration)
    Alert {
        /// Select a device by USB serial number
        #[arg(long)]
        serial: Option<String>,
    },

    /// Desktop lock controls
    Desktop {
        #[command(subcommand)]
        command: DesktopCommand,

        /// Select a device by USB serial number
        #[arg(long)]
        serial: Option<String>,
    },

    /// Control the running application
    App {
        #[command(subcommand)]
        command: AppCommand,

        /// Select a device by USB serial number
        #[arg(long)]
        serial: Option<String>,
    },

    /// GPIO and 5 V OTG controls
    Gpio {
        #[command(subcommand)]
        command: GpioCommand,

        /// Select a device by USB serial number
        #[arg(long)]
        serial: Option<String>,
    },

    /// STM32WB55 recovery-mode operations (device must be in DFU mode)
    Recovery {
        #[command(subcommand)]
        command: RecoveryCommand,

        /// Select a DFU device by serial number
        #[arg(long)]
        serial: Option<String>,
    },

    /// Remote storage operations
    Storage {
        #[command(subcommand)]
        command: StorageCommand,

        /// Select a device by USB serial number
        #[arg(long)]
        serial: Option<String>,
    },

    /// Reboot the device
    Reboot {
        /// Target mode
        #[arg(long, value_enum, default_value_t = RebootMode::Os)]
        mode: RebootMode,

        /// Select a device by USB serial number
        #[arg(long)]
        serial: Option<String>,
    },

    /// Capture one frame from the device screen
    Screen {
        /// Output format
        #[arg(long, value_enum, default_value_t = ScreenFormat::Ascii)]
        format: ScreenFormat,

        /// Write to a file instead of stdout
        #[arg(long)]
        output: Option<PathBuf>,

        /// Select a device by USB serial number
        #[arg(long)]
        serial: Option<String>,
    },

    /// Firmware update directory operations
    Firmware {
        #[command(subcommand)]
        command: FirmwareCommand,
    },

    /// Browse and install apps from the Flipper catalog
    Apps {
        #[command(subcommand)]
        command: AppsCommand,
    },

    /// Manage a project-side `flip.toml` (flippy-compatible)
    Project {
        #[command(subcommand)]
        command: ProjectCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum AppsCommand {
    /// List catalog applications
    List {
        /// Hardware target (f7, f18). Detected from the device when omitted.
        #[arg(long)]
        target: Option<String>,
        /// Firmware API version (e.g. 88.2). Detected from the device when omitted.
        #[arg(long)]
        api: Option<String>,
        /// Filter by category name (Sub-GHz, GPIO, ...)
        #[arg(long)]
        category: Option<String>,
        /// Search text
        #[arg(long)]
        query: Option<String>,
        /// Only this source: `official` or a JSON index URL
        #[arg(long)]
        source: Option<String>,
    },
    /// Install an application by id
    Install {
        /// Catalog id (from `apps list`)
        id: String,
        /// Hardware target (f7, f18). Detected from the device when omitted.
        #[arg(long)]
        target: Option<String>,
        /// Firmware API version. Detected from the device when omitted.
        #[arg(long)]
        api: Option<String>,
        /// Select a device by USB serial number
        #[arg(long)]
        serial: Option<String>,
        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
    },
    /// List the apps installed on the SD card
    Installed {
        /// Select a device by USB serial number
        #[arg(long)]
        serial: Option<String>,
    },
    /// List the installed apps whose catalog build differs
    Outdated {
        /// Select a device by USB serial number
        #[arg(long)]
        serial: Option<String>,
        /// Hardware target (f7, f18). Detected from the device when omitted.
        #[arg(long)]
        target: Option<String>,
        /// Firmware API version. Detected from the device when omitted.
        #[arg(long)]
        api: Option<String>,
    },
    /// Remove an installed app by name or path
    Uninstall {
        /// App name (`cube_timer`) or remote path (`/ext/apps/Tools/cube_timer.fap`)
        name: String,
        /// Select a device by USB serial number
        #[arg(long)]
        serial: Option<String>,
        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
    },
    /// Manage custom JSON app sources
    Sources {
        #[command(subcommand)]
        command: AppsSourcesCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum AppsSourcesCommand {
    /// List configured JSON sources
    List,
    /// Add a JSON source URL
    Add { url: String },
}

#[derive(Debug, Subcommand)]
pub enum ProjectCommand {
    /// Create a project directory with `flip.toml` and `store/`
    New {
        /// Path used to store project files
        path: PathBuf,
    },
    /// Update `flip.toml`
    Map {
        #[command(subcommand)]
        command: ProjectMapCommand,
    },
    /// Manage the local `store/` directory and fetch plan
    Store {
        #[command(subcommand)]
        command: ProjectStoreCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProjectMapCommand {
    /// Edit repository definitions in `flip.toml`
    Repo {
        #[command(subcommand)]
        command: ProjectRepoCommand,
    },
    /// Edit the firmware source in `flip.toml`
    Firmware {
        #[command(subcommand)]
        command: ProjectFirmwareCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProjectRepoCommand {
    /// Add a repository source to `flip.toml`
    Add {
        /// Repository URL, SCP-style Git remote, or local path
        source: String,
        /// Optional local identifier for the repository
        name: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProjectFirmwareCommand {
    /// Set the firmware source used in `flip.toml`
    Set {
        /// Preset (`ofw`, `unleashed`, `momentum`, `rogue-free`) or tarball path/URL
        source: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum ProjectStoreCommand {
    /// Create the local `store/` directory if it does not exist
    Create,
    /// Validate sources and write `store/fetch-plan.txt`
    Fetch {
        /// Record whether sparse or otherwise optimized fetching is intended
        #[arg(short, long)]
        optimize: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum StorageCommand {
    /// List a directory
    Ls { path: String },
    /// Show filesystem capacity
    Info { path: String },
    /// Show metadata for one path
    Stat { path: String },
    /// Read a remote file
    Read {
        path: String,
        /// Write to a file instead of stdout
        #[arg(long)]
        output: Option<PathBuf>,
    },
    /// Write a local file to the device
    Write { local: PathBuf, remote: String },
    /// Create a directory
    Mkdir { path: String },
    /// Remove a file or directory
    Rm {
        path: String,
        /// Remove directories recursively
        #[arg(long)]
        recursive: bool,
        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
    },
    /// Rename or move a path
    Mv { from: String, to: String },
    /// Compute the MD5 checksum of a remote file
    Md5 { path: String },
    /// Show the device-side modification timestamp
    Timestamp { path: String },
    /// Show full properties (type, size, timestamp, optional MD5)
    Properties {
        path: String,
        /// Also compute the MD5 checksum
        #[arg(long)]
        md5: bool,
    },
    /// Extract a tar archive stored on the device
    TarExtract {
        tar_path: String,
        out_path: String,
        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum FirmwareCommand {
    /// List the releases published in the update directory
    List {
        /// Restrict output to one channel
        #[arg(long)]
        channel: Option<String>,
    },

    /// Download an artifact without flashing it
    Download {
        /// Release channel
        #[arg(long, default_value = "release")]
        channel: String,

        /// Artifact type (full_dfu, update_tgz, ...)
        #[arg(long, default_value = "full_dfu")]
        kind: String,

        /// Hardware target (f18, f7, any)
        #[arg(long)]
        target: Option<String>,

        /// Destination directory
        #[arg(long, default_value = ".")]
        output: PathBuf,
    },

    /// Download an update bundle, upload it to the SD card and start the updater
    Install {
        /// Release channel
        #[arg(long, default_value = "release")]
        channel: String,

        /// Hardware target (defaults to the connected device, then f18)
        #[arg(long)]
        target: Option<String>,

        /// Select a device by USB serial number
        #[arg(long)]
        serial: Option<String>,

        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
    },

    /// Download and flash the newest full firmware image
    Flash {
        /// Release channel
        #[arg(long, default_value = "release")]
        channel: String,

        /// Hardware target (defaults to the connected device, then f18)
        #[arg(long)]
        target: Option<String>,

        /// Select a device by USB serial number
        #[arg(long)]
        serial: Option<String>,

        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum InputKeyArg {
    Up,
    Down,
    Left,
    Right,
    Ok,
    Back,
}

#[derive(Debug, Subcommand)]
pub enum RecoveryCommand {
    /// Read factory information, option bytes, FUS state and radio versions
    Info,
    /// Dump the raw option bytes
    OptionBytes,
    /// Leave recovery mode and reboot the device
    Exit,
    /// Write the boot-mode bits in the option bytes
    SetBootMode {
        #[arg(value_enum)]
        mode: BootModeArg,
        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
    },
    /// Send a FUS command
    Fus {
        #[arg(value_enum)]
        action: FusActionArg,
        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
    },
    /// Flash a radio (wireless stack) image
    FlashRadio {
        file: PathBuf,
        /// Override the computed destination address
        #[arg(long)]
        address: Option<String>,
        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
    },
    /// Flash a FUS image at an explicit address
    FlashFus {
        file: PathBuf,
        address: String,
        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum BootModeArg {
    Normal,
    Recovery,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum FusActionArg {
    Delete,
    Upgrade,
    StartWirelessStack,
}

#[derive(Debug, Subcommand)]
pub enum DesktopCommand {
    /// Show whether the desktop is locked
    Status,
    /// Unlock the desktop
    Unlock,
    /// Watch lock state broadcasts for a number of seconds
    Watch {
        #[arg(long, default_value_t = 5)]
        seconds: u64,
    },
}

#[derive(Debug, Subcommand)]
pub enum AppCommand {
    /// Start an application by name
    Start {
        name: String,
        /// Optional application arguments
        #[arg(long, default_value = "")]
        args: String,
    },
    /// Ask the running application to exit
    Exit,
    /// Load a file in the running application
    Load { path: String },
    /// Send a button press event
    Press { args: String, index: i32 },
    /// Send a button release event
    Release,
    /// Show whether the loader is locked
    Status,
    /// Show the last application error
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum GpioModeArg {
    Input,
    Output,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum GpioPullArg {
    Up,
    Down,
    Floating,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum GpioOtgArg {
    On,
    Off,
}

#[derive(Debug, Subcommand)]
pub enum GpioCommand {
    /// Read a pin (the pin must already be an input)
    Read { pin: String },
    /// Show the configured mode of a pin
    Mode { pin: String },
    /// Configure a pin as input or output
    SetMode {
        pin: String,
        #[arg(value_enum)]
        mode: GpioModeArg,
        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
    },
    /// Drive an output pin
    Write {
        pin: String,
        value: u32,
        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
    },
    /// Set the pull resistor of an input pin
    Pull {
        pin: String,
        #[arg(value_enum)]
        pull: GpioPullArg,
        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
    },
    /// Show the 5 V OTG state
    Otg,
    /// Enable or disable the 5 V OTG output
    SetOtg {
        #[arg(value_enum)]
        mode: GpioOtgArg,
        /// Do not ask for confirmation
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum RebootMode {
    Os,
    Dfu,
    Update,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ScreenFormat {
    Ascii,
    Pbm,
}
