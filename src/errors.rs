use thiserror::Error;

#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum ApiError {
    #[error("Unauthorized: your token is invalid or expired")]
    Unauthorized,

    #[error("Forbidden: you don't have access to this resource")]
    Forbidden,

    #[error("{resource} not found")]
    NotFound { resource: String },

    #[error("Server error (HTTP {status})")]
    ServerError { status: u16 },

    #[error("Rate limited: too many requests")]
    RateLimited,

    #[error("Network error: {0}")]
    NetworkError(String),

    #[error("Failed to parse response: {0}")]
    ParseError(String),

    #[error("{0}")]
    Other(String),
}

impl ApiError {
    pub fn hint(&self) -> Option<&str> {
        match self {
            ApiError::Unauthorized => Some("Run `hooklistener login` to re-authenticate."),
            ApiError::Forbidden => Some("Check that your account has access to this resource."),
            ApiError::NotFound { .. } => {
                Some("Verify the resource exists in your Hooklistener dashboard.")
            }
            ApiError::ServerError { .. } => {
                Some("The Hooklistener server may be temporarily unavailable. Try again shortly.")
            }
            ApiError::RateLimited => Some("Wait a moment and try again."),
            ApiError::NetworkError(_) => Some("Check your internet connection and try again."),
            ApiError::ParseError(_) => None,
            ApiError::Other(_) => None,
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum TunnelError {
    #[error("Authentication failed: token is invalid or expired")]
    AuthenticationFailed,

    #[error("Endpoint not found: '{slug}'")]
    EndpointNotFound { slug: String },

    #[error("Connection refused: {0}")]
    ConnectionRefused(String),

    #[error("WebSocket error: {0}")]
    WebSocketError(String),

    #[error("Channel join failed: {reason}")]
    JoinFailed { reason: String },

    #[error("Connection timed out")]
    Timeout,

    #[error("{0}")]
    Other(String),
}

impl TunnelError {
    pub fn hint(&self) -> Option<&str> {
        match self {
            TunnelError::AuthenticationFailed => {
                Some("Run `hooklistener login` to re-authenticate.")
            }
            TunnelError::EndpointNotFound { .. } => {
                Some("Check the endpoint slug in your Hooklistener dashboard.")
            }
            TunnelError::ConnectionRefused(_) => {
                Some("Check your internet connection and that the server is reachable.")
            }
            TunnelError::WebSocketError(_) => {
                Some("The connection was interrupted. It will reconnect automatically.")
            }
            TunnelError::JoinFailed { .. } => Some(
                "The channel could not be joined. Verify the endpoint exists and you have access.",
            ),
            TunnelError::Timeout => Some("The server did not respond in time. Try again shortly."),
            TunnelError::Other(_) => None,
        }
    }

    #[allow(dead_code)]
    pub fn is_retryable(&self) -> bool {
        match self {
            TunnelError::AuthenticationFailed
            | TunnelError::EndpointNotFound { .. }
            | TunnelError::JoinFailed { .. } => false,
            TunnelError::ConnectionRefused(_)
            | TunnelError::WebSocketError(_)
            | TunnelError::Timeout
            | TunnelError::Other(_) => true,
        }
    }
}

#[allow(dead_code)]
#[derive(Debug, Error)]
pub enum ConfigError {
    #[error("Could not find config directory")]
    NoConfigDir,

    #[error("Failed to parse config: {0}")]
    ParseError(String),

    #[error("Permission denied: {0}")]
    PermissionDenied(String),
}

impl ConfigError {
    pub fn hint(&self) -> Option<&str> {
        match self {
            ConfigError::NoConfigDir => Some("Ensure your home directory is accessible."),
            ConfigError::ParseError(_) => Some(
                "Delete ~/.config/hooklistener/config.json and run `hooklistener login` again.",
            ),
            ConfigError::PermissionDenied(_) => {
                Some("Check file permissions on ~/.config/hooklistener/.")
            }
        }
    }
}
