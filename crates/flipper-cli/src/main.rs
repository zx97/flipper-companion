//! `flipper` — command-line companion for the Flipper Zero.

mod cli;
mod commands;
mod render;

use anyhow::Result;
use clap::Parser;
use flipper_companion_app::Companion;
use tracing_subscriber::EnvFilter;

use cli::{Cli, Command};

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();

    init_logging(&cli.log_level);

    let companion = Companion::new();

    match cli.command {
        Command::Devices => commands::devices::run(&companion, cli.json).await,
        Command::Info { serial } => commands::info::run(&companion, serial, cli.json).await,
        Command::Validate { file } => commands::validate::run(file),
        Command::Flash { file, serial, yes } => {
            commands::flash::run(&companion, file, serial, yes).await
        }
        Command::Backup { archive, serial } => {
            commands::maintenance::backup(&companion, archive, serial, cli.json).await
        }
        Command::Restore {
            archive,
            serial,
            yes,
        } => commands::maintenance::restore(&companion, archive, serial, yes, cli.json).await,
        Command::FactoryReset { serial, yes } => {
            commands::maintenance::factory_reset(&companion, serial, yes, cli.json).await
        }
        Command::Region {
            country,
            serial,
            yes,
        } => commands::maintenance::region(&companion, country, serial, yes, cli.json).await,
        Command::Input { key, serial } => commands::device::input(&companion, key, serial).await,
        Command::Power { serial } => commands::device::power(&companion, serial, cli.json).await,
        Command::Alert { serial } => commands::device::alert(&companion, serial).await,
        Command::Desktop { command, serial } => {
            commands::device::desktop(&companion, command, serial).await
        }
        Command::App { command, serial } => {
            commands::device::app(&companion, command, serial, cli.json).await
        }
        Command::Gpio { command, serial } => {
            commands::device::gpio(&companion, command, serial).await
        }
        Command::Recovery { command, serial } => {
            commands::recovery::run(&companion, command, serial).await
        }
        Command::Storage { command, serial } => {
            commands::storage::run(&companion, command, serial, cli.json).await
        }
        Command::Reboot { mode, serial } => commands::reboot::run(&companion, mode, serial).await,
        Command::Screen {
            format,
            output,
            serial,
        } => commands::screen::run(&companion, format, output, serial).await,
        Command::Firmware { command } => {
            commands::firmware::run(&companion, command, cli.json).await
        }
        Command::Apps { command } => commands::apps::run(&companion, command, cli.json).await,
        Command::Project { command } => commands::project::run(command, cli.json),
    }
}

fn init_logging(level: &str) {
    let filter = EnvFilter::try_new(level).unwrap_or_else(|_| EnvFilter::new("warn"));

    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .without_time()
        .compact()
        .init();
}
