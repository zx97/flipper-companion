//! Radio region provisioning.
//!
//! The firmware expects its allowed frequency bands in `/int/.region_data` as
//! a protobuf `PB_Region` message.  The band database is served by the update
//! infrastructure and keyed by ISO country code.

use flipper_companion_protocol::pb;
use flipper_companion_update::region::{Band, DEFAULT_REGION_URL, fetch_region_info};
use prost::Message;

use crate::{AppError, Companion};

/// Outcome of a region provisioning run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionReport {
    /// `true` when the device uses a development region and needs no data.
    pub skipped: bool,
    pub country: String,
    pub bands: usize,
    pub bytes: usize,
}

impl Companion {
    /// Detects the country, downloads the band database and writes
    /// `/int/.region_data`.
    ///
    /// The country is chosen from, in order: the explicit argument, the
    /// country detected by the region service, then the system locale.
    pub async fn provision_region(
        &self,
        serial: Option<&str>,
        country: Option<&str>,
    ) -> Result<RegionReport, AppError> {
        let session = self.connect(serial).await?;

        let result = async {
            let protobuf = session.protobuf_version().await?;
            let info = if protobuf.has_property_device_info() {
                session.device_info().await?
            } else {
                session.device_info_legacy().await?
            };

            // Region 0 is the development/unprovisioned hardware.
            if info.hardware.region == 0 {
                return Ok(RegionReport {
                    skipped: true,
                    country: String::new(),
                    bands: 0,
                    bytes: 0,
                });
            }

            let region = fetch_region_info(DEFAULT_REGION_URL).await?;

            let country = country
                .map(|country| country.trim().to_ascii_uppercase())
                .filter(|country| !country.is_empty())
                .or_else(|| region.detected_country().map(str::to_ascii_uppercase))
                .or_else(system_country_code)
                .unwrap_or_default();

            let bands = region.bands_for(&country)?;
            let encoded = encode_region(&country, &bands);

            session.storage_write("/int/.region_data", &encoded).await?;

            Ok(RegionReport {
                skipped: false,
                country,
                bands: bands.len(),
                bytes: encoded.len(),
            })
        }
        .await;

        let _ = session.stop().await;
        result
    }
}

fn encode_region(country: &str, bands: &[Band]) -> Vec<u8> {
    let region = pb::Region {
        country_code: country.as_bytes().to_vec(),
        bands: bands
            .iter()
            .map(|band| pb::region::Band {
                start: band.start,
                end: band.end,
                power_limit: band.power_limit,
                duty_cycle: band.duty_cycle,
            })
            .collect(),
    };

    region.encode_to_vec()
}

/// Extracts an ISO country code from the POSIX locale environment.
fn system_country_code() -> Option<String> {
    for key in ["LC_ALL", "LC_MESSAGES", "LANG"] {
        let Ok(value) = std::env::var(key) else {
            continue;
        };

        let value = value.split('.').next().unwrap_or(&value);
        let mut parts = value.split(['_', '-']);

        let _language = parts.next()?;
        let country = parts.next()?;

        if country.len() == 2 && country.chars().all(|c| c.is_ascii_alphabetic()) {
            return Some(country.to_ascii_uppercase());
        }
    }

    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn encodes_a_region_message() {
        let bands = [Band {
            start: 433_050_000,
            end: 434_790_000,
            power_limit: 10,
            duty_cycle: 10,
        }];

        let encoded = encode_region("FR", &bands);
        let decoded = pb::Region::decode(&encoded[..]).unwrap();

        assert_eq!(decoded.country_code, b"FR");
        assert_eq!(decoded.bands.len(), 1);
        assert_eq!(decoded.bands[0].start, 433_050_000);
        assert_eq!(decoded.bands[0].power_limit, 10);
    }
}
