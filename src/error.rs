use std::{fmt, path::PathBuf};

/// Availability of one Linux interface. Errors remain attached to their paths.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "status", content = "message", rename_all = "snake_case")]
pub enum InterfaceStatus {
    Available,
    NotAvailable,
    PermissionDenied(String),
    ReadError(String),
    ParseError(String),
}

#[derive(Debug)]
pub struct CollectError {
    pub path: PathBuf,
    pub message: String,
}
impl fmt::Display for CollectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.path.display(), self.message)
    }
}
impl std::error::Error for CollectError {}
