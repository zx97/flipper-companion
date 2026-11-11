//! Domain model for the Flipper Zero companion.
//!
//! This crate is deliberately free of I/O, async runtimes and UI toolkits.
//! Everything here can be unit-tested with plain data, which keeps the
//! protocol, USB and UI layers honest about the rules they implement.

pub mod device;
pub mod error;
pub mod firmware;
pub mod progress;
pub mod storage;
pub mod version;

pub use device::{
    Device, DeviceInfo, FLIPPER_VENDOR_ID, HardwareInfo, ProtobufVersion, UsbDevice, UsbMode,
};
pub use error::CoreError;
pub use firmware::{ApiVersion, FirmwareInfo, RadioInfo};
pub use progress::{Fraction, Progress};
pub use storage::{EntryKind, StorageEntry, StorageInfo};
pub use version::{BuildDate, Channel, PreRelease, Version, VersionError};
