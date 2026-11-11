//! Flipper Zero protobuf RPC protocol.
//!
//! The crate generates Rust types from the official `.proto` definitions and
//! layers a small actor on top of the serial transport.  See [`RpcSession`] for
//! the high-level API and [`codec::FrameCodec`] for the wire framing.

pub mod codec;
pub mod error;
pub mod screen;
pub mod serial;
pub mod session;

// The `prost` modules mirror the protobuf package hierarchy.  `pb.rs`
// references the sibling packages through `super::super::pb_*`, so they must
// live at the crate root.
#[allow(clippy::all, missing_docs)]
pub mod pb {
    include!(concat!(env!("OUT_DIR"), "/pb.rs"));
}

#[allow(clippy::all, missing_docs)]
pub mod pb_app {
    include!(concat!(env!("OUT_DIR"), "/pb_app.rs"));
}

#[allow(clippy::all, missing_docs)]
pub mod pb_desktop {
    include!(concat!(env!("OUT_DIR"), "/pb_desktop.rs"));
}

#[allow(clippy::all, missing_docs)]
pub mod pb_gpio {
    include!(concat!(env!("OUT_DIR"), "/pb_gpio.rs"));
}

#[allow(clippy::all, missing_docs)]
pub mod pb_gui {
    include!(concat!(env!("OUT_DIR"), "/pb_gui.rs"));
}

#[allow(clippy::all, missing_docs)]
pub mod pb_property {
    include!(concat!(env!("OUT_DIR"), "/pb_property.rs"));
}

#[allow(clippy::all, missing_docs)]
pub mod pb_storage {
    include!(concat!(env!("OUT_DIR"), "/pb_storage.rs"));
}

#[allow(clippy::all, missing_docs)]
pub mod pb_system {
    include!(concat!(env!("OUT_DIR"), "/pb_system.rs"));
}

pub use codec::{FrameCodec, MAX_FRAME_SIZE};
pub use error::{ProtocolError, status_label};
pub use screen::{FRAME_BYTES, HEIGHT, ROW_BYTES, ScreenFrame, WIDTH};
pub use serial::{
    SerialPortDescriptor, available_ports, find_flipper_port, open_serial, start_session,
};
pub use session::{DEFAULT_REQUEST_TIMEOUT, RpcSession, STORAGE_CHUNK_SIZE, SessionEvent};
