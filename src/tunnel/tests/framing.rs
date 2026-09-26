//! Channel messages, join payloads, and v2/v3 framing contracts.

use super::*;

fn v2_join_contract() -> serde_json::Value {
    serde_json::json!({
        "limits": {
            "max_stream_bytes": TUNNEL_MAX_STREAM_BYTES,
        },
        "framing": serde_json::from_str::<serde_json::Value>(include_str!(
            "../../../fixtures/tunnel_framing_v2.json"
        ))
        .unwrap(),
    })
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
        serde_json::from_str(include_str!("../../../fixtures/tunnel_framing_v2.json")).unwrap();

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
