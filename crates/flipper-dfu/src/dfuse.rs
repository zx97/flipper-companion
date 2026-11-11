//! Parser for ST DfuSe (`.dfu`) firmware containers.
//!
//! A DfuSe file is a small binary container:
//!
//! ```text
//! +--------------------+ 11 bytes   "DfuSe", version, total size, target count
//! | target 0           | 274 bytes  "Target", alt setting, name, element count
//! |   element 0        | 8 + N      load address, length, payload
//! |   element 1 ...    |
//! | target 1 ...       |
//! +--------------------+ 16 bytes   device IDs, "UFD", length, CRC
//! ```
//!
//! Parsing is strict: every length is checked against the remaining bytes and
//! the trailing CRC is verified before a single USB transfer is attempted.

use std::path::Path;

use crate::{crc::crc32, error::DfuError};

const PREFIX_LEN: usize = 11;
const SUFFIX_LEN: usize = 16;
const TARGET_NAME_LEN: usize = 255;
const TARGET_HEADER_LEN: usize = 6 + 1 + 4 + TARGET_NAME_LEN + 4 + 4;

/// A validated DfuSe image.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DfuseFile {
    pub version: u8,
    pub image_size: u32,
    pub targets: Vec<Target>,
    pub suffix: Suffix,
    pub crc: u32,
}

/// One target (one STM32 flash bank / DFU alternate setting).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Target {
    pub alternate_setting: u8,
    pub name: String,
    pub declared_size: u32,
    pub elements: Vec<Element>,
}

/// A contiguous chunk of payload with its load address.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Element {
    pub address: u32,
    pub data: Vec<u8>,
}

/// USB identifiers and CRC stored in the file trailer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Suffix {
    pub device_version: u16,
    pub product_id: u16,
    pub vendor_id: u16,
    pub dfu_version: u16,
    pub crc: u32,
}

impl DfuseFile {
    /// Parses and fully validates a DfuSe image held in memory.
    pub fn parse(bytes: &[u8]) -> Result<Self, DfuError> {
        if bytes.len() < PREFIX_LEN + SUFFIX_LEN {
            return Err(DfuError::Truncated {
                needed: PREFIX_LEN + SUFFIX_LEN,
                available: bytes.len(),
            });
        }

        let mut cursor = Cursor::new(bytes);

        if cursor.take(5)? != b"DfuSe" {
            return Err(DfuError::BadSignature);
        }

        let version = cursor.u8()?;
        if version != 1 {
            return Err(DfuError::UnsupportedVersion(version));
        }

        let image_size = cursor.u32()?;
        let target_count = cursor.u8()?;

        if target_count == 0 {
            return Err(DfuError::NoTargets);
        }

        let actual_size = bytes.len() - SUFFIX_LEN;
        if image_size as usize != actual_size {
            return Err(DfuError::SizeMismatch {
                declared: image_size,
                actual: bytes.len(),
            });
        }

        let mut targets = Vec::with_capacity(usize::from(target_count));

        for index in 0..usize::from(target_count) {
            targets.push(Target::parse(&mut cursor, index)?);
        }

        let suffix = Suffix::parse(&mut cursor)?;

        if cursor.remaining() != 0 {
            return Err(DfuError::SizeMismatch {
                declared: image_size,
                actual: bytes.len(),
            });
        }

        let computed = crc32(&bytes[..bytes.len() - 4]);
        if computed != suffix.crc {
            return Err(DfuError::CrcMismatch {
                stored: suffix.crc,
                computed,
            });
        }

        Ok(Self {
            version,
            image_size,
            targets,
            suffix,
            crc: computed,
        })
    }

    /// Reads and validates a `.dfu` file from disk.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, DfuError> {
        let bytes = std::fs::read(path)?;
        Self::parse(&bytes)
    }

    /// Total payload bytes across every element of every target.
    #[must_use]
    pub fn payload_size(&self) -> u64 {
        self.targets
            .iter()
            .flat_map(|target| &target.elements)
            .map(|element| element.data.len() as u64)
            .sum()
    }

    /// Number of elements across every target.
    #[must_use]
    pub fn element_count(&self) -> usize {
        self.targets
            .iter()
            .map(|target| target.elements.len())
            .sum()
    }
}

impl Target {
    fn parse(cursor: &mut Cursor<'_>, index: usize) -> Result<Self, DfuError> {
        if cursor.remaining() < TARGET_HEADER_LEN {
            return Err(DfuError::Truncated {
                needed: TARGET_HEADER_LEN,
                available: cursor.remaining(),
            });
        }

        if cursor.take(6)? != b"Target" {
            return Err(DfuError::InvalidTarget {
                index,
                reason: String::from("missing target signature"),
            });
        }

        let alternate_setting = cursor.u8()?;

        let named = cursor.u32()?;
        if named > 1 {
            return Err(DfuError::InvalidTarget {
                index,
                reason: format!("invalid target-name flag {named}"),
            });
        }

        let raw_name = cursor.take(TARGET_NAME_LEN)?;
        let name = String::from_utf8_lossy(trim_nul(raw_name))
            .trim()
            .to_owned();

        let declared_size = cursor.u32()?;
        let element_count = cursor.u32()?;

        // Each element consumes at least its 8-byte header.
        if element_count as usize > cursor.remaining() / 8 {
            return Err(DfuError::InvalidTarget {
                index,
                reason: format!("implausible element count {element_count}"),
            });
        }

        let mut elements = Vec::with_capacity(element_count as usize);
        let mut computed_size = 0u64;

        for _ in 0..element_count {
            let address = cursor.u32()?;
            let length = cursor.u32()? as usize;
            let data = cursor.take(length)?.to_vec();

            computed_size += 8 + length as u64;
            elements.push(Element { address, data });
        }

        if computed_size != u64::from(declared_size) {
            return Err(DfuError::InvalidTarget {
                index,
                reason: format!(
                    "declared size {declared_size} does not match elements ({computed_size})"
                ),
            });
        }

        Ok(Self {
            alternate_setting,
            name,
            declared_size,
            elements,
        })
    }
}

impl Suffix {
    fn parse(cursor: &mut Cursor<'_>) -> Result<Self, DfuError> {
        let device_version = cursor.u16()?;
        let product_id = cursor.u16()?;
        let vendor_id = cursor.u16()?;
        let dfu_version = cursor.u16()?;

        let signature = cursor.take(3)?;
        if signature != b"UFD" {
            return Err(DfuError::BadSignature);
        }

        let length = cursor.u8()?;
        if usize::from(length) != SUFFIX_LEN {
            return Err(DfuError::BadSignature);
        }

        let crc = cursor.u32()?;

        Ok(Self {
            device_version,
            product_id,
            vendor_id,
            dfu_version,
            crc,
        })
    }
}

fn trim_nul(bytes: &[u8]) -> &[u8] {
    match bytes.iter().position(|&byte| byte == 0) {
        Some(end) => &bytes[..end],
        None => bytes,
    }
}

struct Cursor<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    fn remaining(&self) -> usize {
        self.data.len() - self.position
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8], DfuError> {
        let end = self
            .position
            .checked_add(length)
            .ok_or(DfuError::Truncated {
                needed: usize::MAX,
                available: self.remaining(),
            })?;

        if end > self.data.len() {
            return Err(DfuError::Truncated {
                needed: end,
                available: self.data.len(),
            });
        }

        let slice = &self.data[self.position..end];
        self.position = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8, DfuError> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16, DfuError> {
        let bytes = self.take(2)?;
        Ok(u16::from_le_bytes([bytes[0], bytes[1]]))
    }

    fn u32(&mut self) -> Result<u32, DfuError> {
        let bytes = self.take(4)?;
        Ok(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn build_image(payload: &[u8], address: u32) -> Vec<u8> {
        let mut body = Vec::new();

        // Target header.
        body.extend_from_slice(b"Target");
        body.push(0); // alternate setting
        body.extend_from_slice(&1u32.to_le_bytes()); // named
        let mut name = [0u8; TARGET_NAME_LEN];
        name[..10].copy_from_slice(b"Test flash");
        body.extend_from_slice(&name);
        body.extend_from_slice(&(8u32 + payload.len() as u32).to_le_bytes());
        body.extend_from_slice(&1u32.to_le_bytes()); // one element
        body.extend_from_slice(&address.to_le_bytes());
        body.extend_from_slice(&(payload.len() as u32).to_le_bytes());
        body.extend_from_slice(payload);

        let mut image = Vec::new();
        image.extend_from_slice(b"DfuSe");
        image.push(1);
        image.extend_from_slice(&((PREFIX_LEN + body.len()) as u32).to_le_bytes());
        image.push(1);
        image.extend_from_slice(&body);

        // Suffix: the CRC covers everything before the CRC field itself.
        image.extend_from_slice(&0x0220u16.to_le_bytes());
        image.extend_from_slice(&0xdf11u16.to_le_bytes());
        image.extend_from_slice(&0x0483u16.to_le_bytes());
        image.extend_from_slice(&0x011au16.to_le_bytes());
        image.extend_from_slice(b"UFD");
        image.push(SUFFIX_LEN as u8);
        let crc = crc32(&image);
        image.extend_from_slice(&crc.to_le_bytes());
        image
    }

    #[test]
    fn parses_a_well_formed_image() {
        let payload = vec![0xA5; 1024];
        let bytes = build_image(&payload, 0x0800_0000);

        let image = DfuseFile::parse(&bytes).unwrap();

        assert_eq!(image.version, 1);
        assert_eq!(image.targets.len(), 1);
        assert_eq!(image.targets[0].name, "Test flash");
        assert_eq!(image.targets[0].alternate_setting, 0);
        assert_eq!(image.targets[0].elements[0].address, 0x0800_0000);
        assert_eq!(image.targets[0].elements[0].data, payload);
        assert_eq!(image.payload_size(), 1024);
        assert_eq!(image.element_count(), 1);
        assert_eq!(image.suffix.vendor_id, 0x0483);
        assert_eq!(image.suffix.product_id, 0xdf11);
    }

    #[test]
    fn rejects_a_corrupted_payload() {
        let mut bytes = build_image(&[0x11; 64], 0x0800_0000);
        bytes[40] ^= 0xFF;

        assert!(matches!(
            DfuseFile::parse(&bytes),
            Err(DfuError::CrcMismatch { .. })
        ));
    }

    #[test]
    fn rejects_a_bad_signature() {
        let mut bytes = build_image(&[0; 8], 0x0800_0000);
        bytes[0] = b'X';

        assert!(matches!(
            DfuseFile::parse(&bytes),
            Err(DfuError::BadSignature)
        ));
    }

    #[test]
    fn rejects_truncated_input() {
        let bytes = build_image(&[0; 8], 0x0800_0000);
        assert!(matches!(
            DfuseFile::parse(&bytes[..bytes.len() - 4]),
            Err(DfuError::SizeMismatch { .. } | DfuError::Truncated { .. })
        ));
    }
}
