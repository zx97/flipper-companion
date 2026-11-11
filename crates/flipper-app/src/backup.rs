//! Backup and restore of the device's internal storage.
//!
//! A backup is a `.tar.gz` archive whose single top-level directory is
//! `int/`, mirroring the device's `/int` mount.  The format matches qFlipper,
//! so existing backups can be restored and vice versa.

use std::{
    collections::VecDeque,
    path::{Path, PathBuf},
    time::Duration,
};

use flipper_companion_core::{EntryKind, Progress};
use flipper_companion_protocol::{ProtocolError, RpcSession, pb::CommandStatus};

use crate::{AppError, Companion};

const INTERNAL_ROOT: &str = "/int";
const ARCHIVE_ROOT: &str = "int";
const MAX_DEPTH: usize = 64;
const MAX_ENTRIES: usize = 100_000;

/// Statistics of a completed backup.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupReport {
    pub files: usize,
    pub directories: usize,
    pub bytes: u64,
}

/// Statistics of a completed restore.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RestoreReport {
    pub files: usize,
    pub directories: usize,
    pub bytes: u64,
}

#[derive(Debug, Clone)]
struct RemoteEntry {
    relative: PathBuf,
    remote: String,
    kind: EntryKind,
    size: u64,
}

#[derive(Debug, Clone)]
struct LocalEntry {
    relative: PathBuf,
    kind: EntryKind,
    size: u64,
}

impl Companion {
    /// Backs up `/int` into a `.tar.gz` archive.
    pub async fn backup_internal_storage<F>(
        &self,
        serial: Option<&str>,
        archive: &Path,
        mut on_progress: F,
    ) -> Result<BackupReport, AppError>
    where
        F: FnMut(Progress),
    {
        let session = self.connect(serial).await?;

        let result = async {
            let entries = collect_remote_entries(&session).await?;

            let total_bytes: u64 = entries
                .iter()
                .filter(|entry| entry.kind == EntryKind::File)
                .map(|entry| entry.size)
                .sum();

            let directories = entries
                .iter()
                .filter(|entry| entry.kind == EntryKind::Directory)
                .count();
            let files = entries.len() - directories;

            let temporary = tempfile::tempdir()?;
            let internal = temporary.path().join(ARCHIVE_ROOT);
            tokio::fs::create_dir_all(&internal).await?;

            let mut done = 0u64;

            for entry in &entries {
                let local = temporary.path().join(&entry.relative);

                match entry.kind {
                    EntryKind::Directory => {
                        tokio::fs::create_dir_all(&local).await?;
                    }
                    EntryKind::File => {
                        let data = session.storage_read(&entry.remote).await?;

                        if let Some(parent) = local.parent() {
                            tokio::fs::create_dir_all(parent).await?;
                        }

                        tokio::fs::write(&local, &data).await?;
                        done += data.len() as u64;
                        on_progress(Progress::ratio(done, total_bytes));
                    }
                }
            }

            let archive = archive.to_path_buf();
            tokio::task::spawn_blocking(move || create_archive(&archive, &internal))
                .await
                .map_err(|error| AppError::Join(error.to_string()))??;

            on_progress(Progress::fraction(flipper_companion_core::Fraction::ONE));

            Ok(BackupReport {
                files,
                directories,
                bytes: total_bytes,
            })
        }
        .await;

        let _ = session.stop().await;
        result
    }

    /// Restores a `.tar.gz` backup into `/int`.
    ///
    /// Only paths present in the archive are removed and rewritten; extra
    /// files on the device are left untouched.
    pub async fn restore_internal_storage<F>(
        &self,
        serial: Option<&str>,
        archive: &Path,
        mut on_progress: F,
    ) -> Result<RestoreReport, AppError>
    where
        F: FnMut(Progress),
    {
        let session = self.connect(serial).await?;

        let result = async {
            let temporary = tempfile::tempdir()?;
            flipper_companion_update::extract_tar_gz(archive, temporary.path())?;

            let root = temporary.path().join(ARCHIVE_ROOT);
            if !root.is_dir() {
                return Err(AppError::MissingBackupRoot);
            }

            let entries = collect_local_entries(&root)?;
            let files: Vec<_> = entries
                .iter()
                .filter(|entry| entry.kind == EntryKind::File)
                .cloned()
                .collect();
            let directories: Vec<_> = entries
                .iter()
                .filter(|entry| entry.kind == EntryKind::Directory)
                .cloned()
                .collect();

            // Remove files first, then directories from the deepest to the
            // shallowest.  A handful of files may be absent on a fresh device;
            // that is expected.
            let mut removal_order = entries.clone();
            removal_order.sort_by_key(|entry| {
                (
                    std::cmp::Reverse(entry.relative.components().count()),
                    entry.kind == EntryKind::Directory,
                )
            });

            for entry in &removal_order {
                let remote = remote_path(&entry.relative);

                match session.storage_remove(&remote, false).await {
                    Ok(()) => {}
                    Err(ProtocolError::Device {
                        status: CommandStatus::ErrorStorageNotExist,
                        ..
                    }) => {}
                    // A directory that still contains files not in the backup
                    // cannot be removed; the mkdir below tolerates it.
                    Err(ProtocolError::Device {
                        status: CommandStatus::ErrorStorageDirNotEmpty,
                        ..
                    }) if entry.kind == EntryKind::Directory => {}
                    Err(error) => return Err(error.into()),
                }
            }

            let mut creation_order = directories.clone();
            creation_order.sort_by_key(|entry| entry.relative.components().count());

            for entry in &creation_order {
                let remote = remote_path(&entry.relative);

                match session.storage_mkdir(&remote).await {
                    Ok(()) => {}
                    Err(ProtocolError::Device {
                        status: CommandStatus::ErrorStorageExist,
                        ..
                    }) => {}
                    Err(error) => return Err(error.into()),
                }
            }

            let total_bytes: u64 = files.iter().map(|entry| entry.size).sum();
            let mut done = 0u64;

            for entry in &files {
                let local = root.join(&entry.relative);
                let data = tokio::fs::read(&local).await?;
                let remote = remote_path(&entry.relative);

                session.storage_write(&remote, &data).await?;
                done += data.len() as u64;
                on_progress(Progress::ratio(done, total_bytes));
            }

            on_progress(Progress::fraction(flipper_companion_core::Fraction::ONE));

            Ok(RestoreReport {
                files: files.len(),
                directories: directories.len(),
                bytes: total_bytes,
            })
        }
        .await;

        let _ = session.stop().await;
        result
    }

    /// Performs a factory reset.
    ///
    /// The device reboots before (or while) acknowledging the command, so a
    /// timeout or a closed session is treated as the expected outcome.
    pub async fn factory_reset(&self, serial: Option<&str>) -> Result<(), AppError> {
        let session = self
            .connect(serial)
            .await?
            .with_timeout(Duration::from_secs(10));

        let result = match session.factory_reset().await {
            Ok(()) => Ok(()),
            Err(ProtocolError::Timeout(_) | ProtocolError::Closed(_)) => Ok(()),
            Err(error) => Err(error.into()),
        };

        let _ = session.stop().await;
        result
    }
}

async fn collect_remote_entries(session: &RpcSession) -> Result<Vec<RemoteEntry>, AppError> {
    let mut entries = Vec::new();
    let mut queue = VecDeque::from([(
        String::from(INTERNAL_ROOT),
        PathBuf::from(ARCHIVE_ROOT),
        0usize,
    )]);

    while let Some((remote_dir, relative_dir, depth)) = queue.pop_front() {
        if depth > MAX_DEPTH {
            return Err(AppError::BackupTooLarge);
        }

        for entry in session.storage_list(&remote_dir).await? {
            validate_name(&entry.name)?;

            let relative = relative_dir.join(&entry.name);
            let remote = format!("{remote_dir}/{}", entry.name);

            if entry.is_directory() {
                entries.push(RemoteEntry {
                    relative: relative.clone(),
                    remote: remote.clone(),
                    kind: EntryKind::Directory,
                    size: 0,
                });
                queue.push_back((remote, relative, depth + 1));
            } else {
                entries.push(RemoteEntry {
                    relative,
                    remote,
                    kind: EntryKind::File,
                    size: entry.size_bytes,
                });
            }

            if entries.len() > MAX_ENTRIES {
                return Err(AppError::BackupTooLarge);
            }
        }
    }

    Ok(entries)
}

fn collect_local_entries(root: &Path) -> Result<Vec<LocalEntry>, AppError> {
    let mut entries = Vec::new();
    let mut stack = vec![(root.to_path_buf(), PathBuf::new(), 0usize)];

    while let Some((directory, relative_dir, depth)) = stack.pop() {
        if depth > MAX_DEPTH {
            return Err(AppError::BackupTooLarge);
        }

        for entry in std::fs::read_dir(&directory)? {
            let entry = entry?;
            let file_type = entry.file_type()?;
            let name = entry.file_name();
            let name = name
                .to_str()
                .ok_or_else(|| AppError::InvalidBackupEntry(name.to_string_lossy().into()))?;

            validate_name(name)?;

            let relative = relative_dir.join(name);

            if file_type.is_symlink() {
                return Err(AppError::InvalidBackupEntry(relative.display().to_string()));
            } else if file_type.is_dir() {
                entries.push(LocalEntry {
                    relative: relative.clone(),
                    kind: EntryKind::Directory,
                    size: 0,
                });
                stack.push((entry.path(), relative, depth + 1));
            } else if file_type.is_file() {
                entries.push(LocalEntry {
                    relative,
                    kind: EntryKind::File,
                    size: entry.metadata()?.len(),
                });
            } else {
                return Err(AppError::InvalidBackupEntry(relative.display().to_string()));
            }

            if entries.len() > MAX_ENTRIES {
                return Err(AppError::BackupTooLarge);
            }
        }
    }

    Ok(entries)
}

fn validate_name(name: &str) -> Result<(), AppError> {
    if name.is_empty()
        || name == "."
        || name == ".."
        || name.contains('/')
        || name.contains('\\')
        || name.contains('\0')
    {
        return Err(AppError::InvalidBackupEntry(name.to_owned()));
    }

    Ok(())
}

fn remote_path(relative: &Path) -> String {
    format!(
        "{INTERNAL_ROOT}/{}",
        relative.to_string_lossy().replace('\\', "/")
    )
}

fn create_archive(archive: &Path, internal: &Path) -> Result<(), AppError> {
    if let Some(parent) = archive.parent() {
        std::fs::create_dir_all(parent)?;
    }

    let file = std::fs::File::create(archive)?;
    let encoder = flate2::write::GzEncoder::new(file, flate2::Compression::default());
    let mut builder = tar::Builder::new(encoder);

    builder.append_dir_all(ARCHIVE_ROOT, internal)?;
    builder.into_inner()?.finish()?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_archive_names() {
        assert!(validate_name("subghz").is_ok());
        assert!(validate_name(".hidden").is_ok());

        for name in ["", ".", "..", "a/b", "a\\b", "a\0b"] {
            assert!(validate_name(name).is_err(), "{name:?} must be rejected");
        }
    }

    #[test]
    fn builds_remote_paths_with_forward_slashes() {
        assert_eq!(
            remote_path(Path::new("subghz/file.sub")),
            "/int/subghz/file.sub"
        );
    }

    #[test]
    fn creates_and_reads_a_backup_archive() {
        let temp = tempfile::tempdir().unwrap();
        let internal = temp.path().join("int");
        std::fs::create_dir_all(internal.join("subghz")).unwrap();
        std::fs::write(internal.join("subghz").join("signal.sub"), b"data").unwrap();
        std::fs::write(internal.join("Manifest"), b"manifest").unwrap();

        let archive = temp.path().join("backup.tgz");
        create_archive(&archive, &internal).unwrap();

        let extracted = temp.path().join("out");
        flipper_companion_update::extract_tar_gz(&archive, &extracted).unwrap();

        assert_eq!(
            std::fs::read(extracted.join("int/subghz/signal.sub")).unwrap(),
            b"data"
        );
        assert_eq!(
            std::fs::read(extracted.join("int/Manifest")).unwrap(),
            b"manifest"
        );
    }
}
