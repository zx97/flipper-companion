//! `flipper storage`

use std::io::Write;

use anyhow::{Context, Result, bail};
use flipper_companion_app::Companion;
use flipper_companion_core::EntryKind;
use serde_json::json;

use crate::{cli::StorageCommand, render};

pub async fn run(
    companion: &Companion,
    command: StorageCommand,
    serial: Option<String>,
    json_output: bool,
) -> Result<()> {
    let session = companion
        .connect(serial.as_deref())
        .await
        .context("cannot open an RPC session")?;

    let result = async {
        match command {
            StorageCommand::Ls { path } => list(&session, &path, json_output).await,
            StorageCommand::Info { path } => info(&session, &path).await,
            StorageCommand::Stat { path } => stat(&session, &path).await,
            StorageCommand::Read { path, output } => read(&session, &path, output).await,
            StorageCommand::Write { local, remote } => write(&session, &local, &remote).await,
            StorageCommand::Mkdir { path } => {
                session.storage_mkdir(&path).await?;
                println!("created {path}");
                Ok(())
            }
            StorageCommand::Rm {
                path,
                recursive,
                yes,
            } => {
                let prompt = format!("Remove `{path}` from the device?");

                if !render::confirm(&prompt, yes) {
                    bail!("aborted");
                }

                session.storage_remove(&path, recursive).await?;
                println!("removed {path}");
                Ok(())
            }
            StorageCommand::Mv { from, to } => {
                session.storage_rename(&from, &to).await?;
                println!("moved {from} -> {to}");
                Ok(())
            }
            StorageCommand::Md5 { path } => {
                println!("{}", session.storage_md5sum(&path).await?);
                Ok(())
            }
            StorageCommand::Properties { path, md5 } => {
                properties(&session, &path, md5, json_output).await
            }
            StorageCommand::Timestamp { path } => {
                let timestamp = session.storage_timestamp(&path).await?;

                if json_output {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&json!({
                            "timestamp": timestamp,
                            "date": render::date_from_timestamp(u64::from(timestamp)),
                        }))?
                    );
                } else {
                    println!(
                        "{timestamp} ({})",
                        render::date_from_timestamp(u64::from(timestamp))
                    );
                }
                Ok(())
            }
            StorageCommand::TarExtract {
                tar_path,
                out_path,
                yes,
            } => {
                let prompt = format!("Extract `{tar_path}` into `{out_path}` on the device?");

                if !render::confirm(&prompt, yes) {
                    bail!("aborted");
                }

                session.storage_tar_extract(&tar_path, &out_path).await?;
                println!("extracted {tar_path} -> {out_path}");
                Ok(())
            }
        }
    }
    .await;

    let _ = session.stop().await;
    result
}

async fn list(session: &flipper_companion_protocol::RpcSession, path: &str, json_output: bool) -> Result<()> {
    let mut entries = session.storage_list(path).await?;

    entries.sort_by(|left, right| {
        right
            .is_directory()
            .cmp(&left.is_directory())
            .then_with(|| left.name.to_lowercase().cmp(&right.name.to_lowercase()))
    });

    if json_output {
        let value = entries
            .iter()
            .map(|entry| {
                json!({
                    "name": entry.name,
                    "type": kind_label(entry.kind),
                    "size": entry.size_bytes,
                })
            })
            .collect::<Vec<_>>();

        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    if entries.is_empty() {
        println!("{path} is empty.");
        return Ok(());
    }

    let rows = entries
        .iter()
        .map(|entry| {
            vec![
                entry.name.clone(),
                kind_label(entry.kind).to_owned(),
                if entry.is_directory() {
                    String::from("-")
                } else {
                    render::bytes(entry.size_bytes)
                },
            ]
        })
        .collect::<Vec<_>>();

    render::table(&["NAME", "TYPE", "SIZE"], &rows);
    Ok(())
}

async fn info(session: &flipper_companion_protocol::RpcSession, path: &str) -> Result<()> {
    let storage = session.storage_info(path).await?;

    if !storage.is_present() {
        println!("{path} is not mounted.");
        return Ok(());
    }

    println!("Path:   {path}");
    println!("Total:  {}", render::bytes(storage.total_bytes));
    println!("Used:   {}", render::bytes(storage.used_bytes()));
    println!("Free:   {}", render::bytes(storage.free_bytes));
    println!("Free:   {:.1}%", storage.free_fraction().get() * 100.0);

    Ok(())
}

async fn stat(session: &flipper_companion_protocol::RpcSession, path: &str) -> Result<()> {
    match session.storage_stat(path).await? {
        Some(entry) => {
            println!("Name:   {}", entry.name);
            println!("Type:   {}", kind_label(entry.kind));
            println!("Size:   {}", render::bytes(entry.size_bytes));
        }
        None => println!("{path} does not exist."),
    }

    Ok(())
}

async fn read(
    session: &flipper_companion_protocol::RpcSession,
    path: &str,
    output: Option<std::path::PathBuf>,
) -> Result<()> {
    let data = session.storage_read(path).await?;

    match output {
        Some(output) => {
            std::fs::write(&output, &data)
                .with_context(|| format!("cannot write `{}`", output.display()))?;
            eprintln!(
                "wrote {} to {}",
                render::bytes(data.len() as u64),
                output.display()
            );
        }
        None => {
            std::io::stdout()
                .write_all(&data)
                .context("cannot write to stdout")?;
        }
    }

    Ok(())
}

async fn write(
    session: &flipper_companion_protocol::RpcSession,
    local: &std::path::Path,
    remote: &str,
) -> Result<()> {
    let data =
        std::fs::read(local).with_context(|| format!("cannot read `{}`", local.display()))?;
    session.storage_write(remote, &data).await?;
    println!("wrote {} to {remote}", render::bytes(data.len() as u64));
    Ok(())
}

async fn properties(
    session: &flipper_companion_protocol::RpcSession,
    path: &str,
    md5: bool,
    json_output: bool,
) -> Result<()> {
    let Some(entry) = session.storage_stat(path).await? else {
        bail!("{path} does not exist");
    };

    let timestamp = session.storage_timestamp(path).await.ok();
    let entries = if entry.is_directory() {
        session.storage_list(path).await.ok().map(|list| list.len())
    } else {
        None
    };
    let checksum = if md5 && !entry.is_directory() {
        Some(session.storage_md5sum(path).await?)
    } else {
        None
    };

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "path": path,
                "type": kind_label(entry.kind),
                "size": entry.size_bytes,
                "timestamp": timestamp,
                "modified": timestamp.map(|value| render::date_from_timestamp(u64::from(value))),
                "entries": entries,
                "md5": checksum,
            }))?
        );
        return Ok(());
    }

    println!("Path:      {path}");
    println!("Type:      {}", kind_label(entry.kind));
    println!(
        "Size:      {} ({} bytes)",
        if entry.is_directory() {
            String::from("-")
        } else {
            render::bytes(entry.size_bytes)
        },
        entry.size_bytes
    );

    if let Some(timestamp) = timestamp {
        println!(
            "Modified:  {} ({timestamp})",
            render::date_from_timestamp(u64::from(timestamp))
        );
    }

    if let Some(entries) = entries {
        println!("Entries:   {entries}");
    }

    if let Some(checksum) = checksum {
        println!("MD5:       {checksum}");
    }

    Ok(())
}

fn kind_label(kind: EntryKind) -> &'static str {
    match kind {
        EntryKind::File => "file",
        EntryKind::Directory => "dir",
    }
}
