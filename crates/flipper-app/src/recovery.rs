//! STM32WB55 recovery-mode operations.
//!
//! These functions require the device to be in DFU mode.  They are advanced:
//! writing option bytes or radio firmware can leave the device unusable if
//! interrupted, so every write is exposed behind an explicit CLI confirmation.

use std::path::Path;

use flipper_companion_dfu::{
    BootMode, DfuDevice, DfuError, FactoryInfo, FusState, NusbTransport, OptionBytes,
    RadioVersions, Wb55, list_devices,
};

use crate::{AppError, Companion};

/// Snapshot of the recovery-relevant device information.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryInfo {
    pub factory: Option<FactoryInfo>,
    pub boot_mode: BootMode,
    pub option_bytes_valid: bool,
    pub option_bytes: OptionBytes,
    pub fus: FusState,
    pub versions: Option<RadioVersions>,
}

/// FUS commands exposed by the bootloader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FusAction {
    Delete,
    Upgrade,
    StartWirelessStack,
}

impl Companion {
    /// Reads factory information, option bytes, FUS state and radio versions.
    pub async fn recovery_info(&self, serial: Option<String>) -> Result<RecoveryInfo, AppError> {
        tokio::task::spawn_blocking(move || {
            with_wb55(serial.as_deref(), |device| {
                let factory = device.read_factory_info()?;
                let option_bytes = device.read_option_bytes()?;
                let fus = device.read_fus_state()?;
                let versions = device.read_radio_versions()?;

                Ok(RecoveryInfo {
                    factory,
                    boot_mode: option_bytes.boot_mode(),
                    option_bytes_valid: option_bytes.is_valid(),
                    option_bytes,
                    fus,
                    versions,
                })
            })
        })
        .await
        .map_err(|error| AppError::Join(error.to_string()))?
    }

    /// Writes the boot-mode bits in the option bytes.
    pub async fn recovery_set_boot_mode(
        &self,
        serial: Option<String>,
        mode: BootMode,
    ) -> Result<(), AppError> {
        tokio::task::spawn_blocking(move || {
            with_wb55(serial.as_deref(), |device| {
                let mut option_bytes = device.read_option_bytes()?;
                option_bytes.set_boot_mode(mode);
                device.write_option_bytes(&option_bytes)
            })
        })
        .await
        .map_err(|error| AppError::Join(error.to_string()))?
    }

    /// Leaves recovery mode and reboots the device.
    pub async fn recovery_exit(&self, serial: Option<String>) -> Result<(), AppError> {
        tokio::task::spawn_blocking(move || with_wb55(serial.as_deref(), |device| device.leave()))
            .await
            .map_err(|error| AppError::Join(error.to_string()))?
    }

    /// Sends a FUS command.
    pub async fn recovery_fus(
        &self,
        serial: Option<String>,
        action: FusAction,
    ) -> Result<(), AppError> {
        tokio::task::spawn_blocking(move || {
            with_wb55(serial.as_deref(), |device| match action {
                FusAction::Delete => device.fus_delete(),
                FusAction::Upgrade => device.fus_upgrade(),
                FusAction::StartWirelessStack => device.start_wireless_stack(),
            })
        })
        .await
        .map_err(|error| AppError::Join(error.to_string()))?
    }

    /// Flashes a radio (wireless stack) image.
    ///
    /// When `address` is `None`, the destination is derived from the `SFSA`
    /// option byte and the image size, like the official tool.
    pub async fn recovery_flash_radio(
        &self,
        serial: Option<String>,
        image: &Path,
        address: Option<u32>,
    ) -> Result<u32, AppError> {
        let data = std::fs::read(image)?;
        tokio::task::spawn_blocking(move || {
            with_wb55(serial.as_deref(), |device| {
                let address = match address {
                    Some(address) => address,
                    None => device.radio_destination(data.len())?,
                };

                device.flash_radio(&data, address)
            })
        })
        .await
        .map_err(|error| AppError::Join(error.to_string()))?
    }

    /// Flashes a FUS image at an explicit address.
    pub async fn recovery_flash_fus(
        &self,
        serial: Option<String>,
        image: &Path,
        address: u32,
    ) -> Result<(), AppError> {
        let data = std::fs::read(image)?;

        tokio::task::spawn_blocking(move || {
            with_wb55(serial.as_deref(), |device| {
                device.flash_radio(&data, address).map(|_| ())
            })
        })
        .await
        .map_err(|error| AppError::Join(error.to_string()))?
    }
}

fn with_wb55<R>(
    serial: Option<&str>,
    operation: impl FnOnce(&mut Wb55<'_, NusbTransport>) -> Result<R, DfuError>,
) -> Result<R, AppError> {
    let devices = list_devices()?;

    let device = devices
        .into_iter()
        .find(|device| {
            device.is_dfu()
                && serial.is_none_or(|serial| device.serial_number.as_deref() == Some(serial))
        })
        .or_else(|| {
            list_devices()
                .ok()?
                .into_iter()
                .find(|device| device.is_dfu())
        })
        .ok_or(AppError::NoDfuDevice)?;

    let mut transport = NusbTransport::open(&device)?;
    let mut wb55 = Wb55::new(DfuDevice::new(&mut transport));

    operation(&mut wb55).map_err(AppError::from)
}
