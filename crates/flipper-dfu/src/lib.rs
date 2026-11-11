//! DfuSe parsing, STM32 DFU flashing and USB discovery.
//!
//! ```no_run
//! use flipper_companion_dfu::{DfuseFile, DfuDevice, NusbTransport, list_devices};
//!
//! # fn main() -> Result<(), Box<dyn std::error::Error>> {
//! let image = DfuseFile::from_path("firmware.dfu")?;
//! let device = list_devices()?.into_iter().find(|device| device.is_dfu())
//!     .ok_or("no DFU device connected")?;
//!
//! let mut transport = NusbTransport::open(&device)?;
//! DfuDevice::new(&mut transport).flash(&image, |progress| {
//!     eprintln!("{:?}: {:.1}%", progress.stage, progress.fraction() * 100.0);
//! })?;
//! # Ok(())
//! # }
//! ```

pub mod crc;
pub mod dfuse;
pub mod error;
pub mod layout;
pub mod protocol;
pub mod stm32wb55;
pub mod usb;

pub use dfuse::{DfuseFile, Element, Suffix, Target};
pub use error::DfuError;
pub use layout::{MemoryLayout, PageBank};
pub use protocol::{DfuDevice, DfuState, FlashProgress, FlashStage, Status, StatusCode, Transport};
pub use stm32wb55::{BootMode, FactoryInfo, FusState, OptionBytes, Partition, RadioVersions, Wb55};
pub use usb::{NusbTransport, list_devices};
