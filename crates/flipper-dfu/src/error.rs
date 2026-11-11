//! Errors raised by the DfuSe parser, the DFU state machine and the USB
//! transport.

/// Anything that can go wrong while validating or flashing a DfuSe image.
#[derive(Debug, thiserror::Error)]
pub enum DfuError {
    #[error("I/O error: {0}")]
    Io(#[from] std::io::Error),

    #[error("image is truncated: need {needed} bytes, only {available} available")]
    Truncated { needed: usize, available: usize },

    #[error("not a DfuSe image: unexpected signature")]
    BadSignature,

    #[error("unsupported DfuSe version {0}")]
    UnsupportedVersion(u8),

    #[error("image size mismatch: header declares {declared} bytes, file has {actual}")]
    SizeMismatch { declared: u32, actual: usize },

    #[error("image contains no target")]
    NoTargets,

    #[error("target {index} is invalid: {reason}")]
    InvalidTarget { index: usize, reason: String },

    #[error("element at 0x{address:08x} lies outside the device memory layout")]
    ElementOutsideLayout { address: u32 },

    #[error("CRC mismatch: stored 0x{stored:08x}, computed 0x{computed:08x}")]
    CrcMismatch { stored: u32, computed: u32 },

    #[error("invalid memory layout `{0}`")]
    InvalidLayout(String),

    #[error("device reported status 0x{status:02x} in state 0x{state:02x}")]
    Device { status: u8, state: u8 },

    #[error("DFU protocol error: {0}")]
    Protocol(String),

    #[error("USB error: {0}")]
    Usb(String),

    #[error("timed out while waiting for {0}")]
    Timeout(&'static str),
}
