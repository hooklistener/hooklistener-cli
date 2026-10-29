//! Machine-readable receipts and resource URIs for `--json` output.

use chrono::Utc;

use crate::api;
use crate::tunnel::TunnelEvent;

pub fn request_resource_uri(request_id: &str) -> String {
    format!("hooklistener://requests/{request_id}")
}

pub fn request_forwards_resource_uri(request_id: &str) -> String {
    format!("hooklistener://requests/{request_id}/forwards")
}

pub fn endpoint_resource_uri(endpoint_id: &str) -> String {
    format!("hooklistener://endpoints/{endpoint_id}")
}

pub fn endpoint_requests_resource_uri(endpoint_id: &str) -> String {
    format!("hooklistener://endpoints/{endpoint_id}/requests")
}

pub fn endpoint_slug_resource_uri(endpoint_slug: &str) -> String {
    format!("hooklistener://endpoints/by-slug/{endpoint_slug}")
}

pub fn forward_resource_uri(forward_id: &str) -> String {
    format!("hooklistener://forwards/{forward_id}")
}

pub fn tunnel_resource_uri(tunnel_id: &str) -> String {
    format!("hooklistener://tunnels/{tunnel_id}")
}

pub fn listen_session_resource_uri(endpoint_slug: &str) -> String {
    format!("hooklistener://cli/listen/{endpoint_slug}")
}

pub fn tunnel_session_resource_uri(host: &str, port: u16) -> String {
    format!("hooklistener://cli/tunnels/{host}:{port}")
}

pub fn forward_poll_path(forward_id: &str) -> String {
    format!("/api/v1/forwards/{forward_id}")
}

pub fn forward_poll_command(forward_id: &str) -> String {
    format!("hooklistener endpoint show-forward {forward_id}")
}

pub fn emitted_at() -> String {
    Utc::now().to_rfc3339()
}

pub fn command_event_receipt(
    schema: &str,
    command: &str,
    operation: &str,
    event: &str,
    status: &str,
) -> serde_json::Value {
    serde_json::json!({
        "$schema": schema,
        "schema_version": 1,
        "type": "event",
        "event_id": uuid::Uuid::new_v4(),
        "sequence": 0,
        "event": event,
        "status": status,
        "command": command,
        "operation": operation,
        "emitted_at": emitted_at()
    })
}

pub fn listen_event_base(
    event: &str,
    status: &str,
    endpoint_slug: &str,
    target_url: &str,
) -> serde_json::Value {
    let mut receipt = command_event_receipt(
        LISTEN_EVENT_SCHEMA,
        "listen",
        "listen_endpoint",
        event,
        status,
    );
    receipt["endpoint_slug"] = serde_json::json!(endpoint_slug);
    receipt["target_url"] = serde_json::json!(target_url);
    receipt["resource_uri"] = serde_json::json!(listen_session_resource_uri(endpoint_slug));
    receipt
}

pub fn tunnel_event_base(event: &str, status: &str) -> serde_json::Value {
    command_event_receipt(
        TUNNEL_EVENT_SCHEMA,
        "tunnel",
        "start_local_tunnel",
        event,
        status,
    )
}

const REDACTED_VALUE: &str = "[REDACTED]";

/// Substrings that mark a credential however the name is cased or separated:
/// `access_token`, `clientSecret`, `X-Api-Key`, `PHPSESSID`.
const CREDENTIAL_NAME_SUBSTRINGS: &[&str] = &[
    "token",
    "secret",
    "password",
    "passwd",
    "apikey",
    "sessionid",
    "sessid",
];

/// Whole words that name a credential: `Authorization`, `Set-Cookie`,
/// `X-Amz-Signature`, `sig`, `session_id`.
const CREDENTIAL_NAME_WORDS: &[&str] = &[
    "authorization",
    "auth",
    "bearer",
    "jwt",
    "cookie",
    "signature",
    "sig",
    "session",
];

pub fn redact_tunnel_headers(
    headers: &std::collections::HashMap<String, String>,
) -> std::collections::HashMap<String, String> {
    headers
        .iter()
        .map(|(name, value)| {
            let value = if sensitive_credential_name(name) {
                REDACTED_VALUE.to_string()
            } else {
                value.clone()
            };

            (name.clone(), value)
        })
        .collect()
}

/// Reports whether a header or query parameter name looks like it carries a
/// credential. Headers and query strings share this one rule so a receipt
/// redacts a secret the same way whichever channel the sender used.
pub fn sensitive_credential_name(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    let words: Vec<&str> = name
        .split(|character: char| !character.is_ascii_alphanumeric())
        .filter(|word| !word.is_empty())
        .collect();
    let compact = words.concat();

    CREDENTIAL_NAME_SUBSTRINGS
        .iter()
        .any(|needle| compact.contains(needle))
        || words.iter().any(|word| CREDENTIAL_NAME_WORDS.contains(word))
        // `key`, `api_key`, `X-Api-Key`, `private.key`; not `keyword` or `key_id`.
        || words.last() == Some(&"key")
        // An OAuth authorization code; `status_code` and `zip_code` stay visible.
        || compact == "code"
}

/// Replaces the value of each credential-like query parameter with
/// `[REDACTED]`, for receipts and other machine-readable output. The local
/// target must still be sent the original query.
///
/// Parameter order, names, `&` and `;` separators, and all other values are
/// kept as sent. Names match case-insensitively, both as sent and after
/// percent-decoding. A parameter without `=` or with an empty value has
/// nothing to hide and is left alone.
pub fn redact_query_string(query: &str) -> String {
    let mut redacted = String::with_capacity(query.len());

    for segment in query.split_inclusive(['&', ';']) {
        let pair = segment.strip_suffix(['&', ';']).unwrap_or(segment);
        let separator = &segment[pair.len()..];

        match pair.split_once('=') {
            Some((name, value)) if !value.is_empty() && sensitive_query_param_name(name) => {
                redacted.push_str(name);
                redacted.push('=');
                redacted.push_str(REDACTED_VALUE);
            }
            _ => redacted.push_str(pair),
        }
        redacted.push_str(separator);
    }

    redacted
}

/// Applies [`redact_query_string`] to the query of a URL or path, leaving the
/// scheme, host, path, and fragment unchanged.
pub fn redact_url_query(url: &str) -> String {
    let (before_fragment, fragment) = url.split_at(url.find('#').unwrap_or(url.len()));
    match before_fragment.split_once('?') {
        Some((base, query)) => format!("{base}?{}{fragment}", redact_query_string(query)),
        None => url.to_string(),
    }
}

fn sensitive_query_param_name(raw_name: &str) -> bool {
    sensitive_credential_name(raw_name)
        || sensitive_credential_name(&decode_query_component(raw_name))
}

/// Decodes `+` and valid `%XX` escapes as `application/x-www-form-urlencoded`
/// does. Malformed escapes stay literal and invalid UTF-8 is replaced, so the
/// result is only fit for matching names, never for display or forwarding.
fn decode_query_component(raw: &str) -> String {
    fn hex_value(byte: Option<&u8>) -> Option<u8> {
        char::from(*byte?).to_digit(16).map(|digit| digit as u8)
    }

    let bytes = raw.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while let Some(&byte) = bytes.get(index) {
        match byte {
            b'+' => {
                decoded.push(b' ');
                index += 1;
            }
            b'%' => match (
                hex_value(bytes.get(index + 1)),
                hex_value(bytes.get(index + 2)),
            ) {
                (Some(high), Some(low)) => {
                    decoded.push((high << 4) | low);
                    index += 3;
                }
                _ => {
                    decoded.push(b'%');
                    index += 1;
                }
            },
            _ => {
                decoded.push(byte);
                index += 1;
            }
        }
    }

    String::from_utf8_lossy(&decoded).into_owned()
}

pub fn reconnect_failure_reason(event: &TunnelEvent) -> Option<&str> {
    match event {
        TunnelEvent::ReconnectFailed { reason } => Some(reason),
        _ => None,
    }
}

pub fn path_with_query(path: &str, query_string: &str) -> String {
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };

    let query_string = query_string.trim().trim_start_matches('?');
    if query_string.is_empty() {
        path
    } else {
        format!("{path}?{query_string}")
    }
}

pub fn public_tunnel_url(subdomain: &str) -> String {
    let subdomain = subdomain.trim().trim_end_matches('/');
    if subdomain.starts_with("http://") || subdomain.starts_with("https://") {
        subdomain.to_string()
    } else {
        format!("https://{subdomain}")
    }
}

pub fn local_tunnel_target_url(host: &str, port: u16) -> String {
    format!("http://{host}:{port}")
}

pub fn local_tunnel_request_target(
    host: &str,
    port: u16,
    path: &str,
    query_string: &str,
) -> String {
    format!(
        "{}{}",
        local_tunnel_target_url(host, port),
        path_with_query(path, query_string)
    )
}

pub fn endpoint_receipt_parts(
    endpoint_slug: &str,
    endpoint: Option<&api::DebugEndpointSummary>,
) -> (String, String, serde_json::Value) {
    match endpoint {
        Some(endpoint) => (
            endpoint_resource_uri(&endpoint.id),
            endpoint_requests_resource_uri(&endpoint.id),
            serde_json::json!({
                "id": &endpoint.id,
                "name": &endpoint.name,
                "slug": &endpoint.slug,
                "status": &endpoint.status,
                "webhook_url": &endpoint.webhook_url,
                "resource_uri": endpoint_resource_uri(&endpoint.id)
            }),
        ),
        None => {
            let endpoint_uri = endpoint_slug_resource_uri(endpoint_slug);
            (
                endpoint_uri.clone(),
                format!("{endpoint_uri}/requests"),
                serde_json::json!({
                    "slug": endpoint_slug,
                    "resource_uri": endpoint_uri,
                    "resolution": "unresolved"
                }),
            )
        }
    }
}

pub fn listen_started_receipt(
    endpoint_slug: &str,
    target_url: &str,
    ws_url: &str,
    endpoint: Option<&api::DebugEndpointSummary>,
) -> serde_json::Value {
    let (endpoint_resource_uri, requests_resource_uri, endpoint_value) =
        endpoint_receipt_parts(endpoint_slug, endpoint);
    let session_resource_uri = listen_session_resource_uri(endpoint_slug);
    let inspect_command = endpoint
        .map(|endpoint| format!("hooklistener endpoint list-requests {}", endpoint.id))
        .unwrap_or_else(|| "hooklistener endpoint list --json".to_string());

    serde_json::json!({
        "type": "receipt",
        "event": "listen_started",
        "status": "running",
        "command": "listen",
        "operation": "listen_endpoint",
        "emitted_at": emitted_at(),
        "resource_uri": &session_resource_uri,
        "endpoint_slug": endpoint_slug,
        "target_url": target_url,
        "ws_url": ws_url,
        "endpoint": endpoint_value,
        "resources": {
            "self": session_resource_uri,
            "endpoint": endpoint_resource_uri,
            "requests": requests_resource_uri
        },
        "next_actions": [
            format!("Send webhook traffic to endpoint slug `{endpoint_slug}`."),
            inspect_command
        ]
    })
}

pub fn listen_event_receipt(
    event: &TunnelEvent,
    endpoint_slug: &str,
    target_url: &str,
    endpoint: Option<&api::DebugEndpointSummary>,
) -> serde_json::Value {
    let (endpoint_resource_uri, requests_resource_uri, _) =
        endpoint_receipt_parts(endpoint_slug, endpoint);

    match event {
        TunnelEvent::Connecting => {
            listen_event_base("connecting", "connecting", endpoint_slug, target_url)
        }
        TunnelEvent::Connected => {
            let mut receipt =
                listen_event_base("connected", "connected", endpoint_slug, target_url);
            receipt["resources"] = serde_json::json!({
                "endpoint": endpoint_resource_uri,
                "requests": requests_resource_uri
            });
            receipt
        }
        TunnelEvent::WebhookReceived(request) => {
            let request_resource_uri = request_resource_uri(&request.id);
            let mut receipt =
                listen_event_base("webhook_received", "received", endpoint_slug, target_url);
            receipt["request_id"] = serde_json::json!(&request.id);
            receipt["resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["request_resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["endpoint_resource_uri"] = serde_json::json!(endpoint_resource_uri);
            receipt["request"] = serde_json::json!({
                "id": &request.id,
                "method": &request.method,
                "path": redact_url_query(request.path.as_deref().unwrap_or(&request.url)),
                "url": redact_url_query(&request.url),
                "remote_addr": &request.remote_addr,
                "content_length": request.content_length,
                "created_at": &request.created_at,
                "resource_uri": request_resource_uri
            });
            receipt["next_actions"] = serde_json::json!([format!(
                "hooklistener endpoint show-request <endpoint-id> {}",
                request.id
            )]);
            receipt
        }
        TunnelEvent::ForwardSuccess {
            request_id,
            target_url: forward_target_url,
            status,
            duration_ms,
        } => {
            let request_resource_uri = request_resource_uri(request_id);
            let mut receipt =
                listen_event_base("forward_succeeded", "succeeded", endpoint_slug, target_url);
            receipt["request_id"] = serde_json::json!(request_id);
            receipt["resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["request_resource_uri"] = serde_json::json!(request_resource_uri);
            receipt["target_url"] = serde_json::json!(redact_url_query(forward_target_url));
            receipt["status_code"] = serde_json::json!(status);
            receipt["duration_ms"] = serde_json::json!(duration_ms);
            receipt
        }
        TunnelEvent::ForwardError {
            request_id,
            target_url: forward_target_url,
            error,
            duration_ms,
        } => {
            let request_resource_uri = request_resource_uri(request_id);
            let mut receipt =
                listen_event_base("forward_failed", "failed", endpoint_slug, target_url);
            receipt["request_id"] = serde_json::json!(request_id);
            receipt["resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["request_resource_uri"] = serde_json::json!(request_resource_uri);
            receipt["target_url"] = serde_json::json!(redact_url_query(forward_target_url));
            receipt["error"] = serde_json::json!(error);
            receipt["duration_ms"] = serde_json::json!(duration_ms);
            receipt["next_actions"] = serde_json::json!([
                "Check that the local target URL is reachable from this machine."
            ]);
            receipt
        }
        TunnelEvent::ConnectionError(error) => {
            let mut receipt =
                listen_event_base("connection_error", "error", endpoint_slug, target_url);
            receipt["error"] = serde_json::json!(error);
            receipt["retryable"] = serde_json::json!(true);
            receipt["next_actions"] =
                serde_json::json!(["Wait for automatic reconnect or restart the command."]);
            receipt
        }
        TunnelEvent::Disconnected => {
            listen_event_base("disconnected", "disconnected", endpoint_slug, target_url)
        }
        TunnelEvent::Reconnecting {
            attempt,
            max_attempts,
            next_retry_in_secs,
        } => {
            let mut receipt =
                listen_event_base("reconnecting", "reconnecting", endpoint_slug, target_url);
            receipt["attempt"] = serde_json::json!(attempt);
            receipt["max_attempts"] = serde_json::json!(max_attempts);
            receipt["next_retry_in_secs"] = serde_json::json!(next_retry_in_secs);
            receipt
        }
        TunnelEvent::ReconnectFailed { reason } => {
            let mut receipt =
                listen_event_base("reconnect_failed", "failed", endpoint_slug, target_url);
            receipt["error"] = serde_json::json!(reason);
            receipt["retryable"] = serde_json::json!(false);
            receipt["next_actions"] = serde_json::json!([
                "Verify authentication, endpoint slug, and network connectivity."
            ]);
            receipt
        }
        _ => listen_event_base("ignored", "ignored", endpoint_slug, target_url),
    }
}

pub fn tunnel_started_receipt(
    host: &str,
    port: u16,
    organization_id: Option<&str>,
    slug: Option<&str>,
) -> serde_json::Value {
    let session_resource_uri = tunnel_session_resource_uri(host, port);
    let local_target_url = local_tunnel_target_url(host, port);

    serde_json::json!({
        "$schema": TUNNEL_RECEIPT_SCHEMA,
        "schema_version": 1,
        "type": "receipt",
        "event_id": uuid::Uuid::new_v4(),
        "sequence": 0,
        "event": "tunnel_started",
        "status": "starting",
        "command": "tunnel",
        "operation": "start_local_tunnel",
        "emitted_at": emitted_at(),
        "resource_uri": &session_resource_uri,
        "local_host": host,
        "local_port": port,
        "local_target_url": local_target_url,
        "organization_id": organization_id,
        "requested_slug": slug,
        "resources": {
            "self": session_resource_uri
        },
        "next_actions": ["Wait for a tunnel_established event before sending external traffic."]
    })
}

pub fn tunnel_event_receipt(
    event: &TunnelEvent,
    host: &str,
    port: u16,
    organization_id: Option<&str>,
    requested_slug: Option<&str>,
) -> serde_json::Value {
    match event {
        TunnelEvent::Connecting => {
            let mut receipt = tunnel_event_base("connecting", "connecting");
            receipt["resource_uri"] = serde_json::json!(tunnel_session_resource_uri(host, port));
            receipt["local_target_url"] = serde_json::json!(local_tunnel_target_url(host, port));
            receipt["organization_id"] = serde_json::json!(organization_id);
            receipt["requested_slug"] = serde_json::json!(requested_slug);
            receipt
        }
        TunnelEvent::TunnelEstablished {
            subdomain,
            tunnel_id,
            is_static,
        } => {
            let tunnel_resource_uri = tunnel_resource_uri(tunnel_id);
            let public_url = public_tunnel_url(subdomain);
            let mut receipt = tunnel_event_base("tunnel_established", "running");
            receipt["type"] = serde_json::json!("receipt");
            receipt["resource_uri"] = serde_json::json!(&tunnel_resource_uri);
            receipt["tunnel_id"] = serde_json::json!(tunnel_id);
            receipt["tunnel_resource_uri"] = serde_json::json!(&tunnel_resource_uri);
            receipt["subdomain"] = serde_json::json!(subdomain);
            receipt["public_url"] = serde_json::json!(public_url);
            receipt["local_host"] = serde_json::json!(host);
            receipt["local_port"] = serde_json::json!(port);
            receipt["local_target_url"] = serde_json::json!(local_tunnel_target_url(host, port));
            receipt["organization_id"] = serde_json::json!(organization_id);
            receipt["requested_slug"] = serde_json::json!(requested_slug);
            receipt["is_static"] = serde_json::json!(is_static);
            receipt["resources"] = serde_json::json!({
                "self": tunnel_resource_uri,
                "session": tunnel_session_resource_uri(host, port)
            });
            receipt["next_actions"] = serde_json::json!([
                format!("Send external traffic to {public_url}."),
                "Watch subsequent request_received and request_forwarded events."
            ]);
            receipt
        }
        TunnelEvent::RequestReceived {
            request_id,
            method,
            path,
            headers,
            body,
            query_string,
            replay,
        } => {
            let request_resource_uri = request_resource_uri(request_id);
            let mut receipt = tunnel_event_base("request_received", "received");
            receipt["request_id"] = serde_json::json!(request_id);
            receipt["replay"] = serde_json::json!(replay);
            receipt["resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["request_resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["method"] = serde_json::json!(method);
            receipt["path"] = serde_json::json!(path);
            receipt["query_string"] = serde_json::json!(redact_query_string(query_string));
            receipt["local_target_url"] = serde_json::json!(redact_url_query(
                &local_tunnel_request_target(host, port, path, query_string)
            ));
            receipt["headers"] = serde_json::json!(redact_tunnel_headers(headers));
            receipt["body_size"] = serde_json::json!(body.as_ref().map(String::len).unwrap_or(0));
            receipt
        }
        TunnelEvent::RequestForwarded {
            request_id,
            status,
            duration_ms,
            response_headers,
            response_body,
        } => {
            let request_resource_uri = request_resource_uri(request_id);
            let mut receipt = tunnel_event_base("request_forwarded", "succeeded");
            receipt["request_id"] = serde_json::json!(request_id);
            receipt["resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["request_resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["status_code"] = serde_json::json!(status);
            receipt["duration_ms"] = serde_json::json!(duration_ms);
            receipt["response_headers"] =
                serde_json::json!(redact_tunnel_headers(response_headers));
            receipt["response_body_size"] =
                serde_json::json!(response_body.as_ref().map(String::len).unwrap_or(0));
            receipt
        }
        TunnelEvent::RequestFailed { request_id, error } => {
            let request_resource_uri = request_resource_uri(request_id);
            let mut receipt = tunnel_event_base("request_failed", "failed");
            receipt["request_id"] = serde_json::json!(request_id);
            receipt["resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["request_resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["error"] = serde_json::json!(error);
            receipt["next_actions"] =
                serde_json::json!(["Check that the local target is running and reachable."]);
            receipt
        }
        TunnelEvent::StreamGap { dropped_events } => {
            let mut receipt = tunnel_event_base("stream_gap", "recoverable");
            receipt["resource_uri"] = serde_json::json!(tunnel_session_resource_uri(host, port));
            receipt["dropped_events"] = serde_json::json!(dropped_events);
            receipt["delivery_affected"] = serde_json::json!(false);
            receipt["next_actions"] = serde_json::json!([
                "Treat the request presentation stream as incomplete; relay responses remain active."
            ]);
            receipt
        }
        TunnelEvent::ConnectionError(error) => {
            let mut receipt = tunnel_event_base("connection_error", "error");
            receipt["resource_uri"] = serde_json::json!(tunnel_session_resource_uri(host, port));
            receipt["local_target_url"] = serde_json::json!(local_tunnel_target_url(host, port));
            receipt["error"] = serde_json::json!(error);
            receipt["retryable"] = serde_json::json!(true);
            receipt["next_actions"] =
                serde_json::json!(["Wait for automatic reconnect or restart the command."]);
            receipt
        }
        TunnelEvent::Disconnected => {
            let mut receipt = tunnel_event_base("disconnected", "disconnected");
            receipt["resource_uri"] = serde_json::json!(tunnel_session_resource_uri(host, port));
            receipt["local_target_url"] = serde_json::json!(local_tunnel_target_url(host, port));
            receipt
        }
        TunnelEvent::Reconnecting {
            attempt,
            max_attempts,
            next_retry_in_secs,
        } => {
            let mut receipt = tunnel_event_base("reconnecting", "reconnecting");
            receipt["resource_uri"] = serde_json::json!(tunnel_session_resource_uri(host, port));
            receipt["local_target_url"] = serde_json::json!(local_tunnel_target_url(host, port));
            receipt["attempt"] = serde_json::json!(attempt);
            receipt["max_attempts"] = serde_json::json!(max_attempts);
            receipt["next_retry_in_secs"] = serde_json::json!(next_retry_in_secs);
            receipt
        }
        TunnelEvent::ReconnectFailed { reason } => {
            let mut receipt = tunnel_event_base("reconnect_failed", "failed");
            receipt["resource_uri"] = serde_json::json!(tunnel_session_resource_uri(host, port));
            receipt["local_target_url"] = serde_json::json!(local_tunnel_target_url(host, port));
            receipt["error"] = serde_json::json!(reason);
            receipt["retryable"] = serde_json::json!(false);
            receipt["next_actions"] = serde_json::json!([
                "Verify authentication, organization scope, requested slug, and network connectivity."
            ]);
            receipt
        }
        TunnelEvent::BufferedSummary {
            count,
            oldest_captured_at,
        } => {
            let mut receipt = tunnel_event_base("buffered_summary", "buffered");
            receipt["resource_uri"] = serde_json::json!(tunnel_session_resource_uri(host, port));
            receipt["count"] = serde_json::json!(count);
            receipt["oldest_captured_at"] = serde_json::json!(oldest_captured_at);
            receipt
        }
        TunnelEvent::BufferedReplayed { capture_id, status } => {
            let request_resource_uri = request_resource_uri(capture_id);
            let mut receipt = tunnel_event_base("buffered_replayed", "succeeded");
            receipt["capture_id"] = serde_json::json!(capture_id);
            receipt["resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["request_resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["status_code"] = serde_json::json!(status);
            receipt
        }
        TunnelEvent::BufferedReplayFailed { capture_id, reason } => {
            let request_resource_uri = request_resource_uri(capture_id);
            let mut receipt = tunnel_event_base("buffered_replay_failed", "failed");
            receipt["capture_id"] = serde_json::json!(capture_id);
            receipt["resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["request_resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["error"] = serde_json::json!(reason);
            receipt["next_actions"] = serde_json::json!([
                "The request stays buffered. Check the local target and reconnect to retry."
            ]);
            receipt
        }
        _ => {
            let mut receipt = tunnel_event_base("ignored", "ignored");
            receipt["resource_uri"] = serde_json::json!(tunnel_session_resource_uri(host, port));
            receipt["local_target_url"] = serde_json::json!(local_tunnel_target_url(host, port));
            receipt
        }
    }
}

pub const TUNNEL_RECEIPT_SCHEMA: &str = "hooklistener.tunnel.receipt/1";

pub const TUNNEL_EVENT_SCHEMA: &str = "hooklistener.tunnel.event/1";

pub const LISTEN_EVENT_SCHEMA: &str = "hooklistener.listen.event/1";

pub const COMMAND_ERROR_SCHEMA: &str = "hooklistener.cli.error/1";
