//! CRC-32 as used by ST's DfuSe container.
//!
//! DfuSe stores the standard reflected CRC-32 (polynomial `0xEDB88320`,
//! initial value `0xFFFFFFFF`) **without** the usual final XOR.  This was
//! verified against an official Flipper Zero `full_dfu` image.

const fn build_table() -> [u32; 256] {
    let mut table = [0u32; 256];
    let mut index = 0;

    while index < 256 {
        let mut value = index as u32;
        let mut bit = 0;

        while bit < 8 {
            value = if value & 1 == 1 {
                0xEDB8_8320 ^ (value >> 1)
            } else {
                value >> 1
            };
            bit += 1;
        }

        table[index] = value;
        index += 1;
    }

    table
}

static TABLE: [u32; 256] = build_table();

/// Computes the DfuSe variant of CRC-32 over `data`.
#[must_use]
pub fn crc32(data: &[u8]) -> u32 {
    let mut crc = 0xFFFF_FFFFu32;

    for &byte in data {
        crc = TABLE[((crc ^ u32::from(byte)) & 0xFF) as usize] ^ (crc >> 8);
    }

    crc
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn matches_known_values() {
        // Standard CRC-32 check value without the final XOR.
        assert_eq!(crc32(b"123456789"), 0x340B_C6D9);
        assert_eq!(crc32(b""), 0xFFFF_FFFF);
    }
}
