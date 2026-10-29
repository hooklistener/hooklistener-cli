//! Local HTTP forwarding: request body decoding, header filtering, and bounded response reads.

use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use std::collections::HashMap;

use crate::tunnel::limits::{MAX_RAW_BODY_BYTES, TUNNEL_MAX_FRAME_BYTES, TunnelLimits};

/// Extract the string representation of a JSON value.
/// Returns the inner string for `Value::String`, otherwise uses `to_string()`.
pub(crate) fn json_value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        _ => v.to_string(),
    }
}

pub(crate) fn decode_request_body_limited(
    body_encoding: &str,
    raw_body: &str,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    let body = match body_encoding {
        "raw" => {
            if raw_body.len() > max_bytes {
                return Err(anyhow!(
                    "Tunnel request body exceeds advertised limit ({} > {} bytes)",
                    raw_body.len(),
                    max_bytes
                ));
            }
            raw_body.as_bytes().to_vec()
        }
        "base64" => {
            let max_encoded_bytes = base64::encoded_len(max_bytes, false).unwrap_or(usize::MAX);
            if raw_body.len() > max_encoded_bytes {
                return Err(anyhow!(
                    "Tunnel request body exceeds advertised limit (encoded body is too large)"
                ));
            }
            URL_SAFE_NO_PAD
                .decode(raw_body)
                .context("Tunnel request contains invalid base64url body data")?
        }
        encoding => return Err(anyhow!("Unsupported tunnel body encoding: {encoding}")),
    };

    if body.len() > max_bytes {
        return Err(anyhow!(
            "Tunnel request body exceeds advertised limit ({} > {} bytes)",
            body.len(),
            max_bytes
        ));
    }

    Ok(body)
}

/// Proxy and client-address headers that are never forwarded to a local target.
///
/// Whoever sends a webhook chooses every request header. The CLI connects to
/// the local target from loopback, an address many frameworks trust as a
/// reverse proxy, so a forwarded copy of one of these headers would let any
/// sender pick the client IP, scheme, host, or proxy chain the local app sees.
/// `listen` and `tunnel` drop them all; `tunnel` then sets `X-Forwarded-Host`
/// and `X-Forwarded-Proto` itself (see [`TrustedForwardedHeaders`]).
///
/// TUI replays, captured-request forwarding, and exported curl commands skip
/// the same list.
const UNTRUSTED_PROXY_HEADERS: &[&str] = &[
    // RFC 7239 and the de facto X-Forwarded-* family.
    "forwarded",
    "x-forwarded-for",
    "x-forwarded-host",
    "x-forwarded-proto",
    "x-forwarded-port",
    "x-forwarded-prefix",
    // Read as the request scheme by frameworks such as Rack and Spring.
    "x-forwarded-ssl",
    "x-forwarded-scheme",
    // Client address headers set by proxies and CDNs.
    "x-real-ip",
    "client-ip",
    "cf-connecting-ip",
    "true-client-ip",
    "x-client-ip",
    "x-originating-ip",
    "x-remote-ip",
    "x-remote-addr",
    // Proxy chain.
    "via",
];

/// Whether `key` names a sender-controlled proxy header (case-insensitive).
pub(crate) fn is_untrusted_proxy_header(key: &str) -> bool {
    UNTRUSTED_PROXY_HEADERS
        .iter()
        .any(|header| key.eq_ignore_ascii_case(header))
}

/// Whether a request header from the relay may be copied onto the local
/// request: drops framing and hop-by-hop headers, which reqwest sets from the
/// body actually sent, and every untrusted proxy header.
pub(crate) fn should_forward_request_header(key: &str) -> bool {
    const HEADERS_TO_DROP: &[&str] = &[
        "connection",
        "content-length",
        "host",
        "keep-alive",
        "proxy-connection",
        "te",
        "trailer",
        "transfer-encoding",
        "upgrade",
    ];

    !HEADERS_TO_DROP
        .iter()
        .any(|header| key.eq_ignore_ascii_case(header))
        && !is_untrusted_proxy_header(key)
}

/// `X-Forwarded-Host` and `X-Forwarded-Proto` values the CLI sets on `tunnel`
/// requests, so local apps that build absolute URLs see the public tunnel URL.
///
/// Both come from the public host the relay reports when the tunnel joins,
/// never from the incoming request. There is no `X-Forwarded-For`: the relay
/// protocol does not carry a client address the CLI could vouch for.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct TrustedForwardedHeaders {
    host: reqwest::header::HeaderValue,
    proto: reqwest::header::HeaderValue,
}

impl TrustedForwardedHeaders {
    /// Parses the join response's `subdomain`: a bare public host such as
    /// `abc123.hook.events` (served over HTTPS) or an `http(s)://` origin.
    /// Returns `None` for anything that is not a plain origin.
    pub(crate) fn from_public_host(public_host: &str) -> Option<Self> {
        let public_host = public_host.trim().trim_end_matches('/');
        let url = if public_host.starts_with("http://") || public_host.starts_with("https://") {
            reqwest::Url::parse(public_host)
        } else {
            reqwest::Url::parse(&format!("https://{public_host}"))
        }
        .ok()?;

        let proto = match url.scheme() {
            "https" => "https",
            "http" => "http",
            _ => return None,
        };
        if !url.username().is_empty()
            || url.password().is_some()
            || url.path() != "/"
            || url.query().is_some()
            || url.fragment().is_some()
        {
            return None;
        }
        let host = url.host_str()?;
        let host = match url.port() {
            Some(port) => format!("{host}:{port}"),
            None => host.to_string(),
        };

        Some(Self {
            host: reqwest::header::HeaderValue::from_str(&host).ok()?,
            proto: reqwest::header::HeaderValue::from_static(proto),
        })
    }

    pub(crate) fn apply(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request
            .header("x-forwarded-host", self.host.clone())
            .header("x-forwarded-proto", self.proto.clone())
    }
}

pub(crate) fn supported_tunnel_method(method: &str) -> Option<reqwest::Method> {
    reqwest::Method::from_bytes(method.as_bytes()).ok()
}

pub(crate) fn response_body_forbidden(method: &str, status: u16) -> bool {
    method.eq_ignore_ascii_case("HEAD") || matches!(status, 204 | 205 | 304)
}

pub(crate) fn ensure_response_chunk_allowed(method: &str, status: u16) -> Result<()> {
    if response_body_forbidden(method, status) {
        Err(anyhow!(
            "Local response included a body where HTTP forbids one"
        ))
    } else {
        Ok(())
    }
}

pub(crate) fn relayed_response_content_length(
    method: &str,
    status: u16,
    content_length: Option<u64>,
) -> Option<u64> {
    if response_body_forbidden(method, status) {
        Some(0)
    } else {
        content_length
    }
}

pub(crate) fn valid_reset_content_length(headers: &reqwest::header::HeaderMap) -> bool {
    let mut values = headers.get_all(reqwest::header::CONTENT_LENGTH).iter();

    match (values.next(), values.next()) {
        (None, None) => true,
        (Some(value), None) => value.as_bytes() == b"0",
        _ => false,
    }
}

pub(crate) fn response_headers_to_map(
    headers: &reqwest::header::HeaderMap,
) -> HashMap<String, String> {
    headers
        .iter()
        .map(|(key, value)| {
            (
                key.as_str().to_string(),
                value.to_str().unwrap_or("").to_string(),
            )
        })
        .collect()
}

pub(crate) fn response_headers_to_pairs(
    headers: &reqwest::header::HeaderMap,
) -> Vec<(String, String)> {
    headers
        .keys()
        .flat_map(|name| {
            headers.get_all(name).iter().map(move |value| {
                (
                    name.as_str().to_string(),
                    value.to_str().unwrap_or("").to_string(),
                )
            })
        })
        .collect()
}

pub(crate) fn response_headers_to_ordered_pairs(
    headers: &reqwest::header::HeaderMap,
) -> Vec<(String, String)> {
    response_headers_to_pairs(headers)
}

pub(crate) fn encode_response_body(bytes: &[u8]) -> (String, &'static str) {
    if bytes.len() <= MAX_RAW_BODY_BYTES
        && let Ok(text) = std::str::from_utf8(bytes)
    {
        return (text.to_string(), "raw");
    }

    (URL_SAFE_NO_PAD.encode(bytes), "base64")
}

pub(crate) fn response_header_bytes(headers: &reqwest::header::HeaderMap) -> usize {
    headers.iter().fold(0usize, |total, (name, value)| {
        total.saturating_add(name.as_str().len() + value.as_bytes().len() + 4)
    })
}

pub(crate) fn response_header_limit_error(
    headers: &reqwest::header::HeaderMap,
    limits: TunnelLimits,
) -> Option<String> {
    let item_count = headers.len();
    if item_count > limits.max_response_header_items {
        return Some(format!(
            "Local response headers exceed tunnel item limit ({} > {})",
            item_count, limits.max_response_header_items
        ));
    }

    let header_bytes = response_header_bytes(headers);
    (header_bytes > limits.max_response_header_bytes).then(|| {
        format!(
            "Local response headers exceed tunnel limit ({} > {} bytes)",
            header_bytes, limits.max_response_header_bytes
        )
    })
}

pub(crate) async fn read_response_body_limited(
    response: &mut reqwest::Response,
    max_bytes: usize,
) -> Result<Vec<u8>> {
    let content_length = response.content_length();
    if content_length.is_some_and(|length| length > max_bytes as u64) {
        return Err(anyhow!(
            "Local response body exceeds tunnel limit (max {max_bytes} bytes)"
        ));
    }

    let initial_capacity = content_length
        .and_then(|length| usize::try_from(length).ok())
        .unwrap_or(0)
        .min(TUNNEL_MAX_FRAME_BYTES);
    let mut bytes = Vec::with_capacity(initial_capacity);

    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| anyhow!("Failed to read local response: {}", error.without_url()))?
    {
        if bytes.len().saturating_add(chunk.len()) > max_bytes {
            return Err(anyhow!(
                "Local response body exceeds tunnel limit (max {max_bytes} bytes)"
            ));
        }
        bytes.extend_from_slice(&chunk);
    }

    Ok(bytes)
}

#[cfg(test)]
pub(crate) fn tunnel_http_client(timeout: std::time::Duration) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("Failed to build local tunnel HTTP client")
}
