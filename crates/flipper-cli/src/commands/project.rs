//! `flipper project` — flippy-compatible project-side configuration.

use std::{env, path::Path};

use anyhow::{Context, Result};
use flipper_companion_project::{
    FirmwareSource, RepositorySource, add_repository, create_project, create_store, fetch_store,
    set_firmware,
};
use serde_json::json;

use crate::cli::{
    ProjectCommand, ProjectFirmwareCommand, ProjectMapCommand, ProjectRepoCommand,
    ProjectStoreCommand,
};

pub fn run(command: ProjectCommand, json_output: bool) -> Result<()> {
    let cwd = env::current_dir().context("cannot determine the current directory")?;

    match command {
        ProjectCommand::New { path } => new(&path, json_output),
        ProjectCommand::Map { command } => map(&cwd, command, json_output),
        ProjectCommand::Store { command } => store(&cwd, command, json_output),
    }
}

fn new(path: &Path, json_output: bool) -> Result<()> {
    let created = create_project(path)?;

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "directory": created.directory,
                "config": created.config,
                "store": created.store,
            }))?
        );
    } else {
        println!("created {}", created.directory.display());
        println!("wrote {}", created.config.display());
        println!("created {}", created.store.display());
    }

    Ok(())
}

fn map(cwd: &std::path::Path, command: ProjectMapCommand, json_output: bool) -> Result<()> {
    match command {
        ProjectMapCommand::Repo { command } => match command {
            ProjectRepoCommand::Add { source, name } => {
                let source = source
                    .parse::<RepositorySource>()
                    .map_err(|error| anyhow::anyhow!(error))?;
                let (name, source) = add_repository(cwd, source, name)?;

                if json_output {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json!({
                            "name": name,
                            "source": source.to_string(),
                        }))?
                    );
                } else {
                    println!("added repository '{name}' ({source})");
                }
            }
        },
        ProjectMapCommand::Firmware { command } => match command {
            ProjectFirmwareCommand::Set { source } => {
                let source = source
                    .parse::<FirmwareSource>()
                    .map_err(|error| anyhow::anyhow!(error))?;
                set_firmware(cwd, source.clone())?;

                if json_output {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json!({
                            "firmware": source.to_string(),
                        }))?
                    );
                } else {
                    println!("set firmware to {source}");
                }
            }
        },
    }

    Ok(())
}

fn store(cwd: &std::path::Path, command: ProjectStoreCommand, json_output: bool) -> Result<()> {
    match command {
        ProjectStoreCommand::Create => {
            let store = create_store(cwd)?;

            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({ "store": store }))?
                );
            } else {
                println!("ensured {}", store.display());
            }
        }
        ProjectStoreCommand::Fetch { optimize } => {
            let plan = fetch_store(cwd, optimize)?;

            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({
                        "optimize": optimize,
                        "plan": plan,
                    }))?
                );
            } else {
                println!("validated {}", cwd.join("flip.toml").display());
                println!("wrote {}", plan.display());
            }
        }
    }

    Ok(())
}
