//! Relay runtime: v2/v3 stream admission, worker reaping, and backpressure.

use super::*;

#[test]
fn protocol_v3_response_window_admission_never_waits_for_worker_capacity() {
    let (sender, mut receiver) = mpsc::channel(1);
    let first = tunnel_v3::WindowUpdate {
        consumed_bytes: 64_000,
        consumed_items: 1,
    };
    let second = tunnel_v3::WindowUpdate {
        consumed_bytes: 128_000,
        consumed_items: 2,
    };

    assert_eq!(admit_v3_response_window(&sender, first), Ok(()));
    assert_eq!(
        admit_v3_response_window(&sender, second),
        Err("response_window_overflow")
    );
    assert_eq!(receiver.try_recv().unwrap(), first);

    drop(receiver);
    assert_eq!(
        admit_v3_response_window(&sender, second),
        Err("response_window_receiver_closed")
    );
}

#[tokio::test]
async fn protocol_v3_response_window_overflow_aborts_only_the_affected_stream() {
    let address = "127.0.0.1:9".parse().unwrap();
    let (forwarder, _event_rx) = forwarder_for_local_address(address).await;
    let (writer, mut control_rx, _response_rx) = test_priority_writer();
    let limits = test_streaming_limits();
    let mut runtime = V3RelayRuntime::new("tunnel:connect".to_string(), limits).unwrap();
    let stream_id = Uuid::new_v4();
    let (response_tx, _response_rx) = mpsc::channel(1);
    response_tx
        .try_send(tunnel_v3::WindowUpdate {
            consumed_bytes: 1,
            consumed_items: 1,
        })
        .unwrap();
    let phase = Arc::new(AtomicU8::new(DELIVERY_STARTED));
    let abort = runtime.workers.spawn(async move {
        std::future::pending::<()>().await;
        (stream_id, Ok(()))
    });
    runtime.active_deliveries.insert(
        stream_id,
        ActiveV3Delivery {
            abort,
            phase,
            response_tx,
        },
    );

    forwarder
        .handle_v3_control(
            v3_control_message(
                "tunnel:connect",
                "tunnel_stream_window",
                serde_json::json!({
                    "stream_id": stream_id,
                    "direction": "response",
                    "consumed_bytes": 2,
                    "consumed_items": 2,
                }),
            ),
            &writer,
            "tunnel:connect",
            limits,
            &mut runtime,
        )
        .await
        .unwrap();

    assert!(!runtime.active_deliveries.contains_key(&stream_id));
    let abort = protocol_v3_control(control_rx.recv().await.unwrap());
    assert_eq!(abort.event, "tunnel_stream_abort");
    assert_eq!(abort.payload["stream_id"], stream_id.to_string());
    assert_eq!(abort.payload["direction"], "response");
    assert_eq!(abort.payload["code"], "response_window_overflow");
    assert_eq!(abort.payload["outcome"], "outcome_unknown");
    assert!(
        runtime
            .workers
            .join_next()
            .await
            .unwrap()
            .unwrap_err()
            .is_cancelled()
    );
}

#[tokio::test]
async fn protocol_v3_ninth_start_is_explicitly_rejected_before_local_forwarding() {
    let address = "127.0.0.1:9".parse().unwrap();
    let (forwarder, _event_rx) = forwarder_for_local_address(address).await;
    let (writer, mut control_rx, _response_rx) = test_priority_writer();
    let mut limits = test_streaming_limits();
    limits.max_concurrent_streams = 128;
    limits.max_active_local_forwards = LOCAL_WORK_MAX_COUNT;
    let mut runtime = V3RelayRuntime::new("tunnel:connect".to_string(), limits).unwrap();
    let occupied = (0..LOCAL_WORK_MAX_COUNT)
        .map(|_| {
            runtime
                .work_count
                .clone()
                .try_acquire_owned()
                .expect("one of the eight local forwarding slots")
        })
        .collect::<Vec<_>>();
    let ninth_stream_id = Uuid::new_v4();

    forwarder
        .handle_v3_control(
            v3_control_message(
                "tunnel:connect",
                "tunnel_request_start",
                serde_json::json!({
                    "stream_id": ninth_stream_id,
                    "deadline_unix_ms": 1,
                    "timeout_ms": 30_000,
                    "method": "POST",
                    "path": "/overload",
                    "query_string": "",
                    "headers": [],
                    "content_length": 0,
                    "replay": false,
                }),
            ),
            &writer,
            "tunnel:connect",
            limits,
            &mut runtime,
        )
        .await
        .unwrap();

    assert!(!runtime.active_deliveries.contains_key(&ninth_stream_id));
    assert!(!runtime.requests.contains(ninth_stream_id));
    assert_eq!(runtime.work_count.available_permits(), 0);

    let abort = protocol_v3_control(
        tokio::time::timeout(Duration::from_secs(1), control_rx.recv())
            .await
            .expect("ninth start was silently dropped")
            .unwrap(),
    );
    assert_eq!(abort.event, "tunnel_stream_abort");
    assert_eq!(abort.payload["stream_id"], ninth_stream_id.to_string());
    assert_eq!(abort.payload["direction"], "request");
    assert_eq!(abort.payload["code"], "relay_overloaded_before_forward");
    assert_eq!(abort.payload["outcome"], "known_not_executed");
    assert!(control_rx.try_recv().is_err());

    drop(occupied);
    assert_eq!(runtime.work_count.available_permits(), LOCAL_WORK_MAX_COUNT);
}

#[tokio::test]
async fn test_saturated_presentation_emits_stream_gap_after_recovery() {
    let (_token_tx, token_rx) = watch::channel("token".to_string());
    let (event_tx, mut event_rx) = mpsc::channel(2);
    let target = TargetPolicy::resolve("http://127.0.0.1:3000", false, false)
        .await
        .unwrap();
    let forwarder = TunnelForwarder::new(
        token_rx,
        "127.0.0.1".to_string(),
        3000,
        target,
        None,
        None,
        event_tx,
        false,
    );

    forwarder.emit_presentation(TunnelEvent::Connecting);
    forwarder.emit_presentation(TunnelEvent::Connected);
    forwarder.emit_presentation(TunnelEvent::Disconnected);

    assert!(matches!(event_rx.try_recv(), Ok(TunnelEvent::Connecting)));
    assert!(matches!(event_rx.try_recv(), Ok(TunnelEvent::Connected)));

    forwarder.emit_presentation(TunnelEvent::Disconnected);

    assert!(matches!(
        event_rx.try_recv(),
        Ok(TunnelEvent::StreamGap { dropped_events: 1 })
    ));
    assert!(matches!(event_rx.try_recv(), Ok(TunnelEvent::Disconnected)));
}

#[tokio::test]
async fn test_closed_presentation_consumer_does_not_block_delivery_path() {
    let (_token_tx, token_rx) = watch::channel("token".to_string());
    let (event_tx, event_rx) = mpsc::channel(1);
    drop(event_rx);
    let target = TargetPolicy::resolve("http://127.0.0.1:3000", false, false)
        .await
        .unwrap();
    let forwarder = TunnelForwarder::new(
        token_rx,
        "127.0.0.1".to_string(),
        3000,
        target,
        None,
        None,
        event_tx,
        false,
    );

    forwarder.emit_presentation(TunnelEvent::Disconnected);

    assert_eq!(forwarder.presentation_drops.load(Ordering::Acquire), 1);
}

#[tokio::test]
async fn test_resume_token_uses_configured_api_base_url() {
    let mut server = mockito::Server::new_async().await;
    let reconnect = server
        .mock("POST", "/api/v1/tunnel/sessions/session-123/reconnect")
        .match_header("authorization", "Bearer test-token")
        .match_header("x-organization-id", "org-123")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"data":{"session":{"id":"session-123","organization_id":"org-123","status":"active","fence":1,"created_at":"2026-07-14T20:00:00Z","updated_at":"2026-07-14T20:00:01Z","route":null},"topic":"tunnel:connect","resume_token":"short-lived-resume-token","resume_token_expires_in":300,"cursor":"opaque","ownership":"lost"}}"#,
        )
        .create_async()
        .await;
    let (_token_tx, token_rx) = watch::channel("test-token".to_string());
    let (event_tx, _event_rx) = mpsc::channel(1);
    let target = TargetPolicy::resolve("http://127.0.0.1:3000", false, false)
        .await
        .unwrap();
    let mut forwarder = TunnelForwarder::new(
        token_rx,
        "127.0.0.1".to_string(),
        3000,
        target,
        Some("org-123".to_string()),
        None,
        event_tx,
        false,
    );
    forwarder.base_url = server.url();
    *forwarder.resume_session_id.lock().unwrap() = Some("session-123".to_string());

    assert_eq!(
        forwarder.resume_token("test-token").await.unwrap(),
        Some("short-lived-resume-token".to_string())
    );
    reconnect.assert_async().await;
}

#[tokio::test]
async fn test_interrupted_delivery_releases_local_work_capacity() {
    let budget = LocalWorkBudget::new();
    let permit = budget.try_acquire(10 * 1024 * 1024).unwrap();
    let mut workers = JoinSet::new();
    let abort = workers.spawn(async move {
        let _permit = permit;
        std::future::pending::<()>().await;
    });

    abort.abort();
    assert!(
        workers
            .join_next()
            .await
            .unwrap()
            .unwrap_err()
            .is_cancelled()
    );
    assert_eq!(budget.available_count(), LOCAL_WORK_MAX_COUNT);
    assert_eq!(budget.available_bytes(), LOCAL_WORK_MAX_BYTES);
}

#[tokio::test]
async fn test_priority_writer_sends_control_before_queued_response() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut websocket = tokio_tungstenite::accept_async(socket).await.unwrap();
        let mut events = Vec::new();
        for _ in 0..2 {
            let message = websocket.next().await.unwrap().unwrap();
            let Message::Text(text) = message else {
                panic!("expected a text channel message");
            };
            let message: ChannelMessage = serde_json::from_str(&text).unwrap();
            events.push(message.event);
        }
        events
    });

    let (websocket, _) = connect_async(format!("ws://{address}")).await.unwrap();
    let (write, _) = websocket.split();
    let (control_tx, control_rx) = mpsc::channel(OUTBOUND_CONTROL_MAX_COUNT);
    let (response_tx, response_rx) = mpsc::channel(OUTBOUND_RESPONSE_MAX_COUNT);
    let writer = PriorityWriter {
        control_tx,
        response_tx,
        bytes: Arc::new(Semaphore::new(OUTBOUND_MAX_BYTES)),
    };

    writer
        .response(ChannelMessage {
            topic: "cli:tunnel:test".to_string(),
            event: "response".to_string(),
            payload: serde_json::json!({}),
            reference: None,
        })
        .await
        .unwrap();
    writer
        .control(ChannelMessage {
            topic: "cli:tunnel:test".to_string(),
            event: "control".to_string(),
            payload: serde_json::json!({}),
            reference: None,
        })
        .await
        .unwrap();
    assert!(writer.available_bytes() < OUTBOUND_MAX_BYTES);

    let writer_task = tokio::spawn(run_priority_writer(write, control_rx, response_rx));
    let events = server.await.unwrap();
    assert_eq!(events, vec!["control", "response"]);

    drop(writer);
    writer_task.await.unwrap().unwrap();
}

#[tokio::test]
async fn protocol_v3_reaps_completed_workers_before_sustained_ingress() {
    let limits = test_streaming_limits();
    let capacity = limits.max_active_local_forwards;
    let mut runtime = V3RelayRuntime::new("tunnel:connect".to_string(), limits).unwrap();
    let (writer, _control_rx, _response_rx) = test_priority_writer();
    let (ingress_tx, mut ingress_rx) = mpsc::channel(1);
    ingress_tx.send(0usize).await.unwrap();
    let mut peak_active_deliveries = 0;

    for wave in 0..256usize {
        for _worker in 0..capacity {
            let stream_id = Uuid::new_v4();
            let work_permit = runtime
                .work_count
                .clone()
                .try_acquire_owned()
                .expect("bounded local-forward capacity");
            let (response_tx, _response_rx) = mpsc::channel(1);
            let phase = Arc::new(AtomicU8::new(DELIVERY_TERMINAL));
            let abort = runtime.workers.spawn(async move {
                let _work_permit = work_permit;
                tokio::task::yield_now().await;
                (stream_id, Ok(()))
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

        peak_active_deliveries = peak_active_deliveries.max(runtime.active_deliveries.len());
        assert_eq!(runtime.active_deliveries.len(), capacity);
        assert_eq!(
            ingress_rx.len(),
            1,
            "ingress must remain continuously ready"
        );

        while !runtime.workers.is_empty() {
            if reap_ready_v3_workers(&mut runtime, &writer, "tunnel:connect")
                .await
                .unwrap()
                == 0
            {
                tokio::task::yield_now().await;
            }
        }

        assert!(runtime.active_deliveries.is_empty());
        assert_eq!(ingress_rx.recv().await, Some(wave));
        ingress_tx.send(wave + 1).await.unwrap();
    }

    assert_eq!(peak_active_deliveries, capacity);
    assert_eq!(runtime.work_count.available_permits(), capacity);
}

#[tokio::test]
async fn protocol_v2_reaps_completed_workers_before_sustained_ingress() {
    let mut runtime = RelayRuntime::new();
    let (ingress_tx, mut ingress_rx) = mpsc::channel(1);
    ingress_tx.send(0usize).await.unwrap();
    let mut peak_active_deliveries = 0;

    for wave in 0..256usize {
        for worker in 0..LOCAL_WORK_MAX_COUNT {
            let request_id = format!("{wave}-{worker}");
            let permit = runtime
                .work_budget
                .try_acquire(1)
                .expect("bounded local-forward capacity");
            let task_request_id = request_id.clone();
            let phase = Arc::new(AtomicU8::new(DELIVERY_TERMINAL));
            let abort = runtime.workers.spawn(async move {
                let _permit = permit;
                tokio::task::yield_now().await;
                (task_request_id, Ok(()))
            });

            runtime
                .active_deliveries
                .insert(request_id, ActiveDelivery { abort, phase });
        }

        peak_active_deliveries = peak_active_deliveries.max(runtime.active_deliveries.len());
        assert_eq!(runtime.active_deliveries.len(), LOCAL_WORK_MAX_COUNT);
        assert_eq!(
            ingress_rx.len(),
            1,
            "ingress must remain continuously ready"
        );

        while !runtime.workers.is_empty() {
            if reap_ready_tunnel_workers(&mut runtime) == 0 {
                tokio::task::yield_now().await;
            }
        }

        assert!(runtime.active_deliveries.is_empty());
        assert_eq!(ingress_rx.recv().await, Some(wave));
        ingress_tx.send(wave + 1).await.unwrap();
    }

    assert_eq!(peak_active_deliveries, LOCAL_WORK_MAX_COUNT);
    assert_eq!(runtime.work_budget.available_count(), LOCAL_WORK_MAX_COUNT);
    assert_eq!(runtime.work_budget.available_bytes(), LOCAL_WORK_MAX_BYTES);
}

#[tokio::test]
async fn protocol_v2_stream_error_aborts_active_delivery_and_releases_inbound_reservation() {
    let address = "127.0.0.1:9".parse().unwrap();
    let (forwarder, mut event_rx) = forwarder_for_local_address(address).await;
    let (writer, _control_rx, _response_rx) = test_priority_writer();
    let mut runtime = RelayRuntime::new();
    let request_id = "stream-error-delivery".to_string();
    let assembler = TunnelStreamAssembler::from_start(
        &serde_json::json!({
            "version": TUNNEL_PROTOCOL_VERSION,
            "direction": "request",
            "stream_id": request_id,
            "deadline_unix_ms": unix_time_ms() + 30_000,
            "total_bytes": 17,
            "frame_count": 1,
        }),
        "request",
    )
    .unwrap();
    runtime.inbound_reserved_bytes = assembler.total_bytes;
    runtime
        .inbound_streams
        .insert(request_id.clone(), assembler);

    let task_request_id = request_id.clone();
    let phase = Arc::new(AtomicU8::new(DELIVERY_STARTED));
    let abort = runtime.workers.spawn(async move {
        std::future::pending::<()>().await;
        (task_request_id, Ok(()))
    });
    runtime
        .active_deliveries
        .insert(request_id.clone(), ActiveDelivery { abort, phase });

    let message = serde_json::to_string(&ChannelMessage {
        topic: "tunnel:connect".to_string(),
        event: "tunnel_stream_error".to_string(),
        payload: serde_json::json!({
            "stream_id": request_id,
            "code": "deadline_exceeded",
        }),
        reference: None,
    })
    .unwrap();

    forwarder
        .handle_tunnel_message(
            &message,
            &writer,
            "tunnel:connect",
            TunnelLimits::from_join_response(&serde_json::json!({})),
            &mut runtime,
        )
        .await
        .unwrap();

    assert!(!runtime.active_deliveries.contains_key(&request_id));
    assert!(!runtime.inbound_streams.contains_key(&request_id));
    assert_eq!(runtime.inbound_reserved_bytes, 0);
    assert!(matches!(
        event_rx.recv().await,
        Some(TunnelEvent::RequestFailed {
            request_id: failed_request_id,
            error,
        }) if failed_request_id == request_id && error == "deadline_exceeded"
    ));
    assert!(
        runtime
            .workers
            .join_next()
            .await
            .unwrap()
            .unwrap_err()
            .is_cancelled()
    );
}

fn protocol_v3_control(item: OutboundItem) -> tunnel_v3::ControlMessage {
    let Message::Text(text) = item.message else {
        panic!("expected protocol v3 text control");
    };
    tunnel_v3::decode_server_control(&text).unwrap()
}

fn protocol_v3_client_chunk(item: OutboundItem, max_chunk_bytes: usize) -> (Uuid, Vec<u8>) {
    let Message::Binary(encoded) = item.message else {
        panic!("expected protocol v3 binary response chunk");
    };
    assert!(encoded.len() >= 5);
    assert_eq!(encoded[0], 0);
    let metadata_bytes = encoded[1..5]
        .iter()
        .map(|length| usize::from(*length))
        .sum::<usize>();
    let payload_offset = 5 + metadata_bytes;
    let chunk = tunnel_v3::decode_chunk(&encoded[payload_offset..], max_chunk_bytes).unwrap();
    (chunk.stream_id, chunk.data.to_vec())
}

fn test_streaming_limits() -> tunnel_v3::StreamingLimits {
    tunnel_v3::StreamingLimits {
        max_request_body_bytes: 256 * 1024,
        max_response_body_bytes: 256 * 1024,
        max_start_metadata_bytes: 55_000,
        max_control_payload_bytes: 63_000,
        max_websocket_frame_bytes: TUNNEL_MAX_FRAME_BYTES,
        max_chunk_bytes: 16 * 1024,
        stream_window_bytes: 32 * 1024,
        stream_window_items: 2,
        connection_window_bytes: 64 * 1024,
        connection_window_items: 4,
        websocket_admission_bytes: 128 * 1024,
        websocket_admission_items: 32,
        service_ingress_admission_bytes: 64 * 1024,
        service_ingress_admission_items: 4,
        max_concurrent_streams: 4,
        max_active_local_forwards: 4,
        request_timeout_ms: 80_000,
    }
}

#[tokio::test]
async fn protocol_v3_early_local_response_drains_large_request_and_relays_response() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let local_server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = Vec::new();
        let mut buffer = [0_u8; 1024];

        while !request.windows(4).any(|window| window == b"\r\n\r\n") {
            let read = socket.read(&mut buffer).await.unwrap();
            assert!(read > 0);
            request.extend_from_slice(&buffer[..read]);
        }

        socket
            .write_all(
                b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 5\r\nConnection: close\r\n\r\nearly",
            )
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(500)).await;
        socket.shutdown().await.unwrap();
    });

    let (forwarder, mut event_rx) = forwarder_for_local_address(address).await;
    let (writer, mut control_rx, mut outbound_response_rx) = test_priority_writer();
    let limits = test_streaming_limits();
    let stream_id = Uuid::new_v4();
    let request_bytes = limits.stream_window_bytes * 4;
    let request_connection = tunnel_v3::ConnectionBudget::new(
        limits.connection_window_bytes,
        limits.connection_window_items,
    )
    .unwrap();
    let (mut request_stream, request_body) = tunnel_v3::RequestStreamReceiver::new(
        stream_id,
        Some(request_bytes as u64),
        limits,
        request_connection,
    )
    .unwrap();
    let incoming = tunnel_v3::IncomingRequest {
        start: tunnel_v3::RequestStart {
            stream_id,
            deadline_unix_ms: unix_time_ms() + 10_000,
            timeout_ms: 10_000,
            method: "POST".to_string(),
            path: "/early".to_string(),
            query_string: String::new(),
            headers: vec![(
                "content-type".to_string(),
                "application/octet-stream".to_string(),
            )],
            content_length: Some(request_bytes as u64),
            replay: false,
        },
        body: request_body,
    };
    let phase = Arc::new(AtomicU8::new(DELIVERY_QUEUED));
    let (response_window_tx, response_window_rx) = mpsc::channel(limits.stream_window_items + 1);
    let work_permit = Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap();
    let forward_task = tokio::spawn({
        let forwarder = forwarder.clone();
        let writer = writer.clone();
        let phase = phase.clone();
        async move {
            forwarder
                .forward_v3_request(
                    incoming,
                    phase,
                    writer,
                    "tunnel:connect".to_string(),
                    limits,
                    tunnel_v3::ConnectionSendWindow::new(
                        limits.connection_window_bytes,
                        limits.connection_window_items,
                    ),
                    response_window_rx,
                    work_permit,
                )
                .await
        }
    });

    let request_chunk = vec![0xa5; limits.max_chunk_bytes];
    let mut request_hasher = Sha256::new();
    let mut offset = 0usize;
    let mut sequence = 0u32;

    while offset < request_bytes {
        let bytes = (request_bytes - offset).min(request_chunk.len());
        let encoded = tunnel_v3::encode_chunk(
            stream_id,
            sequence,
            offset as u64,
            &request_chunk[..bytes],
            limits.max_chunk_bytes,
        )
        .unwrap();
        request_stream.accept_frame(&encoded).unwrap();
        request_hasher.update(&request_chunk[..bytes]);
        offset += bytes;
        sequence += 1;

        let window = protocol_v3_control(
            tokio::time::timeout(Duration::from_secs(2), control_rx.recv())
                .await
                .expect("request body did not keep draining after the early response")
                .unwrap(),
        );
        assert_eq!(window.event, "tunnel_stream_window");
        assert_eq!(window.payload["direction"], "request");
        assert_eq!(window.payload["consumed_bytes"], offset as u64);
        assert_eq!(window.payload["consumed_items"], u64::from(sequence));
    }

    let checksum = request_hasher
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    request_stream
        .finish(request_bytes as u64, &checksum)
        .unwrap();

    let mut response_bytes = Vec::new();
    let mut response_items = 0u64;
    let mut saw_response_start = false;
    let mut saw_response_end = false;

    while !saw_response_end {
        let item = tokio::time::timeout(Duration::from_secs(2), outbound_response_rx.recv())
            .await
            .expect("early local response was not relayed")
            .unwrap();

        match &item.message {
            Message::Text(_) => {
                let control = protocol_v3_control(item);
                match control.event.as_str() {
                    "tunnel_response_start" => {
                        saw_response_start = true;
                        assert_eq!(control.payload["status"], 413);
                        assert_eq!(control.payload["content_length"], 5);
                    }
                    "tunnel_response_end" => {
                        saw_response_end = true;
                        assert_eq!(control.payload["total_bytes"], 5);
                    }
                    event => panic!("unexpected protocol v3 response control: {event}"),
                }
            }
            Message::Binary(_) => {
                let (chunk_stream_id, data) =
                    protocol_v3_client_chunk(item, limits.max_chunk_bytes);
                assert_eq!(chunk_stream_id, stream_id);
                response_bytes.extend_from_slice(&data);
                response_items += 1;
                response_window_tx
                    .send(tunnel_v3::WindowUpdate {
                        consumed_bytes: response_bytes.len() as u64,
                        consumed_items: response_items,
                    })
                    .await
                    .unwrap();
            }
            message => panic!("unexpected protocol v3 response message: {message:?}"),
        }
    }

    assert!(saw_response_start);
    assert_eq!(response_bytes, b"early");
    tokio::time::timeout(Duration::from_secs(2), forward_task)
        .await
        .expect("early response forwarding did not complete")
        .unwrap()
        .unwrap();
    assert_eq!(phase.load(Ordering::Acquire), DELIVERY_TERMINAL);
    assert!(matches!(
        event_rx.recv().await,
        Some(TunnelEvent::RequestForwarded { status: 413, .. })
    ));
    local_server.await.unwrap();
}
