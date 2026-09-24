//! Protocol v2 request handling for the tunnel forwarder.

use anyhow::{Context, Result, anyhow};
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
use std::time::Duration;
use tracing::{debug, error, info, warn};

use crate::tunnel::TunnelEvent;
use crate::tunnel::forwarder::TunnelForwarder;
use crate::tunnel::framing::{
    ChannelMessage, OutboundTunnelStream, TunnelStreamAssembler, json_usize, ordered_header_pairs,
    required_string, stream_error_message, unix_time_ms,
};
use crate::tunnel::http::{
    decode_request_body_limited, encode_response_body, response_header_limit_error,
    response_headers_to_map, response_headers_to_ordered_pairs, should_forward_request_header,
    supported_tunnel_method,
};
use crate::tunnel::limits::{
    INBOUND_STREAM_MAX_COUNT, ResponseBufferBudget, TUNNEL_MAX_FRAME_BYTES, TunnelLimits,
    inbound_stream_fits_budget,
};
use crate::tunnel::preview::{
    body_preview, bounded_control_neutral_log_text, bounded_presentation_headers,
    bounded_presentation_text,
};
use crate::tunnel::relay::{
    ActiveDelivery, DELIVERY_QUEUED, DELIVERY_STARTED, DELIVERY_TERMINAL, DeliveryFailure,
    LocalDelivery, RelayRuntime, cancellation_classification, delivery_error_message,
};
use crate::tunnel::writer::PriorityWriter;

impl TunnelForwarder {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn handle_tunnel_message(
        &self,
        text: &str,
        writer: &PriorityWriter,
        tunnel_topic: &str,
        tunnel_limits: TunnelLimits,
        runtime: &mut RelayRuntime,
    ) -> Result<()> {
        let msg: ChannelMessage = serde_json::from_str(text)?;
        let log_topic = bounded_control_neutral_log_text(&msg.topic);
        let log_event = bounded_control_neutral_log_text(&msg.event);

        debug!(
            topic = %log_topic,
            event = %log_event,
            "Received tunnel message"
        );

        match msg.event.as_str() {
            "tunnel_stream_start" => {
                let assembler = TunnelStreamAssembler::from_start(&msg.payload, "request")?;
                if runtime.inbound_streams.len() >= INBOUND_STREAM_MAX_COUNT
                    || !inbound_stream_fits_budget(
                        runtime.inbound_reserved_bytes,
                        assembler.total_bytes,
                    )
                {
                    writer
                        .control(stream_error_message(
                            tunnel_topic,
                            &assembler.stream_id,
                            "relay_inbound_overloaded",
                        ))
                        .await?;
                    return Ok(());
                }
                if runtime.inbound_streams.contains_key(&assembler.stream_id) {
                    writer
                        .control(stream_error_message(
                            tunnel_topic,
                            &assembler.stream_id,
                            "duplicate_stream",
                        ))
                        .await?;
                    return Ok(());
                }

                runtime.inbound_reserved_bytes += assembler.total_bytes;
                runtime
                    .inbound_streams
                    .insert(assembler.stream_id.clone(), assembler);
            }
            "tunnel_stream_frame" => {
                let stream_id = msg
                    .payload
                    .get("stream_id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| anyhow!("Tunnel frame is missing its stream id"))?
                    .to_string();
                let sequence = json_usize(&msg.payload, "sequence")?;

                let completed = match runtime.inbound_streams.get_mut(&stream_id) {
                    Some(assembler) => match assembler.append(&msg.payload) {
                        Ok(completed) => completed,
                        Err(error) => {
                            let assembler = runtime
                                .inbound_streams
                                .remove(&stream_id)
                                .expect("invalid stream must still be reserved");
                            runtime.inbound_reserved_bytes = runtime
                                .inbound_reserved_bytes
                                .saturating_sub(assembler.total_bytes);
                            writer
                                .control(stream_error_message(
                                    tunnel_topic,
                                    &stream_id,
                                    "invalid_stream_frame",
                                ))
                                .await?;
                            let log_stream_id = bounded_control_neutral_log_text(&stream_id);
                            let error = bounded_control_neutral_log_text(&error.to_string());
                            warn!(stream_id = %log_stream_id, error = %error, "Rejected invalid tunnel frame");
                            return Ok(());
                        }
                    },
                    None => {
                        writer
                            .control(stream_error_message(
                                tunnel_topic,
                                &stream_id,
                                "unknown_stream",
                            ))
                            .await?;
                        return Ok(());
                    }
                };

                writer
                    .control(ChannelMessage {
                        topic: tunnel_topic.to_string(),
                        event: "tunnel_stream_ack".to_string(),
                        payload: serde_json::json!({
                            "stream_id": stream_id,
                            "sequence": sequence,
                        }),
                        reference: None,
                    })
                    .await?;

                if let Some(payload) = completed {
                    let assembler = runtime
                        .inbound_streams
                        .remove(&stream_id)
                        .ok_or_else(|| anyhow!("Completed tunnel stream disappeared"))?;
                    runtime.inbound_reserved_bytes = runtime
                        .inbound_reserved_bytes
                        .saturating_sub(assembler.total_bytes);
                    let deadline_unix_ms = assembler.deadline_unix_ms;
                    drop(assembler);

                    let delivery = match self.parse_local_delivery(
                        &payload,
                        deadline_unix_ms,
                        tunnel_limits,
                    ) {
                        Ok(delivery) => delivery,
                        Err(error) => {
                            let request_id = payload
                                .get("request_id")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or(&stream_id)
                                .to_string();
                            let error = error.to_string();
                            drop(payload);
                            writer
                                .control(delivery_error_message(
                                    tunnel_topic,
                                    &request_id,
                                    "invalid_relay_request",
                                    "known_not_executed",
                                    &error,
                                ))
                                .await?;
                            return Ok(());
                        }
                    };
                    // Parsing clones the validated metadata and decodes the body into
                    // LocalDelivery, so the larger serialized JSON representation is
                    // no longer needed while the request waits for local capacity.
                    drop(payload);

                    if runtime.active_deliveries.contains_key(&delivery.request_id) {
                        writer
                            .control(delivery_error_message(
                                tunnel_topic,
                                &delivery.request_id,
                                "duplicate_delivery",
                                "known_not_executed",
                                "Delivery is already active",
                            ))
                            .await?;
                        return Ok(());
                    }

                    let Some(permit) = runtime.work_budget.try_acquire(delivery.retained_bytes())
                    else {
                        writer
                            .control(delivery_error_message(
                                tunnel_topic,
                                &delivery.request_id,
                                "relay_overloaded_before_forward",
                                "known_not_executed",
                                "Local relay capacity is full",
                            ))
                            .await?;
                        self.emit_presentation(TunnelEvent::RequestFailed {
                            request_id: delivery.request_id,
                            error: "Local relay capacity is full".to_string(),
                        });
                        return Ok(());
                    };

                    self.emit_presentation(delivery.received_event());
                    let request_id = delivery.request_id.clone();
                    let task_request_id = request_id.clone();
                    let phase = Arc::new(AtomicU8::new(DELIVERY_QUEUED));
                    let task_phase = phase.clone();
                    let worker = self.clone();
                    let task_writer = writer.clone();
                    let task_response_budget = runtime.response_budget.clone();
                    let topic = tunnel_topic.to_string();
                    let abort = runtime.workers.spawn(async move {
                        let _permit = permit;
                        let result = worker
                            .forward_tunnel_request(
                                delivery,
                                task_phase,
                                task_writer,
                                topic,
                                tunnel_limits,
                                task_response_budget,
                            )
                            .await;
                        (task_request_id, result)
                    });
                    runtime
                        .active_deliveries
                        .insert(request_id, ActiveDelivery { abort, phase });
                }
            }
            "tunnel_cancel" => {
                let request_id = required_string(&msg.payload, "request_id")?;
                if let Some(active) = runtime.active_deliveries.remove(&request_id) {
                    let phase = active.phase.load(Ordering::Acquire);
                    if let Some((code, outcome)) = cancellation_classification(phase) {
                        active.abort.abort();
                        writer
                            .control(delivery_error_message(
                                tunnel_topic,
                                &request_id,
                                code,
                                outcome,
                                "Delivery was cancelled",
                            ))
                            .await?;
                        self.emit_presentation(TunnelEvent::RequestFailed {
                            request_id,
                            error: outcome.to_string(),
                        });
                    }
                }
            }
            "tunnel_stream_error" => {
                let stream_id = msg
                    .payload
                    .get("stream_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                let code = msg
                    .payload
                    .get("code")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("stream_error");

                if let Some(stream_id) = stream_id.as_deref() {
                    if let Some(assembler) = runtime.inbound_streams.remove(stream_id) {
                        runtime.inbound_reserved_bytes = runtime
                            .inbound_reserved_bytes
                            .saturating_sub(assembler.total_bytes);
                    }

                    if let Some(active) = runtime.active_deliveries.remove(stream_id) {
                        active.abort.abort();
                        self.emit_presentation(TunnelEvent::RequestFailed {
                            request_id: stream_id.to_string(),
                            error: bounded_presentation_text(code),
                        });
                    }
                }

                let code = bounded_control_neutral_log_text(code);
                warn!(code = %code, "Tunnel stream failed");
            }
            "buffered_summary" => {
                let count = msg
                    .payload
                    .get("count")
                    .and_then(|v| v.as_u64())
                    .unwrap_or(0);
                let oldest_captured_at = msg
                    .payload
                    .get("oldest_captured_at")
                    .and_then(|v| v.as_str())
                    .map(str::to_string);

                info!(count = count, "Buffered requests waiting on server");

                let _ = self
                    .event_tx
                    .send(TunnelEvent::BufferedSummary {
                        count,
                        oldest_captured_at,
                    })
                    .await;

                if self.replay_buffered && count > 0 && !runtime.auto_drain_requested {
                    runtime.auto_drain_requested = true;

                    let replay_msg = ChannelMessage {
                        topic: tunnel_topic.to_string(),
                        event: "buffered:replay".to_string(),
                        payload: serde_json::json!({}),
                        reference: Some("buffered-replay".to_string()),
                    };

                    writer
                        .control(replay_msg)
                        .await
                        .context("Failed to send buffered replay request")?;
                }
            }
            "buffered_replayed" => {
                let capture_id = msg
                    .payload
                    .get("capture_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let status = msg
                    .payload
                    .get("status")
                    .and_then(|v| v.as_u64())
                    .and_then(|v| u16::try_from(v).ok())
                    .unwrap_or(0);

                let _ = self
                    .event_tx
                    .send(TunnelEvent::BufferedReplayed { capture_id, status })
                    .await;
            }
            "buffered_replay_failed" => {
                let capture_id = msg
                    .payload
                    .get("capture_id")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();
                let reason = msg
                    .payload
                    .get("reason")
                    .and_then(|v| v.as_str())
                    .unwrap_or("unknown")
                    .to_string();

                let _ = self
                    .event_tx
                    .send(TunnelEvent::BufferedReplayFailed { capture_id, reason })
                    .await;
            }
            "phx_reply" => {
                if msg.reference.as_deref() == Some("buffered-replay") {
                    let status = msg
                        .payload
                        .get("status")
                        .and_then(|v| v.as_str())
                        .unwrap_or("");

                    if status != "ok" {
                        let reason = msg
                            .payload
                            .get("response")
                            .and_then(|r| r.get("reason"))
                            .and_then(|r| r.as_str())
                            .unwrap_or("unknown")
                            .to_string();

                        if reason != "buffer_empty" {
                            let log_reason = bounded_control_neutral_log_text(&reason);
                            warn!(reason = %log_reason, "Buffered replay request rejected");
                            let _ = self
                                .event_tx
                                .send(TunnelEvent::BufferedReplayFailed {
                                    capture_id: "unknown".to_string(),
                                    reason,
                                })
                                .await;
                        }
                    }
                } else if let Some(response) = msg.payload.get("response")
                    && response
                        .get("pong")
                        .and_then(|v| v.as_bool())
                        .unwrap_or(false)
                {
                    debug!("Received pong from server");
                }
            }
            _ => {
                debug!(event = %log_event, "Unhandled tunnel event");
            }
        }

        Ok(())
    }

    pub(crate) fn parse_local_delivery(
        &self,
        payload: &serde_json::Value,
        deadline_unix_ms: u64,
        tunnel_limits: TunnelLimits,
    ) -> Result<LocalDelivery> {
        let request_id = required_string(payload, "request_id")?;
        let method = required_string(payload, "method")?;
        let path = required_string(payload, "path")?;
        let query_string = payload
            .get("query_string")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("")
            .to_string();
        let headers = ordered_header_pairs(payload.get("headers"))?;
        let replay = payload
            .get("replay")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false);
        let body_encoding = payload
            .get("body_encoding")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("raw");
        let raw_body = payload
            .get("body")
            .and_then(serde_json::Value::as_str)
            .unwrap_or("");
        let body = decode_request_body_limited(
            body_encoding,
            raw_body,
            tunnel_limits.max_request_body_bytes,
        )?;

        Ok(LocalDelivery {
            request_id,
            method,
            path,
            query_string,
            headers,
            body,
            deadline_unix_ms,
            replay,
        })
    }

    pub(crate) async fn forward_tunnel_request(
        &self,
        delivery: LocalDelivery,
        phase: Arc<AtomicU8>,
        writer: PriorityWriter,
        tunnel_topic: String,
        tunnel_limits: TunnelLimits,
        response_budget: ResponseBufferBudget,
    ) -> Result<()> {
        let LocalDelivery {
            request_id,
            method,
            path,
            query_string,
            headers,
            body,
            deadline_unix_ms,
            replay: _,
        } = delivery;
        let start_time = tokio::time::Instant::now();

        info!(
            request_id = %request_id,
            method = %method,
            path = %path,
            "Forwarding tunnel request to local server"
        );

        let target = match self.target.request_url(
            &path,
            (!query_string.is_empty()).then_some(query_string.as_str()),
        ) {
            Ok(target) => target,
            Err(error) => {
                return self
                    .report_tunnel_failure(
                        &request_id,
                        DeliveryFailure::known(error.code(), error.to_string()),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        };

        let remaining_ms = deadline_unix_ms.saturating_sub(unix_time_ms());
        if remaining_ms == 0 {
            return self
                .report_tunnel_failure(
                    &request_id,
                    DeliveryFailure::known(
                        "deadline_exceeded_before_forward",
                        "Delivery deadline elapsed before local forwarding",
                    ),
                    &writer,
                    &tunnel_topic,
                    &phase,
                )
                .await;
        }

        // One end-to-end deadline covers a proxy-free, address-pinned local request
        // and its response stream.
        let client = match self
            .target
            .http_client_with_timeout(Duration::from_millis(remaining_ms))
        {
            Ok(client) => client,
            Err(error) => {
                return self
                    .report_tunnel_failure(
                        &request_id,
                        DeliveryFailure::known(
                            "local_client_error_before_forward",
                            error.to_string(),
                        ),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        };

        let request_method = match supported_tunnel_method(&method) {
            Some(method) => method,
            None => {
                return self
                    .report_tunnel_failure(
                        &request_id,
                        DeliveryFailure::known(
                            "invalid_method_before_forward",
                            format!("Unsupported HTTP method: {method}"),
                        ),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        };
        let mut req_builder = client.request(request_method, target);

        // reqwest sets request framing headers from the body actually sent.
        for (key, value) in headers {
            if should_forward_request_header(&key)
                && let Ok(header_name) = reqwest::header::HeaderName::from_bytes(key.as_bytes())
                && let Ok(header_value) = reqwest::header::HeaderValue::from_str(&value)
            {
                req_builder = req_builder.header(header_name, header_value);
            }
        }

        if !body.is_empty() {
            req_builder = req_builder.body(body);
        }

        // After this point the local server may have observed the request. Every
        // failure must therefore report an unknown outcome.
        phase.store(DELIVERY_STARTED, Ordering::Release);

        // Send request and handle response
        match req_builder.send().await {
            Ok(mut response) => {
                let status = response.status().as_u16();

                if let Some(error_msg) =
                    response_header_limit_error(response.headers(), tunnel_limits)
                {
                    return self
                        .report_tunnel_failure(
                            &request_id,
                            DeliveryFailure::unknown("response_headers_too_large", error_msg),
                            &writer,
                            &tunnel_topic,
                            &phase,
                        )
                        .await;
                }

                let response_header_pairs = response_headers_to_ordered_pairs(response.headers());
                let response_headers = response_headers_to_map(response.headers());

                let response_content_length = response
                    .content_length()
                    .and_then(|length| usize::try_from(length).ok());

                if response_content_length
                    .is_some_and(|length| length > tunnel_limits.max_response_body_bytes)
                {
                    let error_msg = format!(
                        "Local response body exceeds tunnel limit (max {} bytes)",
                        tunnel_limits.max_response_body_bytes
                    );

                    return self
                        .report_tunnel_failure(
                            &request_id,
                            DeliveryFailure::unknown("response_body_too_large", error_msg),
                            &writer,
                            &tunnel_topic,
                            &phase,
                        )
                        .await;
                }

                let Some(mut response_reservation) =
                    response_budget.try_reserve(response_content_length)
                else {
                    return self
                        .report_tunnel_failure(
                            &request_id,
                            DeliveryFailure::unknown(
                                "relay_response_overloaded",
                                "Concurrent local responses exceed the relay buffer budget",
                            ),
                            &writer,
                            &tunnel_topic,
                            &phase,
                        )
                        .await;
                };

                let initial_capacity = response_content_length
                    .unwrap_or(0)
                    .min(TUNNEL_MAX_FRAME_BYTES);
                let mut response_bytes = Vec::with_capacity(initial_capacity);

                loop {
                    match response.chunk().await {
                        Ok(Some(chunk)) => {
                            if response_bytes.len().saturating_add(chunk.len())
                                > tunnel_limits.max_response_body_bytes
                            {
                                let error_msg = format!(
                                    "Local response body exceeds tunnel limit (max {} bytes)",
                                    tunnel_limits.max_response_body_bytes
                                );

                                return self
                                    .report_tunnel_failure(
                                        &request_id,
                                        DeliveryFailure::unknown(
                                            "response_body_too_large",
                                            error_msg,
                                        ),
                                        &writer,
                                        &tunnel_topic,
                                        &phase,
                                    )
                                    .await;
                            }

                            if !response_reservation
                                .try_grow_to(response_bytes.len().saturating_add(chunk.len()))
                            {
                                return self
                                    .report_tunnel_failure(
                                        &request_id,
                                        DeliveryFailure::unknown(
                                            "relay_response_overloaded",
                                            "Concurrent local responses exceed the relay buffer budget",
                                        ),
                                        &writer,
                                        &tunnel_topic,
                                        &phase,
                                    )
                                    .await;
                            }

                            response_bytes.extend_from_slice(&chunk);
                        }
                        Ok(None) => break,
                        Err(error) => {
                            let error_msg =
                                format!("Failed to read local response: {}", error.without_url());

                            return self
                                .report_tunnel_failure(
                                    &request_id,
                                    DeliveryFailure::unknown("response_read_failed", error_msg),
                                    &writer,
                                    &tunnel_topic,
                                    &phase,
                                )
                                .await;
                        }
                    }
                }

                drop(response);
                let response_body_preview = body_preview(&response_bytes, &response_headers);
                drop(response_headers);
                let presentation_response_headers =
                    bounded_presentation_headers(response_header_pairs.iter().cloned());
                let (response_body, body_encoding) = encode_response_body(&response_bytes);
                drop(response_bytes);
                let duration_ms = start_time.elapsed().as_millis() as u64;

                info!(
                    request_id = %request_id,
                    status = %status,
                    duration_ms = %duration_ms,
                    body_encoding = %body_encoding,
                    "Request forwarded successfully"
                );

                let response_result = self
                    .send_framed_tunnel_response(
                        &writer,
                        &tunnel_topic,
                        &request_id,
                        deadline_unix_ms,
                        serde_json::json!({
                            "request_id": request_id.clone(),
                            "status": status,
                            "headers": response_header_pairs,
                            "body": response_body,
                            "body_encoding": body_encoding,
                        }),
                    )
                    .await;
                drop(response_reservation);

                if let Err(error) = response_result {
                    return self
                        .report_tunnel_failure(
                            &request_id,
                            DeliveryFailure::unknown("response_delivery_failed", error.to_string()),
                            &writer,
                            &tunnel_topic,
                            &phase,
                        )
                        .await;
                }

                phase.store(DELIVERY_TERMINAL, Ordering::Release);
                self.emit_presentation(TunnelEvent::RequestForwarded {
                    request_id,
                    status,
                    duration_ms,
                    response_headers: presentation_response_headers,
                    response_body: response_body_preview,
                });
            }
            Err(error) => {
                let duration_ms = start_time.elapsed().as_millis() as u64;
                let error_msg = format!("Failed to forward request: {}", error.without_url());

                error!(
                    request_id = %request_id,
                    error = %error_msg,
                    duration_ms = %duration_ms,
                    "Request forwarding failed"
                );

                return self
                    .report_tunnel_failure(
                        &request_id,
                        DeliveryFailure::unknown("local_forward_failed", error_msg),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        }

        Ok(())
    }

    pub(crate) async fn send_framed_tunnel_response(
        &self,
        writer: &PriorityWriter,
        tunnel_topic: &str,
        request_id: &str,
        deadline_unix_ms: u64,
        payload: serde_json::Value,
    ) -> Result<()> {
        let mut stream =
            OutboundTunnelStream::new(request_id, "response", deadline_unix_ms, &payload)?;
        drop(payload);

        writer.response(stream.start_message(tunnel_topic)).await?;

        while let Some(frame) = stream.next_frame(tunnel_topic)? {
            if unix_time_ms() >= deadline_unix_ms {
                return Err(anyhow!("deadline_exceeded"));
            }

            writer.response(frame).await?;
        }

        Ok(())
    }

    pub(crate) async fn report_tunnel_failure(
        &self,
        request_id: &str,
        failure: DeliveryFailure,
        writer: &PriorityWriter,
        tunnel_topic: &str,
        phase: &AtomicU8,
    ) -> Result<()> {
        let DeliveryFailure {
            code,
            outcome,
            error,
        } = failure;
        phase.store(DELIVERY_TERMINAL, Ordering::Release);
        error!(request_id = %request_id, code, outcome, error = %error, "Tunnel request failed");

        self.emit_presentation(TunnelEvent::RequestFailed {
            request_id: request_id.to_string(),
            error: error.clone(),
        });

        writer
            .control(delivery_error_message(
                tunnel_topic,
                request_id,
                code,
                outcome,
                &error,
            ))
            .await
    }
}
