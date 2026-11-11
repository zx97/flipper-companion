//! Serial-port discovery and the RPC handshake.
//!
//! The Flipper exposes its RPC service behind the CLI.  After a DTR toggle the
//! device prints its message of the day and a `>: ` prompt; sending
//! `start_rpc_session` boots the protobuf service and echoes the command.

use std::time::{Duration, Instant};

use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    time::{sleep, timeout},
};
use tokio_serial::{SerialPort, SerialPortType, SerialStream};

use crate::{error::ProtocolError, session::RpcSession};

const BAUD_RATE: u32 = 115_200;
const DTR_SETTLE: Duration = Duration::from_millis(50);
const PROMPT_TIMEOUT: Duration = Duration::from_secs(2);
const ECHO_TIMEOUT: Duration = Duration::from_secs(2);
const START_RPC_COMMAND: &[u8] = b"start_rpc_session\r";
const CLI_PROMPT: &[u8] = b">: ";

/// A serial port that may belong to a Flipper.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SerialPortDescriptor {
    pub path: String,
    pub serial_number: Option<String>,
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
    pub product: Option<String>,
}

/// Lists the serial ports currently known to the operating system.
#[must_use]
pub fn available_ports() -> Vec<SerialPortDescriptor> {
    tokio_serial::available_ports()
        .unwrap_or_default()
        .into_iter()
        .map(|info| {
            let (serial_number, vendor_id, product_id, product) = match info.port_type {
                SerialPortType::UsbPort(usb) => {
                    (usb.serial_number, Some(usb.vid), Some(usb.pid), usb.product)
                }
                _ => (None, None, None, None),
            };

            SerialPortDescriptor {
                path: info.port_name,
                serial_number,
                vendor_id,
                product_id,
                product,
            }
        })
        .collect()
}

/// Finds the serial port of a Flipper, either by serial number or by USB ID.
pub fn find_flipper_port(serial: Option<&str>) -> Result<SerialPortDescriptor, ProtocolError> {
    let ports = available_ports();

    if let Some(serial) = serial {
        return ports
            .into_iter()
            .find(|port| port.serial_number.as_deref() == Some(serial))
            .ok_or(ProtocolError::PortNotFound);
    }

    ports
        .iter()
        .find(|port| {
            port.vendor_id == Some(flipper_companion_core::FLIPPER_VENDOR_ID)
                && port.product_id == Some(flipper_companion_core::UsbMode::VCP_PRODUCT_ID)
        })
        .or_else(|| {
            ports.iter().find(|port| {
                port.product
                    .as_deref()
                    .is_some_and(|product| product.contains("Flipper"))
            })
        })
        .cloned()
        .ok_or(ProtocolError::PortNotFound)
}

/// Opens a serial port with the settings expected by the RPC service.
pub fn open_serial(path: &str) -> Result<SerialStream, ProtocolError> {
    let builder = tokio_serial::new(path, BAUD_RATE).timeout(Duration::from_millis(100));

    SerialStream::open(&builder).map_err(|error| ProtocolError::Serial(error.to_string()))
}

/// Performs the CLI-to-RPC handshake and returns a ready session.
pub async fn start_session(mut stream: SerialStream) -> Result<RpcSession, ProtocolError> {
    // Toggling DTR resets the device's CLI, which is the only reliable way to
    // reach a known prompt state (the user may have left a shell command
    // running in another terminal).
    let _ = stream.write_data_terminal_ready(false);
    sleep(DTR_SETTLE).await;
    let _ = stream.write_data_terminal_ready(true);

    let mut buffer = Vec::new();

    // The MOTD is advisory: some firmware configurations do not print one.
    let _ = wait_for(
        &mut stream,
        &mut buffer,
        |data| ends_with(data, CLI_PROMPT),
        PROMPT_TIMEOUT,
    )
    .await;

    tracing::debug!("serial: requesting the RPC session");

    stream
        .write_all(START_RPC_COMMAND)
        .await
        .map_err(ProtocolError::Io)?;
    stream.flush().await.map_err(ProtocolError::Io)?;

    wait_for(
        &mut stream,
        &mut buffer,
        |data| contains(data, START_RPC_COMMAND) && data.contains(&b'\n'),
        ECHO_TIMEOUT,
    )
    .await?;

    tracing::debug!("serial: RPC session ready");

    Ok(RpcSession::spawn(stream))
}

async fn wait_for(
    stream: &mut SerialStream,
    buffer: &mut Vec<u8>,
    predicate: impl Fn(&[u8]) -> bool,
    timeout_duration: Duration,
) -> Result<(), ProtocolError> {
    let deadline = Instant::now() + timeout_duration;

    if predicate(buffer) {
        return Ok(());
    }

    let mut chunk = [0u8; 512];

    loop {
        if predicate(buffer) {
            return Ok(());
        }

        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            return Err(ProtocolError::HandshakeTimeout);
        }

        match timeout(remaining, stream.read(&mut chunk)).await {
            Ok(Ok(0)) => return Err(ProtocolError::HandshakeTimeout),
            Ok(Ok(read)) => buffer.extend_from_slice(&chunk[..read]),
            Ok(Err(error)) => return Err(ProtocolError::Io(error)),
            Err(_) => return Err(ProtocolError::HandshakeTimeout),
        }
    }
}

fn ends_with(data: &[u8], needle: &[u8]) -> bool {
    data.len() >= needle.len() && data.ends_with(needle)
}

fn contains(data: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && data.windows(needle.len()).any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_prompt_and_command_echo() {
        assert!(ends_with(b"Welcome\r\n\r\n>: ", CLI_PROMPT));
        assert!(!ends_with(b">: ", b"x"));
        assert!(contains(b"start_rpc_session\r\n", START_RPC_COMMAND));
    }
}
