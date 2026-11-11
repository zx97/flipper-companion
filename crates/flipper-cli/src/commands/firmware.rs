//! `flipper firmware`

use std::{
    sync::{Arc, Mutex},
    time::Duration,
};

use anyhow::{Context, Result, bail};
use flipper_companion_app::{Companion, RebootTarget, select_release};
use flipper_companion_core::UsbDevice;
use flipper_companion_update::FirmwareBundle;
use serde_json::json;

use super::{find_dfu, resolve_target, wait_for_dfu};
use crate::{
    cli::FirmwareCommand,
    render::{self, LineProgress},
};

pub async fn run(companion: &Companion, command: FirmwareCommand, json_output: bool) -> Result<()> {
    match command {
        FirmwareCommand::List { channel } => list(companion, channel, json_output).await,
        FirmwareCommand::Download {
            channel,
            kind,
            target,
            output,
        } => download(companion, channel, kind, target, output).await,
        FirmwareCommand::Install {
            channel,
            target,
            serial,
            yes,
        } => install(companion, channel, target, serial, yes).await,
        FirmwareCommand::Flash {
            channel,
            target,
            serial,
            yes,
        } => flash(companion, channel, target, serial, yes).await,
    }
}

async fn install(
    companion: &Companion,
    channel: String,
    target: Option<String>,
    serial: Option<String>,
    yes: bool,
) -> Result<()> {
    let target = match target {
        Some(target) => target,
        None => resolve_target(companion, serial.as_deref()).await?,
    };

    let devices = companion.scan().await?;
    let device = devices
        .iter()
        .find(|device| {
            !device.is_dfu()
                && serial
                    .as_deref()
                    .is_none_or(|serial| device.usb.serial_number.as_deref() == Some(serial))
        })
        .context("the SD-card update requires a device running its normal firmware")?;

    let directory = companion.firmware_directory().await?;
    let release = select_release(&directory, &channel, "update_tgz", &target)?;
    let file = release.file("update_tgz", &target).with_context(|| {
        format!(
            "release {} has no update bundle for `{target}`",
            release.version
        )
    })?;

    println!(
        "Release: {} ({}) — {}",
        release.version,
        render::date_from_timestamp(release.timestamp),
        release.changelog.lines().next().unwrap_or_default()
    );

    let prompt = format!(
        "Download firmware {} and upload it to the SD card?",
        release.version
    );

    if !render::confirm(&prompt, yes) {
        bail!("aborted");
    }

    let temporary = tempfile::tempdir().context("cannot create a temporary directory")?;
    let mut download = LineProgress::new("Downloading");
    let archive = companion
        .download_artifact(file, temporary.path(), |value| download.set(value))
        .await?;
    download.finish();

    let bundle = FirmwareBundle::extract(&archive, &temporary.path().join("bundle"))
        .context("cannot extract the update bundle")?;

    let mut upload = LineProgress::new("Uploading");
    let report = companion
        .install_bundle(device.usb.serial_number.as_deref(), &bundle, |value| {
            upload.set(value)
        })
        .await?;
    upload.finish();

    println!(
        "Update {} started: {} file(s), {} uploaded, {} total.",
        release.version,
        report.files_total,
        report.files_uploaded,
        render::bytes(report.bytes_uploaded)
    );

    Ok(())
}

async fn list(companion: &Companion, channel: Option<String>, json_output: bool) -> Result<()> {
    let directory = companion.firmware_directory().await?;

    let channels = match channel {
        Some(channel) => vec![
            directory
                .channel(&channel)
                .with_context(|| format!("unknown channel `{channel}`"))?,
        ],
        None => directory.channels.iter().collect(),
    };

    if json_output {
        let value = channels
            .iter()
            .flat_map(|channel| {
                channel.versions.iter().map(move |release| {
                    json!({
                        "channel": channel.id,
                        "version": release.version,
                        "date": render::date_from_timestamp(release.timestamp),
                        "changelog": release.changelog,
                        "files": release.files.len(),
                    })
                })
            })
            .collect::<Vec<_>>();

        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    let rows = channels
        .iter()
        .flat_map(|channel| {
            channel.versions.iter().map(move |release| {
                vec![
                    channel.id.clone(),
                    release.version.clone(),
                    render::date_from_timestamp(release.timestamp),
                    release
                        .changelog
                        .lines()
                        .next()
                        .unwrap_or_default()
                        .chars()
                        .take(52)
                        .collect(),
                    release.files.len().to_string(),
                ]
            })
        })
        .collect::<Vec<_>>();

    render::table(&["CHANNEL", "VERSION", "DATE", "CHANGELOG", "FILES"], &rows);
    Ok(())
}

async fn download(
    companion: &Companion,
    channel: String,
    kind: String,
    target: Option<String>,
    output: std::path::PathBuf,
) -> Result<()> {
    let target = match target {
        Some(target) => target,
        None => resolve_target(companion, None).await?,
    };

    let directory = companion.firmware_directory().await?;
    let release = select_release(&directory, &channel, &kind, &target)?;
    let file = release
        .file(&kind, &target)
        .with_context(|| format!("release {} has no `{kind}` for `{target}`", release.version))?;

    println!(
        "Release {} ({}), artifact `{}` for `{}`",
        release.version,
        render::date_from_timestamp(release.timestamp),
        kind,
        target
    );

    let mut progress = LineProgress::new("Downloading");
    let path = companion
        .download_artifact(file, &output, |value| progress.set(value))
        .await?;
    progress.finish();

    println!("{}", path.display());
    Ok(())
}

async fn flash(
    companion: &Companion,
    channel: String,
    target: Option<String>,
    serial: Option<String>,
    yes: bool,
) -> Result<()> {
    // Resolve the target before rebooting: the DFU bootloader does not expose
    // the hardware model.
    let target = match target {
        Some(target) => target,
        None => resolve_target(companion, serial.as_deref()).await?,
    };

    let dfu = ensure_dfu(companion, serial.as_deref(), yes).await?;

    let directory = companion.firmware_directory().await?;
    let release = select_release(&directory, &channel, "full_dfu", &target)?;
    let file = release.file("full_dfu", &target).with_context(|| {
        format!(
            "release {} has no full image for `{target}`",
            release.version
        )
    })?;

    println!(
        "Release: {} ({}) — {}",
        release.version,
        render::date_from_timestamp(release.timestamp),
        release.changelog.lines().next().unwrap_or_default()
    );

    let prompt = format!(
        "Download and flash firmware {} to the device in DFU mode?",
        release.version
    );

    if !render::confirm(&prompt, yes) {
        bail!("aborted");
    }

    let temporary = tempfile::tempdir().context("cannot create a temporary directory")?;
    let mut download = LineProgress::new("Downloading");
    let image = companion
        .download_artifact(file, temporary.path(), |value| download.set(value))
        .await?;
    download.finish();

    let progress = Arc::new(Mutex::new(LineProgress::new("Flashing")));
    let callback = Arc::clone(&progress);

    companion
        .flash_dfu(dfu.serial_number.clone(), image, move |event| {
            if let Ok(mut printer) = callback.lock() {
                printer.dfu(event);
            }
        })
        .await?;

    if let Ok(mut printer) = progress.lock() {
        printer.finish();
    }

    println!("Firmware {} flashed successfully.", release.version);
    Ok(())
}

async fn ensure_dfu(companion: &Companion, serial: Option<&str>, yes: bool) -> Result<UsbDevice> {
    if let Some(device) = find_dfu(serial).await? {
        return Ok(device);
    }

    if let Some(device) = find_dfu(None).await? {
        return Ok(device);
    }

    let devices = companion.scan().await?;
    let device = devices
        .iter()
        .find(|device| {
            !device.is_dfu()
                && serial.is_none_or(|serial| device.usb.serial_number.as_deref() == Some(serial))
        })
        .context("no Flipper Zero is connected")?;

    let prompt = format!("Reboot {} into DFU mode?", device.display_name());

    if !render::confirm(&prompt, yes) {
        bail!("aborted");
    }

    companion
        .reboot(device.usb.serial_number.as_deref(), RebootTarget::Dfu)
        .await
        .context("cannot reboot the device into DFU mode")?;

    wait_for_dfu(device.usb.serial_number.as_deref(), Duration::from_secs(15)).await
}
