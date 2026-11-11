//! `flipper validate`

use std::path::PathBuf;

use anyhow::{Context, Result};
use flipper_companion_dfu::DfuseFile;

use crate::render;

pub fn run(file: PathBuf) -> Result<()> {
    let image = DfuseFile::from_path(&file)
        .with_context(|| format!("cannot read DfuSe image `{}`", file.display()))?;

    println!("Image:      {}", file.display());
    println!(
        "USB IDs:    {:04x}:{:04x}",
        image.suffix.vendor_id, image.suffix.product_id
    );
    println!("DfuSe:      version {}", image.version);
    println!("CRC:        {:08x} (valid)", image.crc);
    println!("Targets:    {}", image.targets.len());
    println!(
        "Payload:    {} ({} bytes)",
        render::bytes(image.payload_size()),
        image.payload_size()
    );

    for (index, target) in image.targets.iter().enumerate() {
        println!(
            "  [{index}] {} (alternate setting {})",
            if target.name.is_empty() {
                "unnamed target"
            } else {
                &target.name
            },
            target.alternate_setting
        );

        for element in &target.elements {
            println!(
                "        @0x{:08x}  {:>10} bytes",
                element.address,
                element.data.len()
            );
        }
    }

    Ok(())
}
