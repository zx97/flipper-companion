//! `flipper reboot`

use anyhow::Result;
use flipper_companion_app::{Companion, RebootTarget};

use crate::cli::RebootMode;

pub async fn run(companion: &Companion, mode: RebootMode, serial: Option<String>) -> Result<()> {
    let target = match mode {
        RebootMode::Os => RebootTarget::Os,
        RebootMode::Dfu => RebootTarget::Dfu,
        RebootMode::Update => RebootTarget::Update,
    };

    companion.reboot(serial.as_deref(), target).await?;
    println!("Reboot command sent ({mode:?}).");

    Ok(())
}
