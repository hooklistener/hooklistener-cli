//! Local delivery outcomes: deadlines, failures, and concurrency.

use super::*;

fn channel_message(item: OutboundItem) -> ChannelMessage {
    let Message::Text(text) = item.message else {
        panic!("expected text channel message");
    };
    serde_json::from_str(&text).unwrap()
}

fn test_delivery(deadline_unix_ms: u64) -> LocalDelivery {
    LocalDelivery {
        request_id: "failure-contract".to_string(),
        method: "GET".to_string(),
        path: "/".to_string(),
        query_string: String::new(),
        headers: Vec::new(),
        body: Vec::new(),
        deadline_unix_ms,
        replay: false,
    }
}

fn assert_delivery_failure(message: ChannelMessage, code: &str, outcome: &str) {
    assert_eq!(message.event, "tunnel_error");
    assert_eq!(message.payload["code"], code);
    assert_eq!(message.payload["outcome"], outcome);
}

#[tokio::test]
async fn test_deadline_before_forward_reports_known_not_executed() {
    let address = "127.0.0.1:9".parse().unwrap();
    let (forwarder, mut event_rx) = forwarder_for_local_address(address).await;
    let (writer, mut control_rx, _response_rx) = test_priority_writer();

    tokio::time::timeout(
        Duration::from_secs(10),
        forwarder.forward_tunnel_request(
            test_delivery(unix_time_ms()),
            Arc::new(AtomicU8::new(DELIVERY_QUEUED)),
            writer,
            "cli:tunnel:test".to_string(),
            TunnelLimits::from_join_response(&serde_json::json!({})),
            ResponseBufferBudget::new(),
        ),
    )
    .await
    .expect("forwarding an expired delivery timed out")
    .unwrap();

    assert_delivery_failure(
        channel_message(
            tokio::time::timeout(Duration::from_secs(5), control_rx.recv())
                .await
                .expect("timed out waiting for deadline failure")
                .unwrap(),
        ),
        "deadline_exceeded_before_forward",
        "known_not_executed",
    );
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
            .await
            .expect("timed out waiting for request failure event"),
        Some(TunnelEvent::RequestFailed { request_id, .. }) if request_id == "failure-contract"
    ));
}

#[tokio::test]
async fn test_local_response_limit_and_read_failures_report_unknown_outcome() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let cases: &[(&[u8], TunnelLimits, &str)] = &[
        (
            b"HTTP/1.1 200 OK\r\nX-One: 1\r\nX-Two: 2\r\nContent-Length: 0\r\n\r\n",
            TunnelLimits {
                max_request_body_bytes: 1024,
                max_response_body_bytes: 1024,
                max_response_header_bytes: 1024,
                max_response_header_items: 1,
                ordered_response_headers: true,
            },
            "response_headers_too_large",
        ),
        (
            b"HTTP/1.1 200 OK\r\nX-Large: 1234567890\r\nContent-Length: 0\r\n\r\n",
            TunnelLimits {
                max_request_body_bytes: 1024,
                max_response_body_bytes: 1024,
                max_response_header_bytes: 8,
                max_response_header_items: TUNNEL_MAX_RESPONSE_HEADERS,
                ordered_response_headers: true,
            },
            "response_headers_too_large",
        ),
        (
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n3\r\ndef\r\n0\r\n\r\n",
            TunnelLimits {
                max_request_body_bytes: 1024,
                max_response_body_bytes: 4,
                max_response_header_bytes: 1024,
                max_response_header_items: TUNNEL_MAX_RESPONSE_HEADERS,
                ordered_response_headers: true,
            },
            "response_body_too_large",
        ),
        (
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort",
            TunnelLimits {
                max_request_body_bytes: 1024,
                max_response_body_bytes: 1024,
                max_response_header_bytes: 1024,
                max_response_header_items: TUNNEL_MAX_RESPONSE_HEADERS,
                ordered_response_headers: true,
            },
            "response_read_failed",
        ),
    ];

    for (response, limits, expected_code) in cases {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let response = response.to_vec();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _bytes_read = socket.read(&mut request).await.unwrap();
            socket.write_all(&response).await.unwrap();
        });
        let (forwarder, _event_rx) = forwarder_for_local_address(address).await;
        let (writer, mut control_rx, _response_rx) = test_priority_writer();

        tokio::time::timeout(
            Duration::from_secs(10),
            forwarder.forward_tunnel_request(
                test_delivery(unix_time_ms() + 30_000),
                Arc::new(AtomicU8::new(DELIVERY_QUEUED)),
                writer,
                "cli:tunnel:test".to_string(),
                *limits,
                ResponseBufferBudget::new(),
            ),
        )
        .await
        .expect("forwarding a malformed local response timed out")
        .unwrap();

        assert_delivery_failure(
            channel_message(
                tokio::time::timeout(Duration::from_secs(5), control_rx.recv())
                    .await
                    .expect("timed out waiting for local response failure")
                    .unwrap(),
            ),
            expected_code,
            "outcome_unknown",
        );
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("local response server timed out")
            .unwrap();
    }
}

#[tokio::test]
async fn test_response_buffer_overload_reports_unknown_outcome() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _bytes_read = socket.read(&mut request).await.unwrap();
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .await
            .unwrap();
    });
    let budget = ResponseBufferBudget::new();
    let _reservation = budget.try_reserve(Some(RESPONSE_BUFFER_MAX_BYTES)).unwrap();
    let (forwarder, _event_rx) = forwarder_for_local_address(address).await;
    let (writer, mut control_rx, _response_rx) = test_priority_writer();

    tokio::time::timeout(
        Duration::from_secs(10),
        forwarder.forward_tunnel_request(
            test_delivery(unix_time_ms() + 30_000),
            Arc::new(AtomicU8::new(DELIVERY_QUEUED)),
            writer,
            "cli:tunnel:test".to_string(),
            TunnelLimits::from_join_response(&serde_json::json!({})),
            budget,
        ),
    )
    .await
    .expect("forwarding an overloaded response timed out")
    .unwrap();

    assert_delivery_failure(
        channel_message(
            tokio::time::timeout(Duration::from_secs(5), control_rx.recv())
                .await
                .expect("timed out waiting for response overload failure")
                .unwrap(),
        ),
        "relay_response_overloaded",
        "outcome_unknown",
    );
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("local response server timed out")
        .unwrap();
}

#[tokio::test]
async fn test_response_writer_failure_after_delivery_started_reports_unknown_outcome() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _bytes_read = socket.read(&mut request).await.unwrap();
        socket
            .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
            .await
            .unwrap();
    });
    let (forwarder, _event_rx) = forwarder_for_local_address(address).await;
    let (writer, mut control_rx, response_rx) = test_priority_writer();
    drop(response_rx);

    tokio::time::timeout(
        Duration::from_secs(10),
        forwarder.forward_tunnel_request(
            test_delivery(unix_time_ms() + 30_000),
            Arc::new(AtomicU8::new(DELIVERY_QUEUED)),
            writer,
            "cli:tunnel:test".to_string(),
            TunnelLimits::from_join_response(&serde_json::json!({})),
            ResponseBufferBudget::new(),
        ),
    )
    .await
    .expect("forwarding with a closed response writer timed out")
    .unwrap();

    assert_delivery_failure(
        channel_message(
            tokio::time::timeout(Duration::from_secs(5), control_rx.recv())
                .await
                .expect("timed out waiting for response delivery failure")
                .unwrap(),
        ),
        "response_delivery_failed",
        "outcome_unknown",
    );
    tokio::time::timeout(Duration::from_secs(5), server)
        .await
        .expect("local response server timed out")
        .unwrap();
}

#[tokio::test]
async fn test_slow_local_handler_does_not_delay_fast_delivery() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};
    use tokio::sync::oneshot;

    let local_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let local_address = local_listener.local_addr().unwrap();
    let (slow_started_tx, slow_started_rx) = oneshot::channel();
    let release_slow = Arc::new(tokio::sync::Notify::new());
    let server_release_slow = Arc::clone(&release_slow);
    let local_server = tokio::spawn(async move {
        let mut slow_started_tx = Some(slow_started_tx);
        let mut handlers = JoinSet::new();
        for _ in 0..2 {
            let (mut socket, _) = local_listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let read = socket.read(&mut request).await.unwrap();
            let slow = String::from_utf8_lossy(&request[..read]).contains("GET /slow ");
            if slow && let Some(sender) = slow_started_tx.take() {
                let _ = sender.send(());
            }
            let release_slow = Arc::clone(&server_release_slow);
            handlers.spawn(async move {
                if slow {
                    release_slow.notified().await;
                }
                socket
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                    .await
                    .unwrap();
            });
        }
        while handlers.join_next().await.is_some() {}
    });

    let websocket_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let websocket_address = websocket_listener.local_addr().unwrap();
    let websocket_server = tokio::spawn(async move {
        let (socket, _) = websocket_listener.accept().await.unwrap();
        let mut websocket = tokio_tungstenite::accept_async(socket).await.unwrap();
        while websocket.next().await.is_some() {}
    });
    let (websocket, _) = connect_async(format!("ws://{websocket_address}"))
        .await
        .unwrap();
    let (write, read) = websocket.split();
    let (writer, writer_task) = PriorityWriter::spawn(write);

    let (_token_tx, token_rx) = watch::channel("token".to_string());
    let (event_tx, mut event_rx) = mpsc::channel(PRESENTATION_QUEUE_CAPACITY);
    let target = TargetPolicy::resolve(
        &format!("http://127.0.0.1:{}", local_address.port()),
        false,
        false,
    )
    .await
    .unwrap();
    let forwarder = TunnelForwarder::new(
        token_rx,
        "127.0.0.1".to_string(),
        local_address.port(),
        target,
        None,
        None,
        event_tx,
        false,
    );
    let limits = TunnelLimits {
        max_request_body_bytes: 1024,
        max_response_body_bytes: 1024,
        max_response_header_bytes: 1024,
        max_response_header_items: TUNNEL_MAX_RESPONSE_HEADERS,
        ordered_response_headers: true,
    };
    let response_budget = ResponseBufferBudget::new();
    let delivery = |request_id: &str, path: &str| LocalDelivery {
        request_id: request_id.to_string(),
        method: "GET".to_string(),
        path: path.to_string(),
        query_string: String::new(),
        headers: Vec::new(),
        body: Vec::new(),
        deadline_unix_ms: unix_time_ms() + 10_000,
        replay: false,
    };

    let slow = tokio::spawn({
        let forwarder = forwarder.clone();
        let writer = writer.clone();
        let response_budget = response_budget.clone();
        async move {
            forwarder
                .forward_tunnel_request(
                    delivery("slow", "/slow"),
                    Arc::new(AtomicU8::new(DELIVERY_QUEUED)),
                    writer,
                    "cli:tunnel:test".to_string(),
                    limits,
                    response_budget,
                )
                .await
        }
    });
    slow_started_rx.await.unwrap();

    let fast = tokio::spawn({
        let forwarder = forwarder.clone();
        let writer = writer.clone();
        let response_budget = response_budget.clone();
        async move {
            forwarder
                .forward_tunnel_request(
                    delivery("fast", "/fast"),
                    Arc::new(AtomicU8::new(DELIVERY_QUEUED)),
                    writer,
                    "cli:tunnel:test".to_string(),
                    limits,
                    response_budget,
                )
                .await
        }
    });

    let first = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
        .await
        .unwrap()
        .unwrap();
    assert!(matches!(
        first,
        TunnelEvent::RequestForwarded { request_id, .. } if request_id == "fast"
    ));
    release_slow.notify_one();

    fast.await.unwrap().unwrap();
    slow.await.unwrap().unwrap();
    local_server.await.unwrap();
    drop(writer);
    drop(read);
    writer_task.await.unwrap().unwrap();
    websocket_server.abort();
}
