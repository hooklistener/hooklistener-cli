use super::forwarder::*;
use super::framing::*;
use super::http::*;
use super::limits::*;
use super::preview::*;
use super::relay::*;
use super::v3_relay::*;
use super::writer::*;
use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::StreamExt;
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
use std::time::Duration;
use tokio::sync::{Semaphore, mpsc, watch};
use tokio::task::JoinSet;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use uuid::Uuid;

use crate::target_policy::TargetPolicy;
use crate::tunnel_v3;
use flate2::{Compression, write::DeflateEncoder, write::GzEncoder, write::ZlibEncoder};
use sha2::{Digest, Sha256};
use std::io::Write;

fn v2_join_contract() -> serde_json::Value {
    serde_json::json!({
        "limits": {
            "max_stream_bytes": TUNNEL_MAX_STREAM_BYTES,
        },
        "framing": serde_json::from_str::<serde_json::Value>(include_str!(
            "../../fixtures/tunnel_framing_v2.json"
        ))
        .unwrap(),
    })
}

fn preview_headers(values: &[(&str, &str)]) -> HashMap<String, String> {
    values
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}

fn compressed_with<W>(mut encoder: W, body: &[u8]) -> Vec<u8>
where
    W: Write + FinishEncoder,
{
    encoder.write_all(body).unwrap();
    encoder.finish_encoder()
}

trait FinishEncoder {
    fn finish_encoder(self) -> Vec<u8>;
}

impl FinishEncoder for GzEncoder<Vec<u8>> {
    fn finish_encoder(self) -> Vec<u8> {
        self.finish().unwrap()
    }
}

impl FinishEncoder for ZlibEncoder<Vec<u8>> {
    fn finish_encoder(self) -> Vec<u8> {
        self.finish().unwrap()
    }
}

impl FinishEncoder for DeflateEncoder<Vec<u8>> {
    fn finish_encoder(self) -> Vec<u8> {
        self.finish().unwrap()
    }
}

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

#[test]
fn test_join_payloads_select_explicit_activation_modes() {
    assert_eq!(listen_join_payload()["mode"], CAPTURE_FORWARD_MODE);

    let tunnel_payload = tunnel_join_payload(3000, Some("org-1"), Some("payments"), None);
    assert_eq!(tunnel_payload["mode"], DIRECT_RESPONSE_MODE);
    assert_eq!(tunnel_payload["protocol_version"], TUNNEL_PROTOCOL_VERSION);
    assert_eq!(tunnel_payload["local_port"], 3000);
    assert_eq!(tunnel_payload["organization_id"], "org-1");
    assert_eq!(tunnel_payload["slug"], "payments");
}

#[test]
fn protocol_v3_join_uses_phoenix_v2_controls_and_explicit_version() {
    let payload = tunnel_join_payload_for_version(
        3000,
        Some("org-1"),
        Some("payments"),
        None,
        tunnel_v3::VERSION.into(),
    );
    assert_eq!(payload["protocol_version"], 3);

    let encoded = tunnel_v3::encode_client_control(
        &tunnel_v3::ControlMessage {
            join_ref: None,
            reference: Some("1".to_string()),
            topic: "tunnel:connect".to_string(),
            event: "phx_join".to_string(),
            payload,
        },
        TUNNEL_MAX_FRAME_BYTES,
    )
    .unwrap();
    assert!(encoded.starts_with("[null,\"1\",\"tunnel:connect\",\"phx_join\","));

    let reply = decode_channel_message(
        r#"[null,"1","tunnel:connect","phx_reply",{"status":"ok"}]"#,
        true,
    )
    .unwrap();
    assert_eq!(reply.reference.as_deref(), Some("1"));
    assert_eq!(reply.event, "phx_reply");
}

#[test]
fn tunnel_protocol_selection_prefers_v3_and_defaults_old_services_to_v2() {
    assert_eq!(selected_tunnel_protocol(&[3, 2]).unwrap(), 3);
    assert_eq!(selected_tunnel_protocol(&[2, 3]).unwrap(), 3);
    assert_eq!(selected_tunnel_protocol(&[2]).unwrap(), 2);

    for unsupported in [&[][..], &[4][..], &[4, 1][..]] {
        let error = selected_tunnel_protocol(unsupported)
            .unwrap_err()
            .to_string();
        assert!(error.contains("no common tunnel protocol"));
        assert!(is_fatal_error(&error));
    }
}

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

#[test]
fn test_join_mode_must_be_confirmed_by_server() {
    assert!(
        validate_join_mode(
            &serde_json::json!({"mode": DIRECT_RESPONSE_MODE}),
            DIRECT_RESPONSE_MODE
        )
        .is_ok()
    );

    let missing = validate_join_mode(&serde_json::json!({}), DIRECT_RESPONSE_MODE)
        .unwrap_err()
        .to_string();
    assert!(missing.contains("did not confirm"));

    let incompatible = validate_join_mode(
        &serde_json::json!({"mode": CAPTURE_FORWARD_MODE}),
        DIRECT_RESPONSE_MODE,
    )
    .unwrap_err()
    .to_string();
    assert!(incompatible.contains("incompatible mode"));
}

#[test]
fn test_join_mode_rejections_are_fatal() {
    for response in [
        serde_json::json!({}),
        serde_json::json!({"mode": CAPTURE_FORWARD_MODE}),
    ] {
        let error = validate_join_mode(&response, DIRECT_RESPONSE_MODE)
            .unwrap_err()
            .to_string();

        assert!(is_fatal_error(&error), "expected fatal error: {error}");
    }
}

#[test]
fn test_should_forward_request_header_filters_framing_headers() {
    for header in [
        "host",
        "Host",
        "content-length",
        "Content-Length",
        "transfer-encoding",
        "connection",
        "keep-alive",
        "proxy-connection",
        "te",
        "trailer",
        "upgrade",
    ] {
        assert!(!should_forward_request_header(header));
    }

    assert!(should_forward_request_header("content-type"));
    assert!(should_forward_request_header("authorization"));
    assert!(should_forward_request_header("x-custom-header"));
}

#[test]
fn protocol_v3_205_reset_content_forbids_body_and_nonzero_content_length() {
    assert!(response_body_forbidden("GET", 205));
    assert!(ensure_response_chunk_allowed("GET", 205).is_err());
    assert_eq!(
        relayed_response_content_length("GET", 205, Some(123)),
        Some(0)
    );

    let mut headers = reqwest::header::HeaderMap::new();
    assert!(valid_reset_content_length(&headers));

    headers.insert(
        reqwest::header::CONTENT_LENGTH,
        reqwest::header::HeaderValue::from_static("0"),
    );
    assert!(valid_reset_content_length(&headers));

    headers.insert(
        reqwest::header::CONTENT_LENGTH,
        reqwest::header::HeaderValue::from_static("1"),
    );
    assert!(!valid_reset_content_length(&headers));

    headers.append(
        reqwest::header::CONTENT_LENGTH,
        reqwest::header::HeaderValue::from_static("0"),
    );
    assert!(!valid_reset_content_length(&headers));
}

#[test]
fn test_supported_tunnel_method_accepts_token_valid_extension_methods() {
    assert_eq!(supported_tunnel_method("GET"), Some(reqwest::Method::GET));
    assert_eq!(
        supported_tunnel_method("OPTIONS"),
        Some(reqwest::Method::OPTIONS)
    );
    assert_eq!(
        supported_tunnel_method("PURGE"),
        Some(reqwest::Method::from_bytes(b"PURGE").unwrap())
    );
    assert_eq!(supported_tunnel_method("BAD METHOD"), None);
}

#[test]
fn test_tunnel_websocket_config_accepts_advertised_messages() {
    let config = tunnel_websocket_config();

    assert_eq!(config.max_message_size, Some(TUNNEL_MAX_FRAME_BYTES));
    assert_eq!(config.max_frame_size, Some(TUNNEL_MAX_FRAME_BYTES));
}

#[tokio::test]
async fn test_read_response_body_limited_rejects_large_content_length() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/large")
        .with_body("too large")
        .create_async()
        .await;
    let mut response = reqwest::Client::new()
        .get(format!("{}/large", server.url()))
        .send()
        .await
        .unwrap();

    let error = read_response_body_limited(&mut response, 4)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("max 4 bytes"));
    mock.assert_async().await;
}

#[tokio::test]
async fn test_read_response_body_limited_rejects_large_chunked_body() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/chunked")
        .with_chunked_body(|writer| writer.write_all(b"too large"))
        .create_async()
        .await;
    let mut response = reqwest::Client::new()
        .get(format!("{}/chunked", server.url()))
        .send()
        .await
        .unwrap();

    let error = read_response_body_limited(&mut response, 4)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("max 4 bytes"));
    mock.assert_async().await;
}

#[test]
fn test_framed_stream_round_trips_with_one_bounded_frame_in_flight() {
    let payload = serde_json::json!({
        "request_id": "request-1",
        "headers": [["x-repeat", "first"], ["x-repeat", "second"]],
        "body": URL_SAFE_NO_PAD.encode(vec![0xff; 200_000]),
        "body_encoding": "base64",
    });
    let deadline = unix_time_ms() + 30_000;
    let mut outbound =
        OutboundTunnelStream::new("request-1", "request", deadline, &payload).unwrap();
    let start = outbound.start_message("tunnel:connect");
    let mut inbound = TunnelStreamAssembler::from_start(&start.payload, "request").unwrap();
    let mut decoded = None;
    let mut peak_in_flight = 0;

    while let Some(frame) = outbound.next_frame("tunnel:connect").unwrap() {
        assert!(serde_json::to_vec(&frame).unwrap().len() <= TUNNEL_MAX_FRAME_BYTES);
        peak_in_flight = peak_in_flight.max(1);
        decoded = inbound.append(&frame.payload).unwrap().or(decoded);
    }

    assert_eq!(decoded.unwrap(), payload);
    assert_eq!(peak_in_flight, 1);
    assert!(inbound.data.is_empty());
}

#[test]
fn test_published_v2_contract_matches_cli_framing_constants() {
    let fixture: serde_json::Value =
        serde_json::from_str(include_str!("../../fixtures/tunnel_framing_v2.json")).unwrap();

    assert_eq!(fixture["version"], TUNNEL_PROTOCOL_VERSION);
    assert_eq!(fixture["transport"], "framed");
    assert_eq!(fixture["frame_encoding"], "base64url");
    assert_eq!(fixture["max_frame_bytes"], TUNNEL_MAX_FRAME_BYTES);
    assert_eq!(fixture["max_raw_chunk_bytes"], TUNNEL_MAX_RAW_CHUNK_BYTES);
    assert_eq!(fixture["queue_depth_frames"], 1);
    assert_eq!(fixture["backpressure"], "per_frame_ack");
    assert_eq!(fixture["max_json_depth"], TUNNEL_MAX_JSON_DEPTH);
    assert_eq!(
        fixture["max_json_structural_nodes"],
        TUNNEL_MAX_JSON_STRUCTURAL_NODES
    );
    assert_eq!(
        fixture["max_json_primitive_bytes"],
        TUNNEL_MAX_JSON_PRIMITIVE_BYTES
    );
    assert_eq!(fixture["max_response_headers"], TUNNEL_MAX_RESPONSE_HEADERS);
    validate_framing_contract(&v2_join_contract()).unwrap();

    let headers = ordered_header_pairs(fixture["sample_payload"].get("headers")).unwrap();
    assert_eq!(headers[0], ("x-repeated".into(), "first".into()));
    assert_eq!(headers[1], ("x-repeated".into(), "second".into()));
    assert_eq!(
        URL_SAFE_NO_PAD
            .decode(fixture["sample_payload"]["body"].as_str().unwrap())
            .unwrap(),
        vec![0x00, 0xff, b'b', b'i', b'n']
    );
}

#[test]
fn test_framing_contract_rejects_incompatible_core_fields_and_stream_limit() {
    let incompatible_fields = [
        ("version", serde_json::json!(3)),
        ("transport", serde_json::json!("unframed")),
        ("frame_encoding", serde_json::json!("base64")),
        ("max_frame_bytes", serde_json::json!(65_535)),
        ("max_raw_chunk_bytes", serde_json::json!(46_999)),
        ("queue_depth_frames", serde_json::json!(2)),
        ("backpressure", serde_json::json!("none")),
    ];

    for (field, value) in incompatible_fields {
        let mut response = v2_join_contract();
        response["framing"][field] = value;
        assert!(validate_framing_contract(&response).is_err(), "{field}");
    }

    let mut response = v2_join_contract();
    response["limits"]["max_stream_bytes"] = serde_json::json!(TUNNEL_MAX_STREAM_BYTES + 1);
    assert!(validate_framing_contract(&response).is_err());
}

#[test]
fn test_framing_contract_bridges_legacy_and_bounded_scanner_advertisements() {
    let mut legacy = v2_join_contract();
    for field in [
        "max_json_depth",
        "max_json_structural_nodes",
        "max_json_primitive_bytes",
        "max_response_headers",
    ] {
        legacy["framing"].as_object_mut().unwrap().remove(field);
    }
    assert!(legacy["limits"].get("max_response_header_items").is_none());
    validate_framing_contract(&legacy).unwrap();
    assert_eq!(
        TunnelLimits::from_join_response(&legacy).max_response_header_items,
        TUNNEL_MAX_RESPONSE_HEADERS
    );

    let mut bounded = v2_join_contract();
    bounded["framing"]["max_json_depth"] = serde_json::json!(8);
    bounded["framing"]["max_json_structural_nodes"] = serde_json::json!(32_768);
    bounded["framing"]["max_json_primitive_bytes"] = serde_json::json!(128);
    bounded["framing"]["max_response_headers"] = serde_json::json!(2_048);
    bounded["limits"]["max_response_header_items"] = serde_json::json!(1_024);
    validate_framing_contract(&bounded).unwrap();
    assert_eq!(
        TunnelLimits::from_join_response(&bounded).max_response_header_items,
        1_024
    );
}

#[test]
fn test_framing_contract_rejects_scanner_limits_above_local_ceilings() {
    let incompatible_fields = [
        ("max_json_depth", TUNNEL_MAX_JSON_DEPTH + 1),
        (
            "max_json_structural_nodes",
            TUNNEL_MAX_JSON_STRUCTURAL_NODES + 1,
        ),
        (
            "max_json_primitive_bytes",
            TUNNEL_MAX_JSON_PRIMITIVE_BYTES + 1,
        ),
        ("max_response_headers", TUNNEL_MAX_RESPONSE_HEADERS + 1),
    ];

    for (field, value) in incompatible_fields {
        let mut response = v2_join_contract();
        response["framing"][field] = serde_json::json!(value);
        assert!(validate_framing_contract(&response).is_err(), "{field}");
    }

    let mut response = v2_join_contract();
    response["limits"]["max_response_header_items"] =
        serde_json::json!(TUNNEL_MAX_RESPONSE_HEADERS + 1);
    assert!(validate_framing_contract(&response).is_err());
}

#[test]
fn test_framing_rejects_out_of_order_frames() {
    let payload = serde_json::json!({"request_id": "request-1", "body": "ok"});
    let deadline = unix_time_ms() + 30_000;
    let mut outbound =
        OutboundTunnelStream::new("request-1", "request", deadline, &payload).unwrap();
    let start = outbound.start_message("tunnel:connect");
    let mut inbound = TunnelStreamAssembler::from_start(&start.payload, "request").unwrap();
    let mut frame = outbound.next_frame("tunnel:connect").unwrap().unwrap();
    frame.payload["sequence"] = serde_json::json!(1);

    assert!(
        inbound
            .append(&frame.payload)
            .unwrap_err()
            .to_string()
            .contains("Out-of-order")
    );
}

#[test]
fn test_ordered_headers_preserve_duplicates() {
    let headers = ordered_header_pairs(Some(&serde_json::json!([
        ["set-cookie", "first=1"],
        ["set-cookie", "second=2"]
    ])))
    .unwrap();

    assert_eq!(
        headers,
        vec![
            ("set-cookie".to_string(), "first=1".to_string()),
            ("set-cookie".to_string(), "second=2".to_string())
        ]
    );
}

#[test]
fn test_response_header_pairs_preserve_repeated_values() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.append("set-cookie", "first=1".parse().unwrap());
    headers.append("set-cookie", "second=2".parse().unwrap());

    assert_eq!(
        response_headers_to_pairs(&headers),
        vec![
            ("set-cookie".to_string(), "first=1".to_string()),
            ("set-cookie".to_string(), "second=2".to_string())
        ]
    );
}

#[test]
fn test_v2_response_header_item_limit_counts_repeated_values() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.append("set-cookie", "first=1".parse().unwrap());
    headers.append("set-cookie", "second=2".parse().unwrap());
    let mut limits = TunnelLimits::from_join_response(&serde_json::json!({
        "limits": {"max_response_header_items": 1}
    }));
    limits.max_response_header_bytes = usize::MAX;

    assert!(
        response_header_limit_error(&headers, limits)
            .unwrap()
            .contains("item limit")
    );

    limits.max_response_header_items = 2;
    assert!(response_header_limit_error(&headers, limits).is_none());
}

#[tokio::test]
async fn test_local_redirects_are_returned_without_following() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _ = socket.read(&mut request).await.unwrap();
        socket
            .write_all(
                b"HTTP/1.1 302 Found\r\nLocation: /must-not-follow\r\nContent-Length: 0\r\n\r\n",
            )
            .await
            .unwrap();
    });

    let response = tunnel_http_client(Duration::from_secs(1))
        .unwrap()
        .get(format!("http://{address}/start"))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::FOUND);
    server.await.unwrap();
}

#[test]
fn test_tunnel_join_payload_resumes_without_reactivating_route_inputs() {
    let fresh = tunnel_join_payload(3000, Some("org-123"), Some("billing"), None);
    assert_eq!(fresh["organization_id"], "org-123");
    assert_eq!(fresh["slug"], "billing");
    assert!(fresh.get("resume_token").is_none());

    let resumed = tunnel_join_payload(
        4000,
        Some("org-123"),
        Some("billing"),
        Some("short-lived-token"),
    );
    assert_eq!(resumed["local_port"], 4000);
    assert_eq!(resumed["resume_token"], "short-lived-token");
    assert!(resumed.get("organization_id").is_none());
    assert!(resumed.get("slug").is_none());
}

#[test]
fn test_tunnel_limits_clamp_join_response_contract_to_local_maxima() {
    let response = serde_json::json!({
        "limits": {
            "max_body_bytes": u64::MAX,
            "max_response_header_bytes": 16_777_216,
            "max_response_header_items": 2_048,
            "response_headers_format": "ordered_pairs"
        }
    });

    let limits = TunnelLimits::from_join_response(&response);

    assert_eq!(limits.max_request_body_bytes, LOCAL_MAX_TUNNEL_BODY_BYTES);
    assert_eq!(limits.max_response_body_bytes, LOCAL_MAX_TUNNEL_BODY_BYTES);
    assert_eq!(
        limits.max_response_header_bytes,
        LOCAL_MAX_RESPONSE_HEADER_BYTES
    );
    assert_eq!(limits.max_response_header_items, 2_048);
    assert!(limits.ordered_response_headers);
}

#[test]
fn test_tunnel_limits_clamp_untrusted_body_limit_to_local_maximum() {
    let response = serde_json::json!({
        "limits": {
            "max_body_bytes": u64::MAX,
            "max_response_header_bytes": u64::MAX,
            "max_response_header_items": u64::MAX
        }
    });

    let limits = TunnelLimits::from_join_response(&response);

    assert_eq!(
        (
            limits.max_request_body_bytes,
            limits.max_response_body_bytes
        ),
        (LOCAL_MAX_TUNNEL_BODY_BYTES, LOCAL_MAX_TUNNEL_BODY_BYTES)
    );
    assert_eq!(
        limits.max_response_header_bytes,
        LOCAL_MAX_RESPONSE_HEADER_BYTES
    );
    assert_eq!(
        limits.max_response_header_items,
        TUNNEL_MAX_RESPONSE_HEADERS
    );
}

#[test]
fn test_tunnel_limits_keep_legacy_response_header_shape_by_default() {
    let limits = TunnelLimits::from_join_response(&serde_json::json!({}));

    assert_eq!(limits.max_request_body_bytes, LEGACY_MAX_REQUEST_BODY_BYTES);
    assert_eq!(
        limits.max_response_body_bytes,
        LEGACY_MAX_RESPONSE_BODY_BYTES
    );
    assert_eq!(
        limits.max_response_header_bytes,
        LEGACY_MAX_RESPONSE_HEADER_BYTES
    );
    assert_eq!(
        limits.max_response_header_items,
        TUNNEL_MAX_RESPONSE_HEADERS
    );
    assert!(!limits.ordered_response_headers);
}

#[test]
fn test_response_headers_preserve_duplicate_order() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.append("set-cookie", "first=1".parse().unwrap());
    headers.append("set-cookie", "second=2".parse().unwrap());

    assert_eq!(
        response_headers_to_ordered_pairs(&headers),
        vec![
            ("set-cookie".to_string(), "first=1".to_string()),
            ("set-cookie".to_string(), "second=2".to_string())
        ]
    );
}

#[test]
fn test_large_text_response_uses_bounded_base64_encoding() {
    let body = vec![b'a'; MAX_RAW_BODY_BYTES + 1];

    let (encoded, encoding) = encode_response_body(&body);

    assert_eq!(encoding, "base64");
    assert_eq!(URL_SAFE_NO_PAD.decode(encoded).unwrap(), body);
}

#[test]
fn test_body_preview_is_bounded() {
    let body = vec![b'a'; UI_BODY_PREVIEW_BYTES * 2];
    let preview = body_preview(&body, &HashMap::new()).unwrap();

    assert!(preview.len() <= UI_BODY_PREVIEW_BYTES + 64);
    assert!(preview.contains("truncated"));
    assert!(preview.contains(&(UI_BODY_PREVIEW_BYTES * 2).to_string()));
}

#[test]
fn test_gzip_text_body_preview_is_decoded_without_changing_transport_bytes() {
    let body = b"<html><body>Not found</body></html>";
    let compressed = compressed_with(GzEncoder::new(Vec::new(), Compression::default()), body);
    let headers = preview_headers(&[
        ("content-type", "text/html; charset=utf-8"),
        ("content-encoding", "gzip"),
    ]);

    assert_eq!(
        body_preview(&compressed, &headers).as_deref(),
        Some("<html><body>Not found</body></html>")
    );

    let (transport_body, transport_encoding) = encode_response_body(&compressed);
    assert_eq!(transport_encoding, "base64");
    assert_eq!(URL_SAFE_NO_PAD.decode(transport_body).unwrap(), compressed);
}

#[test]
fn test_standard_and_legacy_deflate_body_previews_are_decoded() {
    let body = b"deflate response";
    let headers = preview_headers(&[
        ("content-type", "text/plain"),
        ("content-encoding", "deflate"),
    ]);
    let zlib = compressed_with(ZlibEncoder::new(Vec::new(), Compression::default()), body);
    let raw = compressed_with(
        DeflateEncoder::new(Vec::new(), Compression::default()),
        body,
    );

    assert_eq!(
        body_preview(&zlib, &headers).as_deref(),
        Some("deflate response")
    );
    assert_eq!(
        body_preview(&raw, &headers).as_deref(),
        Some("deflate response")
    );
}

#[test]
fn test_brotli_and_zstd_body_previews_are_decoded() {
    let body = b"compressed response";

    let mut brotli = Vec::new();
    {
        let mut encoder = brotli::CompressorWriter::new(&mut brotli, 4_096, 5, 22);
        encoder.write_all(body).unwrap();
    }
    let brotli_headers =
        preview_headers(&[("content-type", "text/plain"), ("content-encoding", "br")]);
    assert_eq!(
        body_preview(&brotli, &brotli_headers).as_deref(),
        Some("compressed response")
    );

    let zstd = zstd::stream::encode_all(Cursor::new(body), 0).unwrap();
    let zstd_headers =
        preview_headers(&[("content-type", "text/plain"), ("content-encoding", "zstd")]);
    assert_eq!(
        body_preview(&zstd, &zstd_headers).as_deref(),
        Some("compressed response")
    );
}

#[test]
fn test_stacked_content_encodings_are_decoded_in_reverse_order() {
    let body = b"stacked response";
    let gzip = compressed_with(GzEncoder::new(Vec::new(), Compression::default()), body);
    let mut gzip_then_brotli = Vec::new();
    {
        let mut encoder = brotli::CompressorWriter::new(&mut gzip_then_brotli, 4_096, 5, 22);
        encoder.write_all(&gzip).unwrap();
    }
    let headers = preview_headers(&[
        ("content-type", "text/plain"),
        ("content-encoding", "gzip, br"),
    ]);

    assert_eq!(
        body_preview(&gzip_then_brotli, &headers).as_deref(),
        Some("stacked response")
    );
}

#[test]
fn test_content_encoding_list_is_bounded() {
    let at_limit = preview_headers(&[("content-encoding", "gzip, br, zstd, deflate")]);
    let over_limit = preview_headers(&[("content-encoding", "gzip, br, zstd, deflate, gzip")]);

    assert_eq!(
        content_encodings(&at_limit).unwrap().len(),
        MAX_CONTENT_ENCODINGS
    );
    assert_eq!(
        content_encodings(&over_limit).unwrap_err(),
        "too many content encodings (5 > 4)"
    );
    assert!(
        body_preview(b"encoded", &over_limit)
            .unwrap()
            .starts_with("[body preview unavailable: too many content encodings")
    );
}

#[test]
fn test_text_body_preview_honors_declared_charset() {
    let headers = preview_headers(&[("Content-Type", "text/plain; charset=iso-8859-1")]);

    assert_eq!(body_preview(b"caf\xe9", &headers).as_deref(), Some("café"));
}

#[test]
fn test_binary_body_preview_uses_metadata_instead_of_lossy_text() {
    let body = b"\x89PNG\r\n\x1a\n\0\xff";
    let headers = preview_headers(&[("content-type", "image/png")]);
    let preview = body_preview(body, &headers).unwrap();

    assert_eq!(preview, "[binary body: 10 bytes; content-type: image/png]");
    assert!(!preview.contains('\u{fffd}'));
}

#[test]
fn test_unsupported_or_malformed_content_encoding_has_safe_preview() {
    let unsupported = preview_headers(&[("content-encoding", "compress")]);
    let malformed = preview_headers(&[("content-encoding", "gzip")]);

    let unsupported_preview = body_preview(b"encoded", &unsupported).unwrap();
    assert!(unsupported_preview.contains("unsupported content-encoding"));

    let malformed_preview = body_preview(b"not gzip", &malformed).unwrap();
    assert!(malformed_preview.contains("preview unavailable"));
    assert!(!malformed_preview.contains('\u{fffd}'));
}

#[test]
fn test_compressed_body_preview_is_bounded_after_decompression() {
    let body = vec![b'a'; UI_BODY_PREVIEW_BYTES * 2];
    let compressed = compressed_with(GzEncoder::new(Vec::new(), Compression::default()), &body);
    let headers = preview_headers(&[("content-type", "text/plain"), ("content-encoding", "gzip")]);
    let preview = body_preview(&compressed, &headers).unwrap();

    assert!(preview.starts_with("aaaa"));
    assert!(preview.contains("decoded preview truncated"));
    assert!(preview.len() <= UI_BODY_PREVIEW_BYTES + 96);
}

#[test]
fn test_log_text_is_control_neutral_and_bounded() {
    let input = format!(
        "relay\u{1b}]52;c;owned\u{7}\n{}",
        "a".repeat(LOG_TEXT_MAX_BYTES)
    );

    let sanitized = bounded_control_neutral_log_text(&input);

    assert!(sanitized.len() <= LOG_TEXT_MAX_BYTES);
    assert!(!sanitized.chars().any(char::is_control));
    assert!(sanitized.contains("\\u{1b}"));
    assert!(sanitized.contains("\\n"));
    assert!(sanitized.ends_with("..."));
}

#[test]
fn test_local_work_budget_prevents_concurrent_ten_megabyte_deliveries() {
    let budget = LocalWorkBudget::new();
    let ten_mib = 10 * 1024 * 1024;
    let permit = budget.try_acquire(ten_mib).unwrap();

    assert_eq!(budget.available_count(), LOCAL_WORK_MAX_COUNT - 1);
    assert_eq!(budget.available_bytes(), 6 * 1024 * 1024);
    assert!(budget.try_acquire(ten_mib).is_none());

    drop(permit);
    assert_eq!(budget.available_count(), LOCAL_WORK_MAX_COUNT);
    assert_eq!(budget.available_bytes(), LOCAL_WORK_MAX_BYTES);
}

#[test]
fn test_local_work_budget_allows_one_advertised_limit_body() {
    let budget = LocalWorkBudget::new();
    let delivery = LocalDelivery {
        request_id: "request-at-limit".to_string(),
        method: "POST".to_string(),
        path: "/metadata-does-not-reduce-the-body-budget".to_string(),
        query_string: "source=conformance".to_string(),
        headers: vec![(
            "content-type".to_string(),
            "application/octet-stream".to_string(),
        )],
        body: Vec::new(),
        deadline_unix_ms: unix_time_ms() + 1_000,
        replay: false,
    };
    assert_eq!(delivery.retained_bytes(), 0);

    let permit = budget.try_acquire(LOCAL_WORK_MAX_BYTES).unwrap();

    assert_eq!(budget.available_bytes(), 0);
    assert!(budget.try_acquire(1).is_none());

    drop(permit);
    assert_eq!(budget.available_bytes(), LOCAL_WORK_MAX_BYTES);
}

#[test]
fn test_local_work_budget_enforces_count_independently_of_bytes() {
    let budget = LocalWorkBudget::new();
    let permits = (0..LOCAL_WORK_MAX_COUNT)
        .map(|_| budget.try_acquire(1).unwrap())
        .collect::<Vec<_>>();

    assert_eq!(budget.available_count(), 0);
    assert!(budget.try_acquire(1).is_none());
    assert!(budget.available_bytes() > LOCAL_WORK_MAX_BYTES - 1024);

    drop(permits);
    assert_eq!(budget.available_count(), LOCAL_WORK_MAX_COUNT);
}

#[test]
fn test_local_work_budget_rejects_oversized_delivery() {
    let budget = LocalWorkBudget::new();

    assert!(budget.try_acquire(LOCAL_WORK_MAX_BYTES + 1).is_none());
    assert_eq!(budget.available_count(), LOCAL_WORK_MAX_COUNT);
    assert_eq!(budget.available_bytes(), LOCAL_WORK_MAX_BYTES);
}

#[test]
fn test_response_buffer_budget_prevents_concurrent_ten_megabyte_responses() {
    let budget = ResponseBufferBudget::new();
    let ten_mib = 10 * 1024 * 1024;
    let reservation = budget.try_reserve(Some(ten_mib)).unwrap();

    assert_eq!(budget.available_bytes(), 6 * 1024 * 1024);
    assert!(budget.try_reserve(Some(ten_mib)).is_none());

    drop(reservation);
    assert_eq!(budget.available_bytes(), RESPONSE_BUFFER_MAX_BYTES);
}

#[test]
fn test_response_buffer_allows_one_advertised_limit_body() {
    let budget = ResponseBufferBudget::new();
    let permit = budget.try_reserve(Some(RESPONSE_BUFFER_MAX_BYTES)).unwrap();

    assert_eq!(budget.available_bytes(), 0);
    assert!(budget.try_reserve(Some(1)).is_none());

    drop(permit);
    assert_eq!(budget.available_bytes(), RESPONSE_BUFFER_MAX_BYTES);
}

#[test]
fn test_response_buffer_budget_rejects_oversized_response() {
    let budget = ResponseBufferBudget::new();

    assert!(
        budget
            .try_reserve(Some(RESPONSE_BUFFER_MAX_BYTES + 1))
            .is_none()
    );
    assert_eq!(budget.available_bytes(), RESPONSE_BUFFER_MAX_BYTES);
}

#[test]
fn test_unknown_response_length_cannot_grow_past_budget() {
    let budget = ResponseBufferBudget::new();
    let mut reservation = budget.try_reserve(None).unwrap();

    assert!(reservation.try_grow_to(RESPONSE_BUFFER_MAX_BYTES / 2));
    assert!(!reservation.try_grow_to(RESPONSE_BUFFER_MAX_BYTES + 1));
    assert_eq!(budget.available_bytes(), RESPONSE_BUFFER_MAX_BYTES / 2);

    drop(reservation);
    assert_eq!(budget.available_bytes(), RESPONSE_BUFFER_MAX_BYTES);
}

#[test]
fn test_inbound_stream_budget_rejects_oversized_stream() {
    let oversized = INBOUND_STREAM_MAX_BYTES.saturating_add(1);
    let encoded_body_bytes = base64::encoded_len(LOCAL_MAX_TUNNEL_BODY_BYTES, false).unwrap();

    assert_eq!(INBOUND_STREAM_MAX_BYTES, 32 * 1024 * 1024);
    assert!(encoded_body_bytes.saturating_add(TUNNEL_MAX_FRAME_BYTES) < INBOUND_STREAM_MAX_BYTES);
    assert!(!inbound_stream_fits_budget(0, oversized));
    assert!(inbound_stream_fits_budget(INBOUND_STREAM_MAX_BYTES - 1, 1));
}

#[test]
fn test_cancellation_classifies_execution_boundary() {
    assert_eq!(
        cancellation_classification(DELIVERY_QUEUED),
        Some(("cancelled_before_forward", "known_not_executed"))
    );
    assert_eq!(
        cancellation_classification(DELIVERY_STARTED),
        Some(("cancelled_after_forward_started", "outcome_unknown"))
    );
    assert_eq!(cancellation_classification(DELIVERY_TERMINAL), None);
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

fn test_priority_writer() -> (
    PriorityWriter,
    mpsc::Receiver<OutboundItem>,
    mpsc::Receiver<OutboundItem>,
) {
    let (control_tx, control_rx) = mpsc::channel(OUTBOUND_CONTROL_MAX_COUNT);
    let (response_tx, response_rx) = mpsc::channel(OUTBOUND_RESPONSE_MAX_COUNT);
    (
        PriorityWriter {
            control_tx,
            response_tx,
            bytes: Arc::new(Semaphore::new(OUTBOUND_MAX_BYTES)),
        },
        control_rx,
        response_rx,
    )
}

fn channel_message(item: OutboundItem) -> ChannelMessage {
    let Message::Text(text) = item.message else {
        panic!("expected text channel message");
    };
    serde_json::from_str(&text).unwrap()
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

async fn forwarder_for_local_address(
    address: std::net::SocketAddr,
) -> (TunnelForwarder, mpsc::Receiver<TunnelEvent>) {
    let (_token_tx, token_rx) = watch::channel("token".to_string());
    let (event_tx, event_rx) = mpsc::channel(PRESENTATION_QUEUE_CAPACITY);
    let target = TargetPolicy::resolve(
        &format!("http://127.0.0.1:{}", address.port()),
        false,
        false,
    )
    .await
    .unwrap();
    (
        TunnelForwarder::new(
            token_rx,
            "127.0.0.1".to_string(),
            address.port(),
            target,
            None,
            None,
            event_tx,
            false,
        ),
        event_rx,
    )
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

// TunnelWebhookRequest tests
#[test]
fn test_tunnel_webhook_request_full_payload() {
    let json = r#"{
        "id": "req-1",
        "forward_id": "fwd-1",
        "method": "POST",
        "path": "/webhook",
        "query_params": {"foo": "bar"},
        "headers": {"content-type": "application/json"},
        "body": "{\"data\":1}"
    }"#;
    let req: TunnelWebhookRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.id, "req-1");
    assert_eq!(req.forward_id.as_deref(), Some("fwd-1"));
    assert_eq!(req.method, "POST");
    assert_eq!(req.path, "/webhook");
    assert!(req.body.is_some());
}

#[test]
fn test_tunnel_webhook_request_minimal() {
    let json = r#"{"id":"req-2","method":"GET","path":"/"}"#;
    let req: TunnelWebhookRequest = serde_json::from_str(json).unwrap();
    assert_eq!(req.id, "req-2");
    assert!(req.forward_id.is_none());
    assert!(req.body.is_none());
    assert!(req.headers.is_empty());
    assert!(req.query_params.is_empty());
}

// build_ws_url tests
#[test]
fn test_build_ws_url_https_to_wss() {
    let url = build_ws_url("https://api.example.com", "tok123", "socket/websocket");
    assert_eq!(url, "wss://api.example.com/socket/websocket?ticket=tok123");
}

#[test]
fn test_build_ws_url_http_to_ws() {
    let url = build_ws_url("http://localhost:4000", "tok", "tunnel/websocket");
    assert_eq!(url, "ws://localhost:4000/tunnel/websocket?ticket=tok");
}

#[test]
fn test_websocket_endpoint_omits_access_token() {
    let endpoint = websocket_endpoint("https://api.example.com", "tunnel/websocket");

    assert_eq!(endpoint, "wss://api.example.com/tunnel/websocket");
    assert!(!endpoint.contains("token="));
}

#[test]
fn test_redact_access_token_from_connection_errors() {
    let token = "public-cli-secret-token";
    let error = format!("request failed for wss://api.example.com/tunnel?token={token}");
    let redacted = redact_access_token(&error, token);

    assert!(!redacted.contains(token));
    assert!(redacted.contains(REDACTED_SECRET));
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
fn test_relay_handshake_rejections_are_fatal() {
    assert!(is_fatal_error(
        "Relay handshake rejected (HTTP 401 Unauthorized): expired"
    ));
    assert!(is_fatal_error(
        "Relay handshake rejected: ticket receipt was incomplete"
    ));
    assert!(!is_fatal_error(
        "Relay handshake ticket request failed (HTTP 503 Service Unavailable)"
    ));
}

#[test]
fn test_http_auth_statuses_are_fatal_only_as_status_tokens() {
    // The shapes this crate produces for a rejected credential.
    assert!(is_fatal_error(
        "Failed to connect to tunnel: HTTP error: 401 Unauthorized"
    ));
    assert!(is_fatal_error(
        "Tunnel API request failed (token_revoked, HTTP 403): revoked"
    ));
    assert!(is_fatal_error("Connection failed with HTTP status: 403"));
    // Other statuses, or the digits appearing outside an HTTP status, stay retryable.
    assert!(!is_fatal_error("Connection failed with HTTP status: 502"));
    assert!(!is_fatal_error("Connection refused: port 4013 unreachable"));
}

#[test]
fn test_static_tunnel_lease_contention_is_retryable() {
    assert!(!is_fatal_error(
        "Tunnel join failed: This static tunnel is already in use by another connection"
    ));
}

#[test]
fn test_static_tunnel_configuration_errors_are_fatal() {
    assert!(is_fatal_error(
        "Tunnel join failed: Static tunnel slug not found. Create it first."
    ));
    assert!(is_fatal_error(
        "Tunnel join failed: This slug belongs to another organization"
    ));
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

#[test]
fn test_decode_request_body_limited_rejects_oversized_raw_body_before_copying() {
    let error = decode_request_body_limited("raw", "12345", 4).unwrap_err();

    assert!(error.to_string().contains("5 > 4 bytes"));
}

#[test]
fn test_decode_request_body_limited_rejects_oversized_base64_before_decoding() {
    let encoded = URL_SAFE_NO_PAD.encode(b"12345");

    let error = decode_request_body_limited("base64", &encoded, 4).unwrap_err();

    assert!(error.to_string().contains("encoded body is too large"));
}
