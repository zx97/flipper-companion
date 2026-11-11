//! Storage-related domain types.

/// Capacity and free space of a filesystem exposed by the device.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct StorageInfo {
    pub total_bytes: u64,
    pub free_bytes: u64,
}

impl StorageInfo {
    #[must_use]
    pub const fn new(total_bytes: u64, free_bytes: u64) -> Self {
        Self {
            total_bytes,
            free_bytes,
        }
    }

    #[must_use]
    pub const fn used_bytes(self) -> u64 {
        self.total_bytes.saturating_sub(self.free_bytes)
    }

    #[must_use]
    pub const fn is_present(self) -> bool {
        self.total_bytes > 0
    }

    /// Free space as a value in `0.0..=1.0`.  Returns `0.0` for absent media.
    #[must_use]
    pub fn free_fraction(self) -> crate::progress::Fraction {
        crate::progress::Fraction::ratio(self.free_bytes, self.total_bytes)
    }
}

/// The kind of entry returned by a directory listing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EntryKind {
    File,
    Directory,
}

/// One entry of a remote directory listing.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StorageEntry {
    pub name: String,
    pub kind: EntryKind,
    pub size_bytes: u64,
}

impl StorageEntry {
    #[must_use]
    pub const fn is_directory(&self) -> bool {
        matches!(self.kind, EntryKind::Directory)
    }
}
