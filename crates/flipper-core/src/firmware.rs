//! Firmware and radio identification.

use crate::version::{BuildDate, Channel, Version};

/// Identity of the firmware currently running on a device.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct FirmwareInfo {
    /// Parsed version, absent for development builds identified by commit.
    pub version: Option<Version>,
    pub commit: String,
    pub branch: String,
    pub channel: Channel,
    pub build_date: Option<BuildDate>,
    /// Firmware SDK API version advertised to the app catalog.
    pub api_version: Option<ApiVersion>,
}

/// Firmware SDK API version (`major.minor`), used by FAP builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ApiVersion {
    pub major: u16,
    pub minor: u16,
}

impl std::fmt::Display for ApiVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

impl FirmwareInfo {
    /// A short, human-readable identifier: the version when available,
    /// otherwise the abbreviated commit hash.
    #[must_use]
    pub fn label(&self) -> String {
        if let Some(version) = &self.version {
            version.to_string()
        } else if !self.commit.is_empty() {
            self.commit.chars().take(8).collect()
        } else {
            String::from("unknown")
        }
    }

    /// Development builds carry a commit but no parseable version.
    #[must_use]
    pub fn is_development(&self) -> bool {
        self.version.is_none() && !self.commit.is_empty()
    }
}

/// Co-processor (radio) firmware information, when the radio is alive.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RadioInfo {
    pub fus_version: String,
    pub stack_version: String,
    pub stack_type: u8,
}
