//! Protocol v3 streaming request handling for the tunnel forwarder.

use anyhow::{Context, Result, anyhow};
use futures_util::StreamExt;
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
use std::time::Duration;
use tokio::sync::{OwnedSemaphorePermit, mpsc};
use tokio_tungstenite::tungstenite::Message;
use tracing::{debug, info, warn};
use uuid::Uuid;

use crate::tunnel::forwarder::TunnelForwarder;
use crate::tunnel::http::{
    ensure_response_chunk_allowed, relayed_response_content_length, response_header_bytes,
    response_headers_to_map, response_headers_to_ordered_pairs, should_forward_request_header,
    supported_tunnel_method, valid_reset_content_length,
};
use crate::tunnel::limits::{LOCAL_MAX_RESPONSE_HEADER_BYTES, UI_BODY_PREVIEW_BYTES};
use crate::tunnel::preview::{
    body_preview, bounded_control_neutral_log_text, bounded_presentation_headers,
    bounded_presentation_text,
};
use crate::tunnel::relay::{
    DELIVERY_QUEUED, DELIVERY_STARTED, DELIVERY_TERMINAL, cancellation_classification,
};
use crate::tunnel::v3_relay::{
    ActiveV3Delivery, V3RelayRuntime, V3RequestBodyBridge, acknowledge_v3_response_window,
    admit_v3_response_window, complete_v3_worker, enqueue_v3_response_chunk, reap_ready_v3_workers,
    spawn_v3_request_body_bridge, v3_abort_message, v3_control_message, v3_payload_stream_id,
};
use crate::tunnel::writer::PriorityWriter;
use crate::tunnel::{TunnelEvent, WsRead};
use crate::tunnel_v3;

impl TunnelForwarder {
    #[allow(clippy::too_many_arguments)]
    pub(crate) async fn forward_v3_request(
        &self,
        incoming: tunnel_v3::IncomingRequest,
        phase: Arc<AtomicU8>,
        writer: PriorityWriter,
        tunnel_topic: String,
        limits: tunnel_v3::StreamingLimits,
        connection_window: tunnel_v3::ConnectionSendWindow,
        mut response_rx: mpsc::Receiver<tunnel_v3::WindowUpdate>,
        _work_permit: OwnedSemaphorePermit,
    ) -> Result<()> {
        let tunnel_v3::IncomingRequest { start, body } = incoming;
        let stream_id = start.stream_id;
        let started_at = std::time::Instant::now();
        let local_deadline = tokio::time::Instant::now() + Duration::from_millis(start.timeout_ms);

        let target = match self.target.request_url(
            &start.path,
            (!start.query_string.is_empty()).then_some(start.query_string.as_str()),
        ) {
            Ok(target) => target,
            Err(error) => {
                return self
                    .report_v3_failure(
                        stream_id,
                        error.code(),
                        error.to_string(),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        };

        let remaining = local_deadline.saturating_duration_since(tokio::time::Instant::now());

        let method = match supported_tunnel_method(&start.method) {
            Some(method) => method,
            None => {
                return self
                    .report_v3_failure(
                        stream_id,
                        "invalid_method_before_forward",
                        format!("Unsupported HTTP method: {}", start.method),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        };
        let client = match self.target.http_client_with_timeout(remaining) {
            Ok(client) => client,
            Err(error) => {
                return self
                    .report_v3_failure(
                        stream_id,
                        "local_client_error_before_forward",
                        error.to_string(),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        };

        let mut request = client.request(method, target);
        for (name, value) in &start.headers {
            if should_forward_request_header(name)
                && let Ok(name) = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                && let Ok(value) = reqwest::header::HeaderValue::from_str(value)
            {
                request = request.header(name, value);
            }
        }
        if let Some(content_length) = start.content_length {
            request = request.header(reqwest::header::CONTENT_LENGTH, content_length);
        }

        let (consumption_tx, mut consumption_rx) =
            mpsc::channel(limits.stream_window_items.saturating_add(1));
        let V3RequestBodyBridge {
            request_body,
            response_started,
            pump: request_body_pump,
        } = spawn_v3_request_body_bridge(body, consumption_tx);
        request = request.body(request_body);

        let reporter_writer = writer.clone();
        let reporter_topic = tunnel_topic.clone();
        let consumption_reporter = tokio::spawn(async move {
            while let Some(update) = consumption_rx.recv().await {
                reporter_writer
                    .control_v3(v3_control_message(
                        &reporter_topic,
                        "tunnel_stream_window",
                        serde_json::json!({
                            "stream_id": stream_id,
                            "direction": "request",
                            "consumed_bytes": update.consumed_bytes,
                            "consumed_items": update.consumed_items,
                        }),
                    ))
                    .await?;
            }
            Ok::<(), anyhow::Error>(())
        });

        phase.store(DELIVERY_STARTED, Ordering::Release);
        let mut response = match request.send().await {
            Ok(response) => {
                // A final local response transfers ownership away from
                // reqwest. Keep draining the bounded protocol source so the
                // service can finish ingress without waiting on a local
                // server that has already stopped reading the upload.
                let _ = response_started.send(true);
                response
            }
            Err(error) => {
                let _ = response_started.send(true);
                request_body_pump.abort();
                consumption_reporter.abort();
                return self
                    .report_v3_failure(
                        stream_id,
                        "local_forward_failed",
                        format!("Failed to forward request: {}", error.without_url()),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        };

        match request_body_pump.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                consumption_reporter.abort();
                return self
                    .report_v3_failure(
                        stream_id,
                        "request_body_drain_failed",
                        error.to_string(),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
            Err(error) => {
                consumption_reporter.abort();
                return self
                    .report_v3_failure(
                        stream_id,
                        "request_body_pump_failed",
                        error.to_string(),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        }

        match consumption_reporter.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                return self
                    .report_v3_failure(
                        stream_id,
                        "request_window_delivery_failed",
                        error.to_string(),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
            Err(error) => {
                return self
                    .report_v3_failure(
                        stream_id,
                        "request_window_reporter_failed",
                        error.to_string(),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        }

        let status = response.status().as_u16();
        if !(200..=599).contains(&status) || status == 101 {
            return self
                .report_v3_failure(
                    stream_id,
                    "invalid_local_response_status",
                    format!("Local response status {status} cannot be relayed"),
                    &writer,
                    &tunnel_topic,
                    &phase,
                )
                .await;
        }
        if response_header_bytes(response.headers()) > LOCAL_MAX_RESPONSE_HEADER_BYTES
            || response
                .headers()
                .iter()
                .any(|(_name, value)| value.to_str().is_err())
        {
            return self
                .report_v3_failure(
                    stream_id,
                    "response_headers_too_large",
                    "Local response headers cannot be relayed safely".to_string(),
                    &writer,
                    &tunnel_topic,
                    &phase,
                )
                .await;
        }
        if status == 205 && !valid_reset_content_length(response.headers()) {
            return self
                .report_v3_failure(
                    stream_id,
                    "body_not_allowed",
                    "A 205 Reset Content response may only declare Content-Length: 0".to_string(),
                    &writer,
                    &tunnel_topic,
                    &phase,
                )
                .await;
        }

        let response_header_pairs = response_headers_to_ordered_pairs(response.headers());
        let response_headers = response_headers_to_map(response.headers());
        let response_content_length =
            relayed_response_content_length(&start.method, status, response.content_length());
        if response_content_length
            .is_some_and(|length| length > limits.max_response_body_bytes as u64)
        {
            return self
                .report_v3_failure(
                    stream_id,
                    "response_body_too_large",
                    format!(
                        "Local response body exceeds tunnel limit (max {} bytes)",
                        limits.max_response_body_bytes
                    ),
                    &writer,
                    &tunnel_topic,
                    &phase,
                )
                .await;
        }

        let response_start = tunnel_v3::response_start_payload(
            stream_id,
            status,
            &response_header_pairs,
            response_content_length,
        );

        if let Err(error) =
            tunnel_v3::validate_control_payload(&response_start, limits.max_control_payload_bytes)
        {
            return self
                .report_v3_failure(
                    stream_id,
                    "response_headers_too_large",
                    error.to_string(),
                    &writer,
                    &tunnel_topic,
                    &phase,
                )
                .await;
        }

        if let Err(error) = writer
            .response_control_v3(v3_control_message(
                &tunnel_topic,
                "tunnel_response_start",
                response_start,
            ))
            .await
        {
            return self
                .report_v3_failure(
                    stream_id,
                    "response_start_delivery_failed",
                    error.to_string(),
                    &writer,
                    &tunnel_topic,
                    &phase,
                )
                .await;
        }

        let mut response_stream = match tunnel_v3::ResponseStream::new(
            stream_id,
            tunnel_topic.clone(),
            "1".to_string(),
            limits,
            connection_window,
        ) {
            Ok(stream) => stream,
            Err(error) => {
                return self
                    .report_v3_failure(
                        stream_id,
                        "response_stream_initialization_failed",
                        error.to_string(),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        };
        let mut preview = Vec::with_capacity(UI_BODY_PREVIEW_BYTES.saturating_add(1));
        let mut streamed_response_bytes = 0u64;

        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    if let Err(error) = ensure_response_chunk_allowed(&start.method, status) {
                        return self
                            .report_v3_failure(
                                stream_id,
                                "body_not_allowed",
                                error.to_string(),
                                &writer,
                                &tunnel_topic,
                                &phase,
                            )
                            .await;
                    }

                    let preview_remaining = UI_BODY_PREVIEW_BYTES
                        .saturating_add(1)
                        .saturating_sub(preview.len());
                    preview.extend_from_slice(&chunk[..chunk.len().min(preview_remaining)]);

                    for data in chunk.chunks(limits.max_chunk_bytes) {
                        let next_bytes = streamed_response_bytes.saturating_add(data.len() as u64);
                        if response_content_length
                            .is_some_and(|content_length| next_bytes > content_length)
                        {
                            return self
                                .report_v3_failure(
                                    stream_id,
                                    "response_content_length_mismatch",
                                    "Local response exceeded its declared Content-Length"
                                        .to_string(),
                                    &writer,
                                    &tunnel_topic,
                                    &phase,
                                )
                                .await;
                        }
                        if let Err(error) = enqueue_v3_response_chunk(
                            &mut response_stream,
                            &writer,
                            &mut response_rx,
                            data,
                            local_deadline,
                        )
                        .await
                        {
                            return self
                                .report_v3_failure(
                                    stream_id,
                                    "response_delivery_failed",
                                    error.to_string(),
                                    &writer,
                                    &tunnel_topic,
                                    &phase,
                                )
                                .await;
                        }
                        streamed_response_bytes = next_bytes;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    return self
                        .report_v3_failure(
                            stream_id,
                            "response_read_failed",
                            format!("Failed to read local response: {}", error.without_url()),
                            &writer,
                            &tunnel_topic,
                            &phase,
                        )
                        .await;
                }
            }
        }
        drop(response);

        if response_content_length
            .is_some_and(|content_length| streamed_response_bytes != content_length)
        {
            return self
                .report_v3_failure(
                    stream_id,
                    "response_content_length_mismatch",
                    "Local response did not match its declared Content-Length".to_string(),
                    &writer,
                    &tunnel_topic,
                    &phase,
                )
                .await;
        }

        let evidence = match response_stream.finish() {
            Ok(evidence) => evidence,
            Err(error) => {
                return self
                    .report_v3_failure(
                        stream_id,
                        "response_integrity_failed",
                        error.to_string(),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        };
        if let Err(error) = writer
            .response_control_v3(v3_control_message(
                &tunnel_topic,
                "tunnel_response_end",
                evidence.end_payload(stream_id),
            ))
            .await
        {
            return self
                .report_v3_failure(
                    stream_id,
                    "response_end_delivery_failed",
                    error.to_string(),
                    &writer,
                    &tunnel_topic,
                    &phase,
                )
                .await;
        }

        while !response_stream.fully_consumed() {
            if let Err(error) = acknowledge_v3_response_window(
                &mut response_stream,
                &mut response_rx,
                local_deadline,
            )
            .await
            {
                return self
                    .report_v3_failure(
                        stream_id,
                        "response_consumption_failed",
                        error.to_string(),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        }

        phase.store(DELIVERY_TERMINAL, Ordering::Release);
        let mut response_body = body_preview(&preview, &response_headers);
        if evidence.bytes > preview.len() as u64
            && let Some(body) = &mut response_body
        {
            body.push_str(&format!("\n… [{} bytes total]", evidence.bytes));
        }
        self.emit_presentation(TunnelEvent::RequestForwarded {
            request_id: stream_id.to_string(),
            status,
            duration_ms: started_at.elapsed().as_millis() as u64,
            response_headers: bounded_presentation_headers(response_header_pairs),
            response_body,
        });
        Ok(())
    }

    pub(crate) async fn report_v3_failure(
        &self,
        stream_id: Uuid,
        code: &str,
        message: String,
        writer: &PriorityWriter,
        tunnel_topic: &str,
        phase: &AtomicU8,
    ) -> Result<()> {
        let previous_phase = phase.swap(DELIVERY_TERMINAL, Ordering::AcqRel);
        let outcome = cancellation_classification(previous_phase)
            .map(|(_code, outcome)| outcome)
            .unwrap_or("outcome_unknown");
        writer
            .response_control_v3(v3_abort_message(
                tunnel_topic,
                stream_id,
                "response",
                code,
                outcome,
            ))
            .await?;
        self.emit_presentation(TunnelEvent::RequestFailed {
            request_id: stream_id.to_string(),
            error: bounded_presentation_text(&message),
        });
        Err(anyhow!(message))
    }

    pub(crate) async fn run_v3_tunnel(
        &self,
        mut read: WsRead,
        writer: PriorityWriter,
        mut writer_task: tokio::task::JoinHandle<Result<()>>,
        tunnel_topic: String,
        limits: tunnel_v3::StreamingLimits,
    ) -> Result<()> {
        let mut runtime = V3RelayRuntime::new(tunnel_topic.clone(), limits)?;
        let mut ping_interval = tokio::time::interval(Duration::from_secs(30));
        ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ping_interval.tick().await;
        let mut ping_counter = 2u64;

        loop {
            // A completed task has already released its local-forward permit.
            // Reap every ready result before accepting more ingress so the
            // JoinSet and active-delivery index cannot grow while the socket is
            // continuously readable.
            reap_ready_v3_workers(&mut runtime, &writer, &tunnel_topic).await?;

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
                    let mut message = v3_control_message(
                        &tunnel_topic,
                        "ping",
                        serde_json::json!({}),
                    );
                    message.reference = Some(ping_counter.to_string());
                    writer.control_v3(message).await?;
                    ping_counter = ping_counter.saturating_add(1);
                }
                completed = runtime.workers.join_next_with_id(), if !runtime.workers.is_empty() => {
                    if let Some(completion) = completed {
                        complete_v3_worker(
                            completion,
                            &mut runtime,
                            &writer,
                            &tunnel_topic,
                        ).await?;
                    }
                },
                maybe_message = read.next() => match maybe_message {
                    Some(Ok(Message::Text(text))) => {
                        let message = tunnel_v3::decode_server_control(&text)?;
                        if matches!(message.event.as_str(), "phx_error" | "phx_close") {
                            return Err(anyhow!("Protocol v3 channel closed"));
                        }
                        if let Err(error) = self
                            .handle_v3_control(
                                message,
                                &writer,
                                &tunnel_topic,
                                limits,
                                &mut runtime,
                            )
                            .await
                        {
                            let error = bounded_control_neutral_log_text(&error.to_string());
                            warn!(error = %error, "Rejected protocol v3 control event");
                        }
                    }
                    Some(Ok(Message::Binary(encoded))) => {
                        self.handle_v3_binary(
                            &encoded,
                            &writer,
                            &tunnel_topic,
                            limits,
                            &mut runtime,
                        ).await?;
                    }
                    Some(Ok(Message::Ping(data))) => writer.pong(data).await?,
                    Some(Ok(Message::Close(frame))) => {
                        let frame = bounded_control_neutral_log_text(&format!("{frame:?}"));
                        info!(frame = %frame, "Tunnel WebSocket closed");
                        self.emit_presentation(TunnelEvent::Disconnected);
                        break;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        let error = bounded_control_neutral_log_text(&error.to_string());
                        return Err(anyhow!("Tunnel WebSocket error: {error}"));
                    }
                    None => {
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

    pub(crate) async fn handle_v3_control(
        &self,
        message: tunnel_v3::ControlMessage,
        writer: &PriorityWriter,
        tunnel_topic: &str,
        limits: tunnel_v3::StreamingLimits,
        runtime: &mut V3RelayRuntime,
    ) -> Result<()> {
        if message.topic != tunnel_topic && message.topic != "phoenix" {
            return Err(anyhow!("Protocol v3 control event has an unexpected topic"));
        }
        tunnel_v3::validate_control_payload(&message.payload, limits.max_control_payload_bytes)?;

        match message.event.as_str() {
            "tunnel_request_start" => {
                let stream_id = v3_payload_stream_id(&message.payload)?;
                if runtime.active_deliveries.contains_key(&stream_id) {
                    writer
                        .control_v3(v3_abort_message(
                            tunnel_topic,
                            stream_id,
                            "request",
                            "duplicate_stream",
                            "known_not_executed",
                        ))
                        .await?;
                    return Ok(());
                }

                let incoming = match runtime.requests.start(&message.payload) {
                    Ok(incoming) => incoming,
                    Err(error) => {
                        writer
                            .control_v3(v3_abort_message(
                                tunnel_topic,
                                stream_id,
                                "request",
                                "invalid_request_start",
                                "known_not_executed",
                            ))
                            .await?;
                        return Err(error);
                    }
                };

                let Some(work_permit) = runtime.work_count.clone().try_acquire_owned().ok() else {
                    runtime
                        .requests
                        .cancel(stream_id, "relay_overloaded_before_forward");
                    writer
                        .control_v3(v3_abort_message(
                            tunnel_topic,
                            stream_id,
                            "request",
                            "relay_overloaded_before_forward",
                            "known_not_executed",
                        ))
                        .await?;
                    return Ok(());
                };

                let headers = bounded_presentation_headers(incoming.start.headers.iter().cloned());
                self.emit_presentation(TunnelEvent::RequestReceived {
                    request_id: stream_id.to_string(),
                    method: incoming.start.method.clone(),
                    path: bounded_presentation_text(&incoming.start.path),
                    headers,
                    body: None,
                    query_string: bounded_presentation_text(&incoming.start.query_string),
                    replay: incoming.start.replay,
                });

                let (response_tx, response_rx) =
                    mpsc::channel(limits.stream_window_items.saturating_add(1));
                let phase = Arc::new(AtomicU8::new(DELIVERY_QUEUED));
                let task_phase = phase.clone();
                let worker = self.clone();
                let task_writer = writer.clone();
                let task_topic = tunnel_topic.to_string();
                let response_window = runtime.response_window.clone();
                let abort = runtime.workers.spawn(async move {
                    let result = worker
                        .forward_v3_request(
                            incoming,
                            task_phase,
                            task_writer,
                            task_topic,
                            limits,
                            response_window,
                            response_rx,
                            work_permit,
                        )
                        .await;
                    (stream_id, result)
                });

                runtime.active_deliveries.insert(
                    stream_id,
                    ActiveV3Delivery {
                        abort,
                        phase,
                        response_tx,
                    },
                );
            }
            "tunnel_request_end" => {
                let stream_id = v3_payload_stream_id(&message.payload)?;
                if let Err(error) = runtime.requests.finish(&message.payload) {
                    if let Some(active) = runtime.active_deliveries.remove(&stream_id) {
                        active.abort.abort();
                    }
                    writer
                        .control_v3(v3_abort_message(
                            tunnel_topic,
                            stream_id,
                            "request",
                            "invalid_request_end",
                            "outcome_unknown",
                        ))
                        .await?;
                    return Err(error);
                }
            }
            "tunnel_stream_window" => {
                if message
                    .payload
                    .get("direction")
                    .and_then(serde_json::Value::as_str)
                    != Some("response")
                {
                    return Err(anyhow!("Protocol v3 window has an unexpected direction"));
                }
                let stream_id = v3_payload_stream_id(&message.payload)?;
                let update = tunnel_v3::WindowUpdate {
                    consumed_bytes: message
                        .payload
                        .get("consumed_bytes")
                        .and_then(serde_json::Value::as_u64)
                        .ok_or_else(|| anyhow!("Protocol v3 window has invalid bytes"))?,
                    consumed_items: message
                        .payload
                        .get("consumed_items")
                        .and_then(serde_json::Value::as_u64)
                        .ok_or_else(|| anyhow!("Protocol v3 window has invalid items"))?,
                };
                let admission_error = runtime
                    .active_deliveries
                    .get(&stream_id)
                    .and_then(|active| admit_v3_response_window(&active.response_tx, update).err());

                if let Some(code) = admission_error
                    && let Some(active) = runtime.active_deliveries.remove(&stream_id)
                {
                    let phase = active.phase.load(Ordering::Acquire);
                    let outcome = cancellation_classification(phase)
                        .map(|(_code, outcome)| outcome)
                        .unwrap_or("outcome_unknown");

                    active.abort.abort();
                    runtime.requests.cancel(stream_id, code);
                    writer.try_control_v3(v3_abort_message(
                        tunnel_topic,
                        stream_id,
                        "response",
                        code,
                        outcome,
                    ))?;
                }
            }
            "tunnel_stream_abort" => {
                let stream_id = v3_payload_stream_id(&message.payload)?;
                let direction = message
                    .payload
                    .get("direction")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| anyhow!("Protocol v3 abort has no direction"))?;
                if !matches!(direction, "request" | "response") {
                    return Err(anyhow!("Protocol v3 abort has an invalid direction"));
                }

                if runtime.requests.contains(stream_id) {
                    runtime.requests.cancel(stream_id, "remote_abort");
                }
                if let Some(active) = runtime.active_deliveries.remove(&stream_id) {
                    active.abort.abort();
                }
            }
            "buffered_summary" => {
                let count = message
                    .payload
                    .get("count")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                let oldest_captured_at = message
                    .payload
                    .get("oldest_captured_at")
                    .and_then(serde_json::Value::as_str)
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
                    let mut replay =
                        v3_control_message(tunnel_topic, "buffered:replay", serde_json::json!({}));
                    replay.reference = Some("buffered-replay".to_string());
                    writer
                        .control_v3(replay)
                        .await
                        .context("Failed to send buffered replay request")?;
                }
            }
            "buffered_replayed" => {
                let capture_id = message
                    .payload
                    .get("capture_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let status = message
                    .payload
                    .get("status")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|status| u16::try_from(status).ok())
                    .unwrap_or(0);

                let _ = self
                    .event_tx
                    .send(TunnelEvent::BufferedReplayed { capture_id, status })
                    .await;
            }
            "buffered_replay_failed" => {
                let capture_id = message
                    .payload
                    .get("capture_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let reason = message
                    .payload
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();

                let _ = self
                    .event_tx
                    .send(TunnelEvent::BufferedReplayFailed { capture_id, reason })
                    .await;
            }
            "phx_reply" => {
                if message.reference.as_deref() == Some("buffered-replay") {
                    let status = message
                        .payload
                        .get("status")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");

                    if status != "ok" {
                        let reason = message
                            .payload
                            .get("response")
                            .and_then(|response| response.get("reason"))
                            .and_then(serde_json::Value::as_str)
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
                }
            }
            "phx_error" | "phx_close" => {
                return Err(anyhow!("Protocol v3 channel closed"));
            }
            event => debug!(event = %event, "Unhandled protocol v3 tunnel event"),
        }

        Ok(())
    }

    pub(crate) async fn handle_v3_binary(
        &self,
        encoded: &[u8],
        writer: &PriorityWriter,
        tunnel_topic: &str,
        limits: tunnel_v3::StreamingLimits,
        runtime: &mut V3RelayRuntime,
    ) -> Result<()> {
        let push = tunnel_v3::decode_server_binary_push(encoded)?;
        if push.topic != tunnel_topic || push.event != "tunnel_request_chunk" {
            return Err(anyhow!("Unexpected protocol v3 binary channel event"));
        }
        let chunk = tunnel_v3::decode_chunk(push.payload, limits.max_chunk_bytes)?;
        let stream_id = chunk.stream_id;

        if let Err(error) = runtime.requests.accept_binary_push(encoded) {
            runtime.requests.cancel(stream_id, "invalid_request_chunk");
            if let Some(active) = runtime.active_deliveries.remove(&stream_id) {
                active.abort.abort();
            }
            writer
                .control_v3(v3_abort_message(
                    tunnel_topic,
                    stream_id,
                    "request",
                    "invalid_request_chunk",
                    "outcome_unknown",
                ))
                .await?;
            let error = bounded_control_neutral_log_text(&error.to_string());
            warn!(stream_id = %stream_id, error = %error, "Rejected protocol v3 request chunk");
        }

        Ok(())
    }
}
