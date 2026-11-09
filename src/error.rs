use thiserror::Error;

/// Errors the UI treats specially. Everything else travels as plain `anyhow::Error`.
#[derive(Debug, Error)]
pub enum DsError {
    /// A service failed in a way the user should read (shown as-is).
    #[error("{0}")]
    Service(String),
    /// A database didn't shut down cleanly; ask before killing it.
    #[error("{msg}")]
    NeedsForce { sid: String, msg: String },
    /// Quick add / download problems.
    #[error("{0}")]
    Package(String),
}

pub fn service_err(msg: impl Into<String>) -> anyhow::Error {
    DsError::Service(msg.into()).into()
}

pub fn package_err(msg: impl Into<String>) -> anyhow::Error {
    DsError::Package(msg.into()).into()
}

/// The service id if `e` is a "needs force" error.
pub fn needs_force(e: &anyhow::Error) -> Option<&str> {
    match e.downcast_ref::<DsError>() {
        Some(DsError::NeedsForce { sid, .. }) => Some(sid.as_str()),
        _ => None,
    }
}
