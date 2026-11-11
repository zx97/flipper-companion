//! `flipper devices`

use anyhow::Result;
use flipper_companion_app::Companion;
use serde_json::json;

use crate::render;

pub async fn run(companion: &Companion, json_output: bool) -> Result<()> {
    let devices = companion.scan().await?;

    if json_output {
        let value = devices
            .iter()
            .map(|device| {
                json!({
                    "serial": device.usb.serial_number,
                    "bus": device.usb.bus,
                    "address": device.usb.address,
                    "mode": device.usb.mode.label(),
                    "name": device.display_name(),
                    "firmware": device.info.as_ref().map(|info| info.firmware.label()),
                    "target": device.target(),
                    "storage": device.storage.map(|storage| json!({
                        "total": storage.total_bytes,
                        "free": storage.free_bytes,
                    })),
                    "error": device.error,
                })
            })
            .collect::<Vec<_>>();

        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    if devices.is_empty() {
        println!("No Flipper Zero is connected.");
        return Ok(());
    }

    let rows = devices
        .iter()
        .map(|device| {
            let storage = device.storage.map_or_else(
                || String::from("-"),
                |storage| {
                    if storage.is_present() {
                        format!("{} free", render::bytes(storage.free_bytes))
                    } else {
                        String::from("no SD card")
                    }
                },
            );

            vec![
                device
                    .usb
                    .serial_number
                    .clone()
                    .unwrap_or_else(|| device.usb.key()),
                device.usb.mode.label().to_owned(),
                device.display_name(),
                device
                    .info
                    .as_ref()
                    .map_or_else(|| String::from("-"), |info| info.firmware.label()),
                device.target().unwrap_or("-").to_owned(),
                storage,
            ]
        })
        .collect::<Vec<_>>();

    render::table(
        &["SERIAL", "MODE", "NAME", "FIRMWARE", "TARGET", "STORAGE"],
        &rows,
    );

    for device in &devices {
        if let Some(error) = &device.error {
            eprintln!("note: {}: {error}", device.usb.key());
        }
    }

    Ok(())
}
