//! `tunnel` forwarder: connection setup, reconnect loop, and presentation events.

mod v2;
mod v3;

use anyhow::{Context, Result, anyhow};
use futures_util::{SinkExt, StreamExt};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::{connect_async_with_config, tungstenite::Message};
use tracing::{debug, error, info, warn};

use crate::api;
use crate::api::ApiClient;
use crate::target_policy::TargetPolicy;
use crate::tunnel::framing::{
    ChannelMessage, DIRECT_RESPONSE_MODE, decode_channel_message, selected_tunnel_protocol,
    tunnel_join_payload_for_version, validate_framing_contract, validate_join_mode,
};
use crate::tunnel::limits::{
    LEGACY_MAX_REQUEST_BODY_BYTES, LEGACY_MAX_RESPONSE_BODY_BYTES,
    LEGACY_MAX_RESPONSE_HEADER_BYTES, TUNNEL_MAX_FRAME_BYTES, TUNNEL_MAX_RESPONSE_HEADERS,
    TunnelLimits, tunnel_websocket_config,
};
use crate::tunnel::preview::bounded_control_neutral_log_text;
use crate::tunnel::relay::{RelayRuntime, complete_tunnel_worker, reap_ready_tunnel_workers};
use crate::tunnel::writer::PriorityWriter;
use crate::tunnel::{
    ReconnectConfig, TunnelEvent, build_ws_url, calculate_backoff, is_fatal_error,
    redact_access_token, websocket_endpoint,
};
use crate::tunnel_v3;

/// HTTP Tunnel forwarder - connects to /tunnel endpoint and forwards HTTP requests
#[derive(Clone)]
pub struct TunnelForwarder {
    access_token_rx: watch::Receiver<String>,
    local_host: String,
    local_port: u16,
    target: TargetPolicy,
    org_id: Option<String>,
    slug: Option<String>,
    pub(crate) base_url: String,
    event_tx: mpsc::Sender<TunnelEvent>,
    replay_buffered: bool,
    pub(crate) presentation_drops: Arc<AtomicUsize>,
    pub(crate) resume_session_id: Arc<Mutex<Option<String>>>,
    anonymous_route: Option<AnonymousRouteCredential>,
}

#[derive(Clone)]
pub struct AnonymousRouteCredential {
    route_id: String,
    route_token: String,
    initial_ticket: Arc<Mutex<Option<api::RelayTicket>>>,
}

impl TunnelForwarder {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        access_token_rx: watch::Receiver<String>,
        local_host: String,
        local_port: u16,
        target: TargetPolicy,
        org_id: Option<String>,
        slug: Option<String>,
        event_tx: mpsc::Sender<TunnelEvent>,
        replay_buffered: bool,
    ) -> Self {
        let base_url = std::env::var("HOOKLISTENER_API_URL")
            .unwrap_or_else(|_| "https://app.hooklistener.com".to_string());

        Self {
            access_token_rx,
            local_host,
            local_port,
            target,
            org_id,
            slug,
            base_url,
            event_tx,
            replay_buffered,
            presentation_drops: Arc::new(AtomicUsize::new(0)),
            resume_session_id: Arc::new(Mutex::new(None)),
            anonymous_route: None,
        }
    }

    pub fn with_anonymous_route(
        mut self,
        route_id: String,
        route_token: String,
        initial_ticket: Option<api::RelayTicket>,
    ) -> Self {
        self.anonymous_route = Some(AnonymousRouteCredential {
            route_id,
            route_token,
            initial_ticket: Arc::new(Mutex::new(initial_ticket)),
        });
        self
    }

    pub(crate) fn emit_presentation(&self, event: TunnelEvent) {
        let dropped = self.presentation_drops.swap(0, Ordering::AcqRel);
        if dropped > 0
            && self
                .event_tx
                .try_send(TunnelEvent::StreamGap {
                    dropped_events: dropped,
                })
                .is_err()
        {
            self.presentation_drops.fetch_add(dropped, Ordering::AcqRel);
        }

        if self.event_tx.try_send(event).is_err() {
            self.presentation_drops.fetch_add(1, Ordering::AcqRel);
        }
    }

    pub(crate) async fn resume_token(&self, access_token: &str) -> Result<Option<String>> {
        if self.anonymous_route.is_some() {
            return Ok(None);
        }

        let session_id = self
            .resume_session_id
            .lock()
            .map_err(|_| anyhow!("Tunnel resume state is unavailable"))?
            .clone();
        let Some(session_id) = session_id else {
            return Ok(None);
        };
        let organization_id = self
            .org_id
            .clone()
            .ok_or_else(|| anyhow!("Tunnel resume requires an organization"))?;
        let client = ApiClient::with_base_url(
            access_token.to_string(),
            self.base_url.clone(),
            Some(organization_id),
        )?;
        let descriptor = client.reconnect_tunnel_session(&session_id).await?;
        Ok(Some(descriptor.resume_token))
    }

    pub async fn connect_and_forward(&self) -> Result<()> {
        api::validate_api_base_url(&self.base_url)?;
        info!(
            local_host = %self.local_host,
            local_port = %self.local_port,
            "Starting HTTP tunnel"
        );

        let _ = self.event_tx.send(TunnelEvent::Connecting).await;

        // Exchange the long-lived HTTP credential for a one-time, scoped handshake ticket.
        let access_token = self.access_token_rx.borrow().clone();
        let resume_token = self.resume_token(&access_token).await?;
        let relay_ticket = if let Some(anonymous_route) = &self.anonymous_route {
            let initial_ticket = anonymous_route
                .initial_ticket
                .lock()
                .map_err(|_| anyhow!("Anonymous tunnel credential state is unavailable"))?
                .take();

            match initial_ticket {
                Some(ticket) => ticket,
                None => {
                    ApiClient::unauthenticated_at(self.base_url.clone())?
                        .issue_anonymous_tunnel_ticket(
                            &anonymous_route.route_id,
                            &anonymous_route.route_token,
                        )
                        .await?
                }
            }
        } else {
            let plan = serde_json::json!({
                "mode": DIRECT_RESPONSE_MODE,
                "route": {"organization_id": &self.org_id, "slug": &self.slug},
                "target": self.target.plan(),
            });
            api::issue_relay_ticket(&access_token, &self.base_url, &plan).await?
        };
        if relay_ticket.scope != "relay:tunnel" {
            return Err(anyhow!(
                "Relay handshake rejected: ticket returned an incompatible scope"
            ));
        }
        if relay_ticket.plan_fingerprint.is_empty() || relay_ticket.expires_at.is_empty() {
            return Err(anyhow!(
                "Relay handshake rejected: ticket receipt was incomplete"
            ));
        }

        let protocol_version = selected_tunnel_protocol(&relay_ticket.supported_protocol_versions)?;
        let protocol_v3 = protocol_version == u64::from(tunnel_v3::VERSION);
        let mut ws_url = build_ws_url(&self.base_url, &relay_ticket.ticket, "tunnel/websocket");
        if protocol_v3 {
            ws_url.push_str("&vsn=2.0.0");
        }

        debug!(
            endpoint = %websocket_endpoint(&self.base_url, "tunnel/websocket"),
            "Connecting tunnel WebSocket"
        );

        // Connect to WebSocket
        let (ws_stream, _) = match connect_async_with_config(
            &ws_url,
            Some(tunnel_websocket_config()),
            false,
        )
        .await
        {
            Ok(stream) => stream,
            Err(e) => {
                let detail = redact_access_token(&e.to_string(), &relay_ticket.ticket);
                let detail = redact_access_token(&detail, &access_token);
                let msg = format!("Failed to connect to tunnel: {detail}");
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
        let join_payload = tunnel_join_payload_for_version(
            self.local_port,
            self.org_id.as_deref(),
            self.slug.as_deref(),
            resume_token.as_deref(),
            protocol_version,
        );

        if resume_token.is_some() {
            info!("Resuming canonical tunnel session");
        } else if let Some(slug) = &self.slug {
            info!(slug = %slug, "Requesting static tunnel");
        }

        let join_json = if protocol_v3 {
            tunnel_v3::encode_client_control(
                &tunnel_v3::ControlMessage {
                    join_ref: None,
                    reference: Some("1".to_string()),
                    topic: "tunnel:connect".to_string(),
                    event: "phx_join".to_string(),
                    payload: join_payload,
                },
                TUNNEL_MAX_FRAME_BYTES,
            )?
        } else {
            serde_json::to_string(&ChannelMessage {
                topic: "tunnel:connect".to_string(),
                event: "phx_join".to_string(),
                payload: join_payload,
                reference: Some("1".to_string()),
            })?
        };
        write
            .send(Message::Text(join_json.into()))
            .await
            .context("Failed to send join message")?;

        // Wait for join confirmation
        let mut joined = false;
        let mut tunnel_topic = String::new();
        let mut tunnel_limits = TunnelLimits {
            max_request_body_bytes: LEGACY_MAX_REQUEST_BODY_BYTES,
            max_response_body_bytes: LEGACY_MAX_RESPONSE_BODY_BYTES,
            max_response_header_bytes: LEGACY_MAX_RESPONSE_HEADER_BYTES,
            max_response_header_items: TUNNEL_MAX_RESPONSE_HEADERS,
            ordered_response_headers: false,
        };
        let mut streaming_limits = None;

        while !joined {
            match tokio::time::timeout(Duration::from_secs(10), read.next()).await {
                Ok(Some(msg_result)) => match msg_result {
                    Ok(Message::Text(text)) => {
                        let msg = decode_channel_message(&text, protocol_v3)?;
                        if msg.event == "phx_reply"
                            && msg.reference.as_deref() == Some("1")
                            && let Some(status) = msg.payload.get("status")
                        {
                            if status == "ok" {
                                // Extract the negotiated contract and canonical session identity.
                                let Some(response) = msg.payload.get("response") else {
                                    let error = anyhow!("Tunnel join response was missing");
                                    let _ = self
                                        .event_tx
                                        .send(TunnelEvent::ConnectionError(error.to_string()))
                                        .await;
                                    return Err(error);
                                };

                                if let Err(error) =
                                    validate_join_mode(response, DIRECT_RESPONSE_MODE)
                                {
                                    let reason = error.to_string();
                                    let _ = self
                                        .event_tx
                                        .send(TunnelEvent::ConnectionError(reason.clone()))
                                        .await;
                                    return Err(error);
                                }

                                if protocol_v3 {
                                    streaming_limits = Some(
                                        tunnel_v3::StreamingLimits::from_join_response(response)?,
                                    );
                                } else {
                                    if let Err(error) = validate_framing_contract(response) {
                                        let reason = error.to_string();
                                        let _ = self
                                            .event_tx
                                            .send(TunnelEvent::ConnectionError(reason.clone()))
                                            .await;
                                        return Err(error);
                                    }

                                    tunnel_limits = TunnelLimits::from_join_response(response);
                                    if !tunnel_limits.ordered_response_headers {
                                        let error = anyhow!(
                                            "Server did not advertise ordered response headers"
                                        );
                                        let _ = self
                                            .event_tx
                                            .send(TunnelEvent::ConnectionError(error.to_string()))
                                            .await;
                                        return Err(error);
                                    }
                                }

                                let session_id = response
                                    .get("session_id")
                                    .and_then(|id| id.as_str())
                                    .ok_or_else(|| {
                                        anyhow!(
                                            "Tunnel service did not return a canonical session id"
                                        )
                                    })?
                                    .to_string();
                                *self
                                    .resume_session_id
                                    .lock()
                                    .map_err(|_| anyhow!("Tunnel resume state is unavailable"))? =
                                    Some(session_id);

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

                                let tunnel_type = if is_static { "static" } else { "ephemeral" };
                                let log_subdomain = bounded_control_neutral_log_text(&subdomain);
                                let log_tunnel_id = bounded_control_neutral_log_text(&tunnel_id);
                                info!(
                                    subdomain = %log_subdomain,
                                    tunnel_id = %log_tunnel_id,
                                    tunnel_type = %tunnel_type,
                                    protocol_version = protocol_version,
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
                                return Err(anyhow!("Tunnel join failed: {}", reason));
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
                Err(_) => return Err(anyhow!("Timeout waiting for tunnel join response")),
            }
        }

        let (writer, mut writer_task) = PriorityWriter::spawn(write);
        if let Some(streaming_limits) = streaming_limits {
            return self
                .run_v3_tunnel(read, writer, writer_task, tunnel_topic, streaming_limits)
                .await;
        }

        let mut ping_interval = tokio::time::interval(Duration::from_secs(30));
        ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ping_interval.tick().await;
        let mut ping_counter = 2;
        let mut runtime = RelayRuntime::new();

        // Listen for tunnel_request events
        loop {
            // Completed tasks have already returned their local-forward
            // permits. Remove their JoinSet results and delivery indexes
            // before admitting more messages from a continuously readable
            // socket.
            reap_ready_tunnel_workers(&mut runtime);

            tokio::select! {
                biased;
                writer_result = &mut writer_task => {
                    match writer_result {
                        Ok(Ok(())) => return Err(anyhow!("Tunnel writer stopped")),
                        Ok(Err(error)) => return Err(error.context("Tunnel writer failed")),
                        Err(error) => return Err(anyhow!("Tunnel writer task failed: {error}")),
                    }
                }
                _ = ping_interval.tick() => {
                    writer.control(ChannelMessage {
                        topic: tunnel_topic.clone(),
                        event: "ping".to_string(),
                        payload: serde_json::json!({}),
                        reference: Some(ping_counter.to_string()),
                    }).await?;
                    ping_counter += 1;
                },
                completed = runtime.workers.join_next_with_id(), if !runtime.workers.is_empty() => {
                    if let Some(completion) = completed {
                        complete_tunnel_worker(completion, &mut runtime);
                    }
                },
                maybe_msg = read.next() => match maybe_msg {
                    Some(msg) => match msg {
                    Ok(Message::Text(text)) => {
                        if let Err(e) = self
                            .handle_tunnel_message(
                                &text,
                                &writer,
                                &tunnel_topic,
                                tunnel_limits,
                                &mut runtime,
                            )
                            .await
                        {
                            let error = bounded_control_neutral_log_text(&e.to_string());
                            error!(error = %error, "Error handling tunnel message");
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        let frame = bounded_control_neutral_log_text(&format!("{frame:?}"));
                        info!(frame = %frame, "Tunnel WebSocket closed");
                        self.emit_presentation(TunnelEvent::Disconnected);
                        break;
                    }
                    Ok(Message::Ping(data)) => {
                        if let Err(e) = writer.pong(data).await {
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
                    None => {
                    warn!("Tunnel WebSocket stream ended");
                    self.emit_presentation(TunnelEvent::Disconnected);
                    break;
                    }
                },
            }
        }

        runtime.workers.abort_all();
        while runtime.workers.join_next().await.is_some() {}
        writer_task.abort();
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
                    let log_error = bounded_control_neutral_log_text(&err_msg);
                    warn!(error = %log_error, "Tunnel connection attempt failed");
                    if is_fatal_error(&err_msg) {
                        let _ = self
                            .event_tx
                            .send(TunnelEvent::ReconnectFailed { reason: err_msg })
                            .await;
                        return result;
                    }

                    warn!(error = %log_error, "Tunnel connection attempt failed; retrying");

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
