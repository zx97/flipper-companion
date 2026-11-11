//! `flipper screen`

use std::{
    io::Write,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use flipper_companion_app::{Companion, SCREEN_FRAME_TIMEOUT};

use crate::cli::ScreenFormat;

pub async fn run(
    companion: &Companion,
    format: ScreenFormat,
    output: Option<PathBuf>,
    serial: Option<String>,
) -> Result<()> {
    let frame = companion
        .screen_frame(serial.as_deref(), SCREEN_FRAME_TIMEOUT)
        .await
        .context("cannot capture a screen frame")?;

    match format {
        ScreenFormat::Ascii => write_bytes(output.as_deref(), frame.to_ascii().as_bytes()),
        ScreenFormat::Pbm => write_bytes(output.as_deref(), &frame.to_pbm()?),
    }
}

fn write_bytes(output: Option<&Path>, bytes: &[u8]) -> Result<()> {
    match output {
        Some(path) => {
            std::fs::write(path, bytes)
                .with_context(|| format!("cannot write `{}`", path.display()))?;
            eprintln!("wrote {} bytes to {}", bytes.len(), path.display());
        }
        None => {
            std::io::stdout()
                .write_all(bytes)
                .context("cannot write to stdout")?;
        }
    }

    Ok(())
}
