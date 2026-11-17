//! Platform-aware standard directories.
//!
//! Linux and other Unix systems follow the XDG base directory specification;
//! Windows uses the roaming and local application data directories.  Keeping
//! the logic here avoids scattering `HOME` checks through the application.

use std::path::PathBuf;

fn variable(name: &str) -> Option<PathBuf> {
    std::env::var_os(name).map(PathBuf::from)
}

/// The user's home directory.
#[must_use]
pub fn home_dir() -> Option<PathBuf> {
    #[cfg(windows)]
    {
        variable("USERPROFILE")
    }

    #[cfg(not(windows))]
    {
        variable("HOME")
    }
}

/// Configuration directory: `XDG_CONFIG_HOME`, `%APPDATA%` or `~/.config`.
#[must_use]
pub fn config_dir() -> Option<PathBuf> {
    if let Some(directory) = variable("XDG_CONFIG_HOME") {
        return Some(directory);
    }

    #[cfg(windows)]
    if let Some(directory) = variable("APPDATA") {
        return Some(directory);
    }

    home_dir().map(|home| home.join(".config"))
}

/// Data directory: `XDG_DATA_HOME`, `%LOCALAPPDATA%` or `~/.local/share`.
#[must_use]
pub fn data_dir() -> Option<PathBuf> {
    if let Some(directory) = variable("XDG_DATA_HOME") {
        return Some(directory);
    }

    #[cfg(windows)]
    if let Some(directory) = variable("LOCALAPPDATA") {
        return Some(directory);
    }

    home_dir().map(|home| home.join(".local").join("share"))
}

/// Default download directory (`~/Downloads`).
#[must_use]
pub fn downloads_dir() -> Option<PathBuf> {
    home_dir().map(|home| home.join("Downloads"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn downloads_dir_is_under_home() {
        let Some(home) = home_dir() else {
            return;
        };

        assert_eq!(downloads_dir(), Some(home.join("Downloads")));
    }
}
