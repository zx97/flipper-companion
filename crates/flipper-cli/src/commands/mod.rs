//! Shared helpers for subcommands.

pub mod apps;
pub mod device;
pub mod devices;
pub mod firmware;
pub mod flash;
pub mod info;
pub mod maintenance;
pub mod project;
pub mod reboot;
pub mod recovery;
pub mod screen;
pub mod storage;
pub mod validate;

use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};
use flipper_companion_app::DeviceSummary;
use flipper_companion_core::UsbDevice;
use tokio::time::sleep;

/// Picks a device from a scan, by serial number or by "first usable one".
pub fn select<'a>(devices: &'a [DeviceSummary], serial: Option<&str>) -> Result<&'a DeviceSummary> {
    if let Some(serial) = serial {
        return devices
            .iter()
            .find(|device| device.usb.serial_number.as_deref() == Some(serial))
            .with_context(|| format!("no Flipper Zero with serial `{serial}` is connected"));
    }

    devices
        .iter()
        .find(|device| device.is_probed())
        .or_else(|| devices.first())
        .context("no Flipper Zero is connected")
}

/// Looks for a DFU device, optionally matching a serial number.
pub async fn find_dfu(serial: Option<&str>) -> Result<Option<UsbDevice>> {
    let devices = tokio::task::spawn_blocking(flipper_companion_dfu::list_devices)
        .await
        .context("USB discovery task failed")??;

    Ok(devices.into_iter().find(|device| {
        device.is_dfu()
            && serial.is_none_or(|serial| device.serial_number.as_deref() == Some(serial))
    }))
}

/// Waits until a DFU device appears, preferring an exact serial match.
pub async fn wait_for_dfu(serial: Option<&str>, wait: Duration) -> Result<UsbDevice> {
    let deadline = Instant::now() + wait;

    loop {
        if let Some(device) = find_dfu(serial).await? {
            return Ok(device);
        }

        if serial.is_some() {
            // The ST bootloader may expose a different serial number than the
            // running firmware; accept any DFU device once we have waited a
            // little for the exact match.
            if let Some(device) = find_dfu(None).await? {
                return Ok(device);
            }
        }

        if Instant::now() >= deadline {
            bail!(
                "device did not enter DFU mode within {} seconds",
                wait.as_secs()
            );
        }

        sleep(Duration::from_millis(250)).await;
    }
}

/// Resolves the hardware target (`f18`, ...) of a device, defaulting to `f18`.
pub async fn resolve_target(
    companion: &flipper_companion_app::Companion,
    serial: Option<&str>,
) -> Result<String> {
    let devices = companion.scan().await?;

    if let Some(device) = devices.iter().find(|device| {
        serial.is_none_or(|serial| device.usb.serial_number.as_deref() == Some(serial))
    }) && let Some(target) = device.target()
    {
        return Ok(target.to_owned());
    }

    eprintln!("note: hardware target is unknown, assuming f18");
    Ok(String::from("f18"))
}
