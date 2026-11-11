//! `flipper info`

use anyhow::{Context, Result, bail};
use flipper_companion_app::Companion;
use serde_json::json;

use super::select;
use crate::render;

pub async fn run(companion: &Companion, serial: Option<String>, json_output: bool) -> Result<()> {
    let devices = companion.scan().await?;
    let device = select(&devices, serial.as_deref())?;

    if let Some(error) = &device.error {
        bail!("cannot read device information: {error}");
    }

    let info = device
        .info
        .as_ref()
        .context("device did not report its information")?;

    if json_output {
        let value = json!({
            "serial": device.usb.serial_number,
            "mode": device.usb.mode.label(),
            "name": info.name,
            "firmware": {
                "version": info.firmware.label(),
                "commit": info.firmware.commit,
                "branch": info.firmware.branch,
                "channel": info.firmware.channel.as_str(),
                "build_date": info.firmware.build_date.map(|date| date.to_string()),
                "api_version": info.firmware.api_version.map(|version| version.to_string()),
            },
            "hardware": {
                "version": info.hardware.version,
                "target": info.hardware.target,
                "body": info.hardware.body,
                "connect": info.hardware.connect,
                "color": info.hardware.color,
                "region": info.hardware.region,
            },
            "radio": info.radio.as_ref().map(|radio| json!({
                "fus": radio.fus_version,
                "stack": radio.stack_version,
                "type": radio.stack_type,
            })),
            "protobuf": device.protobuf.map(|version| version.to_string()),
            "storage": device.storage.map(|storage| json!({
                "total": storage.total_bytes,
                "free": storage.free_bytes,
            })),
        });

        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    println!("Name:       {}", info.name);
    println!(
        "Serial:     {}",
        device.usb.serial_number.as_deref().unwrap_or("-")
    );
    println!(
        "Mode:       {} (bus {}, address {})",
        device.usb.mode.label(),
        device.usb.bus,
        device.usb.address
    );
    println!(
        "Firmware:   {} (commit {}, branch {}, channel {})",
        info.firmware.label(),
        empty_or_dash(&info.firmware.commit),
        empty_or_dash(&info.firmware.branch),
        info.firmware.channel
    );

    if let Some(date) = info.firmware.build_date {
        println!("Built:      {date}");
    }

    if let Some(api) = info.firmware.api_version {
        println!("Firmware API: {api}");
    }

    println!(
        "Hardware:   ver {}, target {}, body {}, connect {}, color {}, region {}",
        empty_or_dash(&info.hardware.version),
        empty_or_dash(&info.hardware.target),
        empty_or_dash(&info.hardware.body),
        empty_or_dash(&info.hardware.connect),
        info.hardware.color,
        info.hardware.region
    );

    if let Some(radio) = &info.radio {
        println!(
            "Radio:      FUS {}, stack {} (type {})",
            radio.fus_version, radio.stack_version, radio.stack_type
        );
    }

    if let Some(protobuf) = device.protobuf {
        println!("Protobuf:   {protobuf}");
    }

    if let Some(storage) = device.storage {
        if storage.is_present() {
            println!(
                "Storage:    {} free of {} ({}% used)",
                render::bytes(storage.free_bytes),
                render::bytes(storage.total_bytes),
                (100.0 * storage.used_bytes() as f64 / storage.total_bytes as f64).round()
            );
        } else {
            println!("Storage:    no SD card");
        }
    }

    Ok(())
}

fn empty_or_dash(value: &str) -> &str {
    if value.is_empty() { "-" } else { value }
}
