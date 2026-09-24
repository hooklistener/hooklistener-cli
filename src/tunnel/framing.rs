//! Phoenix channel messages, join payloads, and v2 stream framing.

use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use serde::{Deserialize, Serialize};
use std::time::SystemTime;

use crate::tunnel::http::json_value_to_string;
use crate::tunnel::limits::{
    TUNNEL_MAX_FRAME_BYTES, TUNNEL_MAX_JSON_DEPTH, TUNNEL_MAX_JSON_PRIMITIVE_BYTES,
    TUNNEL_MAX_JSON_STRUCTURAL_NODES, TUNNEL_MAX_RAW_CHUNK_BYTES, TUNNEL_MAX_RESPONSE_HEADERS,
    TUNNEL_MAX_STREAM_BYTES, TUNNEL_PROTOCOL_VERSION, optional_positive_limit_at_most,
};
use crate::tunnel::preview::bounded_control_neutral_log_text;
use crate::tunnel_v3;

pub(crate) fn stream_error_message(topic: &str, stream_id: &str, code: &str) -> ChannelMessage {
    ChannelMessage {
        topic: topic.to_string(),
        event: "tunnel_stream_error".to_string(),
        payload: serde_json::json!({"stream_id": stream_id, "code": code}),
        reference: None,
    }
}

pub(crate) fn with_forward_id(
    mut payload: serde_json::Value,
    forward_id: Option<&str>,
) -> serde_json::Value {
    if let Some(forward_id) = forward_id {
        payload["forward_id"] = serde_json::Value::String(forward_id.to_string());
    }

    payload
}

/// Phoenix Channel message structure
#[derive(Debug, Serialize, Deserialize)]
pub(crate) struct ChannelMessage {
    pub(crate) topic: String,
    pub(crate) event: String,
    pub(crate) payload: serde_json::Value,
    #[serde(rename = "ref")]
    pub(crate) reference: Option<String>,
}

pub(crate) fn decode_channel_message(text: &str, protocol_v3: bool) -> Result<ChannelMessage> {
    if protocol_v3 {
        let message = tunnel_v3::decode_server_control(text)?;
        Ok(ChannelMessage {
            topic: message.topic,
            event: message.event,
            payload: message.payload,
            reference: message.reference,
        })
    } else {
        serde_json::from_str(text).map_err(Into::into)
    }
}

pub(crate) const DIRECT_RESPONSE_MODE: &str = "direct_response";

pub(crate) const CAPTURE_FORWARD_MODE: &str = "capture_forward";

pub(crate) struct TunnelStreamAssembler {
    pub(crate) stream_id: String,
    direction: String,
    pub(crate) deadline_unix_ms: u64,
    pub(crate) total_bytes: usize,
    frame_count: usize,
    next_sequence: usize,
    pub(crate) data: Vec<u8>,
}

impl TunnelStreamAssembler {
    pub(crate) fn from_start(
        payload: &serde_json::Value,
        expected_direction: &str,
    ) -> Result<Self> {
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
        if total_bytes > TUNNEL_MAX_STREAM_BYTES {
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

    pub(crate) fn append(
        &mut self,
        payload: &serde_json::Value,
    ) -> Result<Option<serde_json::Value>> {
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

            let serialized_payload = std::mem::take(&mut self.data);
            let payload = serde_json::from_slice(&serialized_payload)
                .context("Tunnel stream contains invalid JSON")?;
            drop(serialized_payload);
            return Ok(Some(payload));
        }

        if self.data.len() == self.total_bytes {
            return Err(anyhow!("Tunnel stream omitted its final marker"));
        }

        Ok(None)
    }
}

pub(crate) struct OutboundTunnelStream {
    stream_id: String,
    direction: &'static str,
    deadline_unix_ms: u64,
    encoded: Vec<u8>,
    offset: usize,
    sequence: usize,
    frame_count: usize,
}

impl OutboundTunnelStream {
    pub(crate) fn new(
        stream_id: &str,
        direction: &'static str,
        deadline_unix_ms: u64,
        payload: &serde_json::Value,
    ) -> Result<Self> {
        let encoded = serde_json::to_vec(payload)?;
        if encoded.len() > TUNNEL_MAX_STREAM_BYTES {
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

    pub(crate) fn start_message(&self, topic: &str) -> ChannelMessage {
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

    pub(crate) fn next_frame(&mut self, topic: &str) -> Result<Option<ChannelMessage>> {
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

pub(crate) fn json_usize(payload: &serde_json::Value, key: &str) -> Result<usize> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .and_then(|value| value.try_into().ok())
        .ok_or_else(|| anyhow!("Tunnel stream has invalid {key}"))
}

pub(crate) fn unix_time_ms() -> u64 {
    SystemTime::now()
        .duration_since(SystemTime::UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis()
        .try_into()
        .unwrap_or(u64::MAX)
}

pub(crate) fn required_string(payload: &serde_json::Value, key: &str) -> Result<String> {
    payload
        .get(key)
        .and_then(serde_json::Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| anyhow!("Tunnel request is missing {key}"))
}

pub(crate) fn ordered_header_pairs(
    value: Option<&serde_json::Value>,
) -> Result<Vec<(String, String)>> {
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

pub(crate) fn validate_framing_contract(response: &serde_json::Value) -> Result<()> {
    let framing = response
        .get("framing")
        .ok_or_else(|| anyhow!("Server did not advertise bounded tunnel framing"))?;
    let limits = response
        .get("limits")
        .ok_or_else(|| anyhow!("Server did not advertise bounded tunnel limits"))?;

    if framing.get("version").and_then(serde_json::Value::as_u64) != Some(TUNNEL_PROTOCOL_VERSION)
        || framing.get("transport").and_then(serde_json::Value::as_str) != Some("framed")
        || framing
            .get("frame_encoding")
            .and_then(serde_json::Value::as_str)
            != Some("base64url")
        || framing
            .get("max_frame_bytes")
            .and_then(serde_json::Value::as_u64)
            != Some(TUNNEL_MAX_FRAME_BYTES as u64)
        || framing
            .get("max_raw_chunk_bytes")
            .and_then(serde_json::Value::as_u64)
            != Some(TUNNEL_MAX_RAW_CHUNK_BYTES as u64)
        || framing
            .get("queue_depth_frames")
            .and_then(serde_json::Value::as_u64)
            != Some(1)
        || framing
            .get("backpressure")
            .and_then(serde_json::Value::as_str)
            != Some("per_frame_ack")
        || !optional_positive_limit_at_most(framing, "max_json_depth", TUNNEL_MAX_JSON_DEPTH)
        || !optional_positive_limit_at_most(
            framing,
            "max_json_structural_nodes",
            TUNNEL_MAX_JSON_STRUCTURAL_NODES,
        )
        || !optional_positive_limit_at_most(
            framing,
            "max_json_primitive_bytes",
            TUNNEL_MAX_JSON_PRIMITIVE_BYTES,
        )
        || !optional_positive_limit_at_most(
            framing,
            "max_response_headers",
            TUNNEL_MAX_RESPONSE_HEADERS,
        )
        || limits
            .get("max_stream_bytes")
            .and_then(serde_json::Value::as_u64)
            != Some(TUNNEL_MAX_STREAM_BYTES as u64)
        || !optional_positive_limit_at_most(
            limits,
            "max_response_header_items",
            TUNNEL_MAX_RESPONSE_HEADERS,
        )
    {
        return Err(anyhow!(
            "Server advertised an incompatible framing contract"
        ));
    }

    Ok(())
}

pub(crate) fn listen_join_payload() -> serde_json::Value {
    serde_json::json!({"mode": CAPTURE_FORWARD_MODE})
}

#[cfg(test)]
pub(crate) fn tunnel_join_payload(
    local_port: u16,
    organization_id: Option<&str>,
    slug: Option<&str>,
    resume_token: Option<&str>,
) -> serde_json::Value {
    tunnel_join_payload_for_version(
        local_port,
        organization_id,
        slug,
        resume_token,
        TUNNEL_PROTOCOL_VERSION,
    )
}

pub(crate) fn tunnel_join_payload_for_version(
    local_port: u16,
    organization_id: Option<&str>,
    slug: Option<&str>,
    resume_token: Option<&str>,
    protocol_version: u64,
) -> serde_json::Value {
    let mut payload = serde_json::json!({
        "mode": DIRECT_RESPONSE_MODE,
        "protocol_version": protocol_version,
        "local_port": local_port,
    });

    if let Some(resume_token) = resume_token {
        payload["resume_token"] = serde_json::Value::String(resume_token.to_string());
        return payload;
    }

    if let Some(organization_id) = organization_id {
        payload["organization_id"] = serde_json::Value::String(organization_id.to_string());
    }

    if let Some(slug) = slug {
        payload["slug"] = serde_json::Value::String(slug.to_string());
    }

    payload
}

pub(crate) fn selected_tunnel_protocol(supported_protocol_versions: &[u64]) -> Result<u64> {
    if supported_protocol_versions.contains(&tunnel_v3::VERSION.into()) {
        Ok(tunnel_v3::VERSION.into())
    } else if supported_protocol_versions.contains(&TUNNEL_PROTOCOL_VERSION) {
        Ok(TUNNEL_PROTOCOL_VERSION)
    } else {
        Err(anyhow!(
            "Relay handshake rejected: service and CLI have no common tunnel protocol version"
        ))
    }
}

pub(crate) fn validate_join_mode(response: &serde_json::Value, expected: &str) -> Result<()> {
    match response.get("mode").and_then(|mode| mode.as_str()) {
        Some(mode) if mode == expected => Ok(()),
        Some(mode) => {
            let mode = bounded_control_neutral_log_text(mode);
            Err(anyhow!(
                "Channel join failed: Server activated incompatible mode '{mode}' (expected '{expected}'); no requests were forwarded"
            ))
        }
        None => Err(anyhow!(
            "Channel join failed: Server did not confirm activation mode '{expected}'; upgrade the Hooklistener service before retrying"
        )),
    }
}
