//! USB identity and device information reported by the firmware.
//!
//! The Flipper Zero exposes two USB personalities under the same vendor ID:
//! a CDC-ACM virtual COM port used by the RPC protocol, and an STM32 DFU
//! interface used for recovery and full firmware images.

use crate::{
    error::CoreError,
    firmware::{FirmwareInfo, RadioInfo},
    storage::StorageInfo,
    version::{Channel, Version},
};

pub const FLIPPER_VENDOR_ID: u16 = 0x0483;

/// USB personality of a connected Flipper.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum UsbMode {
    /// CDC-ACM serial port exposing the protobuf RPC service.
    VirtualComPort,
    /// STM32 system-bootloader (DfuSe) mode.
    Dfu,
}

impl UsbMode {
    pub const VCP_PRODUCT_ID: u16 = 0x5740;
    pub const DFU_PRODUCT_ID: u16 = 0xdf11;

    #[must_use]
    pub const fn from_product_id(product_id: u16) -> Option<Self> {
        match product_id {
            Self::VCP_PRODUCT_ID => Some(Self::VirtualComPort),
            Self::DFU_PRODUCT_ID => Some(Self::Dfu),
            _ => None,
        }
    }

    #[must_use]
    pub const fn product_id(self) -> u16 {
        match self {
            Self::VirtualComPort => Self::VCP_PRODUCT_ID,
            Self::Dfu => Self::DFU_PRODUCT_ID,
        }
    }

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::VirtualComPort => "serial",
            Self::Dfu => "DFU",
        }
    }
}

/// A USB device discovered on the bus.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct UsbDevice {
    pub bus: String,
    pub address: u8,
    pub vendor_id: u16,
    pub product_id: u16,
    pub serial_number: Option<String>,
    pub manufacturer: Option<String>,
    pub product: Option<String>,
    pub mode: UsbMode,
}

impl UsbDevice {
    /// A stable key suitable for selection across re-enumerations.  The serial
    /// number is preferred because bus addresses change on every replug.
    #[must_use]
    pub fn key(&self) -> String {
        self.serial_number
            .clone()
            .unwrap_or_else(|| format!("{}:{}", self.bus, self.address))
    }

    #[must_use]
    pub fn is_dfu(&self) -> bool {
        self.mode == UsbMode::Dfu
    }
}

/// Hardware revision information burned into the device at the factory.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct HardwareInfo {
    pub version: String,
    pub target: String,
    pub body: String,
    pub connect: String,
    pub color: u8,
    pub region: u8,
}

/// Static information reported by a running firmware.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    pub name: String,
    pub hardware: HardwareInfo,
    pub firmware: FirmwareInfo,
    pub radio: Option<RadioInfo>,
}

impl DeviceInfo {
    /// Parses the modern `devinfo` property set (protobuf >= 0.14).
    pub fn from_properties(properties: &[(String, String)]) -> Result<Self, CoreError> {
        parse(properties, &PropertyKeys::DEVINFO)
    }

    /// Parses the legacy `system_device_info` key/value response.
    pub fn from_legacy_properties(properties: &[(String, String)]) -> Result<Self, CoreError> {
        parse(properties, &PropertyKeys::LEGACY)
    }
}

/// Protobuf protocol revision implemented by the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Default)]
pub struct ProtobufVersion {
    pub major: u32,
    pub minor: u32,
}

impl ProtobufVersion {
    #[must_use]
    pub const fn new(major: u32, minor: u32) -> Self {
        Self { major, minor }
    }

    /// Device info is exposed as a property starting with this version.
    #[must_use]
    pub const fn has_property_device_info(self) -> bool {
        self.major > 0 || self.minor >= 14
    }
}

impl std::fmt::Display for ProtobufVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// A probed device: USB identity plus everything learned over RPC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub usb: UsbDevice,
    pub info: DeviceInfo,
    pub protobuf: Option<ProtobufVersion>,
    pub storage: Option<StorageInfo>,
}

impl Device {
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.info.name
    }

    #[must_use]
    pub fn key(&self) -> String {
        self.usb.key()
    }
}

struct PropertyKeys {
    name: &'static str,
    firmware_version: &'static str,
    commit: &'static str,
    branch: &'static str,
    build_date: &'static str,
    api_major: &'static str,
    api_minor: &'static str,
    hardware_version: &'static str,
    target: &'static str,
    body: &'static str,
    connect: &'static str,
    color: &'static str,
    region: &'static str,
    radio_alive: &'static str,
    fus_version: (&'static str, &'static str, &'static str),
    stack_version: (&'static str, &'static str, &'static str),
    stack_type: &'static str,
}

impl PropertyKeys {
    const DEVINFO: Self = Self {
        name: "hardware.name",
        firmware_version: "firmware.version",
        commit: "firmware.commit.hash",
        branch: "firmware.branch.name",
        build_date: "firmware.build.date",
        api_major: "firmware.api.major",
        api_minor: "firmware.api.minor",
        hardware_version: "hardware.ver",
        target: "hardware.target",
        body: "hardware.body",
        connect: "hardware.connect",
        color: "hardware.color",
        region: "hardware.region.builtin",
        radio_alive: "radio.alive",
        fus_version: ("radio.fus.major", "radio.fus.minor", "radio.fus.sub"),
        stack_version: ("radio.stack.major", "radio.stack.minor", "radio.stack.sub"),
        stack_type: "radio.stack.type",
    };

    const LEGACY: Self = Self {
        name: "hardware_name",
        firmware_version: "firmware_version",
        commit: "firmware_commit",
        branch: "firmware_branch",
        build_date: "firmware_build_date",
        api_major: "",
        api_minor: "",
        hardware_version: "hardware_ver",
        target: "hardware_target",
        body: "hardware_body",
        connect: "hardware_connect",
        color: "hardware_color",
        region: "hardware_region",
        radio_alive: "radio_alive",
        fus_version: ("radio_fus_major", "radio_fus_minor", "radio_fus_sub"),
        stack_version: ("radio_stack_major", "radio_stack_minor", "radio_stack_sub"),
        stack_type: "radio_stack_type",
    };
}

fn lookup<'a>(properties: &'a [(String, String)], key: &str) -> Option<&'a str> {
    properties
        .iter()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value.as_str())
        .filter(|value| !value.is_empty())
}

fn parse(properties: &[(String, String)], keys: &PropertyKeys) -> Result<DeviceInfo, CoreError> {
    let name = lookup(properties, keys.name)
        .ok_or(CoreError::MissingProperty(keys.name))?
        .to_owned();

    let branch = lookup(properties, keys.branch)
        .unwrap_or_default()
        .to_owned();
    let api_version = match (
        lookup(properties, keys.api_major).and_then(|value| value.parse::<u16>().ok()),
        lookup(properties, keys.api_minor).and_then(|value| value.parse::<u16>().ok()),
    ) {
        (Some(major), Some(minor)) => Some(crate::firmware::ApiVersion { major, minor }),
        _ => None,
    };

    let firmware = FirmwareInfo {
        version: lookup(properties, keys.firmware_version).and_then(|v| v.parse().ok()),
        commit: lookup(properties, keys.commit)
            .unwrap_or_default()
            .to_owned(),
        channel: Channel::from_branch(&branch),
        branch,
        build_date: lookup(properties, keys.build_date).and_then(|v| v.parse().ok()),
        api_version,
    };

    let hardware = HardwareInfo {
        version: lookup(properties, keys.hardware_version)
            .unwrap_or_default()
            .to_owned(),
        target: prefixed(lookup(properties, keys.target), 'f'),
        body: prefixed(lookup(properties, keys.body), 'b'),
        connect: prefixed(lookup(properties, keys.connect), 'c'),
        color: lookup(properties, keys.color)
            .and_then(|v| v.parse().ok())
            .unwrap_or_default(),
        region: lookup(properties, keys.region)
            .and_then(|v| v.parse().ok())
            .unwrap_or_default(),
    };

    let radio = (lookup(properties, keys.radio_alive) == Some("true")).then(|| RadioInfo {
        fus_version: joined_version(properties, keys.fus_version),
        stack_version: joined_version(properties, keys.stack_version),
        stack_type: lookup(properties, keys.stack_type)
            .and_then(|v| v.parse().ok())
            .unwrap_or_default(),
    });

    Ok(DeviceInfo {
        name,
        hardware,
        firmware,
        radio,
    })
}

/// Prefixes a factory value with its category (`f18`, `b1`, `c3`) unless it
/// already carries one.
fn prefixed(value: Option<&str>, prefix: char) -> String {
    match value {
        Some(value) if value.starts_with(prefix) => value.to_owned(),
        Some(value) => format!("{prefix}{value}"),
        None => String::new(),
    }
}

fn joined_version(properties: &[(String, String)], keys: (&str, &str, &str)) -> String {
    let parts = [keys.0, keys.1, keys.2].map(|key| lookup(properties, key).unwrap_or("0"));
    format!("{}.{}.{}", parts[0], parts[1], parts[2])
}

impl DeviceInfo {
    /// Convenience accessor used by the UI to show a compact firmware label.
    #[must_use]
    pub fn firmware_label(&self) -> String {
        self.firmware.label()
    }

    /// Parses a version string, for callers that keep the raw value.
    #[must_use]
    pub fn parse_version(value: &str) -> Option<Version> {
        value.parse().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn modern_properties() -> Vec<(String, String)> {
        [
            ("hardware.name", "Flipper Zero"),
            ("hardware.ver", "12"),
            ("hardware.target", "18"),
            ("hardware.body", "1"),
            ("hardware.connect", "3"),
            ("hardware.color", "2"),
            ("hardware.region.builtin", "1"),
            ("firmware.version", "1.4.3-rc2"),
            ("firmware.commit.hash", "3060cee9"),
            ("firmware.branch.name", "1.4.3-rc2"),
            ("firmware.build.date", "08-11-2026"),
            ("firmware.api.major", "88"),
            ("firmware.api.minor", "2"),
            ("radio.alive", "true"),
            ("radio.fus.major", "1"),
            ("radio.fus.minor", "2"),
            ("radio.fus.sub", "3"),
            ("radio.stack.major", "1"),
            ("radio.stack.minor", "4"),
            ("radio.stack.sub", "0"),
            ("radio.stack.type", "1"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect()
    }

    #[test]
    fn parses_modern_device_info() {
        let info = DeviceInfo::from_properties(&modern_properties()).unwrap();

        assert_eq!(info.name, "Flipper Zero");
        assert_eq!(info.hardware.target, "f18");
        assert_eq!(info.hardware.body, "b1");
        assert_eq!(info.hardware.connect, "c3");
        assert_eq!(info.hardware.version, "12");
        assert_eq!(info.firmware.label(), "1.4.3-rc2");
        assert_eq!(info.firmware.channel, Channel::ReleaseCandidate);
        assert_eq!(info.firmware.build_date.unwrap().to_string(), "08-11-2026");
        assert_eq!(info.firmware.api_version.unwrap().to_string(), "88.2");
        assert!(!info.firmware.is_development());

        let radio = info.radio.unwrap();
        assert_eq!(radio.fus_version, "1.2.3");
        assert_eq!(radio.stack_version, "1.4.0");
        assert_eq!(radio.stack_type, 1);
    }

    #[test]
    fn parses_legacy_device_info() {
        let properties = [
            ("hardware_name", "Flipper Zero"),
            ("hardware_ver", "12"),
            ("hardware_target", "18"),
            ("firmware_version", "1.4.3"),
            ("firmware_commit", "abcdef123456"),
            ("firmware_branch", "1.4.3"),
            ("radio_alive", "false"),
        ]
        .into_iter()
        .map(|(key, value)| (key.to_owned(), value.to_owned()))
        .collect::<Vec<_>>();

        let info = DeviceInfo::from_legacy_properties(&properties).unwrap();

        assert_eq!(info.firmware.label(), "1.4.3");
        assert_eq!(info.firmware.commit, "abcdef123456");
        assert!(info.radio.is_none());
    }

    #[test]
    fn development_build_uses_short_commit() {
        let info = DeviceInfo {
            name: "Flipper Zero".into(),
            hardware: HardwareInfo::default(),
            firmware: FirmwareInfo {
                version: None,
                commit: "3060cee9cafe".into(),
                branch: "dev".into(),
                channel: Channel::Development,
                build_date: None,
                api_version: None,
            },
            radio: None,
        };

        assert_eq!(info.firmware.label(), "3060cee9");
        assert!(info.firmware.is_development());
    }

    #[test]
    fn missing_name_is_an_error() {
        let error = DeviceInfo::from_properties(&[]).unwrap_err();
        assert!(matches!(error, CoreError::MissingProperty("hardware.name")));
    }

    #[test]
    fn maps_product_ids() {
        assert_eq!(
            UsbMode::from_product_id(0x5740),
            Some(UsbMode::VirtualComPort)
        );
        assert_eq!(UsbMode::from_product_id(0xdf11), Some(UsbMode::Dfu));
        assert_eq!(UsbMode::from_product_id(0x1234), None);
    }
}
