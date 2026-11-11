//! Backup, restore, factory reset and region provisioning.

use std::{path::PathBuf, sync::Arc};

use anyhow::{Result, bail};
use flipper_companion_app::Companion;
use serde_json::json;

use crate::render::{self, LineProgress};

pub async fn backup(
    companion: &Companion,
    archive: PathBuf,
    serial: Option<String>,
    json_output: bool,
) -> Result<()> {
    let mut progress = LineProgress::new("Backing up");
    let report = companion
        .backup_internal_storage(serial.as_deref(), &archive, |value| progress.set(value))
        .await?;
    progress.finish();

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "archive": archive,
                "files": report.files,
                "directories": report.directories,
                "bytes": report.bytes,
            }))?
        );
    } else {
        println!(
            "Backup written to {} ({} file(s), {} directorie(s), {})",
            archive.display(),
            report.files,
            report.directories,
            render::bytes(report.bytes)
        );
    }

    Ok(())
}

pub async fn restore(
    companion: &Companion,
    archive: PathBuf,
    serial: Option<String>,
    yes: bool,
    json_output: bool,
) -> Result<()> {
    if !archive.is_file() {
        bail!("backup archive `{}` does not exist", archive.display());
    }

    let prompt = format!(
        "Restore `{}`? Files with the same name on the device will be overwritten.",
        archive.display()
    );

    if !render::confirm(&prompt, yes) {
        bail!("aborted");
    }

    let progress = Arc::new(std::sync::Mutex::new(LineProgress::new("Restoring")));
    let callback = Arc::clone(&progress);

    let report = companion
        .restore_internal_storage(serial.as_deref(), &archive, move |value| {
            if let Ok(mut printer) = callback.lock() {
                printer.set(value);
            }
        })
        .await?;

    if let Ok(mut printer) = progress.lock() {
        printer.finish();
    }

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "archive": archive,
                "files": report.files,
                "directories": report.directories,
                "bytes": report.bytes,
            }))?
        );
    } else {
        println!(
            "Restored {} file(s) and {} directorie(s) from {} ({})",
            report.files,
            report.directories,
            archive.display(),
            render::bytes(report.bytes)
        );
    }

    Ok(())
}

pub async fn factory_reset(
    companion: &Companion,
    serial: Option<String>,
    yes: bool,
    json_output: bool,
) -> Result<()> {
    if !render::confirm(
        "Factory reset the device? Internal storage and settings will be erased.",
        yes,
    ) {
        bail!("aborted");
    }

    companion.factory_reset(serial.as_deref()).await?;

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({ "factory_reset": true }))?
        );
    } else {
        println!("Factory reset sent. The device is rebooting.");
    }

    Ok(())
}

pub async fn region(
    companion: &Companion,
    country: Option<String>,
    serial: Option<String>,
    yes: bool,
    json_output: bool,
) -> Result<()> {
    if !render::confirm("Write radio region data to /int/.region_data?", yes) {
        bail!("aborted");
    }

    let report = companion
        .provision_region(serial.as_deref(), country.as_deref())
        .await?;

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "skipped": report.skipped,
                "country": report.country,
                "bands": report.bands,
                "bytes": report.bytes,
            }))?
        );
    } else if report.skipped {
        println!("Development hardware region detected; region data not required.");
    } else {
        println!(
            "Region data written for country `{}` ({} band(s), {} bytes).",
            if report.country.is_empty() {
                "default"
            } else {
                &report.country
            },
            report.bands,
            report.bytes
        );
    }

    Ok(())
}
