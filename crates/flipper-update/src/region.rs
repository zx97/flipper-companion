//! Radio region data served by the Flipper update infrastructure.
//!
//! The endpoint returns the allowed frequency bands per country.  The device
//! expects the selected bands encoded as a protobuf `PB_Region` message in
//! `/int/.region_data`.

use std::collections::BTreeMap;

use serde::Deserialize;

use crate::UpdateError;

/// Default region bundle endpoint.
pub const DEFAULT_REGION_URL: &str = "https://update.flipperzero.one/regions/api/v0/bundle";

/// One allowed frequency band.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Band {
    pub start: u32,
    pub end: u32,
    pub power_limit: i32,
    pub duty_cycle: u32,
}

/// Parsed region bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RegionInfo {
    bands: BTreeMap<String, Band>,
    countries: BTreeMap<String, Vec<String>>,
    default_bands: Vec<String>,
    detected_country: Option<String>,
}

impl RegionInfo {
    /// Parses the JSON returned by [`DEFAULT_REGION_URL`].
    pub fn parse(json: &str) -> Result<Self, UpdateError> {
        let wire: WireRoot = serde_json::from_str(json)
            .map_err(|error| UpdateError::InvalidRegion(error.to_string()))?;

        if let Some(error) = wire.error {
            return Err(UpdateError::RegionRejected {
                code: error.code,
                text: error.text,
            });
        }

        let success = wire
            .success
            .ok_or_else(|| UpdateError::InvalidRegion(String::from("missing `success` field")))?;

        if success.bands.is_empty() {
            return Err(UpdateError::InvalidRegion(String::from(
                "region bundle has no bands",
            )));
        }

        if success.countries.is_empty() {
            return Err(UpdateError::InvalidRegion(String::from(
                "region bundle has no countries",
            )));
        }

        if success.default.is_empty() {
            return Err(UpdateError::InvalidRegion(String::from(
                "region bundle has no default bands",
            )));
        }

        let bands = success
            .bands
            .into_iter()
            .map(|(key, band)| {
                (
                    key,
                    Band {
                        start: band.start,
                        end: band.end,
                        power_limit: band.max_power,
                        duty_cycle: band.duty_cycle,
                    },
                )
            })
            .collect::<BTreeMap<_, _>>();

        for keys in success
            .countries
            .values()
            .chain(std::iter::once(&success.default))
        {
            for key in keys {
                if !bands.contains_key(key) {
                    return Err(UpdateError::InvalidRegion(format!(
                        "country references unknown band `{key}`"
                    )));
                }
            }
        }

        Ok(Self {
            bands,
            countries: success.countries,
            default_bands: success.default,
            detected_country: success.country.filter(|country| !country.is_empty()),
        })
    }

    /// Country detected by the server (usually from the client IP), if any.
    #[must_use]
    pub fn detected_country(&self) -> Option<&str> {
        self.detected_country.as_deref()
    }

    /// Returns the bands allowed for `country`, falling back to the bundle's
    /// default list for unknown or empty country codes.
    pub fn bands_for(&self, country: &str) -> Result<Vec<Band>, UpdateError> {
        let country = country.to_ascii_uppercase();
        let keys = self.countries.get(&country).unwrap_or(&self.default_bands);

        keys.iter()
            .map(|key| {
                self.bands
                    .get(key)
                    .copied()
                    .ok_or_else(|| UpdateError::InvalidRegion(format!("unknown band `{key}`")))
            })
            .collect()
    }
}

/// Fetches and parses the region bundle.
pub async fn fetch_region_info(url: &str) -> Result<RegionInfo, UpdateError> {
    let response = reqwest::get(url).await?.error_for_status()?;
    let body = response.text().await?;

    RegionInfo::parse(&body)
}

#[derive(Deserialize)]
struct WireRoot {
    #[serde(default)]
    success: Option<WireSuccess>,
    #[serde(default)]
    error: Option<WireError>,
}

#[derive(Deserialize)]
struct WireSuccess {
    bands: BTreeMap<String, WireBand>,
    countries: BTreeMap<String, Vec<String>>,
    #[serde(default)]
    country: Option<String>,
    default: Vec<String>,
}

#[derive(Deserialize)]
struct WireBand {
    start: u32,
    end: u32,
    duty_cycle: u32,
    max_power: i32,
}

#[derive(Deserialize)]
struct WireError {
    code: i32,
    text: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "success": {
            "bands": {
                "EU": {"start": 433050000, "end": 434790000, "max_power": 10, "duty_cycle": 10},
                "US": {"start": 902000000, "end": 928000000, "max_power": 20, "duty_cycle": 100}
            },
            "countries": {
                "FR": ["EU"],
                "US": ["US"]
            },
            "country": "FR",
            "default": ["EU"]
        }
    }"#;

    #[test]
    fn parses_and_selects_bands() {
        let info = RegionInfo::parse(SAMPLE).unwrap();

        assert_eq!(info.detected_country(), Some("FR"));

        let bands = info.bands_for("FR").unwrap();
        assert_eq!(bands.len(), 1);
        assert_eq!(bands[0].start, 433_050_000);
        assert_eq!(bands[0].power_limit, 10);

        let bands = info.bands_for("us").unwrap();
        assert_eq!(bands[0].start, 902_000_000);

        // Unknown countries fall back to the default list.
        let bands = info.bands_for("ZZ").unwrap();
        assert_eq!(bands[0].start, 433_050_000);
    }

    #[test]
    fn rejects_server_errors() {
        let error = RegionInfo::parse(r#"{"error":{"code":7,"text":"nope"}}"#).unwrap_err();
        assert!(matches!(error, UpdateError::RegionRejected { code: 7, .. }));
    }

    #[test]
    fn rejects_inconsistent_bundles() {
        let json = r#"{
            "success": {
                "bands": {"EU": {"start": 1, "end": 2, "max_power": 3, "duty_cycle": 4}},
                "countries": {"FR": ["NOPE"]},
                "default": ["EU"]
            }
        }"#;

        assert!(RegionInfo::parse(json).is_err());
    }
}
