//! Firmware extras exposed by the RPC service: power, alerts, desktop, app
//! control and GPIO.

use std::collections::BTreeMap;

use anyhow::{Context, Result, bail};
use flipper_companion_app::Companion;
use flipper_companion_protocol::{SessionEvent, pb, pb_gui};
use serde_json::json;
use tokio::time::{Duration, Instant, timeout};

use crate::{
    cli::{
        AppCommand, DesktopCommand, GpioCommand, GpioModeArg, GpioOtgArg, GpioPullArg, InputKeyArg,
    },
    render,
};

pub async fn input(companion: &Companion, key: InputKeyArg, serial: Option<String>) -> Result<()> {
    let key = match key {
        InputKeyArg::Up => pb_gui::InputKey::Up,
        InputKeyArg::Down => pb_gui::InputKey::Down,
        InputKeyArg::Left => pb_gui::InputKey::Left,
        InputKeyArg::Right => pb_gui::InputKey::Right,
        InputKeyArg::Ok => pb_gui::InputKey::Ok,
        InputKeyArg::Back => pb_gui::InputKey::Back,
    };

    let session = companion.connect(serial.as_deref()).await?;
    session.send_input(key, pb_gui::InputType::Press).await?;
    session.send_input(key, pb_gui::InputType::Short).await?;
    tokio::time::sleep(Duration::from_millis(1)).await;
    session.send_input(key, pb_gui::InputType::Release).await?;
    let _ = session.stop().await;

    println!("input sent.");
    Ok(())
}

pub async fn power(companion: &Companion, serial: Option<String>, json_output: bool) -> Result<()> {
    let session = companion.connect(serial.as_deref()).await?;
    let info = session.system_power_info().await?;
    let _ = session.stop().await;

    let map: BTreeMap<String, String> = info.into_iter().collect();

    if json_output {
        println!("{}", serde_json::to_string_pretty(&map)?);
        return Ok(());
    }

    println!(
        "Battery:      {} ({})",
        map.get("charge_level")
            .map_or_else(|| String::from("-"), |level| format!("{level}%")),
        map.get("charge_state").map_or("unknown", String::as_str)
    );

    if let Some(voltage) = map
        .get("battery_voltage")
        .and_then(|v| v.parse::<f64>().ok())
    {
        println!("Voltage:      {:.2} V", voltage / 1000.0);
    }

    if let Some(current) = map
        .get("battery_current")
        .and_then(|v| v.parse::<i64>().ok())
    {
        println!("Current:      {current} mA");
    }

    if let Some(temperature) = map.get("battery_temp").and_then(|v| v.parse::<i64>().ok()) {
        println!("Temperature:  {temperature} °C");
    }

    if let Some(health) = map.get("battery_health") {
        println!("Health:       {health}%");
    }

    match (map.get("capacity_remain"), map.get("capacity_full")) {
        (Some(remain), Some(full)) => println!("Capacity:     {remain} / {full} mAh"),
        (Some(remain), None) => println!("Capacity:     {remain} mAh"),
        _ => {}
    }

    Ok(())
}

pub async fn alert(companion: &Companion, serial: Option<String>) -> Result<()> {
    let session = companion.connect(serial.as_deref()).await?;
    session.system_play_audiovisual_alert().await?;
    let _ = session.stop().await;

    println!("Alert played.");
    Ok(())
}

pub async fn desktop(
    companion: &Companion,
    command: DesktopCommand,
    serial: Option<String>,
) -> Result<()> {
    match command {
        DesktopCommand::Status => {
            let session = companion.connect(serial.as_deref()).await?;
            let locked = session.desktop_is_locked().await?;
            let _ = session.stop().await;

            println!("Desktop is {}.", if locked { "locked" } else { "unlocked" });
        }
        DesktopCommand::Unlock => {
            let session = companion.connect(serial.as_deref()).await?;
            session.desktop_unlock().await?;
            let _ = session.stop().await;

            println!("Desktop unlocked.");
        }
        DesktopCommand::Watch { seconds } => {
            watch_desktop(companion, serial, seconds).await?;
        }
    }

    Ok(())
}

async fn watch_desktop(companion: &Companion, serial: Option<String>, seconds: u64) -> Result<()> {
    let session = companion.connect(serial.as_deref()).await?;
    let initial = session.desktop_is_locked().await?;
    let mut events = session.subscribe();

    session.desktop_status_subscribe().await?;

    println!(
        "Watching desktop state for {seconds}s (currently {}).",
        if initial { "locked" } else { "unlocked" }
    );

    let deadline = Instant::now() + Duration::from_secs(seconds);

    while Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(Instant::now());

        match timeout(remaining, events.recv()).await {
            Ok(Ok(SessionEvent::Broadcast(message))) => {
                if let Some(pb::main::Content::DesktopStatus(status)) = message.content {
                    println!(
                        "desktop: {}",
                        if status.locked { "locked" } else { "unlocked" }
                    );
                }
            }
            Ok(Ok(SessionEvent::Closed(reason))) => {
                bail!(
                    "session closed: {}",
                    reason.unwrap_or_else(|| String::from("device disconnected"))
                );
            }
            Ok(Err(tokio::sync::broadcast::error::RecvError::Lagged(_))) => continue,
            Ok(Err(tokio::sync::broadcast::error::RecvError::Closed)) => break,
            Err(_) => break,
        }
    }

    let _ = session.desktop_status_unsubscribe().await;
    let _ = session.stop().await;

    Ok(())
}

pub async fn app(
    companion: &Companion,
    command: AppCommand,
    serial: Option<String>,
    json_output: bool,
) -> Result<()> {
    let session = companion.connect(serial.as_deref()).await?;

    let result = match command {
        AppCommand::Start { name, args } => {
            session.app_start(&name, &args).await?;
            println!("started application `{name}`");
            Ok(())
        }
        AppCommand::Exit => {
            session.app_exit().await?;
            println!("exit request sent");
            Ok(())
        }
        AppCommand::Load { path } => {
            session.app_load_file(&path).await?;
            println!("loaded `{path}`");
            Ok(())
        }
        AppCommand::Press { args, index } => {
            session.app_button_press(&args, index).await?;
            println!("button press sent ({args}, index {index})");
            Ok(())
        }
        AppCommand::Release => {
            session.app_button_release().await?;
            println!("button release sent");
            Ok(())
        }
        AppCommand::Status => {
            let locked = session.app_lock_status().await?;

            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({ "locked": locked }))?
                );
            } else {
                println!(
                    "Loader is {}.",
                    if locked {
                        "locked by an application"
                    } else {
                        "free"
                    }
                );
            }

            Ok(())
        }
        AppCommand::Error => {
            let (code, text) = session.app_get_error().await?;

            if json_output {
                println!(
                    "{}",
                    serde_json::to_string_pretty(&json!({ "code": code, "text": text }))?
                );
            } else {
                println!("{code}: {text}");
            }

            Ok(())
        }
    };

    let _ = session.stop().await;
    result
}

pub async fn gpio(
    companion: &Companion,
    command: GpioCommand,
    serial: Option<String>,
) -> Result<()> {
    let session = companion.connect(serial.as_deref()).await?;

    let result = match command {
        GpioCommand::Read { pin } => {
            let pin = pin_value(&pin)?;
            let value = session.gpio_read(pin).await?;
            println!("{}", if value != 0 { "high" } else { "low" });
            Ok(())
        }
        GpioCommand::Mode { pin } => {
            let pin = pin_value(&pin)?;
            let mode = session.gpio_get_mode(pin).await?;
            println!("{}", mode_label(mode));
            Ok(())
        }
        GpioCommand::SetMode { pin, mode, yes } => {
            let pin = pin_value(&pin)?;
            let mode = match mode {
                GpioModeArg::Input => 1,
                GpioModeArg::Output => 0,
            };

            eprintln!("warning: this reconfigures a physical pin; check your wiring");

            if !render::confirm("Configure this GPIO pin?", yes) {
                bail!("aborted");
            }

            session.gpio_set_mode(pin, mode).await?;
            println!("GPIO pin configured as {}.", mode_label(mode));
            Ok(())
        }
        GpioCommand::Write { pin, value, yes } => {
            let pin = pin_value(&pin)?;

            eprintln!("warning: this drives a physical pin; check your wiring");

            if !render::confirm("Drive this GPIO pin?", yes) {
                bail!("aborted");
            }

            session.gpio_write(pin, value).await?;
            println!("GPIO pin written.");
            Ok(())
        }
        GpioCommand::Pull { pin, pull, yes } => {
            let pin = pin_value(&pin)?;
            let pull = match pull {
                GpioPullArg::Up => 1,
                GpioPullArg::Down => 2,
                GpioPullArg::Floating => 0,
            };

            eprintln!("warning: this reconfigures a physical pin; check your wiring");

            if !render::confirm("Change the input pull resistor?", yes) {
                bail!("aborted");
            }

            session.gpio_set_input_pull(pin, pull).await?;
            println!("GPIO pull resistor configured.");
            Ok(())
        }
        GpioCommand::Otg => {
            let mode = session.gpio_get_otg_mode().await?;
            println!("5V OTG is {}.", if mode != 0 { "on" } else { "off" });
            Ok(())
        }
        GpioCommand::SetOtg { mode, yes } => {
            let mode = match mode {
                GpioOtgArg::On => 1,
                GpioOtgArg::Off => 0,
            };

            eprintln!("warning: the 5V OTG output powers external hardware");

            if !render::confirm("Change the 5V OTG output?", yes) {
                bail!("aborted");
            }

            session.gpio_set_otg_mode(mode).await?;
            println!("5V OTG is now {}.", if mode != 0 { "on" } else { "off" });
            Ok(())
        }
    };

    let _ = session.stop().await;
    result
}

fn pin_value(input: &str) -> Result<i32> {
    match input.trim().to_ascii_uppercase().as_str() {
        "PC0" => Ok(0),
        "PC1" => Ok(1),
        "PC3" => Ok(2),
        "PB2" => Ok(3),
        "PB3" => Ok(4),
        "PA4" => Ok(5),
        "PA6" => Ok(6),
        "PA7" => Ok(7),
        other => other
            .parse::<i32>()
            .ok()
            .filter(|value| (0..=7).contains(value))
            .with_context(|| {
                format!(
                    "unknown GPIO pin `{input}`; expected PC0, PC1, PC3, PB2, PB3, PA4, PA6, PA7 or 0-7"
                )
            }),
    }
}

fn mode_label(mode: i32) -> &'static str {
    match mode {
        0 => "output",
        1 => "input",
        _ => "unknown",
    }
}
