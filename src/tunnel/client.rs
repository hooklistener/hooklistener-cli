//! `listen` client: receives webhooks for an endpoint and forwards them to a local target.

use anyhow::{Context, Result, anyhow};
use futures_util::{SinkExt, StreamExt};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::{
    connect_async,
    tungstenite::{Message, error::Error as WsError, http::StatusCode},
};
use tracing::{debug, error, info, warn};

use crate::api;
use crate::target_policy::TargetPolicy;
use crate::tunnel::framing::{
    CAPTURE_FORWARD_MODE, ChannelMessage, listen_join_payload, validate_join_mode, with_forward_id,
};
use crate::tunnel::http::{
    encode_response_body, json_value_to_string, read_response_body_limited,
    response_headers_to_map, should_forward_request_header,
};
use crate::tunnel::limits::LEGACY_MAX_RESPONSE_BODY_BYTES;
use crate::tunnel::preview::bounded_control_neutral_log_text;
use crate::tunnel::{
    ReconnectConfig, TunnelEvent, TunnelWebhookRequest, WsWrite, build_ws_url, calculate_backoff,
    is_fatal_error, redact_access_token, websocket_endpoint,
};

/// Tunnel client for WebSocket connection to Hooklistener server
pub struct TunnelClient {
    access_token_rx: watch::Receiver<String>,
    endpoint_slug: String,
    target: TargetPolicy,
    base_url: String,
    event_tx: mpsc::Sender<TunnelEvent>,
}

impl TunnelClient {
    pub fn new(
        access_token_rx: watch::Receiver<String>,
        endpoint_slug: String,
        target: TargetPolicy,
        base_url: Option<String>,
        event_tx: mpsc::Sender<TunnelEvent>,
    ) -> Self {
        // Check environment variable for local development
        let base_url = base_url
            .or_else(|| std::env::var("HOOKLISTENER_WS_URL").ok())
            .unwrap_or_else(|| "wss://api.hooklistener.com".to_string());

        Self {
            access_token_rx,
            endpoint_slug,
            target,
            base_url,
            event_tx,
        }
    }

    /// Connect to WebSocket and start listening for webhook events
    pub async fn connect_and_listen(&self) -> Result<()> {
        api::validate_websocket_base_url(&self.base_url)?;
        info!(
            endpoint = %self.endpoint_slug,
            target = %self.target.display_url(),
            "Connecting to WebSocket tunnel"
        );

        // Exchange the long-lived HTTP credential for a one-time, scoped handshake ticket.
        let access_token = self.access_token_rx.borrow().clone();
        let plan = serde_json::json!({
            "mode": CAPTURE_FORWARD_MODE,
            "route": {"endpoint_slug": &self.endpoint_slug},
            "target": self.target.plan(),
        });
        let relay_ticket = api::issue_relay_ticket(&access_token, &self.base_url, &plan).await?;
        if relay_ticket.scope != "relay:listen" {
            return Err(anyhow!(
                "Relay handshake rejected: ticket returned an incompatible scope"
            ));
        }
        if relay_ticket.plan_fingerprint.is_empty() || relay_ticket.expires_at.is_empty() {
            return Err(anyhow!(
                "Relay handshake rejected: ticket receipt was incomplete"
            ));
        }

        let ws_url = build_ws_url(&self.base_url, &relay_ticket.ticket, "cli-tunnel/websocket");

        debug!(
            endpoint = %websocket_endpoint(&self.base_url, "cli-tunnel/websocket"),
            "Connecting to WebSocket"
        );

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
                    let detail = redact_access_token(&e.to_string(), &relay_ticket.ticket);
                    let detail = redact_access_token(&detail, &access_token);
                    let msg = format!("Failed to connect to WebSocket: {detail}");
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
            payload: listen_join_payload(),
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
                                let Some(response) = msg.payload.get("response") else {
                                    let error = anyhow!("Channel join response was missing");
                                    let _ = self
                                        .event_tx
                                        .send(TunnelEvent::ConnectionError(error.to_string()))
                                        .await;
                                    return Err(error);
                                };

                                if let Err(error) =
                                    validate_join_mode(response, CAPTURE_FORWARD_MODE)
                                {
                                    let reason = error.to_string();
                                    let _ = self
                                        .event_tx
                                        .send(TunnelEvent::ConnectionError(reason.clone()))
                                        .await;
                                    return Err(error);
                                }

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
                                let reason = bounded_control_neutral_log_text(reason);
                                let _ = self
                                    .event_tx
                                    .send(TunnelEvent::ConnectionError(reason.clone()))
                                    .await;
                                return Err(anyhow!("Channel join failed: {}", reason));
                            }
                        }
                    }
                    Ok(Message::Ping(data)) => {
                        write.send(Message::Pong(data)).await?;
                    }
                    Ok(Message::Close(frame)) => {
                        let frame = bounded_control_neutral_log_text(&format!("{frame:?}"));
                        return Err(anyhow!("WebSocket closed during join: {frame}"));
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
                            let error = bounded_control_neutral_log_text(&e.to_string());
                            error!(error = %error, "Error handling message");
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        let frame = bounded_control_neutral_log_text(&format!("{frame:?}"));
                        info!(frame = %frame, "WebSocket closed");
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

    async fn handle_message(&self, text: &str, write: &mut WsWrite) -> Result<()> {
        let msg: ChannelMessage = serde_json::from_str(text)?;
        let log_topic = bounded_control_neutral_log_text(&msg.topic);
        let log_event = bounded_control_neutral_log_text(&msg.event);

        debug!(
            topic = %log_topic,
            event = %log_event,
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
                                    .map(|(k, v)| (k.clone(), json_value_to_string(v)))
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
                                    .map(|(k, v)| (k.clone(), json_value_to_string(v)))
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
                            let err_msg = format!("Invalid webhook payload: {e}");
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
                debug!(event = %log_event, "Unhandled event");
            }
        }

        Ok(())
    }

    async fn forward_webhook(
        &self,
        request: TunnelWebhookRequest,
        write: &mut WsWrite,
    ) -> Result<()> {
        info!(
            request_id = %request.id,
            method = %request.method,
            path = %request.path,
            "Forwarding webhook to local server"
        );

        let mut target_with_query = self.target.request_url(&request.path, None)?;
        if !request.query_params.is_empty() {
            let mut query = target_with_query.query_pairs_mut();
            for (key, value) in &request.query_params {
                query.append_pair(key, &json_value_to_string(value));
            }
        }

        // The client is pinned to the addresses resolved during activation and ignores proxies.
        let client = self.target.http_client()?;

        // Build request with method
        let mut req_builder = match request.method.as_str() {
            "GET" => client.get(target_with_query.clone()),
            "POST" => client.post(target_with_query.clone()),
            "PUT" => client.put(target_with_query.clone()),
            "DELETE" => client.delete(target_with_query.clone()),
            "PATCH" => client.patch(target_with_query.clone()),
            "HEAD" => client.head(target_with_query.clone()),
            _ => {
                warn!("Unsupported HTTP method: {}", request.method);
                return Ok(());
            }
        };

        // Add headers. reqwest sets request framing headers from the body we actually send.
        for (key, value) in &request.headers {
            if should_forward_request_header(key) {
                req_builder = req_builder.header(key, json_value_to_string(value));
            }
        }

        // Add body if present
        if let Some(body) = &request.body {
            req_builder = req_builder.body(body.clone());
        }

        // Send request
        let start_time = std::time::Instant::now();
        match req_builder.send().await {
            Ok(mut response) => {
                let status = response.status();
                let status_code = status.as_u16();
                let response_headers = response_headers_to_map(response.headers());
                let response_bytes =
                    match read_response_body_limited(&mut response, LEGACY_MAX_RESPONSE_BODY_BYTES)
                        .await
                    {
                        Ok(response_bytes) => response_bytes,
                        Err(error) => {
                            let duration_ms = start_time.elapsed().as_millis() as u64;
                            let error_message = error.to_string();

                            error!(
                                request_id = %request.id,
                                error = %error_message,
                                "Failed to read local response"
                            );

                            let _ = self
                                .event_tx
                                .send(TunnelEvent::ForwardError {
                                    request_id: request.id.clone(),
                                    target_url: target_with_query.to_string(),
                                    error: error_message.clone(),
                                    duration_ms,
                                })
                                .await;

                            let payload = with_forward_id(
                                serde_json::json!({
                                    "request_id": &request.id,
                                    "status": "error",
                                    "proxied_to": target_with_query.as_str(),
                                    "error": error_message,
                                    "duration_ms": duration_ms,
                                }),
                                request.forward_id.as_deref(),
                            );

                            self.send_request_ack(write, payload).await?;
                            return Ok(());
                        }
                    };
                let (response_body, response_body_encoding) = encode_response_body(&response_bytes);
                let duration_ms = start_time.elapsed().as_millis() as u64;

                info!(
                    request_id = %request.id,
                    status = %status,
                    "Request forwarded successfully"
                );

                let _ = self
                    .event_tx
                    .send(TunnelEvent::ForwardSuccess {
                        request_id: request.id.clone(),
                        target_url: target_with_query.to_string(),
                        status: status_code,
                        duration_ms,
                    })
                    .await;

                // Send acknowledgment back to server
                let payload = with_forward_id(
                    serde_json::json!({
                        "request_id": &request.id,
                        "status": "proxied",
                        "proxied_to": target_with_query.as_str(),
                        "status_code": status_code,
                        "response_headers": response_headers,
                        "response_body": response_body,
                        "response_body_encoding": response_body_encoding,
                        "duration_ms": duration_ms,
                    }),
                    request.forward_id.as_deref(),
                );

                self.send_request_ack(write, payload).await?;
            }
            Err(e) => {
                let duration_ms = start_time.elapsed().as_millis() as u64;

                let error_message = e.without_url().to_string();

                error!(
                    request_id = %request.id,
                    error = %error_message,
                    "Failed to forward request"
                );

                let _ = self
                    .event_tx
                    .send(TunnelEvent::ForwardError {
                        request_id: request.id.clone(),
                        target_url: target_with_query.to_string(),
                        error: error_message.clone(),
                        duration_ms,
                    })
                    .await;

                // Send error acknowledgment
                let payload = with_forward_id(
                    serde_json::json!({
                        "request_id": &request.id,
                        "status": "error",
                        "proxied_to": target_with_query.as_str(),
                        "error": error_message,
                        "duration_ms": duration_ms,
                    }),
                    request.forward_id.as_deref(),
                );

                self.send_request_ack(write, payload).await?;
            }
        }

        Ok(())
    }

    async fn send_request_ack(
        &self,
        write: &mut WsWrite,
        payload: serde_json::Value,
    ) -> Result<()> {
        let ack_message = ChannelMessage {
            topic: format!("cli:tunnel:{}", self.endpoint_slug),
            event: "request_ack".to_string(),
            payload,
            reference: None,
        };

        let ack_json = serde_json::to_string(&ack_message)?;
        write.send(Message::Text(ack_json.into())).await?;
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
                    let log_error = bounded_control_neutral_log_text(&err_msg);
                    warn!(error = %log_error, "Tunnel connection attempt failed");
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
