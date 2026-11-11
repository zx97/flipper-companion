//! STM32WB55-specific DFU operations used by the recovery mode.
//!
//! The protocol is documented in ST AN5185.  Only the operations needed by
//! the Flipper recovery workflow are implemented: option bytes, OTP/factory
//! information, FUS state and radio firmware flashing.

use crate::{
    error::DfuError,
    protocol::{DfuDevice, Transport},
};

const OTP_ADDRESS: u32 = 0x1FFF_7000;
const OTP_MAX_SIZE: usize = 1024;
const OPTION_BYTES_ADDRESS: u32 = 0x1FFF_8000;
const OPTION_BYTES_SIZE: usize = 128;
const FUS_STATUS_ADDRESS: u32 = 0xFFFF_0054;
const FUS_STATUS_SIZE: usize = 2;
const SRAM2A_BASE: u32 = 0x2003_0000;
const FUS_INFO_TABLE_SIZE: usize = 56;
const FUS_MAGIC: u32 = 0x0A94_656B;
const RADIO_PAGE_SIZE: u32 = 0x1000;

/// DFU alternate settings exposed by the STM32WB55 bootloader.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Partition {
    Flash = 0,
    OptionBytes = 1,
    Otp = 2,
}

/// Boot mode selected by the option bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootMode {
    Normal,
    Recovery,
}

impl BootMode {
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Normal => "normal",
            Self::Recovery => "recovery",
        }
    }
}

/// Raw STM32WB55 option bytes with accessors for the boot-related fields.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OptionBytes {
    raw: [u8; OPTION_BYTES_SIZE],
}

impl OptionBytes {
    pub fn parse(data: &[u8]) -> Result<Self, DfuError> {
        let raw: [u8; OPTION_BYTES_SIZE] = data.try_into().map_err(|_| {
            DfuError::Protocol(format!(
                "expected {OPTION_BYTES_SIZE} option bytes, got {}",
                data.len()
            ))
        })?;

        Ok(Self { raw })
    }

    #[must_use]
    pub const fn raw(&self) -> &[u8; OPTION_BYTES_SIZE] {
        &self.raw
    }

    #[must_use]
    pub fn is_valid(&self) -> bool {
        self.raw != [0xFF; OPTION_BYTES_SIZE]
    }

    #[must_use]
    pub fn nboot0(&self) -> bool {
        bit(&self.raw, 0, 27)
    }

    #[must_use]
    pub fn nswboot0(&self) -> bool {
        bit(&self.raw, 0, 26)
    }

    #[must_use]
    pub fn boot_mode(&self) -> BootMode {
        if self.nboot0() && self.nswboot0() {
            BootMode::Normal
        } else {
            BootMode::Recovery
        }
    }

    pub fn set_boot_mode(&mut self, mode: BootMode) {
        let normal = mode == BootMode::Normal;
        set_bit(&mut self.raw, 0, 27, normal);
        set_bit(&mut self.raw, 4, 27, !normal);
        set_bit(&mut self.raw, 0, 26, normal);
        set_bit(&mut self.raw, 4, 26, !normal);
    }

    /// Secondary flash start address, in 4 KiB pages.
    #[must_use]
    pub fn sfsa(&self) -> u8 {
        // word9 is at offset 112; SFSA occupies its low eight bits.
        self.raw[112]
    }
}

fn bit(data: &[u8], offset: usize, bit: u32) -> bool {
    data[offset + (bit as usize) / 8] & (1 << (bit % 8)) != 0
}

fn set_bit(data: &mut [u8], offset: usize, bit: u32, value: bool) {
    let byte = &mut data[offset + (bit as usize) / 8];
    let mask = 1 << (bit % 8);

    if value {
        *byte |= mask;
    } else {
        *byte &= !mask;
    }
}

/// Factory information read from the OTP area.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FactoryInfo {
    pub format: u8,
    pub version: u8,
    pub target: u8,
    pub body: u8,
    pub connect: u8,
    pub color: u8,
    pub region: u8,
    pub timestamp: u32,
    pub name: String,
}

impl FactoryInfo {
    /// Parses the first 32 bytes of the OTP area.
    #[must_use]
    pub fn parse(data: &[u8]) -> Option<Self> {
        if data.len() < 32 || data.iter().all(|byte| *byte == 0xFF) {
            return None;
        }

        let magic = u16::from_le_bytes([data[0], data[1]]);
        let format = data[2];
        let timestamp = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);

        let (version, target, body, connect, color, region, name_offset) =
            if magic == 0xBABE && (format == 1 || format == 2) {
                // V1: color/region at offsets 12/13, name at 16.
                // V2: color/region at offsets 16/17, name at 24.
                let (color, region, name_offset) = if format == 2 {
                    (data[16], data[17], 24)
                } else {
                    (data[12], data[13], 16)
                };

                (
                    data[8],
                    data[9],
                    data[10],
                    data[11],
                    color,
                    region,
                    name_offset,
                )
            } else {
                // OTP v0 has no header: version/target/body/connect then a
                // timestamp and the name.
                let timestamp = u32::from_le_bytes([data[4], data[5], data[6], data[7]]);
                let _ = timestamp;
                (data[0], data[1], data[2], data[3], 0, 0, 8)
            };

        let raw_name = &data[name_offset..(name_offset + 8).min(data.len())];
        let end = raw_name
            .iter()
            .position(|byte| *byte == 0)
            .unwrap_or(raw_name.len());
        let name = String::from_utf8_lossy(&raw_name[..end]).to_string();

        if !name
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '.')
        {
            return None;
        }

        Some(Self {
            format,
            version,
            target,
            body,
            connect,
            color,
            region,
            timestamp,
            name,
        })
    }
}

/// FUS state and error bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FusState {
    pub status: u8,
    pub error: u8,
}

impl FusState {
    #[must_use]
    pub const fn status_name(self) -> &'static str {
        match self.status {
            0x00 => "idle",
            0x10 => "firmware upgrade ongoing",
            0x20 => "FUS upgrade ongoing",
            0x30 => "service ongoing",
            0xFF => "error occurred",
            _ => "unknown",
        }
    }

    #[must_use]
    pub const fn error_name(self) -> &'static str {
        match self.error {
            0x00 => "no error",
            0x01 => "image not found",
            0x02 => "image corrupt",
            0x03 => "image not authentic",
            0x04 => "not enough space",
            0x05 => "user abort",
            0x06 => "erase error",
            0x07 => "write error",
            0x08 => "ST tag not found",
            0x09 => "custom tag not found",
            0x0A => "auth key locked",
            0x11 => "rollback error",
            0xFE => "not running",
            _ => "unknown",
        }
    }
}

/// FUS and wireless stack versions read from the device information table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RadioVersions {
    pub fus: String,
    pub wireless: String,
}

impl RadioVersions {
    #[must_use]
    pub fn parse(table: &[u8]) -> Option<Self> {
        if table.len() < 24 {
            return None;
        }

        let magic = u32::from_le_bytes([table[0], table[1], table[2], table[3]]);

        let (fus_offset, wireless_offset) = if magic == FUS_MAGIC {
            (12, 20)
        } else {
            (4, 16)
        };

        if table.len() < wireless_offset + 4 {
            return None;
        }

        Some(Self {
            fus: parse_version(&table[fus_offset..fus_offset + 4]),
            wireless: parse_version(&table[wireless_offset..wireless_offset + 4]),
        })
    }
}

fn parse_version(bytes: &[u8]) -> String {
    // VersionInfo: build/branch nibbles, sub, minor, major.
    format!("{}.{}.{}", bytes[3], bytes[2], bytes[1])
}

/// High-level STM32WB55 operations.
pub struct Wb55<'a, T: Transport> {
    device: DfuDevice<'a, T>,
}

impl<'a, T: Transport> Wb55<'a, T> {
    #[must_use]
    pub fn new(device: DfuDevice<'a, T>) -> Self {
        Self { device }
    }

    pub fn read_option_bytes(&mut self) -> Result<OptionBytes, DfuError> {
        let data = self.device.upload(
            OPTION_BYTES_ADDRESS,
            OPTION_BYTES_SIZE,
            Partition::OptionBytes as u8,
        )?;

        OptionBytes::parse(&data)
    }

    pub fn write_option_bytes(&mut self, option_bytes: &OptionBytes) -> Result<(), DfuError> {
        self.device.download_bytes(
            OPTION_BYTES_ADDRESS,
            option_bytes.raw(),
            Partition::OptionBytes as u8,
        )
    }

    pub fn read_otp(&mut self) -> Result<Vec<u8>, DfuError> {
        self.device
            .upload(OTP_ADDRESS, OTP_MAX_SIZE, Partition::Otp as u8)
    }

    pub fn read_factory_info(&mut self) -> Result<Option<FactoryInfo>, DfuError> {
        let otp = self.read_otp()?;
        Ok(FactoryInfo::parse(&otp[..otp.len().min(32)]))
    }

    pub fn read_fus_state(&mut self) -> Result<FusState, DfuError> {
        let data =
            self.device
                .upload(FUS_STATUS_ADDRESS, FUS_STATUS_SIZE, Partition::Flash as u8)?;

        if data.len() != FUS_STATUS_SIZE {
            return Err(DfuError::Protocol(String::from(
                "device returned an invalid FUS status",
            )));
        }

        Ok(FusState {
            status: data[0],
            error: data[1],
        })
    }

    pub fn read_radio_versions(&mut self) -> Result<Option<RadioVersions>, DfuError> {
        let pointer = self.device.upload(SRAM2A_BASE, 4, Partition::Flash as u8)?;

        if pointer.len() != 4 {
            return Ok(None);
        }

        let address = u32::from_le_bytes([pointer[0], pointer[1], pointer[2], pointer[3]]);
        if address < SRAM2A_BASE {
            return Ok(None);
        }

        let table = self
            .device
            .upload(address, FUS_INFO_TABLE_SIZE, Partition::Flash as u8)?;

        Ok(RadioVersions::parse(&table))
    }

    pub fn fus_delete(&mut self) -> Result<(), DfuError> {
        self.device.download_raw(&[0x52])
    }

    pub fn fus_upgrade(&mut self) -> Result<(), DfuError> {
        self.device.download_raw(&[0x53])
    }

    pub fn start_wireless_stack(&mut self) -> Result<(), DfuError> {
        self.device.download_raw(&[0x5A])
    }

    pub fn leave(&mut self) -> Result<(), DfuError> {
        self.device.leave()
    }

    /// Erases and writes a radio/FUS image at an explicit address.
    pub fn flash_radio(&mut self, data: &[u8], address: u32) -> Result<u32, DfuError> {
        if data.is_empty() {
            return Err(DfuError::Protocol(String::from("radio image is empty")));
        }

        self.device.erase_range(address, data.len())?;
        self.device
            .download_bytes(address, data, Partition::Flash as u8)?;

        Ok(address)
    }

    /// Computes the radio image destination from the option bytes (`SFSA`).
    pub fn radio_destination(&mut self, image_size: usize) -> Result<u32, DfuError> {
        let option_bytes = self.read_option_bytes()?;
        let layout = self.device.memory_layout(Partition::Flash as u8)?;

        let size = u32::try_from(image_size)
            .map_err(|_| DfuError::Protocol(String::from("radio image is too large")))?;
        let address = layout
            .base_address
            .saturating_add(RADIO_PAGE_SIZE * u32::from(option_bytes.sfsa()))
            .saturating_sub(size)
            & !(RADIO_PAGE_SIZE - 1);

        Ok(address)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_and_sets_boot_mode() {
        let mut raw = [0u8; OPTION_BYTES_SIZE];
        let mut option_bytes = OptionBytes::parse(&raw).unwrap();

        assert_eq!(option_bytes.boot_mode(), BootMode::Recovery);

        option_bytes.set_boot_mode(BootMode::Normal);
        raw = *option_bytes.raw();

        assert!(option_bytes.nboot0());
        assert!(option_bytes.nswboot0());
        assert_eq!(option_bytes.boot_mode(), BootMode::Normal);

        // Normal and complement words must be inverted.
        assert!(!bit(&raw, 4, 27));
        assert!(!bit(&raw, 4, 26));

        option_bytes.set_boot_mode(BootMode::Recovery);
        assert_eq!(option_bytes.boot_mode(), BootMode::Recovery);
        raw = *option_bytes.raw();
        assert!(bit(&raw, 4, 27));
        assert!(bit(&raw, 4, 26));
    }

    #[test]
    fn reads_the_secondary_flash_start_address() {
        let mut raw = [0u8; OPTION_BYTES_SIZE];
        raw[112] = 42;

        let option_bytes = OptionBytes::parse(&raw).unwrap();
        assert_eq!(option_bytes.sfsa(), 42);
    }

    #[test]
    fn parses_factory_info_v2() {
        let mut raw = [0u8; 32];
        raw[..2].copy_from_slice(&0xBABEu16.to_le_bytes());
        raw[2] = 2;
        raw[3] = 0;
        raw[4..8].copy_from_slice(&1_700_000_000u32.to_le_bytes());
        raw[8] = 12; // version
        raw[9] = 18; // target
        raw[10] = 9; // body
        raw[11] = 6; // connect
        raw[16] = 2; // color
        raw[17] = 4; // region
        raw[24..31].copy_from_slice(b"Example");

        let info = FactoryInfo::parse(&raw).unwrap();

        assert_eq!(info.format, 2);
        assert_eq!(info.version, 12);
        assert_eq!(info.target, 18);
        assert_eq!(info.body, 9);
        assert_eq!(info.connect, 6);
        assert_eq!(info.color, 2);
        assert_eq!(info.region, 4);
        assert_eq!(info.name, "Example");
    }

    #[test]
    fn parses_radio_versions() {
        let mut table = [0u8; FUS_INFO_TABLE_SIZE];
        table[..4].copy_from_slice(&FUS_MAGIC.to_le_bytes());
        table[12..16].copy_from_slice(&[0x12, 3, 2, 1]); // 1.2.3
        table[20..24].copy_from_slice(&[0x34, 6, 5, 4]); // 4.5.6

        let versions = RadioVersions::parse(&table).unwrap();
        assert_eq!(versions.fus, "1.2.3");
        assert_eq!(versions.wireless, "4.5.6");
    }

    #[test]
    fn names_fus_states() {
        let state = FusState {
            status: 0xFF,
            error: 0xFE,
        };

        assert_eq!(state.status_name(), "error occurred");
        assert_eq!(state.error_name(), "not running");
    }
}
