//! `--json` receipts and streamed events.

use super::*;

#[test]
fn forward_request_receipt_includes_agent_links() {
    let response = api::EndpointRequestForwardResponse {
        forward_id: "fwd_123".to_string(),
        debug_request_id: "req_123".to_string(),
        target_url: "https://example.com/webhook".to_string(),
        status: "pending".to_string(),
    };

    let receipt = forward_request_receipt("org_123", "ep_123", "req_123", &response);

    assert_eq!(receipt["status"], "pending");
    assert_eq!(receipt["delivery_status"], "queued");
    assert_eq!(receipt["resource_uri"], "hooklistener://forwards/fwd_123");
    assert_eq!(
        receipt["request_resource_uri"],
        "hooklistener://requests/req_123"
    );
    assert_eq!(receipt["poll_url"], "/api/v1/forwards/fwd_123");
    assert_eq!(
        receipt["resources"]["request_forwards"],
        "hooklistener://requests/req_123/forwards"
    );
    assert_eq!(
        receipt["next_actions"][0],
        "hooklistener endpoint show-forward fwd_123"
    );
}

#[test]
fn listen_started_receipt_includes_endpoint_resources() {
    let endpoint = api::DebugEndpointSummary {
        id: "ep_123".to_string(),
        name: "GitHub".to_string(),
        slug: "github-webhooks".to_string(),
        status: "active".to_string(),
        webhook_url: "https://hooks.example.dev/github-webhooks".to_string(),
        created_at: None,
        updated_at: None,
    };

    let receipt = listen_started_receipt(
        "github-webhooks",
        "http://localhost:3000/webhooks",
        "wss://api.example.dev/socket/websocket",
        Some(&endpoint),
    );

    assert_eq!(receipt["type"], "receipt");
    assert_eq!(receipt["event"], "listen_started");
    assert_eq!(receipt["status"], "running");
    assert_eq!(
        receipt["resource_uri"],
        "hooklistener://cli/listen/github-webhooks"
    );
    assert_eq!(receipt["endpoint"]["id"], "ep_123");
    assert_eq!(
        receipt["resources"]["endpoint"],
        "hooklistener://endpoints/ep_123"
    );
    assert_eq!(
        receipt["resources"]["requests"],
        "hooklistener://endpoints/ep_123/requests"
    );
}

#[test]
fn listen_webhook_event_includes_request_resource_uri() {
    let request = models::WebhookRequest {
        id: "req_123".to_string(),
        timestamp: 1_781_000_000,
        remote_addr: "Tunnel".to_string(),
        headers: std::collections::HashMap::new(),
        content_length: 42,
        method: "POST".to_string(),
        url: "/webhooks/github".to_string(),
        path: Some("/webhooks/github".to_string()),
        query_params: std::collections::HashMap::new(),
        created_at: "2026-07-08T10:00:00Z".to_string(),
        body_preview: None,
        body: None,
    };
    let event = TunnelEvent::WebhookReceived(Box::new(request));
    let receipt = listen_event_receipt(
        &event,
        "github-webhooks",
        "http://localhost:3000/webhooks",
        None,
    );

    assert_eq!(receipt["event"], "webhook_received");
    assert_eq!(receipt["request_id"], "req_123");
    assert_eq!(receipt["resource_uri"], "hooklistener://requests/req_123");
    assert_eq!(
        receipt["endpoint_resource_uri"],
        "hooklistener://endpoints/by-slug/github-webhooks"
    );
    assert_eq!(receipt["request"]["method"], "POST");
}

#[test]
fn listen_forward_success_event_includes_delivery_receipt() {
    let event = TunnelEvent::ForwardSuccess {
        request_id: "req_123".to_string(),
        target_url: "http://localhost:3000/webhooks/github".to_string(),
        status: 204,
        duration_ms: 37,
    };
    let receipt = listen_event_receipt(
        &event,
        "github-webhooks",
        "http://localhost:3000/webhooks",
        None,
    );

    assert_eq!(receipt["event"], "forward_succeeded");
    assert_eq!(receipt["status"], "succeeded");
    assert_eq!(receipt["status_code"], 204);
    assert_eq!(receipt["duration_ms"], 37);
    assert_eq!(
        receipt["request_resource_uri"],
        "hooklistener://requests/req_123"
    );
}

#[test]
fn tunnel_established_event_includes_public_url_and_resource() {
    let event = TunnelEvent::TunnelEstablished {
        subdomain: "plant-07.hook.events".to_string(),
        tunnel_id: "tun_123".to_string(),
        is_static: false,
    };
    let receipt = tunnel_event_receipt(&event, "localhost", 3000, Some("org_123"), None);

    assert_eq!(receipt["type"], "receipt");
    assert_eq!(receipt["event"], "tunnel_established");
    assert_eq!(receipt["resource_uri"], "hooklistener://tunnels/tun_123");
    assert_eq!(receipt["public_url"], "https://plant-07.hook.events");
    assert_eq!(receipt["local_target_url"], "http://localhost:3000");
    assert_eq!(receipt["organization_id"], "org_123");
}

#[test]
fn tunnel_request_event_includes_local_target_and_request_resource() {
    let mut headers = std::collections::HashMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    headers.insert("authorization".to_string(), "Bearer secret".to_string());
    let event = TunnelEvent::RequestReceived {
        request_id: "req_123".to_string(),
        method: "POST".to_string(),
        path: "webhooks/github".to_string(),
        headers,
        body: Some("{\"ok\":true}".to_string()),
        query_string: "delivery=abc".to_string(),
        replay: false,
    };
    let receipt = tunnel_event_receipt(&event, "127.0.0.1", 8080, None, Some("dev"));

    assert_eq!(receipt["event"], "request_received");
    assert_eq!(receipt["resource_uri"], "hooklistener://requests/req_123");
    assert_eq!(
        receipt["local_target_url"],
        "http://127.0.0.1:8080/webhooks/github?delivery=abc"
    );
    assert_eq!(receipt["body_size"], 11);
    assert_eq!(receipt["headers"]["content-type"], "application/json");
    assert_eq!(receipt["headers"]["authorization"], "[REDACTED]");
    assert!(!receipt.to_string().contains("Bearer secret"));
}

#[test]
fn tunnel_request_event_redacts_sensitive_headers() {
    let headers = std::collections::HashMap::from([
        (
            "authorization".to_string(),
            "Bearer request-secret".to_string(),
        ),
        ("cookie".to_string(), "session=request-secret".to_string()),
        ("x-api-key".to_string(), "request-api-key".to_string()),
        ("content-type".to_string(), "application/json".to_string()),
    ]);
    let event = TunnelEvent::RequestReceived {
        request_id: "req_secure".to_string(),
        method: "POST".to_string(),
        path: "/webhook".to_string(),
        headers,
        body: None,
        query_string: String::new(),
        replay: false,
    };

    let receipt = tunnel_event_receipt(&event, "127.0.0.1", 8080, None, None);

    assert_eq!(receipt["headers"]["authorization"], "[REDACTED]");
    assert_eq!(receipt["headers"]["cookie"], "[REDACTED]");
    assert_eq!(receipt["headers"]["x-api-key"], "[REDACTED]");
    assert_eq!(receipt["headers"]["content-type"], "application/json");
    let output = receipt.to_string();
    assert!(!output.contains("request-secret"));
    assert!(!output.contains("request-api-key"));
}

#[test]
fn tunnel_response_event_redacts_sensitive_headers() {
    let event = TunnelEvent::RequestForwarded {
        request_id: "req_secure".to_string(),
        status: 200,
        duration_ms: 10,
        response_headers: std::collections::HashMap::from([
            (
                "set-cookie".to_string(),
                "session=response-secret".to_string(),
            ),
            ("x-auth-token".to_string(), "response-token".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ]),
        response_body: None,
    };

    let receipt = tunnel_event_receipt(&event, "127.0.0.1", 8080, None, None);

    assert_eq!(receipt["response_headers"]["set-cookie"], "[REDACTED]");
    assert_eq!(receipt["response_headers"]["x-auth-token"], "[REDACTED]");
    assert_eq!(
        receipt["response_headers"]["content-type"],
        "application/json"
    );
    let output = receipt.to_string();
    assert!(!output.contains("response-secret"));
    assert!(!output.contains("response-token"));
}

#[test]
fn buffered_summary_event_receipt_includes_count_and_oldest() {
    let event = TunnelEvent::BufferedSummary {
        count: 3,
        oldest_captured_at: Some("2026-07-14T09:00:00Z".to_string()),
    };

    let receipt = tunnel_event_receipt(&event, "localhost", 3000, None, None);

    assert_eq!(receipt["event"], "buffered_summary");
    assert_eq!(receipt["count"], 3);
    assert_eq!(receipt["oldest_captured_at"], "2026-07-14T09:00:00Z");
}

#[test]
fn buffered_replayed_event_receipt_includes_status() {
    let event = TunnelEvent::BufferedReplayed {
        capture_id: "cap_123".to_string(),
        status: 204,
    };

    let receipt = tunnel_event_receipt(&event, "localhost", 3000, None, None);

    assert_eq!(receipt["event"], "buffered_replayed");
    assert_eq!(receipt["capture_id"], "cap_123");
    assert_eq!(receipt["status_code"], 204);
    assert_eq!(receipt["resource_uri"], "hooklistener://requests/cap_123");
}

#[test]
fn buffered_replay_failed_event_receipt_includes_reason() {
    let event = TunnelEvent::BufferedReplayFailed {
        capture_id: "cap_123".to_string(),
        reason: "local_unreachable".to_string(),
    };

    let receipt = tunnel_event_receipt(&event, "localhost", 3000, None, None);

    assert_eq!(receipt["event"], "buffered_replay_failed");
    assert_eq!(receipt["capture_id"], "cap_123");
    assert_eq!(receipt["error"], "local_unreachable");
}

#[test]
fn tunnel_request_event_marks_replay() {
    let event = TunnelEvent::RequestReceived {
        request_id: "cap_456".to_string(),
        method: "POST".to_string(),
        path: "/webhook".to_string(),
        headers: std::collections::HashMap::new(),
        body: None,
        query_string: String::new(),
        replay: true,
    };

    let receipt = tunnel_event_receipt(&event, "localhost", 3000, None, None);

    assert_eq!(receipt["event"], "request_received");
    assert_eq!(receipt["replay"], true);
}

#[test]
fn tunnel_stream_gap_is_recoverable_and_does_not_affect_delivery() {
    let event = TunnelEvent::StreamGap { dropped_events: 7 };
    let receipt = tunnel_event_receipt(&event, "127.0.0.1", 8080, None, None);

    assert_eq!(receipt["event"], "stream_gap");
    assert_eq!(receipt["status"], "recoverable");
    assert_eq!(receipt["dropped_events"], 7);
    assert_eq!(receipt["delivery_affected"], false);
}

#[test]
fn validate_forward_target_url_requires_http_or_https() {
    assert!(validate_forward_target_url("http://localhost:3000/webhook").is_ok());
    assert!(validate_forward_target_url("https://example.com/webhook").is_ok());

    let err = validate_forward_target_url("ftp://example.com/webhook").unwrap_err();
    assert!(
        err.to_string().contains("Use http or https"),
        "unexpected error: {}",
        err
    );
}
