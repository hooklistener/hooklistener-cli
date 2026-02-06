use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc;
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, error::Error as WsError, http::StatusCode},
};
use tracing::{debug, error, info, warn};

/// Phoenix Channel message structure
#[derive(Debug, Serialize, Deserialize)]
struct ChannelMessage {
    topic: String,
    event: String,
    payload: serde_json::Value,
    #[serde(rename = "ref")]
    reference: Option<String>,
}

/// Webhook request received from the server (Tunnel format)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TunnelWebhookRequest {
    pub id: String,
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
    },
    RequestForwarded {
        request_id: String,
        status: u16,
        duration_ms: u64,
    },
    RequestFailed {
        request_id: String,
        error: String,
    },
    WebhookReceived(Box<crate::models::WebhookRequest>),
    ForwardSuccess,
    ForwardError,
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

/// Build a WebSocket URL from a base HTTP(S) URL
#[cfg(test)]
pub fn build_ws_url(base_url: &str, token: &str, path: &str) -> String {
    format!(
        "{}/{}?token={}",
        base_url
            .replace("https://", "wss://")
            .replace("http://", "ws://"),
        path.trim_start_matches('/'),
        token
    )
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

/// Determine if an error message represents a fatal (non-retryable) error
pub fn is_fatal_error(error_msg: &str) -> bool {
    let lower = error_msg.to_lowercase();
    lower.contains("authentication failed")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("401")
        || lower.contains("403")
        || lower.contains("endpoint not found")
        || lower.contains("channel join failed")
        || lower.contains("tunnel join failed")
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

/// Tunnel client for WebSocket connection to Hooklistener server
pub struct TunnelClient {
    access_token: String,
    endpoint_slug: String,
    target_url: String,
    base_url: String,
    event_tx: mpsc::Sender<TunnelEvent>,
}

impl TunnelClient {
    pub fn new(
        access_token: String,
        endpoint_slug: String,
        target_url: String,
        base_url: Option<String>,
        event_tx: mpsc::Sender<TunnelEvent>,
    ) -> Self {
        // Check environment variable for local development
        let base_url = base_url
            .or_else(|| std::env::var("HOOKLISTENER_WS_URL").ok())
            .unwrap_or_else(|| "wss://api.hooklistener.com".to_string());

        Self {
            access_token,
            endpoint_slug,
            target_url,
            base_url,
            event_tx,
        }
    }

    /// Connect to WebSocket and start listening for webhook events
    pub async fn connect_and_listen(&self) -> Result<()> {
        info!(
            endpoint = %self.endpoint_slug,
            target = %self.target_url,
            "Connecting to WebSocket tunnel"
        );

        // Build WebSocket URL with auth token
        let ws_url = format!(
            "{}/socket/websocket?token={}",
            self.base_url
                .replace("https://", "wss://")
                .replace("http://", "ws://"),
            self.access_token
        );

        debug!("WebSocket URL: {}", ws_url);

        // Connect to WebSocket
        let (ws_stream, _) = match connect_async(&ws_url).await {
            Ok(stream) => stream,
            Err(e) => match e {
                WsError::Http(response) => match response.status() {
                    StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                        let msg = "Authentication failed: The token is invalid or expired.";
                        let _ = self
                            .event_tx
                            .send(TunnelEvent::ConnectionError(msg.to_string()))
                            .await;
                        return Err(anyhow!(msg));
                    }
                    StatusCode::NOT_FOUND => {
                        let msg = format!("Endpoint not found: '{}'.", self.endpoint_slug);
                        let _ = self
                            .event_tx
                            .send(TunnelEvent::ConnectionError(msg.clone()))
                            .await;
                        return Err(anyhow!(msg));
                    }
                    status => {
                        let msg = format!("Connection failed with HTTP status: {}", status);
                        let _ = self
                            .event_tx
                            .send(TunnelEvent::ConnectionError(msg.clone()))
                            .await;
                        return Err(anyhow!(msg));
                    }
                },
                WsError::Io(e) => {
                    let msg = format!("Connection refused: {}.", e);
                    let _ = self
                        .event_tx
                        .send(TunnelEvent::ConnectionError(msg.clone()))
                        .await;
                    return Err(anyhow!(msg));
                }
                _ => {
                    let msg = format!("Failed to connect to WebSocket: {}", e);
                    let _ = self
                        .event_tx
                        .send(TunnelEvent::ConnectionError(msg.clone()))
                        .await;
                    return Err(anyhow!(msg));
                }
            },
        };

        info!("WebSocket connected successfully");

        let (mut write, mut read) = ws_stream.split();

        // Join the CLI tunnel channel
        let channel_topic = format!("cli:tunnel:{}", self.endpoint_slug);
        let join_message = ChannelMessage {
            topic: channel_topic.clone(),
            event: "phx_join".to_string(),
            payload: serde_json::json!({}),
            reference: Some("1".to_string()),
        };

        let join_json = serde_json::to_string(&join_message)?;
        write
            .send(Message::Text(join_json.into()))
            .await
            .context("Failed to send join message")?;

        // Wait for join confirmation
        let mut joined = false;
        while !joined {
            match tokio::time::timeout(Duration::from_secs(5), read.next()).await {
                Ok(Some(msg_result)) => match msg_result {
                    Ok(Message::Text(text)) => {
                        let msg: ChannelMessage = serde_json::from_str(&text)?;
                        if msg.event == "phx_reply"
                            && msg.reference.as_deref() == Some("1")
                            && let Some(status) = msg.payload.get("status")
                        {
                            if status == "ok" {
                                let _ = self.event_tx.send(TunnelEvent::Connected).await;
                                info!(channel = %channel_topic, "Joined channel");
                                joined = true;
                            } else {
                                let reason = msg
                                    .payload
                                    .get("response")
                                    .and_then(|r| r.get("reason"))
                                    .and_then(|r| r.as_str())
                                    .unwrap_or("Unknown error");
                                let _ = self
                                    .event_tx
                                    .send(TunnelEvent::ConnectionError(reason.to_string()))
                                    .await;
                                return Err(anyhow!("Channel join failed: {}", reason));
                            }
                        }
                    }
                    Ok(Message::Ping(data)) => {
                        write.send(Message::Pong(data)).await?;
                    }
                    Ok(Message::Close(frame)) => {
                        return Err(anyhow!("WebSocket closed during join: {:?}", frame));
                    }
                    Err(e) => return Err(anyhow!("WebSocket error during join: {}", e)),
                    _ => {}
                },
                Ok(None) => return Err(anyhow!("WebSocket stream ended during join")),
                Err(_) => return Err(anyhow!("Timeout waiting for channel join response")),
            }
        }

        // Track last heartbeat time and counter
        let mut last_heartbeat = tokio::time::Instant::now();
        let mut heartbeat_counter = 2;
        let heartbeat_interval = Duration::from_secs(30);

        // Listen for messages
        loop {
            // Check if we need to send a heartbeat
            if last_heartbeat.elapsed() >= heartbeat_interval {
                let heartbeat = ChannelMessage {
                    topic: "phoenix".to_string(),
                    event: "heartbeat".to_string(),
                    payload: serde_json::json!({}),
                    reference: Some(heartbeat_counter.to_string()),
                };
                heartbeat_counter += 1;

                if let Ok(json) = serde_json::to_string(&heartbeat)
                    && let Err(e) = write.send(Message::Text(json.into())).await
                {
                    error!("Failed to send heartbeat: {}", e);
                    break;
                }
                last_heartbeat = tokio::time::Instant::now();
            }

            // Use timeout to allow heartbeat checks
            match tokio::time::timeout(Duration::from_millis(100), read.next()).await {
                Ok(Some(msg)) => match msg {
                    Ok(Message::Text(text)) => {
                        if let Err(e) = self.handle_message(&text, &mut write).await {
                            error!("Error handling message: {}", e);
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        info!("WebSocket closed: {:?}", frame);
                        let _ = self
                            .event_tx
                            .send(TunnelEvent::ConnectionError(
                                "WebSocket connection closed".to_string(),
                            ))
                            .await;
                        break;
                    }
                    Ok(Message::Ping(data)) => {
                        debug!("Received ping, sending pong");
                        if let Err(e) = write.send(Message::Pong(data)).await {
                            error!("Failed to send pong: {}", e);
                            break;
                        }
                    }
                    Ok(_) => {
                        // Ignore other message types
                    }
                    Err(e) => {
                        error!("WebSocket error: {}", e);
                        let _ = self
                            .event_tx
                            .send(TunnelEvent::ConnectionError(format!(
                                "WebSocket error: {}",
                                e
                            )))
                            .await;
                        break;
                    }
                },
                Ok(None) => {
                    warn!("WebSocket stream ended");
                    let _ = self
                        .event_tx
                        .send(TunnelEvent::ConnectionError(
                            "WebSocket stream ended".to_string(),
                        ))
                        .await;
                    break;
                }
                Err(_) => {
                    // Timeout - continue to check heartbeat
                    continue;
                }
            }
        }

        Ok(())
    }

    async fn handle_message(
        &self,
        text: &str,
        write: &mut futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
    ) -> Result<()> {
        let msg: ChannelMessage = serde_json::from_str(text)?;

        debug!(
            topic = %msg.topic,
            event = %msg.event,
            "Received message"
        );

        match msg.event.as_str() {
            "phx_reply" => {
                // Already handled join reply, ignoring subsequent ones for now
            }
            "webhook_received" => {
                // New webhook to forward
                if let Some(request_data) = msg.payload.get("request") {
                    match serde_json::from_value::<TunnelWebhookRequest>(request_data.clone()) {
                        Ok(request) => {
                            // Convert to model WebhookRequest for UI
                            let model_request = crate::models::WebhookRequest {
                                id: request.id.clone(),
                                timestamp: chrono::Utc::now().timestamp(),
                                remote_addr: "Tunnel".to_string(),
                                headers: request
                                    .headers
                                    .iter()
                                    .map(|(k, v)| {
                                        (
                                            k.clone(),
                                            match v {
                                                serde_json::Value::String(s) => s.clone(),
                                                _ => v.to_string(),
                                            },
                                        )
                                    })
                                    .collect(),
                                content_length: request
                                    .body
                                    .as_ref()
                                    .map(|b| b.len() as i64)
                                    .unwrap_or(0),
                                method: request.method.clone(),
                                url: request.path.clone(),
                                path: Some(request.path.clone()),
                                query_params: request
                                    .query_params
                                    .iter()
                                    .map(|(k, v)| {
                                        (
                                            k.clone(),
                                            match v {
                                                serde_json::Value::String(s) => s.clone(),
                                                _ => v.to_string(),
                                            },
                                        )
                                    })
                                    .collect(),
                                created_at: chrono::Utc::now().to_rfc3339(),
                                body_preview: request.body.clone(),
                                body: request.body.clone(),
                            };

                            // Notify UI
                            let _ = self
                                .event_tx
                                .send(TunnelEvent::WebhookReceived(Box::new(model_request)))
                                .await;

                            self.forward_webhook(request, write).await?;
                        }
                        Err(e) => {
                            let err_msg =
                                format!("Invalid webhook payload: {}. Data: {}", e, request_data);
                            error!("{}", err_msg);
                            let _ = self
                                .event_tx
                                .send(TunnelEvent::ConnectionError(err_msg.clone()))
                                .await;
                            // We don't return error here to keep connection alive, just log/notify
                        }
                    }
                }
            }
            _ => {
                debug!("Unhandled event: {}", msg.event);
            }
        }

        Ok(())
    }

    async fn forward_webhook(
        &self,
        request: TunnelWebhookRequest,
        write: &mut futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
    ) -> Result<()> {
        info!(
            request_id = %request.id,
            method = %request.method,
            path = %request.path,
            "Forwarding webhook to local server"
        );

        // Build target URL
        let target = format!("{}{}", self.target_url, request.path);

        // Add query params if present
        let target_with_query = if !request.query_params.is_empty() {
            let query_string: Vec<String> = request
                .query_params
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
            format!("{}?{}", target, query_string.join("&"))
        } else {
            target
        };

        // Create HTTP client
        let client = reqwest::Client::new();

        // Build request with method
        let mut req_builder = match request.method.as_str() {
            "GET" => client.get(&target_with_query),
            "POST" => client.post(&target_with_query),
            "PUT" => client.put(&target_with_query),
            "DELETE" => client.delete(&target_with_query),
            "PATCH" => client.patch(&target_with_query),
            "HEAD" => client.head(&target_with_query),
            _ => {
                warn!("Unsupported HTTP method: {}", request.method);
                return Ok(());
            }
        };

        // Add headers
        for (key, value) in &request.headers {
            // Skip host header as it will be set by reqwest
            if key.to_lowercase() != "host" {
                let value_str = match value {
                    serde_json::Value::String(s) => s.clone(),
                    _ => value.to_string(),
                };
                req_builder = req_builder.header(key, value_str);
            }
        }

        // Add body if present
        if let Some(body) = &request.body {
            req_builder = req_builder.body(body.clone());
        }

        // Send request
        match req_builder.send().await {
            Ok(response) => {
                let status = response.status();
                info!(
                    request_id = %request.id,
                    status = %status,
                    "Request forwarded successfully"
                );

                let _ = self.event_tx.send(TunnelEvent::ForwardSuccess).await;

                // Send acknowledgment back to server
                let ack_message = ChannelMessage {
                    topic: format!("cli:tunnel:{}", self.endpoint_slug),
                    event: "request_ack".to_string(),
                    payload: serde_json::json!({
                        "request_id": request.id,
                        "status": "proxied",
                        "proxied_to": target_with_query,
                    }),
                    reference: None,
                };

                let ack_json = serde_json::to_string(&ack_message)?;
                write.send(Message::Text(ack_json.into())).await?;
            }
            Err(e) => {
                error!(
                    request_id = %request.id,
                    error = %e,
                    "Failed to forward request"
                );

                let _ = self.event_tx.send(TunnelEvent::ForwardError).await;

                // Send error acknowledgment
                let ack_message = ChannelMessage {
                    topic: format!("cli:tunnel:{}", self.endpoint_slug),
                    event: "request_ack".to_string(),
                    payload: serde_json::json!({
                        "request_id": request.id,
                        "status": "error",
                        "error": e.to_string(),
                    }),
                    reference: None,
                };

                let ack_json = serde_json::to_string(&ack_message)?;
                write.send(Message::Text(ack_json.into())).await?;
            }
        }

        Ok(())
    }

    /// Connect with automatic reconnection on recoverable errors
    pub async fn connect_with_reconnect(&self, config: ReconnectConfig) -> Result<()> {
        let mut attempt: u32 = 0;

        loop {
            let start = tokio::time::Instant::now();
            let result = self.connect_and_listen().await;

            match result {
                Ok(()) => {
                    // Clean disconnect, try reconnecting
                    if start.elapsed() > Duration::from_secs(5) {
                        attempt = 0; // Reset if connection lasted > 5s
                    }
                }
                Err(ref e) => {
                    let err_msg = e.to_string();
                    if is_fatal_error(&err_msg) {
                        let _ = self
                            .event_tx
                            .send(TunnelEvent::ReconnectFailed { reason: err_msg })
                            .await;
                        return result;
                    }

                    if start.elapsed() > Duration::from_secs(5) {
                        attempt = 0;
                    }
                }
            }

            attempt += 1;
            if attempt > config.max_retries {
                let reason = "Maximum reconnection attempts exceeded".to_string();
                let _ = self
                    .event_tx
                    .send(TunnelEvent::ReconnectFailed {
                        reason: reason.clone(),
                    })
                    .await;
                return Err(anyhow!(reason));
            }

            let backoff = calculate_backoff(attempt, &config);
            let next_retry_secs = backoff.as_secs();

            info!(
                attempt = attempt,
                max_attempts = config.max_retries,
                next_retry_in_secs = next_retry_secs,
                "Reconnecting..."
            );

            let _ = self
                .event_tx
                .send(TunnelEvent::Reconnecting {
                    attempt,
                    max_attempts: config.max_retries,
                    next_retry_in_secs: next_retry_secs,
                })
                .await;

            tokio::time::sleep(backoff).await;
        }
    }
}

/// HTTP Tunnel forwarder - connects to /tunnel endpoint and forwards HTTP requests
pub struct TunnelForwarder {
    access_token: String,
    local_host: String,
    local_port: u16,
    org_id: Option<String>,
    slug: Option<String>,
    base_url: String,
    event_tx: mpsc::Sender<TunnelEvent>,
}

impl TunnelForwarder {
    pub fn new(
        access_token: String,
        local_host: String,
        local_port: u16,
        org_id: Option<String>,
        slug: Option<String>,
        event_tx: mpsc::Sender<TunnelEvent>,
    ) -> Self {
        let base_url = std::env::var("HOOKLISTENER_API_URL")
            .unwrap_or_else(|_| "https://app.hooklistener.com".to_string());

        Self {
            access_token,
            local_host,
            local_port,
            org_id,
            slug,
            base_url,
            event_tx,
        }
    }

    pub async fn connect_and_forward(&self) -> Result<()> {
        info!(
            local_host = %self.local_host,
            local_port = %self.local_port,
            "Starting HTTP tunnel"
        );

        let _ = self.event_tx.send(TunnelEvent::Connecting).await;

        // Build WebSocket URL - connect to /tunnel/websocket endpoint (Phoenix default)
        let ws_url = format!(
            "{}/tunnel/websocket?token={}",
            self.base_url
                .replace("https://", "wss://")
                .replace("http://", "ws://"),
            self.access_token
        );

        debug!("Tunnel WebSocket URL: {}", ws_url);

        // Connect to WebSocket
        let (ws_stream, _) = match connect_async(&ws_url).await {
            Ok(stream) => stream,
            Err(e) => {
                let msg = format!("Failed to connect to tunnel: {}", e);
                let _ = self
                    .event_tx
                    .send(TunnelEvent::ConnectionError(msg.clone()))
                    .await;
                return Err(anyhow!(msg));
            }
        };

        info!("Tunnel WebSocket connected successfully");

        let (mut write, mut read) = ws_stream.split();

        // Join the tunnel:connect channel with local_port, organization_id, and optional slug
        let mut join_payload = serde_json::json!({
            "local_port": self.local_port,
        });

        if let Some(org_id) = &self.org_id {
            join_payload["organization_id"] = serde_json::Value::String(org_id.clone());
        }

        if let Some(slug) = &self.slug {
            join_payload["slug"] = serde_json::Value::String(slug.clone());
            info!(slug = %slug, "Requesting static tunnel");
        }

        let join_message = ChannelMessage {
            topic: "tunnel:connect".to_string(),
            event: "phx_join".to_string(),
            payload: join_payload,
            reference: Some("1".to_string()),
        };

        let join_json = serde_json::to_string(&join_message)?;
        write
            .send(Message::Text(join_json.into()))
            .await
            .context("Failed to send join message")?;

        // Wait for join confirmation
        let mut joined = false;
        let mut tunnel_topic = String::new();

        while !joined {
            match tokio::time::timeout(Duration::from_secs(10), read.next()).await {
                Ok(Some(msg_result)) => match msg_result {
                    Ok(Message::Text(text)) => {
                        let msg: ChannelMessage = serde_json::from_str(&text)?;
                        if msg.event == "phx_reply"
                            && msg.reference.as_deref() == Some("1")
                            && let Some(status) = msg.payload.get("status")
                        {
                            if status == "ok" {
                                // Extract subdomain, tunnel_id, and static flag from response
                                if let Some(response) = msg.payload.get("response") {
                                    let subdomain = response
                                        .get("subdomain")
                                        .and_then(|s| s.as_str())
                                        .unwrap_or("unknown")
                                        .to_string();
                                    let tunnel_id = response
                                        .get("tunnel_id")
                                        .and_then(|s| s.as_str())
                                        .unwrap_or("unknown")
                                        .to_string();
                                    let is_static = response
                                        .get("static")
                                        .and_then(|s| s.as_bool())
                                        .unwrap_or(false);

                                    let tunnel_type =
                                        if is_static { "static" } else { "ephemeral" };
                                    info!(
                                        subdomain = %subdomain,
                                        tunnel_id = %tunnel_id,
                                        tunnel_type = %tunnel_type,
                                        "Tunnel established"
                                    );

                                    let _ = self
                                        .event_tx
                                        .send(TunnelEvent::TunnelEstablished {
                                            subdomain,
                                            tunnel_id,
                                            is_static,
                                        })
                                        .await;

                                    tunnel_topic = msg.topic.clone();
                                    joined = true;
                                }
                            } else {
                                let reason = msg
                                    .payload
                                    .get("response")
                                    .and_then(|r| r.get("reason"))
                                    .and_then(|r| r.as_str())
                                    .unwrap_or("Unknown error");
                                let _ = self
                                    .event_tx
                                    .send(TunnelEvent::ConnectionError(reason.to_string()))
                                    .await;
                                return Err(anyhow!("Tunnel join failed: {}", reason));
                            }
                        }
                    }
                    Ok(Message::Ping(data)) => {
                        write.send(Message::Pong(data)).await?;
                    }
                    Ok(Message::Close(frame)) => {
                        return Err(anyhow!("WebSocket closed during join: {:?}", frame));
                    }
                    Err(e) => return Err(anyhow!("WebSocket error during join: {}", e)),
                    _ => {}
                },
                Ok(None) => return Err(anyhow!("WebSocket stream ended during join")),
                Err(_) => return Err(anyhow!("Timeout waiting for tunnel join response")),
            }
        }

        // Track last ping time
        let mut last_ping = tokio::time::Instant::now();
        let ping_interval = Duration::from_secs(30);
        let mut ping_counter = 2;

        // Listen for tunnel_request events
        loop {
            // Check if we need to send a ping
            if last_ping.elapsed() >= ping_interval {
                let ping_msg = ChannelMessage {
                    topic: tunnel_topic.clone(),
                    event: "ping".to_string(),
                    payload: serde_json::json!({}),
                    reference: Some(ping_counter.to_string()),
                };
                ping_counter += 1;

                if let Ok(json) = serde_json::to_string(&ping_msg)
                    && let Err(e) = write.send(Message::Text(json.into())).await
                {
                    error!("Failed to send ping: {}", e);
                    break;
                }
                last_ping = tokio::time::Instant::now();
            }

            // Use timeout to allow ping checks
            match tokio::time::timeout(Duration::from_millis(100), read.next()).await {
                Ok(Some(msg)) => match msg {
                    Ok(Message::Text(text)) => {
                        if let Err(e) = self
                            .handle_tunnel_message(&text, &mut write, &tunnel_topic)
                            .await
                        {
                            error!("Error handling tunnel message: {}", e);
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        info!("Tunnel WebSocket closed: {:?}", frame);
                        let _ = self.event_tx.send(TunnelEvent::Disconnected).await;
                        break;
                    }
                    Ok(Message::Ping(data)) => {
                        debug!("Received ping, sending pong");
                        if let Err(e) = write.send(Message::Pong(data)).await {
                            error!("Failed to send pong: {}", e);
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        error!("Tunnel WebSocket error: {}", e);
                        let _ = self
                            .event_tx
                            .send(TunnelEvent::ConnectionError(format!(
                                "WebSocket error: {}",
                                e
                            )))
                            .await;
                        break;
                    }
                },
                Ok(None) => {
                    warn!("Tunnel WebSocket stream ended");
                    let _ = self.event_tx.send(TunnelEvent::Disconnected).await;
                    break;
                }
                Err(_) => {
                    // Timeout - continue to check ping
                    continue;
                }
            }
        }

        Ok(())
    }

    async fn handle_tunnel_message(
        &self,
        text: &str,
        write: &mut futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
        tunnel_topic: &str,
    ) -> Result<()> {
        let msg: ChannelMessage = serde_json::from_str(text)?;

        debug!(
            topic = %msg.topic,
            event = %msg.event,
            "Received tunnel message"
        );

        match msg.event.as_str() {
            "tunnel_request" => {
                // Extract request details
                if let Some(payload) = msg.payload.as_object() {
                    let request_id = payload
                        .get("request_id")
                        .and_then(|v| v.as_str())
                        .unwrap_or("unknown")
                        .to_string();
                    let method = payload
                        .get("method")
                        .and_then(|v| v.as_str())
                        .unwrap_or("GET")
                        .to_string();
                    let path = payload
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or("/")
                        .to_string();
                    let query_string = payload
                        .get("query_string")
                        .and_then(|v| v.as_str())
                        .unwrap_or("")
                        .to_string();
                    let headers = payload
                        .get("headers")
                        .and_then(|v| v.as_object())
                        .cloned()
                        .unwrap_or_default();

                    // Decode body based on body_encoding field
                    let body_encoding = payload
                        .get("body_encoding")
                        .and_then(|v| v.as_str())
                        .unwrap_or("raw");
                    let raw_body = payload.get("body").and_then(|v| v.as_str()).unwrap_or("");
                    let body: Vec<u8> = if body_encoding == "base64" {
                        // Decode base64 body
                        URL_SAFE_NO_PAD.decode(raw_body).unwrap_or_else(|e| {
                            warn!("Failed to decode base64 body: {}", e);
                            raw_body.as_bytes().to_vec()
                        })
                    } else {
                        // Use body as-is (raw UTF-8)
                        raw_body.as_bytes().to_vec()
                    };

                    // Notify UI about request
                    let _ = self
                        .event_tx
                        .send(TunnelEvent::RequestReceived {
                            request_id: request_id.clone(),
                            method: method.clone(),
                            path: path.clone(),
                        })
                        .await;

                    // Forward the request
                    self.forward_tunnel_request(
                        request_id,
                        method,
                        path,
                        query_string,
                        headers,
                        body,
                        write,
                        tunnel_topic,
                    )
                    .await?;
                }
            }
            "phx_reply" => {
                // Handle ping replies
                if let Some(response) = msg.payload.get("response")
                    && response
                        .get("pong")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                {
                    debug!("Received pong from server");
                }
            }
            _ => {
                debug!("Unhandled tunnel event: {}", msg.event);
            }
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn forward_tunnel_request(
        &self,
        request_id: String,
        method: String,
        path: String,
        query_string: String,
        headers: serde_json::Map<String, serde_json::Value>,
        body: Vec<u8>,
        write: &mut futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
        tunnel_topic: &str,
    ) -> Result<()> {
        let start_time = tokio::time::Instant::now();

        info!(
            request_id = %request_id,
            method = %method,
            path = %path,
            "Forwarding tunnel request to local server"
        );

        // Build target URL
        let mut target = format!("http://{}:{}{}", self.local_host, self.local_port, path);
        if !query_string.is_empty() {
            target.push('?');
            target.push_str(&query_string);
        }

        // Create HTTP client with timeout
        let client = reqwest::Client::builder()
            .timeout(Duration::from_secs(30))
            .build()?;

        // Build request
        let mut req_builder = match method.as_str() {
            "GET" => client.get(&target),
            "POST" => client.post(&target),
            "PUT" => client.put(&target),
            "DELETE" => client.delete(&target),
            "PATCH" => client.patch(&target),
            "HEAD" => client.head(&target),
            "OPTIONS" => client.request(reqwest::Method::OPTIONS, &target),
            _ => {
                warn!("Unsupported HTTP method: {}", method);
                let _ = self
                    .send_tunnel_error(
                        &request_id,
                        &format!("Unsupported method: {}", method),
                        write,
                        tunnel_topic,
                    )
                    .await;
                return Ok(());
            }
        };

        // Add headers
        for (key, value) in headers {
            if key.to_lowercase() != "host" {
                let value_str = match value {
                    serde_json::Value::String(s) => s,
                    _ => value.to_string(),
                };
                if let Ok(header_name) = reqwest::header::HeaderName::from_bytes(key.as_bytes())
                    && let Ok(header_value) = reqwest::header::HeaderValue::from_str(&value_str)
                {
                    req_builder = req_builder.header(header_name, header_value);
                }
            }
        }

        // Add body if present
        if !body.is_empty() {
            req_builder = req_builder.body(body);
        }

        // Send request and handle response
        match req_builder.send().await {
            Ok(response) => {
                let status = response.status().as_u16();
                let response_headers: HashMap<String, String> = response
                    .headers()
                    .iter()
                    .map(|(k, v)| (k.as_str().to_string(), v.to_str().unwrap_or("").to_string()))
                    .collect();

                // Get response as bytes to handle binary content properly
                let response_bytes = response.bytes().await.unwrap_or_default();

                // Check if the response is valid UTF-8 (text) or binary
                let (response_body, body_encoding) =
                    if let Ok(text) = std::str::from_utf8(&response_bytes) {
                        // Valid UTF-8, send as raw
                        (text.to_string(), "raw")
                    } else {
                        // Binary content, encode as base64
                        (URL_SAFE_NO_PAD.encode(&response_bytes), "base64")
                    };

                let duration_ms = start_time.elapsed().as_millis() as u64;

                info!(
                    request_id = %request_id,
                    status = %status,
                    duration_ms = %duration_ms,
                    body_encoding = %body_encoding,
                    "Request forwarded successfully"
                );

                // Notify UI
                let _ = self
                    .event_tx
                    .send(TunnelEvent::RequestForwarded {
                        request_id: request_id.clone(),
                        status,
                        duration_ms,
                    })
                    .await;

                // Send tunnel_response back to server with body_encoding
                let response_message = ChannelMessage {
                    topic: tunnel_topic.to_string(),
                    event: "tunnel_response".to_string(),
                    payload: serde_json::json!({
                        "request_id": request_id,
                        "status": status,
                        "headers": response_headers,
                        "body": response_body,
                        "body_encoding": body_encoding,
                    }),
                    reference: None,
                };

                let response_json = serde_json::to_string(&response_message)?;
                write.send(Message::Text(response_json.into())).await?;
            }
            Err(e) => {
                let duration_ms = start_time.elapsed().as_millis() as u64;
                let error_msg = format!("Failed to forward request: {}", e);

                error!(
                    request_id = %request_id,
                    error = %error_msg,
                    duration_ms = %duration_ms,
                    "Request forwarding failed"
                );

                // Notify UI
                let _ = self
                    .event_tx
                    .send(TunnelEvent::RequestFailed {
                        request_id: request_id.clone(),
                        error: error_msg.clone(),
                    })
                    .await;

                // Send tunnel_error back to server
                self.send_tunnel_error(&request_id, &error_msg, write, tunnel_topic)
                    .await?;
            }
        }

        Ok(())
    }

    async fn send_tunnel_error(
        &self,
        request_id: &str,
        error: &str,
        write: &mut futures_util::stream::SplitSink<
            tokio_tungstenite::WebSocketStream<
                tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
            >,
            Message,
        >,
        tunnel_topic: &str,
    ) -> Result<()> {
        let error_message = ChannelMessage {
            topic: tunnel_topic.to_string(),
            event: "tunnel_error".to_string(),
            payload: serde_json::json!({
                "request_id": request_id,
                "error": error,
            }),
            reference: None,
        };

        let error_json = serde_json::to_string(&error_message)?;
        write.send(Message::Text(error_json.into())).await?;
        Ok(())
    }

    /// Connect with automatic reconnection on recoverable errors
    pub async fn connect_with_reconnect(&self, config: ReconnectConfig) -> Result<()> {
        let mut attempt: u32 = 0;

        loop {
            let start = tokio::time::Instant::now();
            let result = self.connect_and_forward().await;

            match result {
                Ok(()) => {
                    if start.elapsed() > Duration::from_secs(5) {
                        attempt = 0;
                    }
                }
                Err(ref e) => {
                    let err_msg = e.to_string();
                    if is_fatal_error(&err_msg) {
                        let _ = self
                            .event_tx
                            .send(TunnelEvent::ReconnectFailed { reason: err_msg })
                            .await;
                        return result;
                    }

                    if start.elapsed() > Duration::from_secs(5) {
                        attempt = 0;
                    }
                }
            }

            attempt += 1;
            if attempt > config.max_retries {
                let reason = "Maximum reconnection attempts exceeded".to_string();
                let _ = self
                    .event_tx
                    .send(TunnelEvent::ReconnectFailed {
                        reason: reason.clone(),
                    })
                    .await;
                return Err(anyhow!(reason));
            }

            let backoff = calculate_backoff(attempt, &config);
            let next_retry_secs = backoff.as_secs();

            info!(
                attempt = attempt,
                max_attempts = config.max_retries,
                next_retry_in_secs = next_retry_secs,
                "Reconnecting tunnel..."
            );

            let _ = self
                .event_tx
                .send(TunnelEvent::Reconnecting {
                    attempt,
                    max_attempts: config.max_retries,
                    next_retry_in_secs: next_retry_secs,
                })
                .await;

            tokio::time::sleep(backoff).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ChannelMessage serialization tests
    #[test]
    fn test_channel_message_serialization_with_ref() {
        let msg = ChannelMessage {
            topic: "test:topic".to_string(),
            event: "phx_join".to_string(),
            payload: serde_json::json!({"key": "value"}),
            reference: Some("1".to_string()),
        };
        let json = serde_json::to_string(&msg).unwrap();
        assert!(json.contains("\"ref\":\"1\""));
        assert!(json.contains("\"topic\":\"test:topic\""));
    }

    #[test]
    fn test_channel_message_deserialization_with_ref() {
        let json = r#"{"topic":"t","event":"e","payload":{},"ref":"42"}"#;
        let msg: ChannelMessage = serde_json::from_str(json).unwrap();
        assert_eq!(msg.topic, "t");
        assert_eq!(msg.event, "e");
        assert_eq!(msg.reference, Some("42".to_string()));
    }

    #[test]
    fn test_channel_message_deserialization_without_ref() {
        let json = r#"{"topic":"t","event":"e","payload":{}}"#;
        let msg: ChannelMessage = serde_json::from_str(json).unwrap();
        assert!(msg.reference.is_none());
    }

    // TunnelWebhookRequest tests
    #[test]
    fn test_tunnel_webhook_request_full_payload() {
        let json = r#"{
            "id": "req-1",
            "method": "POST",
            "path": "/webhook",
            "query_params": {"foo": "bar"},
            "headers": {"content-type": "application/json"},
            "body": "{\"data\":1}"
        }"#;
        let req: TunnelWebhookRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.id, "req-1");
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/webhook");
        assert!(req.body.is_some());
    }

    #[test]
    fn test_tunnel_webhook_request_minimal() {
        let json = r#"{"id":"req-2","method":"GET","path":"/"}"#;
        let req: TunnelWebhookRequest = serde_json::from_str(json).unwrap();
        assert_eq!(req.id, "req-2");
        assert!(req.body.is_none());
        assert!(req.headers.is_empty());
        assert!(req.query_params.is_empty());
    }

    // build_ws_url tests
    #[test]
    fn test_build_ws_url_https_to_wss() {
        let url = build_ws_url("https://api.example.com", "tok123", "socket/websocket");
        assert_eq!(url, "wss://api.example.com/socket/websocket?token=tok123");
    }

    #[test]
    fn test_build_ws_url_http_to_ws() {
        let url = build_ws_url("http://localhost:4000", "tok", "tunnel/websocket");
        assert_eq!(url, "ws://localhost:4000/tunnel/websocket?token=tok");
    }

    // build_forward_target tests
    #[test]
    fn test_build_forward_target_no_query_params() {
        let target = build_forward_target("http://localhost:3000", "/api/hook", &HashMap::new());
        assert_eq!(target, "http://localhost:3000/api/hook");
    }

    #[test]
    fn test_build_forward_target_with_query_params() {
        let mut params = HashMap::new();
        params.insert(
            "key".to_string(),
            serde_json::Value::String("val".to_string()),
        );
        let target = build_forward_target("http://localhost:3000", "/hook", &params);
        assert!(target.starts_with("http://localhost:3000/hook?"));
        assert!(target.contains("key=val"));
    }

    // is_fatal_error tests
    #[test]
    fn test_is_fatal_error_auth() {
        assert!(is_fatal_error(
            "Authentication failed: The token is invalid or expired."
        ));
    }

    #[test]
    fn test_is_fatal_error_not_found() {
        assert!(is_fatal_error("Endpoint not found: 'my-slug'."));
    }

    #[test]
    fn test_is_fatal_error_join_failed() {
        assert!(is_fatal_error("Channel join failed: unknown"));
    }

    #[test]
    fn test_is_not_fatal_error_connection_refused() {
        assert!(!is_fatal_error("Connection refused: connection reset"));
    }

    #[test]
    fn test_is_not_fatal_error_ws_error() {
        assert!(!is_fatal_error("WebSocket error: broken pipe"));
    }

    #[test]
    fn test_is_not_fatal_error_stream_ended() {
        assert!(!is_fatal_error("WebSocket stream ended"));
    }

    // calculate_backoff tests
    #[test]
    fn test_calculate_backoff_first_attempt() {
        let config = ReconnectConfig {
            max_retries: 10,
            initial_delay_ms: 1000,
            max_delay_ms: 60000,
            jitter_factor: 0.0, // No jitter for deterministic test
        };
        let backoff = calculate_backoff(1, &config);
        assert_eq!(backoff.as_millis(), 1000);
    }

    #[test]
    fn test_calculate_backoff_capped_at_max() {
        let config = ReconnectConfig {
            max_retries: 20,
            initial_delay_ms: 1000,
            max_delay_ms: 5000,
            jitter_factor: 0.0,
        };
        let backoff = calculate_backoff(15, &config);
        assert_eq!(backoff.as_millis(), 5000);
    }

    // ReconnectConfig default tests
    #[test]
    fn test_reconnect_config_default_values() {
        let config = ReconnectConfig::default();
        assert_eq!(config.max_retries, 10);
        assert_eq!(config.initial_delay_ms, 1000);
        assert_eq!(config.max_delay_ms, 60000);
        assert!((config.jitter_factor - 0.3).abs() < f64::EPSILON);
    }

    // Base64 decode roundtrip
    #[test]
    fn test_base64_body_decode_roundtrip() {
        let original = b"Hello, World!";
        let encoded = URL_SAFE_NO_PAD.encode(original);
        let decoded = URL_SAFE_NO_PAD.decode(&encoded).unwrap();
        assert_eq!(decoded, original);
    }
}
