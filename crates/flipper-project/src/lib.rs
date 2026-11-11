//! Project-side configuration compatible with the `flippy` `flip.toml`
//! format.
//!
//! Portions of this crate are adapted from
//! [flippy](https://github.com/elijah629/flippy), MIT licensed,
//! (c) 2025 elijah629.  The original licence text is in `LICENSE-MIT`.

#![forbid(unsafe_code)]

pub mod config;
pub mod project;
pub mod sources;

pub use config::{CONFIG_FILE_NAME, FlipConfig, RepositoryConfig};
pub use project::{
    CreatedProject, FETCH_PLAN_FILE_NAME, STORE_DIR_NAME, add_repository, create_project,
    create_store, fetch_store, load_config, render_fetch_plan, set_firmware,
};
pub use sources::{
    FirmwarePreset, FirmwareSource, ParseSourceError, RepositoryRemote, RepositorySource,
};
