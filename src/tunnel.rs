//! WebSocket tunnel transport shared by `listen` (TunnelClient) and `tunnel` (TunnelForwarder).

use serde::Deserialize;
use std::collections::HashMap;
use std::time::{Duration, SystemTime};
use tokio_tungstenite::tungstenite::Message;

mod client;
mod forwarder;
mod framing;
mod http;
mod limits;
mod preview;
mod relay;
mod v3_relay;
mod writer;

pub use client::TunnelClient;
pub use forwarder::TunnelForwarder;
pub use limits::PRESENTATION_QUEUE_CAPACITY;
pub(crate) use preview::body_preview;

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsWrite = futures_util::stream::SplitSink<WsStream, Message>;
type WsRead = futures_util::stream::SplitStream<WsStream>;

const REDACTED_SECRET: &str = "[REDACTED]";

/// Webhook request received from the server (Tunnel format)
#[derive(Debug, Clone, Deserialize)]
pub struct TunnelWebhookRequest {
    pub id: String,
    #[serde(default)]
    pub forward_id: Option<String>,
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub query_params: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub headers: HashMap<String, serde_json::Value>,
    pub body: Option<String>,
}

#[derive(Debug)]
pub enum TunnelEvent {
    Connecting,
    Connected,
    TunnelEstablished {
        subdomain: String,
        tunnel_id: String,
        is_static: bool,
    },
    ConnectionError(String),
    Disconnected,
    RequestReceived {
        request_id: String,
        method: String,
        path: String,
        headers: HashMap<String, String>,
        body: Option<String>,
        query_string: String,
        replay: bool,
    },
    RequestForwarded {
        request_id: String,
        status: u16,
        duration_ms: u64,
        response_headers: HashMap<String, String>,
        response_body: Option<String>,
    },
    RequestFailed {
        request_id: String,
        error: String,
    },
    StreamGap {
        dropped_events: usize,
    },
    ReplayCompleted {
        request_id: String,
        status: u16,
        duration_ms: u64,
    },
    ReplayFailed {
        request_id: String,
        error: String,
    },
    BufferedSummary {
        count: u64,
        oldest_captured_at: Option<String>,
    },
    BufferedReplayed {
        capture_id: String,
        status: u16,
    },
    BufferedReplayFailed {
        capture_id: String,
        reason: String,
    },
    WebhookReceived(Box<crate::models::WebhookRequest>),
    ForwardSuccess {
        request_id: String,
        target_url: String,
        status: u16,
        duration_ms: u64,
    },
    ForwardError {
        request_id: String,
        target_url: String,
        error: String,
        duration_ms: u64,
    },
    Reconnecting {
        attempt: u32,
        max_attempts: u32,
        next_retry_in_secs: u64,
    },
    ReconnectFailed {
        reason: String,
    },
}

/// Configuration for reconnection behavior
pub struct ReconnectConfig {
    pub max_retries: u32,
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
    pub jitter_factor: f64,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            max_retries: 10,
            initial_delay_ms: 1000,
            max_delay_ms: 60000,
            jitter_factor: 0.3,
        }
    }
}

fn websocket_endpoint(base_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base_url
            .replace("https://", "wss://")
            .replace("http://", "ws://"),
        path.trim_start_matches('/')
    )
}

/// Build a one-time ticket-authenticated WebSocket URL from a base HTTP(S) URL.
pub fn build_ws_url(base_url: &str, ticket: &str, path: &str) -> String {
    format!("{}?ticket={ticket}", websocket_endpoint(base_url, path))
}

fn redact_access_token(message: &str, access_token: &str) -> String {
    if access_token.is_empty() {
        message.to_string()
    } else {
        message.replace(access_token, REDACTED_SECRET)
    }
}

/// Build a target URL for forwarding, appending path and optional query params
#[cfg(test)]
pub fn build_forward_target(
    target_url: &str,
    path: &str,
    query_params: &HashMap<String, serde_json::Value>,
) -> String {
    let base = format!("{}{}", target_url, path);
    if query_params.is_empty() {
        base
    } else {
        let query_string: Vec<String> = query_params
            .iter()
            .map(|(k, v)| {
                let value_str = match v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    _ => v.to_string(),
                };
                format!("{}={}", k, value_str)
            })
            .collect();
        format!("{}?{}", base, query_string.join("&"))
    }
}

/// HTTP status codes that mean our credentials were rejected outright, so
/// reconnecting with the same credentials cannot succeed.
const FATAL_HTTP_STATUSES: [&str; 2] = ["401", "403"];

/// Whether `lower` (an already lowercased error message) reports one of
/// [`FATAL_HTTP_STATUSES`] as an HTTP status. Matches the shapes this crate
/// actually produces: "(HTTP 403)" / "(HTTP 401 Unauthorized; ...)" from the
/// API client and lifecycle errors, and tungstenite's "HTTP error: 401
/// Unauthorized". Bare digits are deliberately not matched, because
/// server-supplied detail text ("timed out after 4013 ms", "shard 403",
/// "lease 7f401") must not stop automatic reconnection.
fn mentions_fatal_http_status(lower: &str) -> bool {
    let tokens: Vec<&str> = lower
        .split(|c: char| !c.is_ascii_alphanumeric())
        .filter(|token| !token.is_empty())
        .collect();

    tokens.iter().enumerate().any(|(index, token)| {
        *token == "http"
            && tokens[index + 1..]
                .iter()
                .find(|next| !matches!(**next, "error" | "status"))
                .is_some_and(|code| FATAL_HTTP_STATUSES.contains(code))
    })
}

/// Determine if an error message represents a fatal (non-retryable) error
pub fn is_fatal_error(error_msg: &str) -> bool {
    let lower = error_msg.to_lowercase();
    lower.contains("authentication failed")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || mentions_fatal_http_status(&lower)
        || lower.contains("relay handshake rejected")
        || lower.contains("endpoint not found")
        || lower.contains("channel join failed")
        || lower.contains("static tunnel slug not found")
        || lower.contains("belongs to another organization")
        || lower.contains("not a member of the specified organization")
        || lower.contains("user has no organizations")
        || lower.contains("tunnel limit reached")
        || lower.contains("tunnel route has expired")
        || lower.contains("tunnel route has been revoked")
}

/// Calculate backoff duration with exponential backoff and jitter
pub fn calculate_backoff(attempt: u32, config: &ReconnectConfig) -> Duration {
    let base_delay = config.initial_delay_ms as f64 * 2_f64.powi(attempt.saturating_sub(1) as i32);
    let capped_delay = base_delay.min(config.max_delay_ms as f64);

    // Simple jitter using SystemTime nanos
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let jitter_range = capped_delay * config.jitter_factor;
    let jitter = (nanos as f64 / u32::MAX as f64) * jitter_range * 2.0 - jitter_range;

    let final_delay = (capped_delay + jitter).max(100.0);
    Duration::from_millis(final_delay as u64)
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod audit_findings;
