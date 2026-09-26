//! WebSocket client URLs, fatal-error classification, and backoff.

use super::*;

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
