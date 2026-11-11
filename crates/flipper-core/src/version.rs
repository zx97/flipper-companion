//! Firmware version numbers, build dates and release channels.
//!
//! Flipper firmware is identified either by a semantic-looking version
//! (`1.4.3`, `1.4.3-rc2`) or, for development builds, by a commit hash.  The
//! types in this module model both cases without forcing callers to reason
//! about strings.

use std::{cmp::Ordering, fmt, str::FromStr};

/// A firmware version of the form `major.minor.patch[-rcN]`.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Version {
    pub major: u32,
    pub minor: u32,
    pub patch: u32,
    pub pre: Option<PreRelease>,
}

/// A pre-release marker.  `-rc` without a number and `-rc0` compare equal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PreRelease {
    Rc(Option<u32>),
}

impl PreRelease {
    fn number(self) -> u32 {
        match self {
            Self::Rc(number) => number.unwrap_or(0),
        }
    }
}

impl Version {
    #[must_use]
    pub const fn new(major: u32, minor: u32, patch: u32) -> Self {
        Self {
            major,
            minor,
            patch,
            pre: None,
        }
    }

    /// Returns `true` when this is a release candidate rather than a final
    /// release.
    #[must_use]
    pub const fn is_prerelease(&self) -> bool {
        self.pre.is_some()
    }
}

impl FromStr for Version {
    type Err = VersionError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let (base, pre) = match input.split_once('-') {
            Some((base, suffix)) => (base, Some(suffix)),
            None => (input, None),
        };

        let mut parts = base.split('.');
        let major = parse_number(parts.next(), "major")?;
        let minor = parse_number(parts.next(), "minor")?;
        let patch = parse_number(parts.next(), "patch")?;

        if parts.next().is_some() {
            return Err(VersionError::Malformed(input.to_owned()));
        }

        let pre = match pre {
            None => None,
            Some("rc") => Some(PreRelease::Rc(None)),
            Some(suffix) => {
                let digits = suffix
                    .strip_prefix("rc")
                    .ok_or_else(|| VersionError::UnsupportedPreRelease(suffix.to_owned()))?;
                let number = digits
                    .parse::<u32>()
                    .map_err(|_| VersionError::UnsupportedPreRelease(suffix.to_owned()))?;
                Some(PreRelease::Rc(Some(number)))
            }
        };

        Ok(Self {
            major,
            minor,
            patch,
            pre,
        })
    }
}

fn parse_number(part: Option<&str>, component: &'static str) -> Result<u32, VersionError> {
    part.ok_or(VersionError::MissingComponent(component))?
        .parse::<u32>()
        .map_err(|_| VersionError::MissingComponent(component))
}

impl Ord for Version {
    fn cmp(&self, other: &Self) -> Ordering {
        (self.major, self.minor, self.patch)
            .cmp(&(other.major, other.minor, other.patch))
            .then_with(|| match (self.pre, other.pre) {
                // A final release sorts after every release candidate.
                (None, None) => Ordering::Equal,
                (None, Some(_)) => Ordering::Greater,
                (Some(_), None) => Ordering::Less,
                (Some(left), Some(right)) => left.number().cmp(&right.number()),
            })
    }
}

impl PartialOrd for Version {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}.{}", self.major, self.minor, self.patch)?;
        match self.pre {
            Some(PreRelease::Rc(None)) => f.write_str("-rc"),
            Some(PreRelease::Rc(Some(number))) => write!(f, "-rc{number}"),
            None => Ok(()),
        }
    }
}

/// A calendar date, parsed from the firmware's `dd-mm-yyyy` build stamp.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct BuildDate {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}

impl BuildDate {
    #[must_use]
    pub const fn new(year: u16, month: u8, day: u8) -> Self {
        Self { year, month, day }
    }

    /// Converts a Unix timestamp (UTC) to a calendar date.
    #[must_use]
    pub fn from_unix_timestamp(timestamp: u64) -> Self {
        let (year, month, day) = civil_from_days((timestamp / 86_400) as i64);
        Self::new(year as u16, month, day)
    }

    fn is_valid(self) -> bool {
        let Self { year, month, day } = self;
        (1..=12).contains(&month) && (1..=days_in_month(year, month)).contains(&day)
    }
}

/// Howard Hinnant's `civil_from_days` algorithm.
fn civil_from_days(days: i64) -> (i64, u8, u8) {
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let day_of_era = (z - era * 146_097) as u64;
    let year_of_era =
        (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era as i64 + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_prime = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_prime + 2) / 5 + 1) as u8;
    let month = if month_prime < 10 {
        month_prime + 3
    } else {
        month_prime - 9
    } as u8;

    (year + i64::from(month <= 2), month, day)
}

fn days_in_month(year: u16, month: u8) -> u8 {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if is_leap_year(year) => 29,
        2 => 28,
        _ => 0,
    }
}

fn is_leap_year(year: u16) -> bool {
    year.is_multiple_of(4) && !year.is_multiple_of(100) || year.is_multiple_of(400)
}

impl FromStr for BuildDate {
    type Err = VersionError;

    fn from_str(input: &str) -> Result<Self, Self::Err> {
        let mut parts = input.split('-');

        let day = parse_date_part(parts.next())?;
        let month = parse_date_part(parts.next())?;
        let year = parts
            .next()
            .and_then(|part| part.parse::<u16>().ok())
            .ok_or_else(|| VersionError::BuildDate(input.to_owned()))?;

        if parts.next().is_some() {
            return Err(VersionError::BuildDate(input.to_owned()));
        }

        let date = Self { year, month, day };

        if date.is_valid() {
            Ok(date)
        } else {
            Err(VersionError::BuildDate(input.to_owned()))
        }
    }
}

fn parse_date_part(part: Option<&str>) -> Result<u8, VersionError> {
    part.and_then(|part| part.parse::<u8>().ok())
        .ok_or_else(|| VersionError::BuildDate(String::from("incomplete date")))
}

impl fmt::Display for BuildDate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{:02}-{:02}-{}", self.day, self.month, self.year)
    }
}

/// The release channel a firmware build belongs to.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Channel {
    #[default]
    Release,
    ReleaseCandidate,
    Development,
    Custom,
}

impl Channel {
    /// Derives the channel from the firmware's branch name, matching the
    /// conventions used by the official update server.
    #[must_use]
    pub fn from_branch(branch: &str) -> Self {
        if let Ok(version) = branch.parse::<Version>() {
            return if version.is_prerelease() {
                Self::ReleaseCandidate
            } else {
                Self::Release
            };
        }

        match branch {
            "dev" | "development" => Self::Development,
            _ => Self::Custom,
        }
    }

    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Release => "release",
            Self::ReleaseCandidate => "release-candidate",
            Self::Development => "development",
            Self::Custom => "custom",
        }
    }
}

impl fmt::Display for Channel {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Errors produced while parsing a version or a build date.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VersionError {
    #[error("malformed version string `{0}`")]
    Malformed(String),
    #[error("missing or non-numeric {0} component")]
    MissingComponent(&'static str),
    #[error("unsupported pre-release suffix `{0}`")]
    UnsupportedPreRelease(String),
    #[error("invalid build date `{0}`")]
    BuildDate(String),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_final_release() {
        let version: Version = "1.4.3".parse().unwrap();
        assert_eq!((version.major, version.minor, version.patch), (1, 4, 3));
        assert!(!version.is_prerelease());
        assert_eq!(version.to_string(), "1.4.3");
    }

    #[test]
    fn parses_release_candidates() {
        assert_eq!(
            "1.4.3-rc".parse::<Version>().unwrap(),
            Version {
                major: 1,
                minor: 4,
                patch: 3,
                pre: Some(PreRelease::Rc(None)),
            }
        );
        assert_eq!(
            "1.4.3-rc2".parse::<Version>().unwrap(),
            Version {
                major: 1,
                minor: 4,
                patch: 3,
                pre: Some(PreRelease::Rc(Some(2))),
            }
        );
    }

    #[test]
    fn rejects_malformed_versions() {
        for input in ["", "1", "1.2", "1.2.3.4", "1.2.x", "1.2.3-beta1"] {
            assert!(input.parse::<Version>().is_err(), "{input} should fail");
        }
    }

    #[test]
    fn orders_release_candidates_before_release() {
        let rc1: Version = "1.4.3-rc1".parse().unwrap();
        let rc2: Version = "1.4.3-rc2".parse().unwrap();
        let final_release: Version = "1.4.3".parse().unwrap();

        assert!(rc1 < rc2);
        assert!(rc2 < final_release);
        assert!("1.4.3".parse::<Version>().unwrap() < "1.10.0".parse::<Version>().unwrap());
    }

    #[test]
    fn bare_rc_and_rc0_compare_equal() {
        let bare: Version = "1.4.3-rc".parse().unwrap();
        let zero: Version = "1.4.3-rc0".parse().unwrap();
        assert_eq!(bare.cmp(&zero), Ordering::Equal);
    }

    #[test]
    fn derives_channel_from_branch() {
        assert_eq!(Channel::from_branch("1.4.3"), Channel::Release);
        assert_eq!(Channel::from_branch("1.4.3-rc1"), Channel::ReleaseCandidate);
        assert_eq!(Channel::from_branch("dev"), Channel::Development);
        assert_eq!(Channel::from_branch("feature/foo"), Channel::Custom);
    }

    #[test]
    fn parses_and_validates_build_dates() {
        let date: BuildDate = "09-11-2026".parse().unwrap();
        assert_eq!(date, BuildDate::new(2026, 11, 9));
        assert_eq!(date.to_string(), "09-11-2026");

        assert!("31-02-2026".parse::<BuildDate>().is_err());
        assert!("29-02-2024".parse::<BuildDate>().is_ok());
        assert!("29-02-2026".parse::<BuildDate>().is_err());

        assert_eq!(
            BuildDate::from_unix_timestamp(0),
            BuildDate::new(1970, 1, 1)
        );
        assert_eq!(
            BuildDate::from_unix_timestamp(1_791_000_000),
            BuildDate::new(2026, 10, 3)
        );
    }
}
