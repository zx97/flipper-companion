//! STM32 DFU memory layout string descriptors.
//!
//! A DFU alternate setting advertises its flash geometry with a string like
//! `"Flipper Zero F18/0x08000000/4*1024Kg,2*64Kg,1*128Kg"`:
//!
//! * `0x08000000` is the base address,
//! * `4*1024Kg` means 4 sectors of 1024 KiB,
//! * `Kg` is the ST unit suffix (`K`/`M` multiplier, `g` for pages).

use crate::error::DfuError;

/// Parsed geometry of one DFU alternate setting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MemoryLayout {
    pub name: String,
    pub base_address: u32,
    pub banks: Vec<PageBank>,
}

/// A group of equally sized erase pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageBank {
    pub pages: u32,
    pub page_size: u32,
}

impl MemoryLayout {
    /// Parses an ST memory-layout descriptor.
    pub fn parse(descriptor: &str) -> Result<Self, DfuError> {
        let mut fields = descriptor.split('/');

        let name = fields
            .next()
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| DfuError::InvalidLayout(descriptor.to_owned()))?
            .to_owned();

        let address = fields
            .next()
            .map(str::trim)
            .ok_or_else(|| DfuError::InvalidLayout(descriptor.to_owned()))?;
        let base_address =
            parse_address(address).ok_or_else(|| DfuError::InvalidLayout(descriptor.to_owned()))?;

        let banks_field = fields
            .next()
            .ok_or_else(|| DfuError::InvalidLayout(descriptor.to_owned()))?;

        if fields.next().is_some() {
            return Err(DfuError::InvalidLayout(descriptor.to_owned()));
        }

        let mut banks = Vec::new();
        let mut total_bytes = 0u64;

        for bank in banks_field.split(',') {
            let bank = parse_bank(bank.trim())
                .ok_or_else(|| DfuError::InvalidLayout(descriptor.to_owned()))?;

            total_bytes += u64::from(bank.pages) * u64::from(bank.page_size);

            // Refuse absurd geometries: a descriptor must not be able to make
            // the flasher iterate for hours.
            if total_bytes > 64 * 1024 * 1024 {
                return Err(DfuError::InvalidLayout(descriptor.to_owned()));
            }

            banks.push(bank);
        }

        if banks.is_empty() {
            return Err(DfuError::InvalidLayout(descriptor.to_owned()));
        }

        Ok(Self {
            name,
            base_address,
            banks,
        })
    }

    /// Returns every page start address that touches `[start, end)`.
    ///
    /// Mirrors the behaviour of ST's tools: the range start is always
    /// included, then each sector boundary strictly inside the range.
    #[must_use]
    pub fn page_addresses(&self, start: u32, end: u32) -> Vec<u32> {
        let mut addresses = Vec::new();

        if start < self.base_address || start >= end || start >= self.end_address() {
            return addresses;
        }

        addresses.push(start);
        let mut address = self.base_address;

        for bank in &self.banks {
            for _ in 0..bank.pages {
                address = address.saturating_add(bank.page_size);

                if address > start && address < end {
                    addresses.push(address);
                } else if address >= end {
                    return addresses;
                }
            }
        }

        addresses
    }

    /// Exclusive end address of the advertised flash.
    #[must_use]
    pub fn end_address(&self) -> u32 {
        self.banks.iter().fold(self.base_address, |address, bank| {
            address.saturating_add(bank.pages.saturating_mul(bank.page_size))
        })
    }
}

fn parse_address(value: &str) -> Option<u32> {
    match value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        Some(hex) => u32::from_str_radix(hex, 16).ok(),
        None => value.parse().ok(),
    }
}

fn parse_bank(value: &str) -> Option<PageBank> {
    let (count, size) = value.split_once('*')?;
    let pages = count.trim().parse::<u32>().ok()?;

    if pages == 0 {
        return None;
    }

    let size = size.trim();
    let digits_end = size.find(|character: char| !character.is_ascii_digit())?;
    let number = size[..digits_end].parse::<u32>().ok()?;

    if number == 0 {
        return None;
    }

    let suffix = size[digits_end..].to_ascii_lowercase();
    let multiplier = match suffix.as_bytes().first()? {
        b'k' => 1024,
        b'm' => 1024 * 1024,
        _ => return None,
    };

    Some(PageBank {
        pages,
        page_size: number.checked_mul(multiplier)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_flipper_layout() {
        let layout =
            MemoryLayout::parse("Flipper Zero F18/0x08000000/4*1024Kg,2*64Kg,1*128Kg").unwrap();

        assert_eq!(layout.name, "Flipper Zero F18");
        assert_eq!(layout.base_address, 0x0800_0000);
        assert_eq!(
            layout.banks,
            vec![
                PageBank {
                    pages: 4,
                    page_size: 1024 * 1024
                },
                PageBank {
                    pages: 2,
                    page_size: 64 * 1024
                },
                PageBank {
                    pages: 1,
                    page_size: 128 * 1024
                },
            ]
        );
        assert_eq!(
            layout.end_address(),
            0x0800_0000 + 4 * 1024 * 1024 + 128 * 1024 + 128 * 1024
        );
    }

    #[test]
    fn computes_page_boundaries() {
        let layout = MemoryLayout::parse("flash/0x08000000/2*1Kg,2*2Kg").unwrap();

        assert_eq!(
            layout.page_addresses(0x0800_0000, 0x0800_0000 + 3 * 1024),
            vec![0x0800_0000, 0x0800_0400, 0x0800_0800,]
        );

        assert_eq!(
            layout.page_addresses(0x0800_0400, 0x0800_0C00),
            vec![0x0800_0400, 0x0800_0800]
        );

        assert!(layout.page_addresses(0x2000_0000, 0x2000_0010).is_empty());
        assert!(layout.page_addresses(0x0800_1800, 0x0800_1900).is_empty());
    }

    #[test]
    fn rejects_malformed_layouts() {
        for descriptor in [
            "",
            "flash",
            "flash/0x08000000",
            "flash/0x08000000/",
            "flash/0x08000000/0*1Kg",
            "flash/0x08000000/1*0Kg",
            "flash/0x08000000/1*1Xg",
            "flash/0x08000000/1*1Kg/extra",
        ] {
            assert!(
                MemoryLayout::parse(descriptor).is_err(),
                "`{descriptor}` should be rejected"
            );
        }
    }
}
