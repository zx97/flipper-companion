//! Errors that can occur while building the domain model.

use crate::version::VersionError;

/// A failure to interpret data reported by a device or the update server.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CoreError {
    #[error("device did not report the required `{0}` property")]
    MissingProperty(&'static str),

    #[error("invalid value `{value}` for `{key}`: {reason}")]
    InvalidProperty {
        key: &'static str,
        value: String,
        reason: &'static str,
    },

    #[error(transparent)]
    Version(#[from] VersionError),
}
