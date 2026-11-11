//! `flipper apps` — catalog browsing and FAP installation.

use anyhow::{Context, Result, bail};
use flipper_companion_app::Companion;
use serde_json::json;

use crate::{
    cli::{AppsCommand, AppsSourcesCommand},
    render,
};

pub async fn run(companion: &Companion, command: AppsCommand, json_output: bool) -> Result<()> {
    match command {
        AppsCommand::List {
            target,
            api,
            category,
            query,
            source,
        } => list(companion, target, api, category, query, source, json_output).await,
        AppsCommand::Install {
            id,
            target,
            api,
            serial,
            yes,
        } => install(companion, id, target, api, serial, yes, json_output).await,
        AppsCommand::Installed { serial } => installed(companion, serial, json_output).await,
        AppsCommand::Outdated {
            serial,
            target,
            api,
        } => outdated(companion, serial, target, api, json_output).await,
        AppsCommand::Uninstall { name, serial, yes } => {
            uninstall(companion, &name, serial, yes, json_output).await
        }
        AppsCommand::Sources { command } => sources(command, json_output),
    }
}

async fn resolve_context(
    companion: &Companion,
    target: Option<String>,
    api: Option<String>,
    serial: Option<&str>,
) -> Result<(String, String)> {
    if let (Some(target), Some(api)) = (target.as_deref(), api.as_deref()) {
        return Ok((target.to_owned(), api.to_owned()));
    }

    let devices = companion.scan().await?;
    let device = devices
        .iter()
        .find(|device| {
            !device.is_dfu()
                && serial.is_none_or(|serial| device.usb.serial_number.as_deref() == Some(serial))
        })
        .or_else(|| devices.iter().find(|device| !device.is_dfu()))
        .context("no running Flipper is connected")?;
    let info = device.info.as_ref().context("device info unavailable")?;

    Ok((
        target.unwrap_or_else(|| info.hardware.target.clone()),
        api.or_else(|| info.firmware.api_version.map(|version| version.to_string()))
            .context("firmware API version is not reported by this firmware")?,
    ))
}

async fn list(
    companion: &Companion,
    target: Option<String>,
    api: Option<String>,
    category: Option<String>,
    query: Option<String>,
    source: Option<String>,
    json_output: bool,
) -> Result<()> {
    let (target, api) = resolve_context(companion, target, api, None).await?;
    let apps = companion
        .catalog_apps(
            &target,
            &api,
            category.as_deref(),
            query.as_deref(),
            source.as_deref(),
        )
        .await?;

    if json_output {
        let value = apps
            .iter()
            .map(|app| {
                json!({
                    "id": app.id,
                    "alias": app.alias,
                    "name": app.name,
                    "version": app.version,
                    "category": app.category,
                    "author": app.author,
                    "source": app.source,
                    "target": app.target,
                    "api": app.api,
                    "size": app.size_bytes,
                    "description": app.description,
                })
            })
            .collect::<Vec<_>>();
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    if apps.is_empty() {
        println!("No app found.");
        return Ok(());
    }

    let rows = apps
        .iter()
        .map(|app| {
            vec![
                app.id.clone(),
                app.name.clone(),
                app.version.clone(),
                app.category.clone(),
                app.source.clone(),
                app.size_bytes
                    .map_or_else(|| String::from("-"), render::bytes),
            ]
        })
        .collect::<Vec<_>>();

    render::table(
        &["ID", "NAME", "VERSION", "CATEGORY", "SOURCE", "SIZE"],
        &rows,
    );
    Ok(())
}

async fn install(
    companion: &Companion,
    id: String,
    target: Option<String>,
    api: Option<String>,
    serial: Option<String>,
    yes: bool,
    json_output: bool,
) -> Result<()> {
    let (target, api) = resolve_context(companion, target, api, serial.as_deref()).await?;
    let apps = companion
        .catalog_apps(&target, &api, None, None, None)
        .await?;
    let app = apps
        .iter()
        .find(|app| app.id == id || app.alias == id)
        .with_context(|| format!("app `{id}` not found in the catalog"))?;

    let prompt = format!("Install {} {} on the Flipper?", app.name, app.version);

    if !render::confirm(&prompt, yes) {
        bail!("aborted");
    }

    let mut progress = render::LineProgress::new("Downloading");
    let report = companion
        .install_app(
            serial.as_deref(),
            app,
            |value| progress.set(value),
            |_phase| {},
        )
        .await?;
    progress.finish();

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "name": report.name,
                "version": report.version,
                "remote_path": report.remote_path,
                "bytes": report.bytes,
            }))?
        );
    } else {
        println!(
            "Installed {} {} -> {} ({})",
            report.name,
            report.version,
            report.remote_path,
            render::bytes(report.bytes)
        );
    }

    Ok(())
}

async fn installed(companion: &Companion, serial: Option<String>, json_output: bool) -> Result<()> {
    let apps = companion.installed_apps(serial.as_deref()).await?;

    if json_output {
        let value = apps
            .iter()
            .map(|app| {
                json!({
                    "name": app.name,
                    "category": app.category,
                    "path": app.path,
                    "size": app.size_bytes,
                })
            })
            .collect::<Vec<_>>();
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    if apps.is_empty() {
        println!("No app installed on the SD card.");
        return Ok(());
    }

    let rows = apps
        .iter()
        .map(|app| {
            vec![
                app.name.clone(),
                app.category.clone(),
                render::bytes(app.size_bytes),
                app.path.clone(),
            ]
        })
        .collect::<Vec<_>>();

    render::table(&["NAME", "CATEGORY", "SIZE", "PATH"], &rows);
    Ok(())
}

async fn outdated(
    companion: &Companion,
    serial: Option<String>,
    target: Option<String>,
    api: Option<String>,
    json_output: bool,
) -> Result<()> {
    let (target, api) = resolve_context(companion, target, api, serial.as_deref()).await?;
    let installed = companion.installed_apps(serial.as_deref()).await?;
    let catalog = companion
        .catalog_apps(&target, &api, None, None, None)
        .await?;

    let mut candidates = Vec::new();

    for app in &installed {
        if let Some(catalog_app) = catalog.iter().find(|candidate| {
            candidate.alias.eq_ignore_ascii_case(&app.name)
                && candidate.category.eq_ignore_ascii_case(&app.category)
        }) {
            candidates.push((app, catalog_app));
        }
    }

    let ids = candidates
        .iter()
        .map(|(_, catalog_app)| catalog_app.id.clone())
        .collect::<Vec<_>>();
    let sizes = companion.catalog_sizes(&target, &api, &ids).await;

    let updates = candidates
        .iter()
        .filter_map(|(app, catalog_app)| {
            let size = *sizes.get(&catalog_app.id)?;

            (size != app.size_bytes).then_some((*app, *catalog_app, size))
        })
        .collect::<Vec<_>>();

    if json_output {
        let value = updates
            .iter()
            .map(|(app, catalog_app, size)| {
                json!({
                    "name": app.name,
                    "category": app.category,
                    "path": app.path,
                    "installed_size": app.size_bytes,
                    "available_size": size,
                    "app_id": catalog_app.id,
                    "version": catalog_app.version,
                })
            })
            .collect::<Vec<_>>();
        println!("{}", serde_json::to_string_pretty(&value)?);
        return Ok(());
    }

    if updates.is_empty() {
        println!("Everything is up to date.");
        return Ok(());
    }

    let rows = updates
        .iter()
        .map(|(app, catalog_app, size)| {
            vec![
                app.name.clone(),
                app.category.clone(),
                render::bytes(app.size_bytes),
                render::bytes(*size),
                catalog_app.version.clone(),
            ]
        })
        .collect::<Vec<_>>();

    render::table(
        &["NAME", "CATEGORY", "INSTALLED", "AVAILABLE", "VERSION"],
        &rows,
    );
    Ok(())
}

async fn uninstall(
    companion: &Companion,
    name: &str,
    serial: Option<String>,
    yes: bool,
    json_output: bool,
) -> Result<()> {
    let apps = companion.installed_apps(serial.as_deref()).await?;

    let app = apps
        .iter()
        .find(|app| {
            app.path == name
                || app.name.eq_ignore_ascii_case(name)
                || app
                    .path
                    .to_ascii_lowercase()
                    .ends_with(&format!("/{}.fap", name.to_ascii_lowercase()))
        })
        .with_context(|| format!("no app named `{name}` is installed"))?;

    let prompt = format!("Uninstall {} ({})?", app.name, app.path);

    if !render::confirm(&prompt, yes) {
        bail!("aborted");
    }

    companion
        .uninstall_app(serial.as_deref(), &app.path)
        .await?;

    if json_output {
        println!(
            "{}",
            serde_json::to_string_pretty(&json!({
                "name": app.name,
                "path": app.path,
                "removed": true,
            }))?
        );
    } else {
        println!("Uninstalled {} ({})", app.name, app.path);
    }

    Ok(())
}

fn sources(command: AppsSourcesCommand, json_output: bool) -> Result<()> {
    match command {
        AppsSourcesCommand::List => {
            let sources = flipper_companion_app::load_custom_sources();

            if json_output {
                println!("{}", serde_json::to_string_pretty(&sources)?);
            } else if sources.is_empty() {
                println!("No custom app source configured.");
            } else {
                for source in sources {
                    println!("{source}");
                }
            }
        }
        AppsSourcesCommand::Add { url } => {
            flipper_companion_app::add_custom_source(&url)?;
            println!("Added {url}");
        }
    }

    Ok(())
}
