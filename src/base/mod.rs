//! The base: the only code that talks to herdr-core (spec section 6). M0-3 adds the `Base` trait;
//! this holds the error type and the version check.

use std::fmt;

pub mod version;

/// The base's own account of an error (spec section 6.1).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Detail {
    pub kind: String,
    pub message: String,
    pub hint: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BaseError {
    NotFound(Detail),
    Blocked(Detail),
    StatusMismatch(Detail),
    Finished(Detail),
    Unavailable(Detail),
    UnsupportedVersion { found: String, supported: String },
    Other(Detail),
}

impl fmt::Display for BaseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            BaseError::UnsupportedVersion { found, supported } => write!(
                f,
                "herdr-core version {found:?} is not supported; makiba supports {supported}"
            ),
            BaseError::NotFound(d)
            | BaseError::Blocked(d)
            | BaseError::StatusMismatch(d)
            | BaseError::Finished(d)
            | BaseError::Unavailable(d)
            | BaseError::Other(d) => write!(f, "{}: {}", d.kind, d.message),
        }
    }
}

impl std::error::Error for BaseError {}

/// What `Base::hello()` returns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseInfo {
    pub name: String,
    pub version: String,
}
