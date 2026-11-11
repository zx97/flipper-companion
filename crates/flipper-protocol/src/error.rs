//! Errors produced by the RPC framing and session layers.

use std::time::Duration;

use flipper_companion_core::CoreError;

use crate::pb::CommandStatus;

/// A failure while exchanging protobuf messages with a device.
#[derive(Debug, thiserror::Error)]
pub enum ProtocolError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("serial port error: {0}")]
    Serial(String),

    #[error("frame is too large ({0} bytes)")]
    FrameTooLarge(usize),

    #[error("invalid frame length prefix")]
    InvalidLengthPrefix,

    #[error("protobuf decode error: {0}")]
    Decode(#[from] prost::DecodeError),

    #[error("protobuf encode error: {0}")]
    Encode(#[from] prost::EncodeError),

    #[error("device returned `{status:?}`: {message}")]
    Device {
        status: CommandStatus,
        message: &'static str,
    },

    #[error("unexpected response from device")]
    UnexpectedResponse,

    #[error("RPC session is closed: {0}")]
    Closed(String),

    #[error("request timed out after {0:?}")]
    Timeout(Duration),

    #[error("device did not answer the RPC handshake")]
    HandshakeTimeout,

    #[error("no Flipper serial port found")]
    PortNotFound,

    #[error(transparent)]
    Domain(#[from] CoreError),
}

/// Maps a protobuf status code to a human-readable label.
#[must_use]
pub fn status_label(value: i32) -> &'static str {
    match CommandStatus::try_from(value) {
        Ok(CommandStatus::Ok) => "ok",
        Ok(CommandStatus::Error) => "unknown error",
        Ok(CommandStatus::ErrorDecode) => "command could not be decoded",
        Ok(CommandStatus::ErrorNotImplemented) => "command not implemented",
        Ok(CommandStatus::ErrorBusy) => "device is busy",
        Ok(CommandStatus::ErrorContinuousCommandInterrupted) => "continuous command interrupted",
        Ok(CommandStatus::ErrorInvalidParameters) => "invalid parameters",
        Ok(CommandStatus::ErrorStorageNotReady) => "storage not ready",
        Ok(CommandStatus::ErrorStorageExist) => "path already exists",
        Ok(CommandStatus::ErrorStorageNotExist) => "path does not exist",
        Ok(CommandStatus::ErrorStorageInvalidParameter) => "invalid storage parameter",
        Ok(CommandStatus::ErrorStorageDenied) => "storage access denied",
        Ok(CommandStatus::ErrorStorageInvalidName) => "invalid storage name",
        Ok(CommandStatus::ErrorStorageInternal) => "internal storage error",
        Ok(CommandStatus::ErrorStorageNotImplemented) => "storage operation not implemented",
        Ok(CommandStatus::ErrorStorageAlreadyOpen) => "file already open",
        Ok(CommandStatus::ErrorStorageDirNotEmpty) => "directory is not empty",
        Ok(CommandStatus::ErrorAppCantStart) => "application could not start",
        Ok(CommandStatus::ErrorAppSystemLocked) => "another application is running",
        Ok(CommandStatus::ErrorAppNotRunning) => "application is not running",
        Ok(CommandStatus::ErrorAppCmdError) => "application command failed",
        Ok(CommandStatus::ErrorVirtualDisplayAlreadyStarted) => "virtual display already started",
        Ok(CommandStatus::ErrorVirtualDisplayNotStarted) => "virtual display not started",
        Ok(CommandStatus::ErrorGpioModeIncorrect) => "incorrect GPIO mode",
        Ok(CommandStatus::ErrorGpioUnknownPinMode) => "unknown GPIO pin mode",
        Err(_) => "unknown status code",
    }
}
