//! USB discovery and the `nusb` transport used to talk to the STM32
//! bootloader.
//!
//! The transport is deliberately thin: it maps USB control transfers to the
//! [`Transport`] trait and leaves every protocol decision to
//! [`DfuDevice`](crate::protocol::DfuDevice).

use std::time::Duration;

use flipper_companion_core::{FLIPPER_VENDOR_ID, UsbDevice, UsbMode};
use nusb::{
    MaybeFuture,
    descriptors::language_id::US_ENGLISH,
    transfer::{ControlIn, ControlOut, ControlType, Recipient},
};

use crate::{error::DfuError, protocol::Transport};

const CONTROL_TIMEOUT: Duration = Duration::from_secs(5);
const DFU_INTERFACE: u8 = 0;
const DFU_FUNCTIONAL_DESCRIPTOR: u8 = 0x21;

/// Lists every Flipper Zero USB personality currently on the bus.
pub fn list_devices() -> Result<Vec<UsbDevice>, DfuError> {
    let devices = nusb::list_devices()
        .wait()
        .map_err(|error| DfuError::Usb(error.to_string()))?;

    Ok(devices.filter_map(to_domain).collect())
}

fn to_domain(info: nusb::DeviceInfo) -> Option<UsbDevice> {
    if info.vendor_id() != FLIPPER_VENDOR_ID {
        return None;
    }

    let mode = UsbMode::from_product_id(info.product_id())?;

    Some(UsbDevice {
        bus: info.bus_id().to_owned(),
        address: info.device_address(),
        vendor_id: info.vendor_id(),
        product_id: info.product_id(),
        serial_number: info.serial_number().map(str::to_owned),
        manufacturer: info.manufacturer_string().map(str::to_owned),
        product: info.product_string().map(str::to_owned),
        mode,
    })
}

/// A claimed DFU interface backed by `nusb`.
pub struct NusbTransport {
    device: nusb::Device,
    interface: nusb::Interface,
}

impl NusbTransport {
    /// Opens and claims the DFU interface of `selector`.
    ///
    /// The device is matched by serial number when available, so a replug that
    /// changes the bus address does not invalidate the selection.
    pub fn open(selector: &UsbDevice) -> Result<Self, DfuError> {
        if !selector.is_dfu() {
            return Err(DfuError::Usb(format!(
                "device {} is not in DFU mode",
                selector.key()
            )));
        }

        let info = nusb::list_devices()
            .wait()
            .map_err(|error| DfuError::Usb(error.to_string()))?
            .find(|candidate| matches_selector(candidate, selector))
            .ok_or_else(|| DfuError::Usb(String::from("DFU device is no longer connected")))?;

        let device = info
            .open()
            .wait()
            .map_err(|error| DfuError::Usb(error.to_string()))?;
        let interface = device
            .claim_interface(DFU_INTERFACE)
            .wait()
            .map_err(|error| DfuError::Usb(error.to_string()))?;

        Ok(Self { device, interface })
    }
}

fn matches_selector(candidate: &nusb::DeviceInfo, selector: &UsbDevice) -> bool {
    if candidate.vendor_id() != selector.vendor_id || candidate.product_id() != selector.product_id
    {
        return false;
    }

    match &selector.serial_number {
        Some(serial) => candidate.serial_number() == Some(serial.as_str()),
        None => {
            candidate.bus_id() == selector.bus && candidate.device_address() == selector.address
        }
    }
}

impl Transport for NusbTransport {
    fn set_alternate_setting(&mut self, alternate: u8) -> Result<(), DfuError> {
        self.interface
            .set_alt_setting(alternate)
            .wait()
            .map_err(|error| DfuError::Usb(error.to_string()))
    }

    fn control_in(
        &mut self,
        request: u8,
        value: u16,
        index: u16,
        length: usize,
    ) -> Result<Vec<u8>, DfuError> {
        let length = u16::try_from(length)
            .map_err(|_| DfuError::Protocol(String::from("control-IN length exceeds u16")))?;

        self.interface
            .control_in(
                ControlIn {
                    control_type: ControlType::Class,
                    recipient: Recipient::Interface,
                    request,
                    value,
                    index,
                    length,
                },
                CONTROL_TIMEOUT,
            )
            .wait()
            .map_err(|error| DfuError::Usb(error.to_string()))
    }

    fn control_out(
        &mut self,
        request: u8,
        value: u16,
        index: u16,
        data: &[u8],
    ) -> Result<(), DfuError> {
        self.interface
            .control_out(
                ControlOut {
                    control_type: ControlType::Class,
                    recipient: Recipient::Interface,
                    request,
                    value,
                    index,
                    data,
                },
                CONTROL_TIMEOUT,
            )
            .wait()
            .map_err(|error| DfuError::Usb(error.to_string()))
    }

    fn interface_string(&mut self, alternate: u8) -> Result<String, DfuError> {
        let index = {
            let configuration = self
                .device
                .active_configuration()
                .map_err(|error| DfuError::Usb(error.to_string()))?;

            configuration
                .interface_alt_settings()
                .find(|descriptor| {
                    descriptor.interface_number() == DFU_INTERFACE
                        && descriptor.alternate_setting() == alternate
                })
                .and_then(|descriptor| descriptor.string_index())
                .ok_or_else(|| {
                    DfuError::Protocol(format!(
                        "no string descriptor for alternate setting {alternate}"
                    ))
                })?
        };

        let language = self
            .device
            .get_string_descriptor_supported_languages(CONTROL_TIMEOUT)
            .wait()
            .ok()
            .and_then(|mut languages| languages.next())
            .unwrap_or(US_ENGLISH);

        self.device
            .get_string_descriptor(index, language, CONTROL_TIMEOUT)
            .wait()
            .map_err(|error| DfuError::Usb(error.to_string()))
    }

    fn functional_descriptor(&mut self, interface: u8) -> Result<Vec<u8>, DfuError> {
        let configuration = self
            .device
            .active_configuration()
            .map_err(|error| DfuError::Usb(error.to_string()))?;

        for alternate in configuration
            .interface_alt_settings()
            .filter(|descriptor| descriptor.interface_number() == interface)
        {
            for descriptor in alternate.descriptors() {
                let bytes = descriptor.as_bytes();
                if bytes.len() >= 2 && bytes[1] == DFU_FUNCTIONAL_DESCRIPTOR {
                    return Ok(bytes.to_vec());
                }
            }
        }

        Err(DfuError::Protocol(String::from(
            "DFU functional descriptor not found",
        )))
    }
}
