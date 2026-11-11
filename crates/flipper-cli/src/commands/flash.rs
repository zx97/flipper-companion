//! `flipper flash`

use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};

use anyhow::{Context, Result, bail};
use flipper_companion_app::Companion;
use flipper_companion_dfu::DfuseFile;

use super::find_dfu;
use crate::render::{self, LineProgress};

pub async fn run(
    companion: &Companion,
    file: PathBuf,
    serial: Option<String>,
    yes: bool,
) -> Result<()> {
    let image = DfuseFile::from_path(&file)
        .with_context(|| format!("cannot read DfuSe image `{}`", file.display()))?;

    let mut dfu = find_dfu(serial.as_deref()).await?;

    if dfu.is_none() && serial.is_some() {
        dfu = find_dfu(None).await?;

        if dfu.is_some() {
            eprintln!("note: the DFU serial number differs from the requested one");
        }
    }

    let dfu = dfu.context(
        "no Flipper Zero in DFU mode; run `flipper reboot --mode dfu` or hold the DFU buttons while plugging the device",
    )?;

    println!(
        "Image:  {} ({} targets, {})",
        file.display(),
        image.targets.len(),
        render::bytes(image.payload_size())
    );
    println!(
        "Device: {} ({})",
        dfu.serial_number.as_deref().unwrap_or("unknown serial"),
        dfu.bus
    );

    let prompt = format!("Flash `{}` to this device?", file.display());

    if !render::confirm(&prompt, yes) {
        bail!("aborted");
    }

    let progress = Arc::new(Mutex::new(LineProgress::new("Flashing")));
    let progress_cb = Arc::clone(&progress);

    companion
        .flash_dfu(dfu.serial_number.clone(), file, move |event| {
            if let Ok(mut printer) = progress_cb.lock() {
                printer.dfu(event);
            }
        })
        .await?;

    if let Ok(mut printer) = progress.lock() {
        printer.finish();
    }

    println!("Firmware flashed successfully.");

    Ok(())
}
