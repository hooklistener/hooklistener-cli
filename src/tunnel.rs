use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use brotli::Decompressor as BrotliDecoder;
use encoding_rs::{Encoding, UTF_8};
use flate2::read::{DeflateDecoder, MultiGzDecoder, ZlibDecoder};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Cursor, Read};
use std::time::{Duration, SystemTime};
use tokio::sync::{mpsc, watch};
use tokio_tungstenite::{
    connect_async, connect_async_with_config,
    tungstenite::{Message, error::Error as WsError, http::StatusCode, protocol::WebSocketConfig},
};
use tracing::{debug, error, info, warn};

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsWrite = futures_util::stream::SplitSink<WsStream, Message>;

// A 256 MiB body expands to about 342 MiB as unpadded base64. The remaining
// space covers the Phoenix envelope and the advertised response-header limit.
const TUNNEL_MAX_WEBSOCKET_MESSAGE_BYTES: usize = 402_653_184;
const TUNNEL_PROTOCOL_VERSION: u64 = 2;
const TUNNEL_MAX_FRAME_BYTES: usize = 65_536;
const TUNNEL_MAX_RAW_CHUNK_BYTES: usize = 47_000;
const LEGACY_MAX_REQUEST_BODY_BYTES: usize = 10_485_760;
const LEGACY_MAX_RESPONSE_BODY_BYTES: usize = 7_000_000;
const LEGACY_MAX_RESPONSE_HEADER_BYTES: usize = 1_048_576;
const MAX_RAW_BODY_BYTES: usize = 1_048_576;
const UI_BODY_PREVIEW_BYTES: usize = 65_536;
const REDACTED_SECRET: &str = "[REDACTED]";

#[derive(Clone, Copy, Debug)]
struct TunnelLimits {
    max_request_body_bytes: usize,
    max_response_body_bytes: usize,
    max_response_header_bytes: usize,
    ordered_response_headers: bool,
}

impl TunnelLimits {
    fn from_join_response(response: &serde_json::Value) -> Self {
        let limits = response.get("limits");
        let advertised_body_limit = json_limit(limits, "max_body_bytes");

        Self {
            max_request_body_bytes: advertised_body_limit.unwrap_or(LEGACY_MAX_REQUEST_BODY_BYTES),
            max_response_body_bytes: advertised_body_limit
                .unwrap_or(LEGACY_MAX_RESPONSE_BODY_BYTES),
            max_response_header_bytes: json_limit(limits, "max_response_header_bytes")
                .unwrap_or(LEGACY_MAX_RESPONSE_HEADER_BYTES),
            ordered_response_headers: limits
                .and_then(|limits| limits.get("response_headers_format"))
                .and_then(|format| format.as_str())
                == Some("ordered_pairs"),
        }
    }
}

fn json_limit(limits: Option<&serde_json::Value>, key: &str) -> Option<usize> {
    limits?.get(key)?.as_u64()?.try_into().ok()
}

fn tunnel_websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(TUNNEL_MAX_FRAME_BYTES))
        .max_frame_size(Some(TUNNEL_MAX_FRAME_BYTES))
}

/// Extract the string representation of a JSON value.
/// Returns the inner string for `Value::String`, otherwise uses `to_string()`.
fn json_value_to_string(v: &serde_json::Value) -> String {
    match v {
        serde_json::Value::String(s) => s.clone(),
        _ => v.to_string(),
    }
}

fn should_forward_request_header(key: &str) -> bool {
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
}

fn response_headers_to_map(headers: &reqwest::header::HeaderMap) -> HashMap<String, String> {
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

fn response_headers_to_pairs(headers: &reqwest::header::HeaderMap) -> Vec<(String, String)> {
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

fn response_headers_to_ordered_pairs(
    headers: &reqwest::header::HeaderMap,
) -> Vec<(String, String)> {
    response_headers_to_pairs(headers)
}

fn encode_response_body(bytes: &[u8]) -> (String, &'static str) {
    if bytes.len() <= MAX_RAW_BODY_BYTES
        && let Ok(text) = std::str::from_utf8(bytes)
    {
        return (text.to_string(), "raw");
    }

    (URL_SAFE_NO_PAD.encode(bytes), "base64")
}

fn response_header_bytes(headers: &reqwest::header::HeaderMap) -> usize {
    headers.iter().fold(0usize, |total, (name, value)| {
        total.saturating_add(name.as_str().len() + value.as_bytes().len() + 4)
    })
}

fn tunnel_http_client(timeout: Duration) -> Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(timeout)
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .context("Failed to build local tunnel HTTP client")
}

struct DecodedBodyPreview {
    bytes: Vec<u8>,
    truncated: bool,
    content_encoded: bool,
}

pub(crate) fn body_preview(bytes: &[u8], headers: &HashMap<String, String>) -> Option<String> {
    if bytes.is_empty() {
        return None;
    }

    let decoded = match decode_body_preview(bytes, headers) {
        Ok(decoded) => decoded,
        Err(error) => {
            return Some(format!(
                "[body preview unavailable: {error}; {} bytes received]",
                bytes.len()
            ));
        }
    };

    let content_type = header_value(headers, "content-type");
    if !is_text_body(content_type, &decoded.bytes) {
        let content_type = content_type
            .and_then(|value| value.split(';').next())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .unwrap_or("unknown content type");
        let encoding = header_value(headers, "content-encoding")
            .map(str::trim)
            .filter(|value| !value.is_empty() && !value.eq_ignore_ascii_case("identity"));

        return Some(match encoding {
            Some(encoding) => format!(
                "[binary body: {} encoded bytes; content-type: {content_type}; content-encoding: {encoding}]",
                bytes.len()
            ),
            None => format!(
                "[binary body: {} bytes; content-type: {content_type}]",
                bytes.len()
            ),
        });
    }

    let mut preview = match decode_text(&decoded.bytes, content_type, decoded.truncated) {
        Ok(text) => sanitize_preview_text(&text),
        Err(error) => {
            return Some(format!(
                "[text body preview unavailable: {error}; {} bytes received]",
                bytes.len()
            ));
        }
    };

    if decoded.truncated {
        if decoded.content_encoded {
            preview.push_str(&format!(
                "\n… [decoded preview truncated; {} encoded bytes received]",
                bytes.len()
            ));
        } else {
            preview.push_str(&format!(
                "\n… [preview truncated; {} bytes total]",
                bytes.len()
            ));
        }
    }

    Some(preview)
}

fn decode_body_preview(
    bytes: &[u8],
    headers: &HashMap<String, String>,
) -> std::result::Result<DecodedBodyPreview, String> {
    let encodings = content_encodings(headers)?;
    let content_encoded = !encodings.is_empty();
    let mut reader: Box<dyn Read + '_> = Box::new(Cursor::new(bytes));

    // Content codings are listed in application order, so decoding wraps them
    // in reverse order. Keeping this as a reader chain avoids materializing an
    // unbounded intermediate body for stacked encodings.
    for encoding in encodings.iter().rev() {
        reader = match encoding.as_str() {
            "gzip" | "x-gzip" => Box::new(MultiGzDecoder::new(reader)),
            "deflate" => {
                let mut buffered = BufReader::new(reader);
                let is_zlib_wrapped = buffered
                    .fill_buf()
                    .map_err(|error| format!("could not inspect deflate body: {error}"))?
                    .get(..2)
                    .is_some_and(|prefix| is_zlib_header(prefix[0], prefix[1]));

                if is_zlib_wrapped {
                    Box::new(ZlibDecoder::new(buffered))
                } else {
                    // Some older servers use raw DEFLATE despite RFC 9110
                    // defining the coding as a zlib-wrapped stream.
                    Box::new(DeflateDecoder::new(buffered))
                }
            }
            "br" => Box::new(BrotliDecoder::new(reader, 4_096)),
            "zstd" => Box::new(
                zstd::stream::read::Decoder::new(reader)
                    .map_err(|error| format!("could not initialize zstd decoder: {error}"))?,
            ),
            _ => unreachable!("content_encodings validates supported values"),
        };
    }

    let mut limited = reader.take((UI_BODY_PREVIEW_BYTES + 1) as u64);
    let mut preview = Vec::with_capacity(UI_BODY_PREVIEW_BYTES.min(bytes.len()));
    limited
        .read_to_end(&mut preview)
        .map_err(|error| format!("could not decode response body: {error}"))?;
    let truncated = preview.len() > UI_BODY_PREVIEW_BYTES;
    preview.truncate(UI_BODY_PREVIEW_BYTES);

    Ok(DecodedBodyPreview {
        bytes: preview,
        truncated,
        content_encoded,
    })
}

fn content_encodings(
    headers: &HashMap<String, String>,
) -> std::result::Result<Vec<String>, String> {
    let Some(value) = header_value(headers, "content-encoding") else {
        return Ok(Vec::new());
    };

    value
        .split(',')
        .map(str::trim)
        .filter(|encoding| !encoding.is_empty() && !encoding.eq_ignore_ascii_case("identity"))
        .map(|encoding| {
            let normalized = encoding.to_ascii_lowercase();
            match normalized.as_str() {
                "gzip" | "x-gzip" | "deflate" | "br" | "zstd" => Ok(normalized),
                _ => Err(format!("unsupported content-encoding {encoding:?}")),
            }
        })
        .collect()
}

fn is_zlib_header(cmf: u8, flags: u8) -> bool {
    cmf & 0x0f == 8 && (u16::from(cmf) << 8 | u16::from(flags)) % 31 == 0
}

fn header_value<'a>(headers: &'a HashMap<String, String>, name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

fn is_text_body(content_type: Option<&str>, bytes: &[u8]) -> bool {
    match content_type.and_then(|value| value.split(';').next()) {
        Some(media_type) => is_text_media_type(media_type.trim()),
        None => looks_like_text(bytes),
    }
}

fn is_text_media_type(media_type: &str) -> bool {
    let media_type = media_type.to_ascii_lowercase();

    media_type.starts_with("text/")
        || media_type.ends_with("+json")
        || media_type.ends_with("+xml")
        || matches!(
            media_type.as_str(),
            "application/json"
                | "application/xml"
                | "application/javascript"
                | "application/x-javascript"
                | "application/graphql"
                | "application/x-www-form-urlencoded"
                | "application/sql"
                | "application/rtf"
                | "application/yaml"
                | "application/x-yaml"
                | "application/toml"
                | "application/x-ndjson"
                | "image/svg+xml"
        )
}

fn looks_like_text(bytes: &[u8]) -> bool {
    (0..=3.min(bytes.len())).any(|trim| std::str::from_utf8(&bytes[..bytes.len() - trim]).is_ok())
        && !bytes.contains(&0)
        && bytes
            .iter()
            .filter(|byte| byte.is_ascii_control() && !matches!(byte, b'\n' | b'\r' | b'\t'))
            .count()
            <= bytes.len() / 100
}

fn decode_text(
    bytes: &[u8],
    content_type: Option<&str>,
    truncated: bool,
) -> std::result::Result<String, String> {
    let charset = content_type.and_then(content_type_charset);
    let encoding = match charset {
        Some(charset) => Encoding::for_label(charset.as_bytes())
            .ok_or_else(|| format!("unsupported charset {charset:?}"))?,
        None => UTF_8,
    };

    let max_trim = if truncated { 8.min(bytes.len()) } else { 0 };
    for trim in 0..=max_trim {
        let candidate = &bytes[..bytes.len() - trim];
        let (decoded, _, had_errors) = encoding.decode(candidate);
        if !had_errors {
            return Ok(decoded.into_owned());
        }
    }

    Err(format!("body is not valid {} text", encoding.name()))
}

fn content_type_charset(content_type: &str) -> Option<&str> {
    content_type.split(';').skip(1).find_map(|parameter| {
        let (name, value) = parameter.split_once('=')?;
        name.trim()
            .eq_ignore_ascii_case("charset")
            .then(|| value.trim().trim_matches(['\"', '\'']))
    })
}

fn sanitize_preview_text(text: &str) -> String {
    text.chars()
        .map(|character| {
            if character.is_control() && !matches!(character, '\n' | '\r' | '\t') {
                '\u{fffd}'
            } else {
                character
            }
        })
        .collect()
}

fn with_forward_id(mut payload: serde_json::Value, forward_id: Option<&str>) -> serde_json::Value {
    if let Some(forward_id) = forward_id {
        payload["forward_id"] = serde_json::Value::String(forward_id.to_string());
    }

    payload
}

/// Phoenix Channel message structure
#[derive(Debug, Serialize, Deserialize)]
struct ChannelMessage {
    topic: String,
    event: String,
    payload: serde_json::Value,
    #[serde(rename = "ref")]
    reference: Option<String>,
}

const DIRECT_RESPONSE_MODE: &str = "direct_response";
const CAPTURE_FORWARD_MODE: &str = "capture_forward";

struct TunnelStreamAssembler {
    stream_id: String,
    direction: String,
    deadline_unix_ms: u64,
    total_bytes: usize,
    frame_count: usize,
    next_sequence: usize,
    data: Vec<u8>,
}

impl TunnelStreamAssembler {
    fn from_start(payload: &serde_json::Value, expected_direction: &str) -> Result<Self> {
        let version = payload
            .get("version")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| anyhow!("Framed stream is missing a protocol version"))?;
        if version != TUNNEL_PROTOCOL_VERSION {
            return Err(anyhow!("Unsupported tunnel framing version: {version}"));
        }

        let direction = payload
            .get("direction")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow!("Framed stream is missing a direction"))?;
        if direction != expected_direction {
            return Err(anyhow!("Unexpected tunnel stream direction: {direction}"));
        }

        let stream_id = payload
            .get("stream_id")
            .and_then(serde_json::Value::as_str)
            .filter(|stream_id| !stream_id.is_empty())
            .ok_or_else(|| anyhow!("Framed stream is missing an id"))?
            .to_string();
        let total_bytes = json_usize(payload, "total_bytes")?;
        if total_bytes > TUNNEL_MAX_WEBSOCKET_MESSAGE_BYTES {
            return Err(anyhow!("Tunnel stream exceeds the advertised limit"));
        }

        let frame_count = json_usize(payload, "frame_count")?;
        let expected_frame_count = total_bytes.div_ceil(TUNNEL_MAX_RAW_CHUNK_BYTES).max(1);
        if frame_count != expected_frame_count {
            return Err(anyhow!("Tunnel stream has an invalid frame count"));
        }

        let deadline_unix_ms = payload
            .get("deadline_unix_ms")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| anyhow!("Framed stream is missing its deadline"))?;
        if deadline_unix_ms <= unix_time_ms() {
            return Err(anyhow!("Tunnel stream deadline has expired"));
        }

        Ok(Self {
            stream_id,
            direction: direction.to_string(),
            deadline_unix_ms,
            total_bytes,
            frame_count,
            next_sequence: 0,
            data: Vec::new(),
        })
    }

    fn append(&mut self, payload: &serde_json::Value) -> Result<Option<serde_json::Value>> {
        if payload.get("version").and_then(serde_json::Value::as_u64)
            != Some(TUNNEL_PROTOCOL_VERSION)
            || payload.get("stream_id").and_then(serde_json::Value::as_str)
                != Some(self.stream_id.as_str())
            || payload.get("direction").and_then(serde_json::Value::as_str)
                != Some(self.direction.as_str())
        {
            return Err(anyhow!("Tunnel frame does not match its stream"));
        }

        let sequence = json_usize(payload, "sequence")?;
        if sequence != self.next_sequence {
            return Err(anyhow!(
                "Out-of-order tunnel frame: expected {}, got {sequence}",
                self.next_sequence
            ));
        }

        let encoded = payload
            .get("data")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| anyhow!("Tunnel frame is missing data"))?;
        let chunk = URL_SAFE_NO_PAD
            .decode(encoded)
            .context("Tunnel frame contains invalid base64url data")?;
        if chunk.len() > TUNNEL_MAX_RAW_CHUNK_BYTES
            || self.data.len().saturating_add(chunk.len()) > self.total_bytes
        {
            return Err(anyhow!("Tunnel frame exceeds the 64 KiB framing contract"));
        }

        self.data.extend_from_slice(&chunk);
        self.next_sequence += 1;
        let final_frame = payload
            .get("final")
            .and_then(serde_json::Value::as_bool)
            .ok_or_else(|| anyhow!("Tunnel frame is missing its final marker"))?;

        if final_frame {
            if self.data.len() != self.total_bytes || self.next_sequence != self.frame_count {
                return Err(anyhow!("Tunnel stream ended before all bytes arrived"));
            }

            return Ok(Some(
                serde_json::from_slice(&self.data)
                    .context("Tunnel stream contains invalid JSON")?,
            ));
        }

        if self.data.len() == self.total_bytes {
            return Err(anyhow!("Tunnel stream omitted its final marker"));
        }

        Ok(None)
    }
}

struct OutboundTunnelStream {
    stream_id: String,
    direction: &'static str,
    deadline_unix_ms: u64,
    encoded: Vec<u8>,
    offset: usize,
    sequence: usize,
    frame_count: usize,
}

impl OutboundTunnelStream {
    fn new(
        stream_id: &str,
        direction: &'static str,
        deadline_unix_ms: u64,
        payload: &serde_json::Value,
    ) -> Result<Self> {
        let encoded = serde_json::to_vec(payload)?;
        if encoded.len() > TUNNEL_MAX_WEBSOCKET_MESSAGE_BYTES {
            return Err(anyhow!("Tunnel stream exceeds the advertised limit"));
        }

        let frame_count = encoded.len().div_ceil(TUNNEL_MAX_RAW_CHUNK_BYTES).max(1);
        Ok(Self {
            stream_id: stream_id.to_string(),
            direction,
            deadline_unix_ms,
            encoded,
            offset: 0,
            sequence: 0,
            frame_count,
        })
    }

    fn start_message(&self, topic: &str) -> ChannelMessage {
        ChannelMessage {
            topic: topic.to_string(),
            event: "tunnel_stream_start".to_string(),
            payload: serde_json::json!({
                "version": TUNNEL_PROTOCOL_VERSION,
                "stream_id": self.stream_id,
                "direction": self.direction,
                "deadline_unix_ms": self.deadline_unix_ms,
                "total_bytes": self.encoded.len(),
                "frame_count": self.frame_count,
                "frame_encoding": "base64url",
            }),
            reference: None,
        }
    }

    fn next_frame(&mut self, topic: &str) -> Result<Option<ChannelMessage>> {
        if self.offset == self.encoded.len() && self.sequence > 0 {
            return Ok(None);
        }

        let end = self
            .offset
            .saturating_add(TUNNEL_MAX_RAW_CHUNK_BYTES)
            .min(self.encoded.len());
        let chunk = &self.encoded[self.offset..end];
        let final_frame = end == self.encoded.len();
        let message = ChannelMessage {
            topic: topic.to_string(),
            event: "tunnel_stream_frame".to_string(),
            payload: serde_json::json!({
                "version": TUNNEL_PROTOCOL_VERSION,
                "stream_id": self.stream_id,
                "direction": self.direction,
                "sequence": self.sequence,
                "final": final_frame,
                "data": URL_SAFE_NO_PAD.encode(chunk),
            }),
            reference: None,
        };

        if serde_json::to_vec(&message)?.len() > TUNNEL_MAX_FRAME_BYTES {
            return Err(anyhow!("Serialized tunnel frame exceeds 64 KiB"));
        }

        self.offset = end;
        self.sequence += 1;
        Ok(Some(message))
    }
}

fn json_usize(payload: &serde_json::Value, key: &str) -> Result<usize> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| value.try_into().ok())
        .ok_or_else(|| anyhow!("Tunnel stream has invalid {key}"))
}

fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

fn required_string(payload: &serde_json::Value, key: &str) -> Result<String> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("Tunnel request is missing {key}"))
}

fn ordered_header_pairs(value: Option<&serde_json::Value>) -> Result<Vec<(String, String)>> {
    match value {
        None => Ok(Vec::new()),
        Some(serde_json::Value::Array(headers)) => headers
            .iter()
            .map(|header| {
                let pair = header
                    .as_array()
                    .filter(|pair| pair.len() == 2)
                    .ok_or_else(|| anyhow!("Tunnel header is not an ordered name/value pair"))?;
                let name = pair[0]
                    .as_str()
                    .ok_or_else(|| anyhow!("Tunnel header name is not text"))?;
                let value = pair[1]
                    .as_str()
                    .ok_or_else(|| anyhow!("Tunnel header value is not text"))?;
                Ok((name.to_string(), value.to_string()))
            })
            .collect(),
        Some(serde_json::Value::Object(headers)) => Ok(headers
            .iter()
            .map(|(name, value)| (name.clone(), json_value_to_string(value)))
            .collect()),
        Some(_) => Err(anyhow!("Tunnel headers have an unsupported shape")),
    }
}

async fn send_channel_message(
    outbound_tx: &mpsc::Sender<Message>,
    message: ChannelMessage,
) -> Result<()> {
    let json = serde_json::to_vec(&message)?;
    if json.len() > TUNNEL_MAX_FRAME_BYTES {
        return Err(anyhow!("Tunnel channel message exceeds 64 KiB"));
    }

    outbound_tx
        .send(Message::Text(String::from_utf8(json)?.into()))
        .await
        .context("Tunnel connection closed before framed message was sent")?;
    Ok(())
}

fn validate_framing_contract(response: &serde_json::Value) -> Result<()> {
    let framing = response
        .get("framing")
        .ok_or_else(|| anyhow!("Server did not advertise bounded tunnel framing"))?;

    if framing.get("version").and_then(serde_json::Value::as_u64) != Some(TUNNEL_PROTOCOL_VERSION)
        || framing
            .get("max_frame_bytes")
            .and_then(serde_json::Value::as_u64)
            != Some(TUNNEL_MAX_FRAME_BYTES as u64)
        || framing
            .get("queue_depth_frames")
            .and_then(serde_json::Value::as_u64)
            != Some(1)
    {
        return Err(anyhow!(
            "Server advertised an incompatible framing contract"
        ));
    }

    Ok(())
}

fn listen_join_payload() -> serde_json::Value {
    serde_json::json!({"mode": CAPTURE_FORWARD_MODE})
}

fn tunnel_join_payload(
    local_port: u16,
    organization_id: Option<&str>,
    slug: Option<&str>,
) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "mode": DIRECT_RESPONSE_MODE,
        "protocol_version": TUNNEL_PROTOCOL_VERSION,
        "local_port": local_port,
    });

    if let Some(organization_id) = organization_id {
        payload["organization_id"] = serde_json::Value::String(organization_id.to_string());
    }

    if let Some(slug) = slug {
        payload["slug"] = serde_json::Value::String(slug.to_string());
    }

    payload
}

fn validate_join_mode(response: &serde_json::Value, expected: &str) -> Result<()> {
    match response.get("mode").and_then(|mode| mode.as_str()) {
        Some(mode) if mode == expected => Ok(()),
        Some(mode) => Err(anyhow!(
            "Channel join failed: Server activated incompatible mode '{mode}' (expected '{expected}'); no requests were forwarded"
        )),
        None => Err(anyhow!(
            "Channel join failed: Server did not confirm activation mode '{expected}'; upgrade the Hooklistener service before retrying"
        )),
    }
}

/// Webhook request received from the server (Tunnel format)
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct TunnelWebhookRequest {
    pub id: String,
    #[serde(default)]
    pub forward_id: Option<String>,
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub query_params: HashMap<String, serde_json::Value>,
    #[serde(default)]
    pub headers: HashMap<String, serde_json::Value>,
    pub body: Option<String>,
}

#[derive(Debug)]
pub enum TunnelEvent {
    Connecting,
    Connected,
    TunnelEstablished {
        subdomain: String,
        tunnel_id: String,
        is_static: bool,
    },
    ConnectionError(String),
    Disconnected,
    RequestReceived {
        request_id: String,
        method: String,
        path: String,
        headers: HashMap<String, String>,
        body: Option<String>,
        query_string: String,
        replay: bool,
    },
    RequestForwarded {
        request_id: String,
        status: u16,
        duration_ms: u64,
        response_headers: HashMap<String, String>,
        response_body: Option<String>,
    },
    RequestFailed {
        request_id: String,
        error: String,
    },
    ReplayCompleted {
        request_id: String,
        status: u16,
        duration_ms: u64,
    },
    ReplayFailed {
        request_id: String,
        error: String,
    },
    BufferedSummary {
        count: u64,
        oldest_captured_at: Option<String>,
    },
    BufferedReplayed {
        capture_id: String,
        status: u16,
    },
    BufferedReplayFailed {
        capture_id: String,
        reason: String,
    },
    WebhookReceived(Box<crate::models::WebhookRequest>),
    ForwardSuccess {
        request_id: String,
        target_url: String,
        status: u16,
        duration_ms: u64,
    },
    ForwardError {
        request_id: String,
        target_url: String,
        error: String,
        duration_ms: u64,
    },
    Reconnecting {
        attempt: u32,
        max_attempts: u32,
        next_retry_in_secs: u64,
    },
    ReconnectFailed {
        reason: String,
    },
}

/// Configuration for reconnection behavior
pub struct ReconnectConfig {
    pub max_retries: u32,
    pub initial_delay_ms: u64,
    pub max_delay_ms: u64,
    pub jitter_factor: f64,
}

impl Default for ReconnectConfig {
    fn default() -> Self {
        Self {
            max_retries: 10,
            initial_delay_ms: 1000,
            max_delay_ms: 60000,
            jitter_factor: 0.3,
        }
    }
}

fn websocket_endpoint(base_url: &str, path: &str) -> String {
    format!(
        "{}/{}",
        base_url
            .replace("https://", "wss://")
            .replace("http://", "ws://"),
        path.trim_start_matches('/')
    )
}

/// Build a WebSocket URL from a base HTTP(S) URL.
pub fn build_ws_url(base_url: &str, token: &str, path: &str) -> String {
    format!("{}?token={token}", websocket_endpoint(base_url, path))
}

fn redact_access_token(message: &str, access_token: &str) -> String {
    if access_token.is_empty() {
        message.to_string()
    } else {
        message.replace(access_token, REDACTED_SECRET)
    }
}

/// Build a target URL for forwarding, appending path and optional query params
#[cfg(test)]
pub fn build_forward_target(
    target_url: &str,
    path: &str,
    query_params: &HashMap<String, serde_json::Value>,
) -> String {
    let base = format!("{}{}", target_url, path);
    if query_params.is_empty() {
        base
    } else {
        let query_string: Vec<String> = query_params
            .iter()
            .map(|(k, v)| {
                let value_str = match v {
                    serde_json::Value::String(s) => s.clone(),
                    serde_json::Value::Number(n) => n.to_string(),
                    serde_json::Value::Bool(b) => b.to_string(),
                    _ => v.to_string(),
                };
                format!("{}={}", k, value_str)
            })
            .collect();
        format!("{}?{}", base, query_string.join("&"))
    }
}

/// Determine if an error message represents a fatal (non-retryable) error
pub fn is_fatal_error(error_msg: &str) -> bool {
    let lower = error_msg.to_lowercase();
    lower.contains("authentication failed")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
        || lower.contains("401")
        || lower.contains("403")
        || lower.contains("endpoint not found")
        || lower.contains("channel join failed")
        || lower.contains("static tunnel slug not found")
        || lower.contains("belongs to another organization")
        || lower.contains("not a member of the specified organization")
        || lower.contains("user has no organizations")
        || lower.contains("tunnel limit reached")
        || lower.contains("tunnel route has expired")
        || lower.contains("tunnel route has been revoked")
}

/// Calculate backoff duration with exponential backoff and jitter
pub fn calculate_backoff(attempt: u32, config: &ReconnectConfig) -> Duration {
    let base_delay = config.initial_delay_ms as f64 * 2_f64.powi(attempt.saturating_sub(1) as i32);
    let capped_delay = base_delay.min(config.max_delay_ms as f64);

    // Simple jitter using SystemTime nanos
    let nanos = SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .subsec_nanos();
    let jitter_range = capped_delay * config.jitter_factor;
    let jitter = (nanos as f64 / u32::MAX as f64) * jitter_range * 2.0 - jitter_range;

    let final_delay = (capped_delay + jitter).max(100.0);
    Duration::from_millis(final_delay as u64)
}

/// Tunnel client for WebSocket connection to Hooklistener server
pub struct TunnelClient {
    access_token_rx: watch::Receiver<String>,
    endpoint_slug: String,
    target_url: String,
    base_url: String,
    event_tx: mpsc::Sender<TunnelEvent>,
}

impl TunnelClient {
    pub fn new(
        access_token_rx: watch::Receiver<String>,
        endpoint_slug: String,
        target_url: String,
        base_url: Option<String>,
        event_tx: mpsc::Sender<TunnelEvent>,
    ) -> Self {
        // Check environment variable for local development
        let base_url = base_url
            .or_else(|| std::env::var("HOOKLISTENER_WS_URL").ok())
            .unwrap_or_else(|| "wss://api.hooklistener.com".to_string());

        Self {
            access_token_rx,
            endpoint_slug,
            target_url,
            base_url,
            event_tx,
        }
    }

    /// Connect to WebSocket and start listening for webhook events
    pub async fn connect_and_listen(&self) -> Result<()> {
        info!(
            endpoint = %self.endpoint_slug,
            target = %self.target_url,
            "Connecting to WebSocket tunnel"
        );

        // Build WebSocket URL with auth token
        let access_token = self.access_token_rx.borrow().clone();
        let ws_url = build_ws_url(&self.base_url, &access_token, "socket/websocket");

        debug!(
            endpoint = %websocket_endpoint(&self.base_url, "socket/websocket"),
            "Connecting to WebSocket"
        );

        // Connect to WebSocket
        let (ws_stream, _) = match connect_async(&ws_url).await {
            Ok(stream) => stream,
            Err(e) => match e {
                WsError::Http(response) => match response.status() {
                    StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
                        let msg = "Authentication failed: The token is invalid or expired.";
                        let _ = self
                            .event_tx
                            .send(TunnelEvent::ConnectionError(msg.to_string()))
                            .await;
                        return Err(anyhow!(msg));
                    }
                    StatusCode::NOT_FOUND => {
                        let msg = format!("Endpoint not found: '{}'.", self.endpoint_slug);
                        let _ = self
                            .event_tx
                            .send(TunnelEvent::ConnectionError(msg.clone()))
                            .await;
                        return Err(anyhow!(msg));
                    }
                    status => {
                        let msg = format!("Connection failed with HTTP status: {}", status);
                        let _ = self
                            .event_tx
                            .send(TunnelEvent::ConnectionError(msg.clone()))
                            .await;
                        return Err(anyhow!(msg));
                    }
                },
                WsError::Io(e) => {
                    let msg = format!("Connection refused: {}.", e);
                    let _ = self
                        .event_tx
                        .send(TunnelEvent::ConnectionError(msg.clone()))
                        .await;
                    return Err(anyhow!(msg));
                }
                _ => {
                    let detail = redact_access_token(&e.to_string(), &access_token);
                    let msg = format!("Failed to connect to WebSocket: {detail}");
                    let _ = self
                        .event_tx
                        .send(TunnelEvent::ConnectionError(msg.clone()))
                        .await;
                    return Err(anyhow!(msg));
                }
            },
        };

        info!("WebSocket connected successfully");

        let (mut write, mut read) = ws_stream.split();

        // Join the CLI tunnel channel
        let channel_topic = format!("cli:tunnel:{}", self.endpoint_slug);
        let join_message = ChannelMessage {
            topic: channel_topic.clone(),
            event: "phx_join".to_string(),
            payload: listen_join_payload(),
            reference: Some("1".to_string()),
        };

        let join_json = serde_json::to_string(&join_message)?;
        write
            .send(Message::Text(join_json.into()))
            .await
            .context("Failed to send join message")?;

        // Wait for join confirmation
        let mut joined = false;
        while !joined {
            match tokio::time::timeout(Duration::from_secs(5), read.next()).await {
                Ok(Some(msg_result)) => match msg_result {
                    Ok(Message::Text(text)) => {
                        let msg: ChannelMessage = serde_json::from_str(&text)?;
                        if msg.event == "phx_reply"
                            && msg.reference.as_deref() == Some("1")
                            && let Some(status) = msg.payload.get("status")
                        {
                            if status == "ok" {
                                let Some(response) = msg.payload.get("response") else {
                                    let error = anyhow!("Channel join response was missing");
                                    let _ = self
                                        .event_tx
                                        .send(TunnelEvent::ConnectionError(error.to_string()))
                                        .await;
                                    return Err(error);
                                };

                                if let Err(error) =
                                    validate_join_mode(response, CAPTURE_FORWARD_MODE)
                                {
                                    let reason = error.to_string();
                                    let _ = self
                                        .event_tx
                                        .send(TunnelEvent::ConnectionError(reason.clone()))
                                        .await;
                                    return Err(error);
                                }

                                let _ = self.event_tx.send(TunnelEvent::Connected).await;
                                info!(channel = %channel_topic, "Joined channel");
                                joined = true;
                            } else {
                                let reason = msg
                                    .payload
                                    .get("response")
                                    .and_then(|r| r.get("reason"))
                                    .and_then(|r| r.as_str())
                                    .unwrap_or("Unknown error");
                                let _ = self
                                    .event_tx
                                    .send(TunnelEvent::ConnectionError(reason.to_string()))
                                    .await;
                                return Err(anyhow!("Channel join failed: {}", reason));
                            }
                        }
                    }
                    Ok(Message::Ping(data)) => {
                        write.send(Message::Pong(data)).await?;
                    }
                    Ok(Message::Close(frame)) => {
                        return Err(anyhow!("WebSocket closed during join: {:?}", frame));
                    }
                    Err(e) => return Err(anyhow!("WebSocket error during join: {}", e)),
                    _ => {}
                },
                Ok(None) => return Err(anyhow!("WebSocket stream ended during join")),
                Err(_) => return Err(anyhow!("Timeout waiting for channel join response")),
            }
        }

        // Track last heartbeat time and counter
        let mut last_heartbeat = tokio::time::Instant::now();
        let mut heartbeat_counter = 2;
        let heartbeat_interval = Duration::from_secs(30);

        // Listen for messages
        loop {
            // Check if we need to send a heartbeat
            if last_heartbeat.elapsed() >= heartbeat_interval {
                let heartbeat = ChannelMessage {
                    topic: "phoenix".to_string(),
                    event: "heartbeat".to_string(),
                    payload: serde_json::json!({}),
                    reference: Some(heartbeat_counter.to_string()),
                };
                heartbeat_counter += 1;

                if let Ok(json) = serde_json::to_string(&heartbeat)
                    && let Err(e) = write.send(Message::Text(json.into())).await
                {
                    error!("Failed to send heartbeat: {}", e);
                    break;
                }
                last_heartbeat = tokio::time::Instant::now();
            }

            // Use timeout to allow heartbeat checks
            match tokio::time::timeout(Duration::from_millis(100), read.next()).await {
                Ok(Some(msg)) => match msg {
                    Ok(Message::Text(text)) => {
                        if let Err(e) = self.handle_message(&text, &mut write).await {
                            error!("Error handling message: {}", e);
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        info!("WebSocket closed: {:?}", frame);
                        let _ = self
                            .event_tx
                            .send(TunnelEvent::ConnectionError(
                                "WebSocket connection closed".to_string(),
                            ))
                            .await;
                        break;
                    }
                    Ok(Message::Ping(data)) => {
                        debug!("Received ping, sending pong");
                        if let Err(e) = write.send(Message::Pong(data)).await {
                            error!("Failed to send pong: {}", e);
                            break;
                        }
                    }
                    Ok(_) => {
                        // Ignore other message types
                    }
                    Err(e) => {
                        error!("WebSocket error: {}", e);
                        let _ = self
                            .event_tx
                            .send(TunnelEvent::ConnectionError(format!(
                                "WebSocket error: {}",
                                e
                            )))
                            .await;
                        break;
                    }
                },
                Ok(None) => {
                    warn!("WebSocket stream ended");
                    let _ = self
                        .event_tx
                        .send(TunnelEvent::ConnectionError(
                            "WebSocket stream ended".to_string(),
                        ))
                        .await;
                    break;
                }
                Err(_) => {
                    // Timeout - continue to check heartbeat
                    continue;
                }
            }
        }

        Ok(())
    }

    async fn handle_message(&self, text: &str, write: &mut WsWrite) -> Result<()> {
        let msg: ChannelMessage = serde_json::from_str(text)?;

        debug!(
            topic = %msg.topic,
            event = %msg.event,
            "Received message"
        );

        match msg.event.as_str() {
            "phx_reply" => {
                // Already handled join reply, ignoring subsequent ones for now
            }
            "webhook_received" => {
                // New webhook to forward
                if let Some(request_data) = msg.payload.get("request") {
                    match serde_json::from_value::<TunnelWebhookRequest>(request_data.clone()) {
                        Ok(request) => {
                            // Convert to model WebhookRequest for UI
                            let model_request = crate::models::WebhookRequest {
                                id: request.id.clone(),
                                timestamp: chrono::Utc::now().timestamp(),
                                remote_addr: "Tunnel".to_string(),
                                headers: request
                                    .headers
                                    .iter()
                                    .map(|(k, v)| (k.clone(), json_value_to_string(v)))
                                    .collect(),
                                content_length: request
                                    .body
                                    .as_ref()
                                    .map(|b| b.len() as i64)
                                    .unwrap_or(0),
                                method: request.method.clone(),
                                url: request.path.clone(),
                                path: Some(request.path.clone()),
                                query_params: request
                                    .query_params
                                    .iter()
                                    .map(|(k, v)| (k.clone(), json_value_to_string(v)))
                                    .collect(),
                                created_at: chrono::Utc::now().to_rfc3339(),
                                body_preview: request.body.clone(),
                                body: request.body.clone(),
                            };

                            // Notify UI
                            let _ = self
                                .event_tx
                                .send(TunnelEvent::WebhookReceived(Box::new(model_request)))
                                .await;

                            self.forward_webhook(request, write).await?;
                        }
                        Err(e) => {
                            let err_msg =
                                format!("Invalid webhook payload: {}. Data: {}", e, request_data);
                            error!("{}", err_msg);
                            let _ = self
                                .event_tx
                                .send(TunnelEvent::ConnectionError(err_msg.clone()))
                                .await;
                            // We don't return error here to keep connection alive, just log/notify
                        }
                    }
                }
            }
            _ => {
                debug!("Unhandled event: {}", msg.event);
            }
        }

        Ok(())
    }

    async fn forward_webhook(
        &self,
        request: TunnelWebhookRequest,
        write: &mut WsWrite,
    ) -> Result<()> {
        info!(
            request_id = %request.id,
            method = %request.method,
            path = %request.path,
            "Forwarding webhook to local server"
        );

        // Build target URL
        let target = format!("{}{}", self.target_url, request.path);

        // Add query params if present
        let target_with_query = if !request.query_params.is_empty() {
            let query_string: Vec<String> = request
                .query_params
                .iter()
                .map(|(k, v)| format!("{}={}", k, json_value_to_string(v)))
                .collect();
            format!("{}?{}", target, query_string.join("&"))
        } else {
            target
        };

        // Create HTTP client
        let client = reqwest::Client::new();

        // Build request with method
        let mut req_builder = match request.method.as_str() {
            "GET" => client.get(&target_with_query),
            "POST" => client.post(&target_with_query),
            "PUT" => client.put(&target_with_query),
            "DELETE" => client.delete(&target_with_query),
            "PATCH" => client.patch(&target_with_query),
            "HEAD" => client.head(&target_with_query),
            _ => {
                warn!("Unsupported HTTP method: {}", request.method);
                return Ok(());
            }
        };

        // Add headers. reqwest sets request framing headers from the body we actually send.
        for (key, value) in &request.headers {
            if should_forward_request_header(key) {
                req_builder = req_builder.header(key, json_value_to_string(value));
            }
        }

        // Add body if present
        if let Some(body) = &request.body {
            req_builder = req_builder.body(body.clone());
        }

        // Send request
        let start_time = std::time::Instant::now();
        match req_builder.send().await {
            Ok(response) => {
                let status = response.status();
                let status_code = status.as_u16();
                let response_headers = response_headers_to_map(response.headers());
                let response_bytes = response.bytes().await.unwrap_or_default();
                let (response_body, response_body_encoding) = encode_response_body(&response_bytes);
                let duration_ms = start_time.elapsed().as_millis() as u64;

                info!(
                    request_id = %request.id,
                    status = %status,
                    "Request forwarded successfully"
                );

                let _ = self
                    .event_tx
                    .send(TunnelEvent::ForwardSuccess {
                        request_id: request.id.clone(),
                        target_url: target_with_query.clone(),
                        status: status_code,
                        duration_ms,
                    })
                    .await;

                // Send acknowledgment back to server
                let payload = with_forward_id(
                    serde_json::json!({
                        "request_id": &request.id,
                        "status": "proxied",
                        "proxied_to": &target_with_query,
                        "status_code": status_code,
                        "response_headers": response_headers,
                        "response_body": response_body,
                        "response_body_encoding": response_body_encoding,
                        "duration_ms": duration_ms,
                    }),
                    request.forward_id.as_deref(),
                );

                self.send_request_ack(write, payload).await?;
            }
            Err(e) => {
                let duration_ms = start_time.elapsed().as_millis() as u64;

                let error_message = e.to_string();

                error!(
                    request_id = %request.id,
                    error = %error_message,
                    "Failed to forward request"
                );

                let _ = self
                    .event_tx
                    .send(TunnelEvent::ForwardError {
                        request_id: request.id.clone(),
                        target_url: target_with_query.clone(),
                        error: error_message.clone(),
                        duration_ms,
                    })
                    .await;

                // Send error acknowledgment
                let payload = with_forward_id(
                    serde_json::json!({
                        "request_id": &request.id,
                        "status": "error",
                        "proxied_to": &target_with_query,
                        "error": error_message,
                        "duration_ms": duration_ms,
                    }),
                    request.forward_id.as_deref(),
                );

                self.send_request_ack(write, payload).await?;
            }
        }

        Ok(())
    }

    async fn send_request_ack(
        &self,
        write: &mut WsWrite,
        payload: serde_json::Value,
    ) -> Result<()> {
        let ack_message = ChannelMessage {
            topic: format!("cli:tunnel:{}", self.endpoint_slug),
            event: "request_ack".to_string(),
            payload,
            reference: None,
        };

        let ack_json = serde_json::to_string(&ack_message)?;
        write.send(Message::Text(ack_json.into())).await?;
        Ok(())
    }

    /// Connect with automatic reconnection on recoverable errors
    pub async fn connect_with_reconnect(&self, config: ReconnectConfig) -> Result<()> {
        let mut attempt: u32 = 0;

        loop {
            let start = tokio::time::Instant::now();
            let result = self.connect_and_listen().await;

            match result {
                Ok(()) => {
                    // Clean disconnect, try reconnecting
                    if start.elapsed() > Duration::from_secs(5) {
                        attempt = 0; // Reset if connection lasted > 5s
                    }
                }
                Err(ref e) => {
                    let err_msg = e.to_string();
                    if is_fatal_error(&err_msg) {
                        let _ = self
                            .event_tx
                            .send(TunnelEvent::ReconnectFailed { reason: err_msg })
                            .await;
                        return result;
                    }

                    if start.elapsed() > Duration::from_secs(5) {
                        attempt = 0;
                    }
                }
            }

            attempt += 1;
            if attempt > config.max_retries {
                let reason = "Maximum reconnection attempts exceeded".to_string();
                let _ = self
                    .event_tx
                    .send(TunnelEvent::ReconnectFailed {
                        reason: reason.clone(),
                    })
                    .await;
                return Err(anyhow!(reason));
            }

            let backoff = calculate_backoff(attempt, &config);
            let next_retry_secs = backoff.as_secs();

            info!(
                attempt = attempt,
                max_attempts = config.max_retries,
                next_retry_in_secs = next_retry_secs,
                "Reconnecting..."
            );

            let _ = self
                .event_tx
                .send(TunnelEvent::Reconnecting {
                    attempt,
                    max_attempts: config.max_retries,
                    next_retry_in_secs: next_retry_secs,
                })
                .await;

            tokio::time::sleep(backoff).await;
        }
    }
}

/// HTTP Tunnel forwarder - connects to /tunnel endpoint and forwards HTTP requests
#[derive(Clone)]
pub struct TunnelForwarder {
    access_token_rx: watch::Receiver<String>,
    local_host: String,
    local_port: u16,
    org_id: Option<String>,
    slug: Option<String>,
    base_url: String,
    event_tx: mpsc::Sender<TunnelEvent>,
    replay_buffered: bool,
}

impl TunnelForwarder {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        access_token_rx: watch::Receiver<String>,
        local_host: String,
        local_port: u16,
        org_id: Option<String>,
        slug: Option<String>,
        event_tx: mpsc::Sender<TunnelEvent>,
        replay_buffered: bool,
    ) -> Self {
        let base_url = std::env::var("HOOKLISTENER_API_URL")
            .unwrap_or_else(|_| "https://app.hooklistener.com".to_string());

        Self {
            access_token_rx,
            local_host,
            local_port,
            org_id,
            slug,
            base_url,
            event_tx,
            replay_buffered,
        }
    }

    pub async fn connect_and_forward(&self) -> Result<()> {
        info!(
            local_host = %self.local_host,
            local_port = %self.local_port,
            "Starting HTTP tunnel"
        );

        let _ = self.event_tx.send(TunnelEvent::Connecting).await;

        // Build WebSocket URL - connect to /tunnel/websocket endpoint (Phoenix default)
        let access_token = self.access_token_rx.borrow().clone();
        let ws_url = build_ws_url(&self.base_url, &access_token, "tunnel/websocket");

        debug!(
            endpoint = %websocket_endpoint(&self.base_url, "tunnel/websocket"),
            "Connecting tunnel WebSocket"
        );

        // Connect to WebSocket
        let (ws_stream, _) = match connect_async_with_config(
            &ws_url,
            Some(tunnel_websocket_config()),
            false,
        )
        .await
        {
            Ok(stream) => stream,
            Err(e) => {
                let detail = redact_access_token(&e.to_string(), &access_token);
                let msg = format!("Failed to connect to tunnel: {detail}");
                let _ = self
                    .event_tx
                    .send(TunnelEvent::ConnectionError(msg.clone()))
                    .await;
                return Err(anyhow!(msg));
            }
        };

        info!("Tunnel WebSocket connected successfully");

        let (mut write, mut read) = ws_stream.split();

        // Join the tunnel:connect channel with local_port, organization_id, and optional slug
        let join_payload = tunnel_join_payload(
            self.local_port,
            self.org_id.as_deref(),
            self.slug.as_deref(),
        );

        if let Some(slug) = &self.slug {
            info!(slug = %slug, "Requesting static tunnel");
        }

        let join_message = ChannelMessage {
            topic: "tunnel:connect".to_string(),
            event: "phx_join".to_string(),
            payload: join_payload,
            reference: Some("1".to_string()),
        };

        let join_json = serde_json::to_string(&join_message)?;
        write
            .send(Message::Text(join_json.into()))
            .await
            .context("Failed to send join message")?;

        // Wait for join confirmation
        let mut joined = false;
        let mut tunnel_topic = String::new();
        let mut tunnel_limits = TunnelLimits {
            max_request_body_bytes: LEGACY_MAX_REQUEST_BODY_BYTES,
            max_response_body_bytes: LEGACY_MAX_RESPONSE_BODY_BYTES,
            max_response_header_bytes: LEGACY_MAX_RESPONSE_HEADER_BYTES,
            ordered_response_headers: false,
        };

        while !joined {
            match tokio::time::timeout(Duration::from_secs(10), read.next()).await {
                Ok(Some(msg_result)) => match msg_result {
                    Ok(Message::Text(text)) => {
                        let msg: ChannelMessage = serde_json::from_str(&text)?;
                        if msg.event == "phx_reply"
                            && msg.reference.as_deref() == Some("1")
                            && let Some(status) = msg.payload.get("status")
                        {
                            if status == "ok" {
                                // Extract subdomain, tunnel_id, and static flag from response
                                let Some(response) = msg.payload.get("response") else {
                                    let error = anyhow!("Tunnel join response was missing");
                                    let _ = self
                                        .event_tx
                                        .send(TunnelEvent::ConnectionError(error.to_string()))
                                        .await;
                                    return Err(error);
                                };

                                if let Err(error) =
                                    validate_join_mode(response, DIRECT_RESPONSE_MODE)
                                {
                                    let reason = error.to_string();
                                    let _ = self
                                        .event_tx
                                        .send(TunnelEvent::ConnectionError(reason.clone()))
                                        .await;
                                    return Err(error);
                                }

                                if let Err(error) = validate_framing_contract(response) {
                                    let reason = error.to_string();
                                    let _ = self
                                        .event_tx
                                        .send(TunnelEvent::ConnectionError(reason.clone()))
                                        .await;
                                    return Err(error);
                                }

                                tunnel_limits = TunnelLimits::from_join_response(response);
                                if !tunnel_limits.ordered_response_headers {
                                    let error = anyhow!(
                                        "Server did not advertise ordered response headers"
                                    );
                                    let _ = self
                                        .event_tx
                                        .send(TunnelEvent::ConnectionError(error.to_string()))
                                        .await;
                                    return Err(error);
                                }

                                let subdomain = response
                                    .get("subdomain")
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("unknown")
                                    .to_string();
                                let tunnel_id = response
                                    .get("tunnel_id")
                                    .and_then(|s| s.as_str())
                                    .unwrap_or("unknown")
                                    .to_string();
                                let is_static = response
                                    .get("static")
                                    .and_then(|s| s.as_bool())
                                    .unwrap_or(false);

                                let tunnel_type = if is_static { "static" } else { "ephemeral" };
                                info!(
                                    subdomain = %subdomain,
                                    tunnel_id = %tunnel_id,
                                    tunnel_type = %tunnel_type,
                                    max_request_body_bytes = tunnel_limits.max_request_body_bytes,
                                    max_response_body_bytes = tunnel_limits.max_response_body_bytes,
                                    max_response_header_bytes = tunnel_limits.max_response_header_bytes,
                                    "Tunnel established"
                                );

                                let _ = self
                                    .event_tx
                                    .send(TunnelEvent::TunnelEstablished {
                                        subdomain,
                                        tunnel_id,
                                        is_static,
                                    })
                                    .await;

                                tunnel_topic = msg.topic.clone();
                                joined = true;
                            } else {
                                let reason = msg
                                    .payload
                                    .get("response")
                                    .and_then(|r| r.get("reason"))
                                    .and_then(|r| r.as_str())
                                    .unwrap_or("Unknown error");
                                let _ = self
                                    .event_tx
                                    .send(TunnelEvent::ConnectionError(reason.to_string()))
                                    .await;
                                return Err(anyhow!("Tunnel join failed: {}", reason));
                            }
                        }
                    }
                    Ok(Message::Ping(data)) => {
                        write.send(Message::Pong(data)).await?;
                    }
                    Ok(Message::Close(frame)) => {
                        return Err(anyhow!("WebSocket closed during join: {:?}", frame));
                    }
                    Err(e) => return Err(anyhow!("WebSocket error during join: {}", e)),
                    _ => {}
                },
                Ok(None) => return Err(anyhow!("WebSocket stream ended during join")),
                Err(_) => return Err(anyhow!("Timeout waiting for tunnel join response")),
            }
        }

        // Keep WebSocket ownership in one task. Forwarded HTTP requests only enqueue
        // their responses, which lets slow requests complete independently.
        let (outbound_tx, mut outbound_rx) = mpsc::channel::<Message>(128);
        let mut writer_task = tokio::spawn(async move {
            while let Some(message) = outbound_rx.recv().await {
                write.send(message).await?;
            }

            write.close().await
        });
        let mut in_flight = tokio::task::JoinSet::new();

        // Auto-drain buffered requests at most once per connection so a
        // persistently failing replay cannot loop forever.
        let mut auto_drain_requested = false;

        // Track last ping time
        let mut last_ping = tokio::time::Instant::now();
        let ping_interval = Duration::from_secs(30);
        let mut ping_counter = 2;
        let mut inbound_streams = HashMap::new();

        // Listen for tunnel_request events
        loop {
            while let Some(result) = in_flight.try_join_next() {
                match result {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => error!(%error, "Tunnel request task failed"),
                    Err(error) => error!(%error, "Tunnel request task stopped unexpectedly"),
                }
            }

            if writer_task.is_finished() {
                match (&mut writer_task).await {
                    Ok(Ok(())) => warn!("Tunnel WebSocket writer stopped"),
                    Ok(Err(error)) => error!(%error, "Tunnel WebSocket writer failed"),
                    Err(error) => error!(%error, "Tunnel WebSocket writer task failed"),
                }
                break;
            }

            // Check if we need to send a ping
            if last_ping.elapsed() >= ping_interval {
                let ping_msg = ChannelMessage {
                    topic: tunnel_topic.clone(),
                    event: "ping".to_string(),
                    payload: serde_json::json!({}),
                    reference: Some(ping_counter.to_string()),
                };
                ping_counter += 1;

                if let Ok(json) = serde_json::to_string(&ping_msg)
                    && let Err(e) = outbound_tx.send(Message::Text(json.into())).await
                {
                    error!("Failed to send ping: {}", e);
                    break;
                }
                last_ping = tokio::time::Instant::now();
            }

            // Use timeout to allow ping checks
            match tokio::time::timeout(Duration::from_millis(100), read.next()).await {
                Ok(Some(msg)) => match msg {
                    Ok(Message::Text(text)) => {
                        if let Err(e) = self
                            .handle_tunnel_message(
                                &text,
                                &outbound_tx,
                                &tunnel_topic,
                                tunnel_limits,
                                &mut in_flight,
                                &mut auto_drain_requested,
                                &mut inbound_streams,
                            )
                            .await
                        {
                            error!("Error handling tunnel message: {}", e);
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        info!("Tunnel WebSocket closed: {:?}", frame);
                        let _ = self.event_tx.send(TunnelEvent::Disconnected).await;
                        break;
                    }
                    Ok(Message::Ping(data)) => {
                        debug!("Received ping, sending pong");
                        if let Err(e) = outbound_tx.send(Message::Pong(data)).await {
                            error!("Failed to send pong: {}", e);
                            break;
                        }
                    }
                    Ok(_) => {}
                    Err(e) => {
                        error!("Tunnel WebSocket error: {}", e);
                        let _ = self
                            .event_tx
                            .send(TunnelEvent::ConnectionError(format!(
                                "WebSocket error: {}",
                                e
                            )))
                            .await;
                        break;
                    }
                },
                Ok(None) => {
                    warn!("Tunnel WebSocket stream ended");
                    let _ = self.event_tx.send(TunnelEvent::Disconnected).await;
                    break;
                }
                Err(_) => {
                    // Timeout - continue to check ping
                    continue;
                }
            }
        }

        // A replacement connection gets a new fenced session. Do not allow work
        // from this connection to emit stale completions after reconnecting.
        in_flight.abort_all();
        while in_flight.join_next().await.is_some() {}
        drop(outbound_tx);

        if !writer_task.is_finished() {
            let _ = writer_task.await;
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_tunnel_message(
        &self,
        text: &str,
        outbound_tx: &mpsc::Sender<Message>,
        tunnel_topic: &str,
        tunnel_limits: TunnelLimits,
        in_flight: &mut tokio::task::JoinSet<Result<()>>,
        auto_drain_requested: &mut bool,
        inbound_streams: &mut HashMap<String, TunnelStreamAssembler>,
    ) -> Result<()> {
        let msg: ChannelMessage = serde_json::from_str(text)?;

        debug!(
            topic = %msg.topic,
            event = %msg.event,
            "Received tunnel message"
        );

        match msg.event.as_str() {
            "tunnel_stream_start" => {
                let assembler = TunnelStreamAssembler::from_start(&msg.payload, "request")?;
                if inbound_streams.len() >= 128 {
                    return Err(anyhow!("Too many concurrent tunnel streams"));
                }
                if inbound_streams.contains_key(&assembler.stream_id) {
                    return Err(anyhow!("Duplicate tunnel stream id"));
                }

                inbound_streams.insert(assembler.stream_id.clone(), assembler);
            }
            "tunnel_stream_frame" => {
                let stream_id = msg
                    .payload
                    .get("stream_id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| anyhow!("Tunnel frame is missing its stream id"))?
                    .to_string();
                let sequence = json_usize(&msg.payload, "sequence")?;

                let completed = inbound_streams
                    .get_mut(&stream_id)
                    .ok_or_else(|| anyhow!("Tunnel frame references an unknown stream"))?
                    .append(&msg.payload)?;

                send_channel_message(
                    outbound_tx,
                    ChannelMessage {
                        topic: tunnel_topic.to_string(),
                        event: "tunnel_stream_ack".to_string(),
                        payload: serde_json::json!({
                            "stream_id": stream_id,
                            "sequence": sequence,
                        }),
                        reference: None,
                    },
                )
                .await?;

                if let Some(payload) = completed {
                    let assembler = inbound_streams
                        .remove(&stream_id)
                        .ok_or_else(|| anyhow!("Completed tunnel stream disappeared"))?;
                    self.handle_framed_tunnel_request(
                        &payload,
                        assembler.deadline_unix_ms,
                        outbound_tx,
                        tunnel_topic,
                        tunnel_limits,
                        in_flight,
                    )
                    .await?;
                }
            }
            "tunnel_stream_error" => {
                let code = msg
                    .payload
                    .get("code")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("stream_error");
                return Err(anyhow!("Tunnel stream failed: {code}"));
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

                if self.replay_buffered && count > 0 && !*auto_drain_requested {
                    *auto_drain_requested = true;

                    let replay_msg = ChannelMessage {
                        topic: tunnel_topic.to_string(),
                        event: "buffered:replay".to_string(),
                        payload: serde_json::json!({}),
                        reference: Some("buffered-replay".to_string()),
                    };

                    let json = serde_json::to_string(&replay_msg)?;
                    outbound_tx
                        .send(Message::Text(json.into()))
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
                            warn!(reason = %reason, "Buffered replay request rejected");
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
                debug!("Unhandled tunnel event: {}", msg.event);
            }
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_framed_tunnel_request(
        &self,
        payload: &serde_json::Value,
        deadline_unix_ms: u64,
        outbound_tx: &mpsc::Sender<Message>,
        tunnel_topic: &str,
        tunnel_limits: TunnelLimits,
        in_flight: &mut tokio::task::JoinSet<Result<()>>,
    ) -> Result<()> {
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
        let body = match body_encoding {
            "raw" => raw_body.as_bytes().to_vec(),
            "base64" => URL_SAFE_NO_PAD
                .decode(raw_body)
                .context("Tunnel request contains invalid base64url body data")?,
            encoding => return Err(anyhow!("Unsupported tunnel body encoding: {encoding}")),
        };

        if body.len() > tunnel_limits.max_request_body_bytes {
            self.report_tunnel_failure(
                &request_id,
                format!(
                    "Tunnel request body exceeds advertised limit ({} > {} bytes)",
                    body.len(),
                    tunnel_limits.max_request_body_bytes
                ),
                outbound_tx,
                tunnel_topic,
            )
            .await?;
            return Ok(());
        }

        let headers_map: HashMap<String, String> = headers.iter().cloned().collect();
        let body_string = body_preview(&body, &headers_map);
        let _ = self
            .event_tx
            .send(TunnelEvent::RequestReceived {
                request_id: request_id.clone(),
                method: method.clone(),
                path: path.clone(),
                headers: headers_map,
                body: body_string,
                query_string: query_string.clone(),
                replay,
            })
            .await;

        let forwarder = self.clone();
        let outbound_tx = outbound_tx.clone();
        let tunnel_topic = tunnel_topic.to_string();

        in_flight.spawn(async move {
            forwarder
                .forward_tunnel_request(
                    request_id,
                    method,
                    path,
                    query_string,
                    headers,
                    body,
                    deadline_unix_ms,
                    &outbound_tx,
                    &tunnel_topic,
                    tunnel_limits,
                )
                .await
        });

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn forward_tunnel_request(
        &self,
        request_id: String,
        method: String,
        path: String,
        query_string: String,
        headers: Vec<(String, String)>,
        body: Vec<u8>,
        deadline_unix_ms: u64,
        outbound_tx: &mpsc::Sender<Message>,
        tunnel_topic: &str,
        tunnel_limits: TunnelLimits,
    ) -> Result<()> {
        let start_time = tokio::time::Instant::now();

        info!(
            request_id = %request_id,
            method = %method,
            path = %path,
            "Forwarding tunnel request to local server"
        );

        let mut target = format!("http://{}:{}{}", self.local_host, self.local_port, path);
        if !query_string.is_empty() {
            target.push('?');
            target.push_str(&query_string);
        }

        let remaining_ms = deadline_unix_ms.saturating_sub(unix_time_ms());
        if remaining_ms == 0 {
            return self
                .report_tunnel_failure(
                    &request_id,
                    "deadline_exceeded".to_string(),
                    outbound_tx,
                    tunnel_topic,
                )
                .await;
        }

        let client = tunnel_http_client(Duration::from_millis(remaining_ms))?;
        let request_method = match reqwest::Method::from_bytes(method.as_bytes()) {
            Ok(method) => method,
            Err(_) => {
                warn!("Unsupported HTTP method: {}", method);
                self.send_tunnel_error(
                    &request_id,
                    &format!("Unsupported method: {method}"),
                    outbound_tx,
                    tunnel_topic,
                )
                .await?;
                return Ok(());
            }
        };
        let mut req_builder = client.request(request_method, &target);

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

        match req_builder.send().await {
            Ok(mut response) => {
                let status = response.status().as_u16();

                let header_bytes = response_header_bytes(response.headers());
                if header_bytes > tunnel_limits.max_response_header_bytes {
                    let error_msg = format!(
                        "Local response headers exceed tunnel limit ({} > {} bytes)",
                        header_bytes, tunnel_limits.max_response_header_bytes
                    );

                    return self
                        .report_tunnel_failure(&request_id, error_msg, outbound_tx, tunnel_topic)
                        .await;
                }

                let response_header_pairs = response_headers_to_ordered_pairs(response.headers());
                let response_headers = response_headers_to_map(response.headers());

                if response
                    .content_length()
                    .is_some_and(|length| length > tunnel_limits.max_response_body_bytes as u64)
                {
                    let error_msg = format!(
                        "Local response body exceeds tunnel limit (max {} bytes)",
                        tunnel_limits.max_response_body_bytes
                    );

                    return self
                        .report_tunnel_failure(&request_id, error_msg, outbound_tx, tunnel_topic)
                        .await;
                }

                let initial_capacity = response
                    .content_length()
                    .and_then(|length| usize::try_from(length).ok())
                    .unwrap_or(0)
                    .min(tunnel_limits.max_response_body_bytes);
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
                                        error_msg,
                                        outbound_tx,
                                        tunnel_topic,
                                    )
                                    .await;
                            }

                            response_bytes.extend_from_slice(&chunk);
                        }
                        Ok(None) => break,
                        Err(error) => {
                            let error_msg = format!("Failed to read local response: {error}");

                            return self
                                .report_tunnel_failure(
                                    &request_id,
                                    error_msg,
                                    outbound_tx,
                                    tunnel_topic,
                                )
                                .await;
                        }
                    }
                }

                let response_body_preview = body_preview(&response_bytes, &response_headers);
                let (response_body, body_encoding) = encode_response_body(&response_bytes);
                let duration_ms = start_time.elapsed().as_millis() as u64;

                info!(
                    request_id = %request_id,
                    status = %status,
                    duration_ms = %duration_ms,
                    body_encoding = %body_encoding,
                    "Request forwarded successfully"
                );

                let _ = self
                    .event_tx
                    .send(TunnelEvent::RequestForwarded {
                        request_id: request_id.clone(),
                        status,
                        duration_ms,
                        response_headers: response_headers.clone(),
                        response_body: response_body_preview,
                    })
                    .await;

                self.send_framed_tunnel_response(
                    outbound_tx,
                    tunnel_topic,
                    &request_id,
                    deadline_unix_ms,
                    serde_json::json!({
                        "request_id": request_id,
                        "status": status,
                        "headers": response_header_pairs,
                        "body": response_body,
                        "body_encoding": body_encoding,
                    }),
                )
                .await?;
            }
            Err(error) => {
                let duration_ms = start_time.elapsed().as_millis() as u64;
                let error_msg = format!("Failed to forward request: {error}");

                error!(
                    request_id = %request_id,
                    error = %error_msg,
                    duration_ms = %duration_ms,
                    "Request forwarding failed"
                );

                let _ = self
                    .event_tx
                    .send(TunnelEvent::RequestFailed {
                        request_id: request_id.clone(),
                        error: error_msg.clone(),
                    })
                    .await;

                self.send_tunnel_error(&request_id, &error_msg, outbound_tx, tunnel_topic)
                    .await?;
            }
        }

        Ok(())
    }

    async fn send_framed_tunnel_response(
        &self,
        outbound_tx: &mpsc::Sender<Message>,
        tunnel_topic: &str,
        request_id: &str,
        deadline_unix_ms: u64,
        payload: serde_json::Value,
    ) -> Result<()> {
        let mut stream =
            OutboundTunnelStream::new(request_id, "response", deadline_unix_ms, &payload)?;

        send_channel_message(outbound_tx, stream.start_message(tunnel_topic)).await?;

        while let Some(frame) = stream.next_frame(tunnel_topic)? {
            if unix_time_ms() >= deadline_unix_ms {
                return Err(anyhow!("deadline_exceeded"));
            }

            // The bounded writer channel applies transport backpressure while
            // preserving single-task ownership of the WebSocket sink.
            send_channel_message(outbound_tx, frame).await?;
        }

        Ok(())
    }

    async fn send_tunnel_error(
        &self,
        request_id: &str,
        error: &str,
        outbound_tx: &mpsc::Sender<Message>,
        tunnel_topic: &str,
    ) -> Result<()> {
        let error_message = ChannelMessage {
            topic: tunnel_topic.to_string(),
            event: "tunnel_error".to_string(),
            payload: serde_json::json!({
                "request_id": request_id,
                "error": error,
            }),
            reference: None,
        };

        let error_json = serde_json::to_string(&error_message)?;
        outbound_tx
            .send(Message::Text(error_json.into()))
            .await
            .context("Tunnel connection closed before error was sent")?;
        Ok(())
    }

    async fn report_tunnel_failure(
        &self,
        request_id: &str,
        error: String,
        outbound_tx: &mpsc::Sender<Message>,
        tunnel_topic: &str,
    ) -> Result<()> {
        error!(request_id = %request_id, error = %error, "Tunnel request failed");

        let _ = self
            .event_tx
            .send(TunnelEvent::RequestFailed {
                request_id: request_id.to_string(),
                error: error.clone(),
            })
            .await;

        self.send_tunnel_error(request_id, &error, outbound_tx, tunnel_topic)
            .await
    }

    /// Connect with automatic reconnection on recoverable errors
    pub async fn connect_with_reconnect(&self, config: ReconnectConfig) -> Result<()> {
        let mut attempt: u32 = 0;

        loop {
            let start = tokio::time::Instant::now();
            let result = self.connect_and_forward().await;

            match result {
                Ok(()) => {
                    if start.elapsed() > Duration::from_secs(5) {
                        attempt = 0;
                    }
                }
                Err(ref e) => {
                    let err_msg = e.to_string();
                    if is_fatal_error(&err_msg) {
                        let _ = self
                            .event_tx
                            .send(TunnelEvent::ReconnectFailed { reason: err_msg })
                            .await;
                        return result;
                    }

                    if start.elapsed() > Duration::from_secs(5) {
                        attempt = 0;
                    }
                }
            }

            attempt += 1;
            if attempt > config.max_retries {
                let reason = "Maximum reconnection attempts exceeded".to_string();
                let _ = self
                    .event_tx
                    .send(TunnelEvent::ReconnectFailed {
                        reason: reason.clone(),
                    })
                    .await;
                return Err(anyhow!(reason));
            }

            let backoff = calculate_backoff(attempt, &config);
            let next_retry_secs = backoff.as_secs();

            info!(
                attempt = attempt,
                max_attempts = config.max_retries,
                next_retry_in_secs = next_retry_secs,
                "Reconnecting tunnel..."
            );

            let _ = self
                .event_tx
                .send(TunnelEvent::Reconnecting {
                    attempt,
                    max_attempts: config.max_retries,
                    next_retry_in_secs: next_retry_secs,
                })
                .await;

            tokio::time::sleep(backoff).await;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::DeflateEncoder, write::GzEncoder, write::ZlibEncoder};
    use std::io::Write;

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

        let tunnel_payload = tunnel_join_payload(3000, Some("org-1"), Some("payments"));
        assert_eq!(tunnel_payload["mode"], DIRECT_RESPONSE_MODE);
        assert_eq!(tunnel_payload["protocol_version"], TUNNEL_PROTOCOL_VERSION);
        assert_eq!(tunnel_payload["local_port"], 3000);
        assert_eq!(tunnel_payload["organization_id"], "org-1");
        assert_eq!(tunnel_payload["slug"], "payments");
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
    fn test_tunnel_websocket_config_accepts_advertised_messages() {
        let config = tunnel_websocket_config();

        assert_eq!(config.max_message_size, Some(TUNNEL_MAX_FRAME_BYTES));
        assert_eq!(config.max_frame_size, Some(TUNNEL_MAX_FRAME_BYTES));
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
    }

    #[test]
    fn test_published_v2_contract_matches_cli_framing_constants() {
        let fixture: serde_json::Value =
            serde_json::from_str(include_str!("../fixtures/tunnel_framing_v2.json")).unwrap();

        assert_eq!(fixture["version"], TUNNEL_PROTOCOL_VERSION);
        assert_eq!(fixture["max_frame_bytes"], TUNNEL_MAX_FRAME_BYTES);
        assert_eq!(fixture["max_raw_chunk_bytes"], TUNNEL_MAX_RAW_CHUNK_BYTES);
        assert_eq!(fixture["queue_depth_frames"], 1);

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
    fn test_tunnel_limits_use_join_response_contract() {
        let response = serde_json::json!({
            "limits": {
                "max_body_bytes": 268_435_456,
                "max_response_header_bytes": 16_777_216,
                "response_headers_format": "ordered_pairs"
            }
        });

        let limits = TunnelLimits::from_join_response(&response);

        assert_eq!(limits.max_request_body_bytes, 268_435_456);
        assert_eq!(limits.max_response_body_bytes, 268_435_456);
        assert_eq!(limits.max_response_header_bytes, 16_777_216);
        assert!(limits.ordered_response_headers);
    }

    #[test]
    fn test_tunnel_limits_keep_legacy_response_header_shape_by_default() {
        let limits = TunnelLimits::from_join_response(&serde_json::json!({}));

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
        let headers =
            preview_headers(&[("content-type", "text/plain"), ("content-encoding", "gzip")]);
        let preview = body_preview(&compressed, &headers).unwrap();

        assert!(preview.starts_with("aaaa"));
        assert!(preview.contains("decoded preview truncated"));
        assert!(preview.len() <= UI_BODY_PREVIEW_BYTES + 96);
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
        assert_eq!(url, "wss://api.example.com/socket/websocket?token=tok123");
    }

    #[test]
    fn test_build_ws_url_http_to_ws() {
        let url = build_ws_url("http://localhost:4000", "tok", "tunnel/websocket");
        assert_eq!(url, "ws://localhost:4000/tunnel/websocket?token=tok");
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
}
