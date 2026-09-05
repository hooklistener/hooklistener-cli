use thiserror::Error;

#[derive(Debug, Error)]
pub enum TunnelLifecycleError {
    #[error("Tunnel lifecycle schema {actual} is incompatible with supported major {supported}")]
    IncompatibleSchema { supported: u64, actual: u64 },

    #[error("Tunnel event cursor expired")]
    CursorExpired {
        earliest_cursor: Option<String>,
        resync: serde_json::Value,
    },

    #[error("Tunnel API request failed ({code}, HTTP {status}): {message}")]
    Api {
        status: u16,
        code: String,
        message: String,
    },
}

impl TunnelLifecycleError {
    pub fn code(&self) -> &str {
        match self {
            Self::IncompatibleSchema { .. } => "incompatible_schema",
            Self::CursorExpired { .. } => "cursor_expired",
            Self::Api { code, .. } => code,
        }
    }

    pub fn hint(&self) -> Option<&str> {
        match self {
            Self::IncompatibleSchema { .. } => {
                Some("Upgrade Hooklistener CLI before activating this tunnel.")
            }
            Self::CursorExpired {
                earliest_cursor: Some(_),
                ..
            } => Some(
                "Rehydrate sessions, captures, attempts, and outcomes, then resume from the earliest cursor.",
            ),
            Self::CursorExpired {
                earliest_cursor: None,
                ..
            } => Some(
                "Resync sessions, captures, attempts, and outcomes, then request a fresh event cursor.",
            ),
            Self::Api { .. } => None,
        }
    }
}

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("Failed to check for updates: {0}")]
    CheckFailed(String),

    #[error("Failed to update: {0}")]
    UpdateFailed(String),
}

impl UpdateError {
    pub fn hint(&self) -> Option<&str> {
        match self {
            UpdateError::CheckFailed(_) => Some("Check your internet connection and try again."),
            UpdateError::UpdateFailed(_) => {
                Some("Try updating manually or check file permissions.")
            }
        }
    }
}
