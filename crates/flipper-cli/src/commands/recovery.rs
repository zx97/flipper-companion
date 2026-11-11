//! `flipper recovery` — STM32WB55 recovery-mode operations.

use anyhow::{Context, Result, bail};
use flipper_companion_app::{Companion, FusAction};
use flipper_companion_dfu::BootMode;

use crate::{
    cli::{BootModeArg, FusActionArg, RecoveryCommand},
    render,
};

pub async fn run(
    companion: &Companion,
    command: RecoveryCommand,
    serial: Option<String>,
) -> Result<()> {
    match command {
        RecoveryCommand::Info => info(companion, serial).await,
        RecoveryCommand::OptionBytes => option_bytes(companion, serial).await,
        RecoveryCommand::Exit => {
            companion.recovery_exit(serial).await?;
            println!("Recovery mode exit requested.");
            Ok(())
        }
        RecoveryCommand::SetBootMode { mode, yes } => {
            let mode = match mode {
                BootModeArg::Normal => BootMode::Normal,
                BootModeArg::Recovery => BootMode::Recovery,
            };

            eprintln!(
                "warning: writing option bytes can leave the device unbootable if interrupted"
            );

            if !render::confirm(&format!("Set boot mode to `{}`?", mode.label()), yes) {
                bail!("aborted");
            }

            companion.recovery_set_boot_mode(serial, mode).await?;
            println!("Boot mode set to `{}`.", mode.label());
            Ok(())
        }
        RecoveryCommand::Fus { action, yes } => {
            let action = match action {
                FusActionArg::Delete => FusAction::Delete,
                FusActionArg::Upgrade => FusAction::Upgrade,
                FusActionArg::StartWirelessStack => FusAction::StartWirelessStack,
            };

            eprintln!("warning: FUS commands modify the radio co-processor firmware");

            if !render::confirm("Send this FUS command?", yes) {
                bail!("aborted");
            }

            companion.recovery_fus(serial, action).await?;
            println!("FUS command sent.");
            Ok(())
        }
        RecoveryCommand::FlashRadio { file, address, yes } => {
            let address = address.as_deref().map(parse_address).transpose()?;

            eprintln!("warning: flashing the radio co-processor is an advanced recovery operation");

            if !render::confirm("Flash this radio image?", yes) {
                bail!("aborted");
            }

            let address = companion
                .recovery_flash_radio(serial, &file, address)
                .await?;

            println!("Radio image written at 0x{address:08x}.");
            Ok(())
        }
        RecoveryCommand::FlashFus { file, address, yes } => {
            let address = parse_address(&address)?;

            eprintln!("warning: flashing the FUS is an advanced recovery operation");

            if !render::confirm("Flash this FUS image?", yes) {
                bail!("aborted");
            }

            companion.recovery_flash_fus(serial, &file, address).await?;
            println!("FUS image written at 0x{address:08x}.");
            Ok(())
        }
    }
}

async fn info(companion: &Companion, serial: Option<String>) -> Result<()> {
    let info = companion.recovery_info(serial).await?;

    match &info.factory {
        Some(factory) => {
            println!(
                "Factory:     {} (v{}, target f{}, body b{}, connect c{}, color {}, region {})",
                factory.name,
                factory.version,
                factory.target,
                factory.body,
                factory.connect,
                factory.color,
                factory.region
            );
        }
        None => println!("Factory:     unavailable"),
    }

    println!(
        "Boot mode:   {} ({})",
        info.boot_mode.label(),
        if info.option_bytes_valid {
            "option bytes valid"
        } else {
            "option bytes look unprogrammed"
        }
    );
    println!(
        "FUS:         {} ({})",
        info.fus.status_name(),
        info.fus.error_name()
    );

    match &info.versions {
        Some(versions) => {
            println!("Radio FUS:   {}", versions.fus);
            println!("Radio stack: {}", versions.wireless);
        }
        None => println!("Radio:       version table unavailable"),
    }

    Ok(())
}

async fn option_bytes(companion: &Companion, serial: Option<String>) -> Result<()> {
    let info = companion.recovery_info(serial).await?;

    println!("Boot mode: {}", info.boot_mode.label());
    println!("nBOOT0:    {}", info.option_bytes.nboot0());
    println!("nSWBOOT0:  {}", info.option_bytes.nswboot0());
    println!("SFSA:      {}", info.option_bytes.sfsa());
    println!();
    println!("Raw:");

    for (index, chunk) in info.option_bytes.raw().chunks(16).enumerate() {
        println!("{:04x}  {}", index * 16, hex_dump(chunk));
    }

    Ok(())
}

fn hex_dump(bytes: &[u8]) -> String {
    bytes
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn parse_address(value: &str) -> Result<u32> {
    let value = value.trim();

    let parsed = match value
        .strip_prefix("0x")
        .or_else(|| value.strip_prefix("0X"))
    {
        Some(hex) => u32::from_str_radix(hex, 16),
        None => value.parse::<u32>(),
    };

    parsed.with_context(|| format!("invalid address `{value}`"))
}
