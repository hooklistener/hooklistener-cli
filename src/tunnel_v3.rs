//! Protocol v3 streaming transport and bounded body primitives.
//!
//! The tunnel activates this path automatically when the relay ticket
//! advertises protocol v3. Tickets from older services omit that capability,
//! so upgraded clients remain backward compatible with protocol v2.

use anyhow::{Result, anyhow};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use thiserror::Error;
use tokio::sync::{Notify, OwnedSemaphorePermit, Semaphore, mpsc};
use tokio_tungstenite::tungstenite::Bytes;
use uuid::Uuid;

pub(crate) const VERSION: u8 = 3;
pub(crate) const CHUNK_HEADER_BYTES: usize = 29;
const CONTRACT_STATUS: &str = "stable";
const CAPTURE_MODES: [&str; 4] = [
    "metadata_only",
    "progressive_optional",
    "progressive_required",
    "store_then_forward",
];
const CONTROL_ENCODING: &str = "json";
const TRANSPORT: &str = "phoenix_binary_push";
const BYTE_ORDER: &str = "big_endian";
const FLOW_CONTROL: &str = "cumulative_consumption";
const CONNECTION_WINDOW_SCOPE: &str = "per_direction";
const PHOENIX_PUSH: u8 = 0;
const MAX_PHOENIX_FIELD_BYTES: usize = u8::MAX as usize;
const MAX_CLIENT_BINARY_ENVELOPE_OVERHEAD_BYTES: usize = 1 + 4 + 4 * 255;
const LOCAL_MAX_WEBSOCKET_FRAME_BYTES: usize = 65_536;
const LOCAL_MAX_REQUEST_TIMEOUT_MS: usize = 110_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct StreamingLimits {
    pub max_request_body_bytes: usize,
    pub max_response_body_bytes: usize,
    pub max_start_metadata_bytes: usize,
    pub max_control_payload_bytes: usize,
    pub max_websocket_frame_bytes: usize,
    pub max_chunk_bytes: usize,
    pub stream_window_bytes: usize,
    pub stream_window_items: usize,
    pub connection_window_bytes: usize,
    pub connection_window_items: usize,
    pub websocket_admission_bytes: usize,
    pub websocket_admission_items: usize,
    pub service_ingress_admission_bytes: usize,
    pub service_ingress_admission_items: usize,
    pub max_concurrent_streams: usize,
    pub max_active_local_forwards: usize,
    pub request_timeout_ms: usize,
}

impl StreamingLimits {
    pub(crate) fn from_contract(contract: &Value) -> Result<Self> {
        if contract.get("version").and_then(Value::as_u64) != Some(VERSION.into()) {
            return Err(anyhow!(
                "Server advertised an incompatible streaming version"
            ));
        }
        require_contract_string(contract, "status", CONTRACT_STATUS)?;
        require_contract_string(contract, "control_encoding", CONTROL_ENCODING)?;
        require_contract_string(contract, "transport", TRANSPORT)?;
        require_contract_string(contract, "byte_order", BYTE_ORDER)?;

        if !contract
            .get("capture_mode")
            .and_then(Value::as_str)
            .is_some_and(|mode| CAPTURE_MODES.contains(&mode))
        {
            return Err(anyhow!(
                "Server advertised an incompatible protocol v3 capture mode"
            ));
        }

        let limits = contract.get("limits").unwrap_or(contract);
        let chunk_header_bytes = contract
            .get("chunk_header_bytes")
            .and_then(Value::as_u64)
            .or_else(|| {
                contract
                    .get("chunk_header")
                    .and_then(|header| header.get("bytes"))
                    .and_then(Value::as_u64)
            });
        if chunk_header_bytes != Some(CHUNK_HEADER_BYTES as u64) {
            return Err(anyhow!(
                "Server advertised an incompatible protocol v3 chunk header"
            ));
        }

        let flow_control = contract.get("flow_control").and_then(|value| {
            value
                .as_str()
                .or_else(|| value.get("scheme").and_then(Value::as_str))
        });
        if flow_control != Some(FLOW_CONTROL) {
            return Err(anyhow!(
                "Server advertised incompatible protocol v3 flow control"
            ));
        }

        require_contract_string(limits, "connection_window_scope", CONNECTION_WINDOW_SCOPE)?;

        let parsed = Self {
            max_request_body_bytes: required_usize(limits, "max_request_body_bytes")?,
            max_response_body_bytes: required_usize(limits, "max_response_body_bytes")?,
            max_start_metadata_bytes: required_usize(limits, "max_start_metadata_bytes")?,
            max_control_payload_bytes: required_usize(limits, "max_control_payload_bytes")?,
            max_websocket_frame_bytes: required_usize(limits, "max_websocket_frame_bytes")?,
            max_chunk_bytes: required_usize(limits, "max_chunk_bytes")?,
            stream_window_bytes: required_usize(limits, "stream_window_bytes")?,
            stream_window_items: required_usize(limits, "stream_window_items")?,
            connection_window_bytes: required_usize(limits, "connection_window_bytes")?,
            connection_window_items: required_usize(limits, "connection_window_items")?,
            websocket_admission_bytes: required_usize(limits, "websocket_admission_bytes")?,
            websocket_admission_items: required_usize(limits, "websocket_admission_items")?,
            service_ingress_admission_bytes: required_usize(
                limits,
                "service_ingress_admission_bytes",
            )?,
            service_ingress_admission_items: required_usize(
                limits,
                "service_ingress_admission_items",
            )?,
            max_concurrent_streams: required_usize(limits, "max_concurrent_streams")?,
            max_active_local_forwards: required_usize(limits, "max_active_local_forwards")?,
            request_timeout_ms: required_usize(limits, "request_timeout_ms")?,
        };
        let advertised_duplex = required_usize(limits, "max_duplex_connection_window_bytes")?;
        if parsed.connection_window_bytes.checked_mul(2) != Some(advertised_duplex) {
            return Err(anyhow!(
                "Server advertised an incompatible protocol v3 duplex connection ceiling"
            ));
        }
        parsed.validate()?;
        Ok(parsed)
    }

    pub(crate) fn from_join_response(response: &Value) -> Result<Self> {
        let streaming = response
            .get("streaming")
            .ok_or_else(|| anyhow!("Server did not advertise protocol v3 streaming"))?;
        Self::from_contract(streaming)
    }

    fn validate(self) -> Result<()> {
        let values = [
            self.max_request_body_bytes,
            self.max_response_body_bytes,
            self.max_start_metadata_bytes,
            self.max_control_payload_bytes,
            self.max_websocket_frame_bytes,
            self.max_chunk_bytes,
            self.stream_window_bytes,
            self.stream_window_items,
            self.connection_window_bytes,
            self.connection_window_items,
            self.websocket_admission_bytes,
            self.websocket_admission_items,
            self.service_ingress_admission_bytes,
            self.service_ingress_admission_items,
            self.max_concurrent_streams,
            self.max_active_local_forwards,
            self.request_timeout_ms,
        ];
        if values.contains(&0) {
            return Err(anyhow!("Protocol v3 limits must be positive"));
        }
        if self.max_websocket_frame_bytes > LOCAL_MAX_WEBSOCKET_FRAME_BYTES {
            return Err(anyhow!(
                "Protocol v3 WebSocket frame limit exceeds the local ceiling"
            ));
        }
        if self.max_start_metadata_bytes > self.max_control_payload_bytes {
            return Err(anyhow!(
                "Protocol v3 start metadata exceeds the control payload limit"
            ));
        }
        if self.max_control_payload_bytes > self.max_websocket_frame_bytes {
            return Err(anyhow!(
                "Protocol v3 control payload exceeds the WebSocket frame limit"
            ));
        }
        if self.max_chunk_bytes > u32::MAX as usize {
            return Err(anyhow!("Protocol v3 chunks exceed Tokio permit limits"));
        }
        let item_channel_capacity = self
            .stream_window_items
            .checked_add(1)
            .ok_or_else(|| anyhow!("Protocol v3 item window exceeds local runtime limits"))?;
        if self.stream_window_bytes > Semaphore::MAX_PERMITS
            || self.connection_window_bytes > Semaphore::MAX_PERMITS
            || self.stream_window_items > Semaphore::MAX_PERMITS
            || self.connection_window_items > Semaphore::MAX_PERMITS
            || item_channel_capacity > Semaphore::MAX_PERMITS
        {
            return Err(anyhow!("Protocol v3 windows exceed local runtime limits"));
        }
        if self.max_chunk_bytes > self.max_request_body_bytes
            || self.max_chunk_bytes > self.max_response_body_bytes
        {
            return Err(anyhow!("Protocol v3 chunk exceeds a body limit"));
        }
        if self.max_chunk_bytes > self.stream_window_bytes {
            return Err(anyhow!("Protocol v3 stream window cannot admit one chunk"));
        }
        if self.stream_window_bytes > self.connection_window_bytes {
            return Err(anyhow!(
                "Protocol v3 stream window exceeds the connection ceiling"
            ));
        }
        if self.stream_window_items > self.connection_window_items {
            return Err(anyhow!(
                "Protocol v3 stream item window exceeds the connection ceiling"
            ));
        }
        if self.max_active_local_forwards > self.max_concurrent_streams {
            return Err(anyhow!(
                "Protocol v3 local forwarding ceiling exceeds the transport stream ceiling"
            ));
        }
        if self.request_timeout_ms > LOCAL_MAX_REQUEST_TIMEOUT_MS {
            return Err(anyhow!(
                "Protocol v3 request timeout exceeds the local safety ceiling"
            ));
        }
        if self
            .max_chunk_bytes
            .saturating_add(CHUNK_HEADER_BYTES)
            .saturating_add(MAX_CLIENT_BINARY_ENVELOPE_OVERHEAD_BYTES)
            > self.max_websocket_frame_bytes
        {
            return Err(anyhow!(
                "Protocol v3 chunks exceed the WebSocket frame ceiling"
            ));
        }
        Ok(())
    }
}

fn require_contract_string(value: &Value, key: &str, expected: &str) -> Result<()> {
    if value.get(key).and_then(Value::as_str) == Some(expected) {
        Ok(())
    } else {
        Err(anyhow!("Server advertised incompatible protocol v3 {key}"))
    }
}

fn required_usize(value: &Value, key: &str) -> Result<usize> {
    value
        .get(key)
        .and_then(Value::as_u64)
        .and_then(|value| value.try_into().ok())
        .filter(|value| *value > 0)
        .ok_or_else(|| anyhow!("Protocol v3 contract has invalid {key}"))
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum CodecError {
    #[error("unsupported protocol version")]
    UnsupportedVersion,
    #[error("invalid protocol v3 chunk header")]
    InvalidHeader,
    #[error("invalid protocol v3 chunk size")]
    InvalidChunkSize,
    #[error("Phoenix binary message is malformed")]
    InvalidPhoenixEnvelope,
    #[error("Phoenix binary field exceeds 255 bytes")]
    PhoenixFieldTooLarge,
    #[error("Phoenix binary message exceeds the WebSocket frame ceiling")]
    PhoenixFrameTooLarge,
    #[error("Phoenix binary message contains invalid UTF-8 metadata")]
    InvalidPhoenixMetadata,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct DecodedChunk<'a> {
    pub stream_id: Uuid,
    pub sequence: u32,
    pub offset: u64,
    pub data: &'a [u8],
}

pub(crate) fn encode_chunk(
    stream_id: Uuid,
    sequence: u32,
    offset: u64,
    data: &[u8],
    max_chunk_bytes: usize,
) -> std::result::Result<Vec<u8>, CodecError> {
    if data.is_empty() || data.len() > max_chunk_bytes {
        return Err(CodecError::InvalidChunkSize);
    }

    let mut encoded = Vec::with_capacity(CHUNK_HEADER_BYTES + data.len());
    encoded.push(VERSION);
    encoded.extend_from_slice(stream_id.as_bytes());
    encoded.extend_from_slice(&sequence.to_be_bytes());
    encoded.extend_from_slice(&offset.to_be_bytes());
    encoded.extend_from_slice(data);
    Ok(encoded)
}

pub(crate) fn decode_chunk(
    encoded: &[u8],
    max_chunk_bytes: usize,
) -> std::result::Result<DecodedChunk<'_>, CodecError> {
    if encoded.is_empty() {
        return Err(CodecError::InvalidHeader);
    }
    if encoded.first().copied() != Some(VERSION) {
        return Err(CodecError::UnsupportedVersion);
    }
    if encoded.len() <= CHUNK_HEADER_BYTES {
        return Err(CodecError::InvalidHeader);
    }

    let data = &encoded[CHUNK_HEADER_BYTES..];
    if data.len() > max_chunk_bytes {
        return Err(CodecError::InvalidChunkSize);
    }

    let stream_id = Uuid::from_slice(&encoded[1..17]).map_err(|_| CodecError::InvalidHeader)?;
    let sequence = u32::from_be_bytes(
        encoded[17..21]
            .try_into()
            .map_err(|_| CodecError::InvalidHeader)?,
    );
    let offset = u64::from_be_bytes(
        encoded[21..29]
            .try_into()
            .map_err(|_| CodecError::InvalidHeader)?,
    );

    Ok(DecodedChunk {
        stream_id,
        sequence,
        offset,
        data,
    })
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct ServerBinaryPush<'a> {
    pub join_ref: &'a str,
    pub topic: &'a str,
    pub event: &'a str,
    pub payload: &'a [u8],
}

pub(crate) fn decode_server_binary_push(
    encoded: &[u8],
) -> std::result::Result<ServerBinaryPush<'_>, CodecError> {
    if encoded.len() < 4 || encoded[0] != PHOENIX_PUSH {
        return Err(CodecError::InvalidPhoenixEnvelope);
    }

    let join_ref_bytes = encoded[1] as usize;
    let topic_bytes = encoded[2] as usize;
    let event_bytes = encoded[3] as usize;
    let metadata_bytes = join_ref_bytes
        .checked_add(topic_bytes)
        .and_then(|bytes| bytes.checked_add(event_bytes))
        .ok_or(CodecError::InvalidPhoenixEnvelope)?;
    if encoded.len() < 4 + metadata_bytes {
        return Err(CodecError::InvalidPhoenixEnvelope);
    }

    let mut offset = 4;
    let join_ref = take_utf8(encoded, &mut offset, join_ref_bytes)?;
    let topic = take_utf8(encoded, &mut offset, topic_bytes)?;
    let event = take_utf8(encoded, &mut offset, event_bytes)?;

    Ok(ServerBinaryPush {
        join_ref,
        topic,
        event,
        payload: &encoded[offset..],
    })
}

pub(crate) fn encode_client_binary_push(
    join_ref: &str,
    reference: &str,
    topic: &str,
    event: &str,
    payload: &[u8],
    max_frame_bytes: usize,
) -> std::result::Result<Vec<u8>, CodecError> {
    let lengths = [join_ref.len(), reference.len(), topic.len(), event.len()];
    if lengths
        .iter()
        .any(|length| *length > MAX_PHOENIX_FIELD_BYTES)
    {
        return Err(CodecError::PhoenixFieldTooLarge);
    }

    let capacity = 5usize
        .saturating_add(lengths.iter().sum::<usize>())
        .saturating_add(payload.len());
    if capacity > max_frame_bytes {
        return Err(CodecError::PhoenixFrameTooLarge);
    }

    let mut encoded = Vec::with_capacity(capacity);
    encoded.extend_from_slice(&[
        PHOENIX_PUSH,
        lengths[0] as u8,
        lengths[1] as u8,
        lengths[2] as u8,
        lengths[3] as u8,
    ]);
    encoded.extend_from_slice(join_ref.as_bytes());
    encoded.extend_from_slice(reference.as_bytes());
    encoded.extend_from_slice(topic.as_bytes());
    encoded.extend_from_slice(event.as_bytes());
    encoded.extend_from_slice(payload);
    Ok(encoded)
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct ControlMessage {
    pub join_ref: Option<String>,
    pub reference: Option<String>,
    pub topic: String,
    pub event: String,
    pub payload: Value,
}

pub(crate) fn encode_client_control(
    message: &ControlMessage,
    max_frame_bytes: usize,
) -> std::result::Result<String, CodecError> {
    let encoded = serde_json::to_string(&json!([
        message.join_ref,
        message.reference,
        message.topic,
        message.event,
        message.payload,
    ]))
    .map_err(|_| CodecError::InvalidPhoenixEnvelope)?;
    if encoded.len() > max_frame_bytes {
        return Err(CodecError::PhoenixFrameTooLarge);
    }
    Ok(encoded)
}

pub(crate) fn decode_server_control(
    encoded: &str,
) -> std::result::Result<ControlMessage, CodecError> {
    let fields: Value =
        serde_json::from_str(encoded).map_err(|_| CodecError::InvalidPhoenixEnvelope)?;
    let fields = fields
        .as_array()
        .filter(|fields| fields.len() >= 5)
        .ok_or(CodecError::InvalidPhoenixEnvelope)?;

    Ok(ControlMessage {
        join_ref: optional_text(&fields[0])?,
        reference: optional_text(&fields[1])?,
        topic: required_text(&fields[2])?,
        event: required_text(&fields[3])?,
        payload: fields[4].clone(),
    })
}

fn optional_text(value: &Value) -> std::result::Result<Option<String>, CodecError> {
    match value {
        Value::Null => Ok(None),
        Value::String(value) => Ok(Some(value.clone())),
        _ => Err(CodecError::InvalidPhoenixEnvelope),
    }
}

fn required_text(value: &Value) -> std::result::Result<String, CodecError> {
    value
        .as_str()
        .map(str::to_string)
        .ok_or(CodecError::InvalidPhoenixEnvelope)
}

fn take_utf8<'a>(
    encoded: &'a [u8],
    offset: &mut usize,
    bytes: usize,
) -> std::result::Result<&'a str, CodecError> {
    let end = offset
        .checked_add(bytes)
        .filter(|end| *end <= encoded.len())
        .ok_or(CodecError::InvalidPhoenixEnvelope)?;
    let value = std::str::from_utf8(&encoded[*offset..end])
        .map_err(|_| CodecError::InvalidPhoenixMetadata)?;
    *offset = end;
    Ok(value)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct StreamEvidence {
    pub bytes: u64,
    pub items: u64,
    pub sha256: String,
}

impl StreamEvidence {
    pub(crate) fn end_payload(&self, stream_id: Uuid) -> Value {
        json!({
            "stream_id": stream_id,
            "total_bytes": self.bytes,
            "sha256": self.sha256,
        })
    }
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum IntegrityError {
    #[error("protocol v3 chunk belongs to another stream")]
    StreamMismatch,
    #[error("protocol v3 chunk sequence is invalid")]
    InvalidSequence,
    #[error("protocol v3 chunk offset is invalid")]
    InvalidOffset,
    #[error("protocol v3 body exceeds its configured limit")]
    BodyTooLarge,
    #[error("protocol v3 body exceeds Content-Length")]
    ContentLengthExceeded,
    #[error("protocol v3 body does not match Content-Length")]
    ContentLengthMismatch,
    #[error("protocol v3 terminal byte count is invalid")]
    ByteCountMismatch,
    #[error("protocol v3 terminal checksum is invalid")]
    InvalidChecksum,
    #[error("protocol v3 terminal checksum does not match")]
    ChecksumMismatch,
}

pub(crate) struct StreamReceiver {
    stream_id: Uuid,
    max_body_bytes: u64,
    declared_length: Option<u64>,
    received_bytes: u64,
    received_items: u64,
    next_sequence: u32,
    hasher: Sha256,
}

impl StreamReceiver {
    pub(crate) fn new(
        stream_id: Uuid,
        max_body_bytes: usize,
        declared_length: Option<u64>,
    ) -> std::result::Result<Self, IntegrityError> {
        let max_body_bytes = max_body_bytes as u64;
        if declared_length.is_some_and(|length| length > max_body_bytes) {
            return Err(IntegrityError::BodyTooLarge);
        }
        Ok(Self {
            stream_id,
            max_body_bytes,
            declared_length,
            received_bytes: 0,
            received_items: 0,
            next_sequence: 0,
            hasher: Sha256::new(),
        })
    }

    #[cfg(test)]
    pub(crate) fn accept(
        &mut self,
        chunk: &DecodedChunk<'_>,
    ) -> std::result::Result<(), IntegrityError> {
        self.validate(chunk)?;
        self.commit(chunk.data);
        Ok(())
    }

    fn validate(&self, chunk: &DecodedChunk<'_>) -> std::result::Result<(), IntegrityError> {
        if chunk.stream_id != self.stream_id {
            return Err(IntegrityError::StreamMismatch);
        }
        if chunk.sequence != self.next_sequence {
            return Err(IntegrityError::InvalidSequence);
        }
        if chunk.offset != self.received_bytes {
            return Err(IntegrityError::InvalidOffset);
        }
        let next_bytes = self
            .received_bytes
            .checked_add(chunk.data.len() as u64)
            .ok_or(IntegrityError::BodyTooLarge)?;
        if next_bytes > self.max_body_bytes {
            return Err(IntegrityError::BodyTooLarge);
        }
        if self
            .declared_length
            .is_some_and(|declared| next_bytes > declared)
        {
            return Err(IntegrityError::ContentLengthExceeded);
        }
        Ok(())
    }

    fn commit(&mut self, data: &[u8]) {
        self.hasher.update(data);
        self.received_bytes += data.len() as u64;
        self.received_items += 1;
        self.next_sequence += 1;
    }

    pub(crate) fn finish(
        self,
        total_bytes: u64,
        checksum: &str,
    ) -> std::result::Result<StreamEvidence, IntegrityError> {
        if total_bytes != self.received_bytes {
            return Err(IntegrityError::ByteCountMismatch);
        }
        if self
            .declared_length
            .is_some_and(|declared| total_bytes != declared)
        {
            return Err(IntegrityError::ContentLengthMismatch);
        }

        let expected = decode_sha256(checksum)?;
        let actual = self.hasher.finalize();
        if actual.as_slice() != expected {
            return Err(IntegrityError::ChecksumMismatch);
        }

        Ok(StreamEvidence {
            bytes: self.received_bytes,
            items: self.received_items,
            sha256: digest_hex(&actual),
        })
    }
}

#[derive(Clone)]
pub(crate) struct StreamEncoder {
    stream_id: Uuid,
    max_body_bytes: u64,
    max_chunk_bytes: usize,
    sent_bytes: u64,
    sent_items: u64,
    next_sequence: u32,
    hasher: Sha256,
}

impl StreamEncoder {
    pub(crate) fn new(stream_id: Uuid, limits: StreamingLimits) -> Self {
        Self {
            stream_id,
            max_body_bytes: limits.max_response_body_bytes as u64,
            max_chunk_bytes: limits.max_chunk_bytes,
            sent_bytes: 0,
            sent_items: 0,
            next_sequence: 0,
            hasher: Sha256::new(),
        }
    }

    pub(crate) fn encode(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        let next_bytes = self
            .sent_bytes
            .checked_add(data.len() as u64)
            .ok_or_else(|| anyhow!("Protocol v3 response body is too large"))?;
        if next_bytes > self.max_body_bytes {
            return Err(anyhow!("Protocol v3 response body is too large"));
        }
        let encoded = encode_chunk(
            self.stream_id,
            self.next_sequence,
            self.sent_bytes,
            data,
            self.max_chunk_bytes,
        )?;
        self.hasher.update(data);
        self.sent_bytes = next_bytes;
        self.sent_items += 1;
        self.next_sequence = self
            .next_sequence
            .checked_add(1)
            .ok_or_else(|| anyhow!("Protocol v3 response has too many chunks"))?;
        Ok(encoded)
    }

    pub(crate) fn finish(self) -> StreamEvidence {
        let digest = self.hasher.finalize();
        StreamEvidence {
            bytes: self.sent_bytes,
            items: self.sent_items,
            sha256: digest_hex(&digest),
        }
    }
}

fn decode_sha256(checksum: &str) -> std::result::Result<[u8; 32], IntegrityError> {
    if checksum.len() != 64 || !checksum.is_ascii() {
        return Err(IntegrityError::InvalidChecksum);
    }
    let mut decoded = [0u8; 32];
    for (index, byte) in decoded.iter_mut().enumerate() {
        let offset = index * 2;
        *byte = u8::from_str_radix(&checksum[offset..offset + 2], 16)
            .map_err(|_| IntegrityError::InvalidChecksum)?;
    }
    Ok(decoded)
}

fn digest_hex(digest: &[u8]) -> String {
    use std::fmt::Write as _;

    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(&mut encoded, "{byte:02x}").expect("writing to a String cannot fail");
    }
    encoded
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct WindowUpdate {
    pub consumed_bytes: u64,
    pub consumed_items: u64,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum WindowError {
    #[error("protocol v3 stream is backpressured")]
    Backpressured,
    #[error("protocol v3 window counters regressed")]
    Regression,
    #[error("protocol v3 window exceeds sent data")]
    ExceedsSent,
    #[error("protocol v3 window counter overflow")]
    CounterOverflow,
    #[error("protocol v3 connection window accounting drifted")]
    AccountingDrift,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct SendWindow {
    max_bytes: u64,
    max_items: u64,
    sent_bytes: u64,
    sent_items: u64,
    consumed_bytes: u64,
    consumed_items: u64,
}

impl SendWindow {
    pub(crate) fn new(max_bytes: usize, max_items: usize) -> Self {
        Self {
            max_bytes: max_bytes as u64,
            max_items: max_items as u64,
            sent_bytes: 0,
            sent_items: 0,
            consumed_bytes: 0,
            consumed_items: 0,
        }
    }

    pub(crate) fn reserve(&mut self, bytes: usize) -> std::result::Result<(), WindowError> {
        let sent_bytes = self
            .sent_bytes
            .checked_add(bytes as u64)
            .ok_or(WindowError::CounterOverflow)?;
        let sent_items = self
            .sent_items
            .checked_add(1)
            .ok_or(WindowError::CounterOverflow)?;
        if sent_bytes - self.consumed_bytes > self.max_bytes
            || sent_items - self.consumed_items > self.max_items
        {
            return Err(WindowError::Backpressured);
        }
        self.sent_bytes = sent_bytes;
        self.sent_items = sent_items;
        Ok(())
    }

    pub(crate) fn acknowledge(
        &mut self,
        update: WindowUpdate,
    ) -> std::result::Result<(), WindowError> {
        if update.consumed_bytes < self.consumed_bytes
            || update.consumed_items < self.consumed_items
        {
            return Err(WindowError::Regression);
        }
        if update.consumed_bytes > self.sent_bytes || update.consumed_items > self.sent_items {
            return Err(WindowError::ExceedsSent);
        }
        self.consumed_bytes = update.consumed_bytes;
        self.consumed_items = update.consumed_items;
        Ok(())
    }

    pub(crate) fn in_flight(&self) -> WindowUpdate {
        WindowUpdate {
            consumed_bytes: self.sent_bytes - self.consumed_bytes,
            consumed_items: self.sent_items - self.consumed_items,
        }
    }
}

#[derive(Clone)]
pub(crate) struct ConnectionSendWindow {
    inner: Arc<Mutex<ConnectionSendWindowState>>,
    credit_available: Arc<Notify>,
}

struct ConnectionSendWindowState {
    max_bytes: u64,
    max_items: u64,
    reserved_bytes: u64,
    reserved_items: u64,
}

impl ConnectionSendWindow {
    pub(crate) fn new(max_bytes: usize, max_items: usize) -> Self {
        Self {
            inner: Arc::new(Mutex::new(ConnectionSendWindowState {
                max_bytes: max_bytes as u64,
                max_items: max_items as u64,
                reserved_bytes: 0,
                reserved_items: 0,
            })),
            credit_available: Arc::new(Notify::new()),
        }
    }

    fn try_reserve(&self, bytes: usize, items: u64) -> std::result::Result<(), WindowError> {
        let mut state = self.inner.lock().map_err(|_| WindowError::Backpressured)?;
        let reserved_bytes = state
            .reserved_bytes
            .checked_add(bytes as u64)
            .ok_or(WindowError::CounterOverflow)?;
        let reserved_items = state
            .reserved_items
            .checked_add(items)
            .ok_or(WindowError::CounterOverflow)?;
        if reserved_bytes > state.max_bytes || reserved_items > state.max_items {
            return Err(WindowError::Backpressured);
        }
        state.reserved_bytes = reserved_bytes;
        state.reserved_items = reserved_items;
        Ok(())
    }

    fn release(&self, bytes: u64, items: u64) -> std::result::Result<(), WindowError> {
        let mut state = self
            .inner
            .lock()
            .map_err(|_| WindowError::AccountingDrift)?;
        if bytes > state.reserved_bytes || items > state.reserved_items {
            return Err(WindowError::AccountingDrift);
        }
        state.reserved_bytes -= bytes;
        state.reserved_items -= items;
        drop(state);

        if bytes > 0 || items > 0 {
            self.credit_available.notify_waiters();
        }
        Ok(())
    }

    pub(crate) fn credit_notifier(&self) -> Arc<Notify> {
        self.credit_available.clone()
    }

    #[cfg(test)]
    fn reserved(&self) -> (u64, u64) {
        let state = self.inner.lock().expect("connection window lock");
        (state.reserved_bytes, state.reserved_items)
    }
}

pub(crate) struct ResponseStream {
    topic: String,
    join_ref: String,
    encoder: Option<StreamEncoder>,
    window: SendWindow,
    connection: ConnectionSendWindow,
    max_frame_bytes: usize,
    reserved_bytes: u64,
    reserved_items: u64,
    ended: bool,
}

impl ResponseStream {
    pub(crate) fn new(
        stream_id: Uuid,
        topic: String,
        join_ref: String,
        limits: StreamingLimits,
        connection: ConnectionSendWindow,
    ) -> Result<Self> {
        limits.validate()?;
        if topic.len() > MAX_PHOENIX_FIELD_BYTES || join_ref.len() > MAX_PHOENIX_FIELD_BYTES {
            return Err(CodecError::PhoenixFieldTooLarge.into());
        }
        Ok(Self {
            topic,
            join_ref,
            encoder: Some(StreamEncoder::new(stream_id, limits)),
            window: SendWindow::new(limits.stream_window_bytes, limits.stream_window_items),
            connection,
            max_frame_bytes: limits.max_websocket_frame_bytes,
            reserved_bytes: 0,
            reserved_items: 0,
            ended: false,
        })
    }

    pub(crate) fn try_encode_chunk(&mut self, data: &[u8]) -> Result<Vec<u8>> {
        if self.ended {
            return Err(anyhow!("Protocol v3 response stream already ended"));
        }
        let encoder = self
            .encoder
            .as_ref()
            .ok_or_else(|| anyhow!("Protocol v3 response stream already ended"))?;
        if data.is_empty()
            || data.len() > encoder.max_chunk_bytes
            || encoder.sent_bytes.saturating_add(data.len() as u64) > encoder.max_body_bytes
        {
            return Err(anyhow!("Protocol v3 response chunk is invalid"));
        }

        let mut next_window = self.window;
        next_window.reserve(data.len())?;
        self.connection.try_reserve(data.len(), 1)?;

        let mut next_encoder = self.encoder.as_ref().expect("checked above").clone();
        let encoded = match next_encoder.encode(data) {
            Ok(encoded) => encoded,
            Err(error) => {
                self.connection.release(data.len() as u64, 1)?;
                return Err(error);
            }
        };
        let envelope = encode_client_binary_push(
            &self.join_ref,
            "",
            &self.topic,
            "tunnel_response_chunk",
            &encoded,
            self.max_frame_bytes,
        );

        match envelope {
            Ok(envelope) => {
                self.encoder = Some(next_encoder);
                self.window = next_window;
                self.reserved_bytes += data.len() as u64;
                self.reserved_items += 1;
                Ok(envelope)
            }
            Err(error) => {
                self.connection.release(data.len() as u64, 1)?;
                Err(error.into())
            }
        }
    }

    pub(crate) fn acknowledge(&mut self, update: WindowUpdate) -> Result<()> {
        let released_bytes = update
            .consumed_bytes
            .checked_sub(self.window.consumed_bytes)
            .ok_or(WindowError::Regression)?;
        let released_items = update
            .consumed_items
            .checked_sub(self.window.consumed_items)
            .ok_or(WindowError::Regression)?;
        if released_bytes > self.reserved_bytes || released_items > self.reserved_items {
            return Err(WindowError::AccountingDrift.into());
        }

        let mut next_window = self.window;
        next_window.acknowledge(update)?;
        self.connection.release(released_bytes, released_items)?;
        self.window = next_window;
        self.reserved_bytes -= released_bytes;
        self.reserved_items -= released_items;
        Ok(())
    }

    pub(crate) fn finish(&mut self) -> Result<StreamEvidence> {
        if self.ended {
            return Err(anyhow!("Protocol v3 response stream already ended"));
        }
        self.ended = true;
        Ok(self.encoder.take().expect("open encoder").finish())
    }

    pub(crate) fn fully_consumed(&self) -> bool {
        self.ended
            && self.window.in_flight()
                == (WindowUpdate {
                    consumed_bytes: 0,
                    consumed_items: 0,
                })
    }

    pub(crate) fn connection_credit_notifier(&self) -> Arc<Notify> {
        self.connection.credit_notifier()
    }
}

impl Drop for ResponseStream {
    fn drop(&mut self) {
        let released = self
            .connection
            .release(self.reserved_bytes, self.reserved_items);
        debug_assert!(
            released.is_ok(),
            "protocol v3 response connection accounting drifted"
        );
        self.reserved_bytes = 0;
        self.reserved_items = 0;
    }
}

#[derive(Clone)]
pub(crate) struct ConnectionBudget {
    bytes: Arc<Semaphore>,
    items: Arc<Semaphore>,
}

impl ConnectionBudget {
    pub(crate) fn new(byte_capacity: usize, item_capacity: usize) -> Result<Self> {
        if byte_capacity == 0
            || byte_capacity > Semaphore::MAX_PERMITS
            || item_capacity == 0
            || item_capacity > Semaphore::MAX_PERMITS
        {
            return Err(anyhow!(
                "Protocol v3 connection window exceeds local runtime limits"
            ));
        }

        Ok(Self {
            bytes: Arc::new(Semaphore::new(byte_capacity)),
            items: Arc::new(Semaphore::new(item_capacity)),
        })
    }

    fn try_acquire(&self, bytes: usize) -> Option<(OwnedSemaphorePermit, OwnedSemaphorePermit)> {
        let permits: u32 = bytes.try_into().ok()?;
        let byte_permit = self.bytes.clone().try_acquire_many_owned(permits).ok()?;
        let item_permit = self.items.clone().try_acquire_owned().ok()?;
        Some((byte_permit, item_permit))
    }

    #[cfg(test)]
    pub(crate) fn available_bytes(&self) -> usize {
        self.bytes.available_permits()
    }

    #[cfg(test)]
    pub(crate) fn available_items(&self) -> usize {
        self.items.available_permits()
    }
}

struct OwnedBodyChunk {
    data: Bytes,
    _item: OwnedSemaphorePermit,
    _stream_bytes: OwnedSemaphorePermit,
    _connection_bytes: OwnedSemaphorePermit,
    _connection_item: OwnedSemaphorePermit,
}

enum BodyMessage {
    Chunk(OwnedBodyChunk),
    End,
    Abort(String),
}

#[derive(Debug, Error, PartialEq, Eq)]
pub(crate) enum PipeError {
    #[error("protocol v3 body pipe is backpressured")]
    Backpressured,
    #[error("protocol v3 body pipe is closed")]
    Closed,
    #[error("protocol v3 body chunk exceeds the negotiated maximum")]
    ChunkTooLarge,
}

pub(crate) struct BodyPipeSender {
    tx: mpsc::Sender<BodyMessage>,
    items: Arc<Semaphore>,
    stream_bytes: Arc<Semaphore>,
    connection: ConnectionBudget,
    max_chunk_bytes: usize,
}

pub(crate) struct BodyPipeReceiver {
    rx: mpsc::Receiver<BodyMessage>,
    consumed_bytes: u64,
    consumed_items: u64,
    terminal_received: bool,
}

#[derive(Debug)]
pub(crate) struct ConsumedBodyChunk {
    pub data: Bytes,
    pub window: WindowUpdate,
}

pub(crate) fn body_pipe(
    limits: StreamingLimits,
    connection: ConnectionBudget,
) -> Result<(BodyPipeSender, BodyPipeReceiver)> {
    limits.validate()?;
    let channel_capacity = limits
        .stream_window_items
        .checked_add(1)
        .ok_or_else(|| anyhow!("Protocol v3 item window exceeds local runtime limits"))?;

    // One extra slot is reserved for an abort notification even when every
    // negotiated data item is waiting for the local HTTP consumer.
    let (tx, rx) = mpsc::channel(channel_capacity);
    Ok((
        BodyPipeSender {
            tx,
            items: Arc::new(Semaphore::new(limits.stream_window_items)),
            stream_bytes: Arc::new(Semaphore::new(limits.stream_window_bytes)),
            connection,
            max_chunk_bytes: limits.max_chunk_bytes,
        },
        BodyPipeReceiver {
            rx,
            consumed_bytes: 0,
            consumed_items: 0,
            terminal_received: false,
        },
    ))
}

impl BodyPipeSender {
    pub(crate) fn try_send(&self, data: Bytes) -> std::result::Result<(), PipeError> {
        if data.is_empty() || data.len() > self.max_chunk_bytes {
            return Err(PipeError::ChunkTooLarge);
        }
        let bytes: u32 = data
            .len()
            .try_into()
            .map_err(|_| PipeError::ChunkTooLarge)?;
        let item = self
            .items
            .clone()
            .try_acquire_owned()
            .map_err(|_| PipeError::Backpressured)?;
        let stream_bytes = self
            .stream_bytes
            .clone()
            .try_acquire_many_owned(bytes)
            .map_err(|_| PipeError::Backpressured)?;
        let (connection_bytes, connection_item) = self
            .connection
            .try_acquire(data.len())
            .ok_or(PipeError::Backpressured)?;

        self.tx
            .try_send(BodyMessage::Chunk(OwnedBodyChunk {
                data,
                _item: item,
                _stream_bytes: stream_bytes,
                _connection_bytes: connection_bytes,
                _connection_item: connection_item,
            }))
            .map_err(|error| match error {
                mpsc::error::TrySendError::Full(_) => PipeError::Backpressured,
                mpsc::error::TrySendError::Closed(_) => PipeError::Closed,
            })
    }

    pub(crate) fn finish(self) -> std::result::Result<(), PipeError> {
        self.send_terminal(BodyMessage::End)
    }

    pub(crate) fn abort(self, reason: impl Into<String>) {
        let _ = self.send_terminal(BodyMessage::Abort(reason.into()));
    }

    fn send_terminal(self, message: BodyMessage) -> std::result::Result<(), PipeError> {
        self.tx.try_send(message).map_err(|error| match error {
            mpsc::error::TrySendError::Full(_) => PipeError::Backpressured,
            mpsc::error::TrySendError::Closed(_) => PipeError::Closed,
        })
    }
}

impl BodyPipeReceiver {
    pub(crate) async fn recv(&mut self) -> std::result::Result<Option<ConsumedBodyChunk>, String> {
        if self.terminal_received {
            return Ok(None);
        }

        match self.rx.recv().await {
            Some(BodyMessage::Chunk(chunk)) => {
                let OwnedBodyChunk { data, .. } = chunk;
                self.consumed_bytes += data.len() as u64;
                self.consumed_items += 1;
                Ok(Some(ConsumedBodyChunk {
                    data,
                    window: WindowUpdate {
                        consumed_bytes: self.consumed_bytes,
                        consumed_items: self.consumed_items,
                    },
                }))
            }
            Some(BodyMessage::End) => {
                self.terminal_received = true;
                Ok(None)
            }
            Some(BodyMessage::Abort(reason)) => {
                self.terminal_received = true;
                Err(reason)
            }
            None => Err("protocol v3 body pipe closed before terminal evidence".to_string()),
        }
    }
}

pub(crate) struct RequestStreamReceiver {
    integrity: StreamReceiver,
    body: BodyPipeSender,
    max_chunk_bytes: usize,
}

impl RequestStreamReceiver {
    pub(crate) fn new(
        stream_id: Uuid,
        declared_length: Option<u64>,
        limits: StreamingLimits,
        connection: ConnectionBudget,
    ) -> Result<(Self, BodyPipeReceiver)> {
        limits.validate()?;
        let integrity =
            StreamReceiver::new(stream_id, limits.max_request_body_bytes, declared_length)?;
        let (body, receiver) = body_pipe(limits, connection)?;
        Ok((
            Self {
                integrity,
                body,
                max_chunk_bytes: limits.max_chunk_bytes,
            },
            receiver,
        ))
    }

    pub(crate) fn accept_frame(&mut self, encoded: &[u8]) -> Result<()> {
        let chunk = decode_chunk(encoded, self.max_chunk_bytes)?;
        self.integrity.validate(&chunk)?;
        self.body.try_send(Bytes::copy_from_slice(chunk.data))?;
        self.integrity.commit(chunk.data);
        Ok(())
    }

    pub(crate) fn finish(self, total_bytes: u64, checksum: &str) -> Result<StreamEvidence> {
        let Self {
            integrity, body, ..
        } = self;
        match integrity.finish(total_bytes, checksum) {
            Ok(evidence) => match body.finish() {
                Ok(()) | Err(PipeError::Closed) => Ok(evidence),
                Err(error) => Err(error.into()),
            },
            Err(error) => {
                body.abort(error.to_string());
                Err(error.into())
            }
        }
    }

    pub(crate) fn abort(self, reason: impl Into<String>) {
        self.body.abort(reason);
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RequestStart {
    pub stream_id: Uuid,
    pub deadline_unix_ms: u64,
    pub timeout_ms: u64,
    pub method: String,
    pub path: String,
    pub query_string: String,
    pub headers: Vec<(String, String)>,
    pub content_length: Option<u64>,
    pub replay: bool,
}

impl RequestStart {
    pub(crate) fn from_payload(payload: &Value, limits: StreamingLimits) -> Result<Self> {
        validate_payload_size(
            payload,
            limits.max_start_metadata_bytes,
            "Protocol v3 request start metadata exceeds the negotiated limit",
        )?;

        let stream_id = payload
            .get("stream_id")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("Protocol v3 request start is missing stream_id"))?
            .parse()?;
        let deadline_unix_ms = payload
            .get("deadline_unix_ms")
            .and_then(Value::as_u64)
            .filter(|deadline| *deadline > 0)
            .ok_or_else(|| anyhow!("Protocol v3 request start has an invalid deadline"))?;
        let timeout_ms = payload
            .get("timeout_ms")
            .and_then(Value::as_u64)
            .filter(|timeout| {
                *timeout > 0 && *timeout <= limits.request_timeout_ms.try_into().unwrap_or(u64::MAX)
            })
            .ok_or_else(|| anyhow!("Protocol v3 request start has an invalid relative timeout"))?;
        let method = non_empty_text(payload, "method", 32)?;
        let path = non_empty_text(payload, "path", 16 * 1024)?;
        let query_string = payload
            .get("query_string")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        if query_string.len() > 16 * 1024 {
            return Err(anyhow!("Protocol v3 request query string is too large"));
        }
        let headers = ordered_headers(payload.get("headers"))?;
        let content_length = match payload.get("content_length") {
            None | Some(Value::Null) => None,
            Some(value) => Some(
                value
                    .as_u64()
                    .filter(|length| *length <= limits.max_request_body_bytes as u64)
                    .ok_or_else(|| anyhow!("Protocol v3 request Content-Length is invalid"))?,
            ),
        };
        let replay = payload
            .get("replay")
            .and_then(Value::as_bool)
            .unwrap_or(false);

        Ok(Self {
            stream_id,
            deadline_unix_ms,
            timeout_ms,
            method,
            path,
            query_string,
            headers,
            content_length,
            replay,
        })
    }
}

fn non_empty_text(payload: &Value, key: &str, max_bytes: usize) -> Result<String> {
    payload
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty() && value.len() <= max_bytes)
        .map(str::to_string)
        .ok_or_else(|| anyhow!("Protocol v3 request has invalid {key}"))
}

fn ordered_headers(value: Option<&Value>) -> Result<Vec<(String, String)>> {
    let headers = value
        .and_then(Value::as_array)
        .ok_or_else(|| anyhow!("Protocol v3 request headers must be ordered pairs"))?;
    headers
        .iter()
        .map(|header| {
            let pair = header
                .as_array()
                .filter(|pair| pair.len() == 2)
                .ok_or_else(|| anyhow!("Protocol v3 request header is malformed"))?;
            let name = pair[0]
                .as_str()
                .filter(|name| !name.is_empty())
                .ok_or_else(|| anyhow!("Protocol v3 request header name is invalid"))?;
            let value = pair[1]
                .as_str()
                .ok_or_else(|| anyhow!("Protocol v3 request header value is invalid"))?;
            if name.len() > 1024
                || value.len() > 64 * 1024
                || name.contains(['\r', '\n', '\0'])
                || value.contains(['\r', '\n', '\0'])
            {
                return Err(anyhow!("Protocol v3 request header is invalid"));
            }
            Ok((name.to_string(), value.to_string()))
        })
        .collect()
}

pub(crate) struct IncomingRequest {
    pub start: RequestStart,
    pub body: BodyPipeReceiver,
}

pub(crate) struct RequestStreams {
    topic: String,
    limits: StreamingLimits,
    connection: ConnectionBudget,
    streams: HashMap<Uuid, RequestStreamReceiver>,
}

impl RequestStreams {
    pub(crate) fn new(topic: String, limits: StreamingLimits) -> Result<Self> {
        limits.validate()?;
        let connection = ConnectionBudget::new(
            limits.connection_window_bytes,
            limits.connection_window_items,
        )?;

        Ok(Self {
            topic,
            limits,
            connection,
            streams: HashMap::new(),
        })
    }

    pub(crate) fn start(&mut self, payload: &Value) -> Result<IncomingRequest> {
        if self.streams.len() >= self.limits.max_concurrent_streams {
            return Err(anyhow!("Protocol v3 request stream capacity is full"));
        }
        let start = RequestStart::from_payload(payload, self.limits)?;
        if self.streams.contains_key(&start.stream_id) {
            return Err(anyhow!("Protocol v3 request stream already exists"));
        }
        let (receiver, body) = RequestStreamReceiver::new(
            start.stream_id,
            start.content_length,
            self.limits,
            self.connection.clone(),
        )?;
        self.streams.insert(start.stream_id, receiver);
        Ok(IncomingRequest { start, body })
    }

    pub(crate) fn accept_binary_push(&mut self, encoded: &[u8]) -> Result<()> {
        let push = decode_server_binary_push(encoded)?;
        if push.topic != self.topic || push.event != "tunnel_request_chunk" {
            return Err(anyhow!("Unexpected protocol v3 binary channel event"));
        }
        let chunk = decode_chunk(push.payload, self.limits.max_chunk_bytes)?;
        let receiver = self
            .streams
            .get_mut(&chunk.stream_id)
            .ok_or_else(|| anyhow!("Protocol v3 request stream is unknown"))?;
        receiver.accept_frame(push.payload)
    }

    pub(crate) fn finish(&mut self, payload: &Value) -> Result<StreamEvidence> {
        let stream_id = payload_stream_id(payload)?;
        let total_bytes = payload
            .get("total_bytes")
            .and_then(Value::as_u64)
            .ok_or_else(|| anyhow!("Protocol v3 request end has invalid total_bytes"))?;
        let checksum = payload
            .get("sha256")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("Protocol v3 request end has invalid sha256"))?;
        let receiver = self
            .streams
            .remove(&stream_id)
            .ok_or_else(|| anyhow!("Protocol v3 request stream is unknown"))?;
        receiver.finish(total_bytes, checksum)
    }

    pub(crate) fn cancel(&mut self, stream_id: Uuid, reason: impl Into<String>) {
        if let Some(receiver) = self.streams.remove(&stream_id) {
            receiver.abort(reason);
        }
    }

    pub(crate) fn contains(&self, stream_id: Uuid) -> bool {
        self.streams.contains_key(&stream_id)
    }
}

impl Drop for RequestStreams {
    fn drop(&mut self) {
        for (_stream_id, receiver) in self.streams.drain() {
            receiver.abort("tunnel_disconnected");
        }
    }
}

fn payload_stream_id(payload: &Value) -> Result<Uuid> {
    payload
        .get("stream_id")
        .and_then(Value::as_str)
        .ok_or_else(|| anyhow!("Protocol v3 event is missing stream_id"))?
        .parse()
        .map_err(Into::into)
}

pub(crate) fn response_start_payload(
    stream_id: Uuid,
    status: u16,
    headers: &[(String, String)],
    content_length: Option<u64>,
) -> Value {
    json!({
        "stream_id": stream_id,
        "status": status,
        "headers": headers,
        "content_length": content_length,
    })
}

pub(crate) fn validate_control_payload(payload: &Value, max_bytes: usize) -> Result<()> {
    validate_payload_size(
        payload,
        max_bytes,
        "Protocol v3 control payload exceeds the negotiated limit",
    )
}

fn validate_payload_size(payload: &Value, max_bytes: usize, message: &str) -> Result<()> {
    if serde_json::to_vec(payload)?.len() <= max_bytes {
        Ok(())
    } else {
        Err(anyhow!(message.to_string()))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::Value;
    use std::fs;

    const CONTRACT_FIXTURE: &str = "fixtures/tunnel_streaming_v3.json";
    const GOLDEN_STREAM_ID: &str = "00112233-4455-6677-8899-aabbccddeeff";

    fn contract() -> Value {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(CONTRACT_FIXTURE);
        serde_json::from_slice(&fs::read(path).expect("read protocol v3 fixture"))
            .expect("decode protocol v3 fixture")
    }

    fn limits() -> StreamingLimits {
        StreamingLimits::from_contract(&contract()).expect("valid protocol v3 fixture")
    }

    fn decode_hex(encoded: &str) -> Vec<u8> {
        encoded
            .as_bytes()
            .chunks_exact(2)
            .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
            .collect()
    }

    #[test]
    fn service_contract_fixture_matches_cli_limits() {
        let fixture = contract();
        let parsed = StreamingLimits::from_contract(&fixture).unwrap();
        assert_eq!(parsed.max_request_body_bytes, 100_000_000);
        assert_eq!(parsed.max_response_body_bytes, 100_000_000);
        assert_eq!(parsed.max_start_metadata_bytes, 55_000);
        assert_eq!(parsed.max_control_payload_bytes, 63_000);
        assert_eq!(parsed.max_chunk_bytes, 64_000);
        assert_eq!(parsed.stream_window_bytes, 1024 * 1024);
        assert_eq!(parsed.stream_window_items, 16);
        assert_eq!(parsed.connection_window_bytes, 16 * 1024 * 1024);
        assert_eq!(parsed.connection_window_items, 256);
        assert_eq!(parsed.websocket_admission_bytes, 25 * 1024 * 1024);
        assert_eq!(parsed.websocket_admission_items, 800);
        assert_eq!(parsed.service_ingress_admission_bytes, 16 * 1024 * 1024);
        assert_eq!(parsed.service_ingress_admission_items, 128);
        assert_eq!(parsed.max_concurrent_streams, 128);
        assert_eq!(parsed.max_active_local_forwards, 8);
        assert_eq!(parsed.request_timeout_ms, 80_000);
    }

    #[test]
    fn unsafe_negotiated_limits_are_rejected_before_runtime_construction() {
        let mut oversized_frame = limits();
        oversized_frame.max_websocket_frame_bytes = LOCAL_MAX_WEBSOCKET_FRAME_BYTES + 1;
        assert!(oversized_frame.validate().is_err());

        let mut oversized_window = limits();
        oversized_window.stream_window_items = Semaphore::MAX_PERMITS;
        assert!(oversized_window.validate().is_err());
        assert!(ConnectionBudget::new(Semaphore::MAX_PERMITS + 1, 1).is_err());
        assert!(ConnectionBudget::new(1, Semaphore::MAX_PERMITS + 1).is_err());

        let mut oversized_stream_items = limits();
        oversized_stream_items.stream_window_items =
            oversized_stream_items.connection_window_items + 1;
        assert!(oversized_stream_items.validate().is_err());

        let mut oversized_start = limits();
        oversized_start.max_start_metadata_bytes = oversized_start.max_control_payload_bytes + 1;
        assert!(oversized_start.validate().is_err());

        let mut oversized_control = limits();
        oversized_control.max_control_payload_bytes =
            oversized_control.max_websocket_frame_bytes + 1;
        assert!(oversized_control.validate().is_err());

        let mut oversized_local_dispatch = limits();
        oversized_local_dispatch.max_active_local_forwards =
            oversized_local_dispatch.max_concurrent_streams + 1;
        assert!(oversized_local_dispatch.validate().is_err());

        let mut oversized_timeout = limits();
        oversized_timeout.request_timeout_ms = LOCAL_MAX_REQUEST_TIMEOUT_MS + 1;
        assert!(oversized_timeout.validate().is_err());
    }

    #[test]
    fn incompatible_wire_semantics_are_rejected_before_v3_activation() {
        let incompatible_fields = [
            ("/status", json!("prototype")),
            ("/control_encoding", json!("msgpack")),
            ("/transport", json!("json_base64")),
            ("/capture_mode", json!("unknown")),
            ("/byte_order", json!("little_endian")),
            ("/chunk_header/bytes", json!(30)),
            ("/flow_control/scheme", json!("incremental_credit")),
            ("/limits/connection_window_scope", json!("shared_duplex")),
            (
                "/limits/max_duplex_connection_window_bytes",
                json!(16_777_216),
            ),
            ("/limits/connection_window_items", json!(0)),
        ];

        for (pointer, replacement) in incompatible_fields {
            let mut fixture = contract();
            *fixture
                .pointer_mut(pointer)
                .expect("protocol fixture field must exist") = replacement;

            assert!(
                StreamingLimits::from_contract(&fixture).is_err(),
                "accepted incompatible protocol semantic at {pointer}"
            );
        }

        let mut fixture = contract();
        fixture["limits"]
            .as_object_mut()
            .expect("limits object")
            .remove("connection_window_items");
        assert!(StreamingLimits::from_contract(&fixture).is_err());
    }

    #[test]
    fn binary_chunk_matches_the_cross_component_golden_frame() {
        let fixture = contract();
        let golden = &fixture["golden_chunk"];
        let stream_id = Uuid::parse_str(golden["stream_id"].as_str().unwrap()).unwrap();
        let data = decode_hex(golden["data_hex"].as_str().unwrap());
        let encoded = encode_chunk(
            stream_id,
            golden["sequence"].as_u64().unwrap().try_into().unwrap(),
            golden["offset"].as_u64().unwrap(),
            &data,
            limits().max_chunk_bytes,
        )
        .unwrap();
        assert_eq!(encoded, decode_hex(golden["encoded_hex"].as_str().unwrap()));

        let decoded = decode_chunk(&encoded, limits().max_chunk_bytes).unwrap();
        assert_eq!(decoded.stream_id, stream_id);
        assert_eq!(decoded.sequence, 0x0102_0304);
        assert_eq!(decoded.offset, 0x0102_0304_0506_0708);
        assert_eq!(decoded.data, &[0, 1, 2, 255]);
    }

    #[test]
    fn phoenix_v2_json_controls_use_the_required_array_shape() {
        let message = ControlMessage {
            join_ref: Some("1".to_string()),
            reference: None,
            topic: "tunnel:connect".to_string(),
            event: "tunnel_stream_window".to_string(),
            payload: json!({
                "stream_id": GOLDEN_STREAM_ID,
                "direction": "request",
                "consumed_bytes": 64_000,
                "consumed_items": 1,
            }),
        };
        let encoded = encode_client_control(&message, 65_536).unwrap();
        assert!(encoded.starts_with("[\"1\",null,\"tunnel:connect\""));
        assert_eq!(decode_server_control(&encoded).unwrap(), message);

        assert_eq!(
            decode_server_control(r#"{"topic":"tunnel:connect"}"#),
            Err(CodecError::InvalidPhoenixEnvelope)
        );
    }

    #[test]
    fn phoenix_v2_binary_envelopes_preserve_metadata_and_payload() {
        let fixture = contract();
        let payload = decode_hex(fixture["golden_chunk"]["encoded_hex"].as_str().unwrap());
        let server = {
            let mut encoded = vec![PHOENIX_PUSH, 1, 14, 20];
            encoded.extend_from_slice(b"1tunnel:connecttunnel_request_chunk");
            encoded.extend_from_slice(&payload);
            encoded
        };
        let decoded = decode_server_binary_push(&server).unwrap();
        assert_eq!(decoded.join_ref, "1");
        assert_eq!(decoded.topic, "tunnel:connect");
        assert_eq!(decoded.event, "tunnel_request_chunk");
        assert_eq!(decoded.payload, payload);

        let client = encode_client_binary_push(
            "1",
            "",
            "tunnel:connect",
            "tunnel_response_chunk",
            &payload,
            limits().max_websocket_frame_bytes,
        )
        .unwrap();
        assert_eq!(client[0..5], [PHOENIX_PUSH, 1, 0, 14, 21]);
        assert!(client.ends_with(&payload));
    }

    #[test]
    fn malformed_binary_envelopes_and_chunks_are_rejected() {
        assert_eq!(
            decode_server_binary_push(&[PHOENIX_PUSH, 2, 1, 1, b'a']),
            Err(CodecError::InvalidPhoenixEnvelope)
        );
        assert_eq!(
            decode_chunk(&[2, 0, 0], limits().max_chunk_bytes),
            Err(CodecError::UnsupportedVersion)
        );
        assert_eq!(
            encode_chunk(Uuid::new_v4(), 0, 0, &[], limits().max_chunk_bytes),
            Err(CodecError::InvalidChunkSize)
        );
    }

    #[test]
    fn receiver_validates_order_length_and_checksum_without_a_body_buffer() {
        let stream_id = Uuid::new_v4();
        let body = b"abcdef";
        let mut receiver = StreamReceiver::new(stream_id, 10, Some(6)).unwrap();
        let first = encode_chunk(stream_id, 0, 0, &body[..3], 4).unwrap();
        let second = encode_chunk(stream_id, 1, 3, &body[3..], 4).unwrap();
        receiver.accept(&decode_chunk(&first, 4).unwrap()).unwrap();
        receiver.accept(&decode_chunk(&second, 4).unwrap()).unwrap();

        let checksum = digest_hex(&Sha256::digest(body));
        let evidence = receiver.finish(6, &checksum).unwrap();
        assert_eq!(evidence.bytes, 6);
        assert_eq!(evidence.items, 2);
        assert_eq!(evidence.sha256, checksum);
    }

    #[test]
    fn send_window_enforces_bytes_items_and_cumulative_credit() {
        let mut window = SendWindow::new(8, 2);
        window.reserve(4).unwrap();
        window.reserve(4).unwrap();
        assert_eq!(window.reserve(1), Err(WindowError::Backpressured));
        window
            .acknowledge(WindowUpdate {
                consumed_bytes: 4,
                consumed_items: 1,
            })
            .unwrap();
        window.reserve(1).unwrap();
        assert_eq!(
            window.acknowledge(WindowUpdate {
                consumed_bytes: 3,
                consumed_items: 1,
            }),
            Err(WindowError::Regression)
        );
        assert_eq!(
            window.acknowledge(WindowUpdate {
                consumed_bytes: 10,
                consumed_items: 3,
            }),
            Err(WindowError::ExceedsSent)
        );
    }

    #[tokio::test]
    async fn shared_response_credit_wakes_blocked_streams_without_polling() {
        let mut limits = limits();
        limits.max_chunk_bytes = 4;
        limits.stream_window_bytes = 8;
        limits.stream_window_items = 1;
        limits.connection_window_bytes = 8;
        limits.connection_window_items = 1;

        let connection = ConnectionSendWindow::new(
            limits.connection_window_bytes,
            limits.connection_window_items,
        );
        let mut first = ResponseStream::new(
            Uuid::new_v4(),
            "tunnel:test".to_string(),
            "1".to_string(),
            limits,
            connection.clone(),
        )
        .unwrap();
        let mut second = ResponseStream::new(
            Uuid::new_v4(),
            "tunnel:test".to_string(),
            "1".to_string(),
            limits,
            connection.clone(),
        )
        .unwrap();

        first.try_encode_chunk(b"1234").unwrap();
        assert_eq!(connection.reserved(), (4, 1));
        assert!(second.try_encode_chunk(b"5678").is_err());

        let notifier = second.connection_credit_notifier();
        let credit = notifier.notified();
        tokio::pin!(credit);
        credit.as_mut().enable();

        first
            .acknowledge(WindowUpdate {
                consumed_bytes: 4,
                consumed_items: 1,
            })
            .unwrap();

        tokio::time::timeout(std::time::Duration::from_millis(100), credit)
            .await
            .expect("released shared credit should wake a waiting stream");
        second.try_encode_chunk(b"5678").unwrap();
        assert_eq!(connection.reserved(), (4, 1));

        drop(first);
        drop(second);
        assert_eq!(connection.reserved(), (0, 0));
    }

    #[test]
    fn response_acknowledgement_and_drop_release_exact_connection_credit() {
        let mut limits = limits();
        limits.max_chunk_bytes = 4;
        limits.stream_window_bytes = 8;
        limits.stream_window_items = 2;
        limits.connection_window_bytes = 8;
        limits.connection_window_items = 2;

        let connection = ConnectionSendWindow::new(8, 2);
        let mut response = ResponseStream::new(
            Uuid::new_v4(),
            "tunnel:test".to_string(),
            "1".to_string(),
            limits,
            connection.clone(),
        )
        .unwrap();

        response.try_encode_chunk(b"1234").unwrap();
        response.try_encode_chunk(b"5678").unwrap();
        assert_eq!(connection.reserved(), (8, 2));

        response
            .acknowledge(WindowUpdate {
                consumed_bytes: 4,
                consumed_items: 1,
            })
            .unwrap();
        assert_eq!(connection.reserved(), (4, 1));

        drop(response);
        assert_eq!(connection.reserved(), (0, 0));
    }

    #[tokio::test]
    async fn body_pipe_is_bounded_by_items_stream_bytes_and_connection_credit() {
        let mut limits = limits();
        limits.stream_window_items = 2;
        limits.stream_window_bytes = 8;
        limits.connection_window_bytes = 8;
        limits.max_chunk_bytes = 4;
        let connection = ConnectionBudget::new(8, limits.connection_window_items).unwrap();
        let (sender, mut receiver) = body_pipe(limits, connection.clone()).unwrap();

        sender.try_send(Bytes::from_static(b"1234")).unwrap();
        sender.try_send(Bytes::from_static(b"5678")).unwrap();
        assert_eq!(
            sender.try_send(Bytes::from_static(b"x")),
            Err(PipeError::Backpressured)
        );
        assert_eq!(connection.available_bytes(), 0);
        assert_eq!(
            connection.available_items(),
            limits.connection_window_items - 2
        );

        let first = receiver.recv().await.unwrap().unwrap();
        assert_eq!(first.data, b"1234"[..]);
        assert_eq!(first.window.consumed_bytes, 4);
        assert_eq!(connection.available_bytes(), 4);
        assert_eq!(
            connection.available_items(),
            limits.connection_window_items - 1
        );
        sender.try_send(Bytes::from_static(b"x")).unwrap();
    }

    #[tokio::test]
    async fn request_connection_item_credit_is_shared_across_streams() {
        let mut limits = limits();
        limits.max_chunk_bytes = 4;
        limits.stream_window_bytes = 8;
        limits.stream_window_items = 1;
        limits.connection_window_bytes = 8;
        limits.connection_window_items = 1;

        let connection = ConnectionBudget::new(8, 1).unwrap();
        let (first_sender, mut first_receiver) = body_pipe(limits, connection.clone()).unwrap();
        let (second_sender, mut second_receiver) = body_pipe(limits, connection.clone()).unwrap();

        first_sender.try_send(Bytes::from_static(b"a")).unwrap();
        assert_eq!(connection.available_bytes(), 7);
        assert_eq!(connection.available_items(), 0);
        assert_eq!(
            second_sender.try_send(Bytes::from_static(b"b")),
            Err(PipeError::Backpressured)
        );

        let first = first_receiver.recv().await.unwrap().unwrap();
        assert_eq!(first.data, b"a"[..]);
        assert_eq!(connection.available_bytes(), 8);
        assert_eq!(connection.available_items(), 1);

        second_sender.try_send(Bytes::from_static(b"b")).unwrap();
        assert_eq!(connection.available_items(), 0);
        let second = second_receiver.recv().await.unwrap().unwrap();
        assert_eq!(second.data, b"b"[..]);
        assert_eq!(connection.available_bytes(), 8);
        assert_eq!(connection.available_items(), 1);
    }

    #[tokio::test]
    async fn body_pipe_requires_explicit_terminal_evidence() {
        let limits = limits();
        let connection = ConnectionBudget::new(
            limits.connection_window_bytes,
            limits.connection_window_items,
        )
        .unwrap();
        let (sender, mut receiver) = body_pipe(limits, connection).unwrap();

        drop(sender);

        assert!(
            receiver
                .recv()
                .await
                .unwrap_err()
                .contains("closed before terminal evidence")
        );
    }

    #[tokio::test]
    async fn dropping_request_registry_aborts_active_local_bodies() {
        let limits = limits();
        let stream_id = Uuid::new_v4();
        let mut requests = RequestStreams::new("tunnel:connect".to_string(), limits).unwrap();
        let incoming = requests
            .start(&json!({
                "stream_id": stream_id,
                "deadline_unix_ms": 1,
                "timeout_ms": 1,
                "method": "POST",
                "path": "/webhook",
                "query_string": "",
                "headers": [],
                "content_length": null,
                "replay": true,
            }))
            .unwrap();
        assert!(incoming.start.replay);
        let mut body = incoming.body;

        drop(requests);

        assert_eq!(body.recv().await.unwrap_err(), "tunnel_disconnected");
        assert!(body.recv().await.unwrap().is_none());
    }

    #[test]
    fn request_start_and_response_control_honor_negotiated_metadata_limits() {
        let limits = limits();
        let stream_id = Uuid::new_v4();
        let oversized = json!({
            "stream_id": stream_id,
            "deadline_unix_ms": 1,
            "timeout_ms": 1,
            "method": "POST",
            "path": "/webhook",
            "query_string": "",
            "headers": [["x-large", "x".repeat(limits.max_start_metadata_bytes)]],
            "content_length": null,
            "replay": false,
        });

        assert!(RequestStart::from_payload(&oversized, limits).is_err());

        let response = response_start_payload(
            stream_id,
            200,
            &[(
                "x-large".to_string(),
                "x".repeat(limits.max_control_payload_bytes),
            )],
            None,
        );

        assert!(validate_control_payload(&response, limits.max_control_payload_bytes).is_err());
    }

    #[test]
    fn request_start_uses_bounded_relative_timeout_independent_of_wall_clock() {
        let limits = limits();
        let stream_id = Uuid::new_v4();
        let payload = |deadline_unix_ms, timeout_ms| {
            json!({
                "stream_id": stream_id,
                "deadline_unix_ms": deadline_unix_ms,
                "timeout_ms": timeout_ms,
                "method": "POST",
                "path": "/webhook",
                "query_string": "",
                "headers": [],
                "content_length": null,
                "replay": false,
            })
        };

        let skewed_past = RequestStart::from_payload(&payload(1, 1_000), limits).unwrap();
        assert_eq!(skewed_past.deadline_unix_ms, 1);
        assert_eq!(skewed_past.timeout_ms, 1_000);

        let skewed_future =
            RequestStart::from_payload(&payload(u64::MAX, limits.request_timeout_ms), limits)
                .unwrap();
        assert_eq!(skewed_future.timeout_ms, limits.request_timeout_ms as u64);

        assert!(RequestStart::from_payload(&payload(1, 0), limits).is_err());
        assert!(
            RequestStart::from_payload(&payload(1, limits.request_timeout_ms + 1), limits,)
                .is_err()
        );
    }

    #[test]
    fn metadata_limits_are_inclusive_encoded_json_byte_ceilings() {
        let limits = limits();
        let stream_id = Uuid::new_v4();
        let mut start = json!({
            "stream_id": stream_id,
            "deadline_unix_ms": 1,
            "timeout_ms": 1,
            "method": "POST",
            "path": "/webhook",
            "query_string": "",
            "headers": [],
            "content_length": null,
            "replay": false,
            "padding": "",
        });
        let base_bytes = serde_json::to_vec(&start).unwrap().len();
        start["padding"] = Value::String("x".repeat(limits.max_start_metadata_bytes - base_bytes));

        assert_eq!(
            serde_json::to_vec(&start).unwrap().len(),
            limits.max_start_metadata_bytes
        );
        assert!(RequestStart::from_payload(&start, limits).is_ok());

        let padding = start["padding"].as_str().unwrap().to_string();
        start["padding"] = Value::String(format!("{padding}x"));
        assert!(RequestStart::from_payload(&start, limits).is_err());

        let mut control = json!({"padding": ""});
        let base_bytes = serde_json::to_vec(&control).unwrap().len();
        control["padding"] =
            Value::String("x".repeat(limits.max_control_payload_bytes - base_bytes));

        assert_eq!(
            serde_json::to_vec(&control).unwrap().len(),
            limits.max_control_payload_bytes
        );
        assert!(validate_control_payload(&control, limits.max_control_payload_bytes).is_ok());

        let padding = control["padding"].as_str().unwrap().to_string();
        control["padding"] = Value::String(format!("{padding}x"));
        assert!(validate_control_payload(&control, limits.max_control_payload_bytes).is_err());
    }

    #[tokio::test]
    async fn invalid_terminal_evidence_aborts_the_local_body_source() {
        let limits = limits();
        let connection = ConnectionBudget::new(
            limits.connection_window_bytes,
            limits.connection_window_items,
        )
        .unwrap();
        let stream_id = Uuid::new_v4();
        let (mut request, mut body) =
            RequestStreamReceiver::new(stream_id, Some(4), limits, connection).unwrap();
        let encoded = encode_chunk(stream_id, 0, 0, b"body", limits.max_chunk_bytes).unwrap();
        request.accept_frame(&encoded).unwrap();
        let consumed = body.recv().await.unwrap().unwrap();
        assert_eq!(consumed.data, b"body"[..]);
        drop(consumed);

        assert!(request.finish(4, &"0".repeat(64)).is_err());
        assert!(body.recv().await.unwrap_err().contains("checksum"));
    }

    #[tokio::test]
    async fn valid_terminal_accepts_a_known_length_consumer_closed_after_the_last_byte() {
        let limits = limits();
        let connection = ConnectionBudget::new(
            limits.connection_window_bytes,
            limits.connection_window_items,
        )
        .unwrap();
        let stream_id = Uuid::new_v4();
        let (mut request, mut body) =
            RequestStreamReceiver::new(stream_id, Some(4), limits, connection).unwrap();
        let encoded = encode_chunk(stream_id, 0, 0, b"body", limits.max_chunk_bytes).unwrap();
        request.accept_frame(&encoded).unwrap();

        let consumed = body.recv().await.unwrap().unwrap();
        assert_eq!(consumed.data, b"body"[..]);
        drop(consumed);
        drop(body);

        let checksum = digest_hex(&Sha256::digest(b"body"));
        let evidence = request.finish(4, &checksum).unwrap();

        assert_eq!(evidence.bytes, 4);
        assert_eq!(evidence.items, 1);
        assert_eq!(evidence.sha256, checksum);
    }

    #[tokio::test]
    async fn exact_negotiated_request_limit_keeps_only_a_bounded_chunk_window() {
        let limits = limits();
        let connection = ConnectionBudget::new(
            limits.connection_window_bytes,
            limits.connection_window_items,
        )
        .unwrap();
        let stream_id = Uuid::new_v4();
        let (mut request, mut body) = RequestStreamReceiver::new(
            stream_id,
            Some(limits.max_request_body_bytes as u64),
            limits,
            connection.clone(),
        )
        .unwrap();
        let mut response_encoder = StreamEncoder::new(stream_id, limits);
        let chunk = vec![0xa5; limits.max_chunk_bytes];
        let mut offset = 0usize;
        let mut sequence = 0u32;
        let mut consumed_items = 0u64;

        while offset < limits.max_request_body_bytes {
            let bytes = (limits.max_request_body_bytes - offset).min(chunk.len());
            let encoded = encode_chunk(
                stream_id,
                sequence,
                offset as u64,
                &chunk[..bytes],
                limits.max_chunk_bytes,
            )
            .unwrap();
            request.accept_frame(&encoded).unwrap();
            let consumed = body.recv().await.unwrap().unwrap();
            assert_eq!(consumed.data.len(), bytes);
            consumed_items = consumed.window.consumed_items;
            response_encoder.encode(&chunk[..bytes]).unwrap();
            drop(consumed);
            assert_eq!(connection.available_bytes(), limits.connection_window_bytes);
            assert_eq!(connection.available_items(), limits.connection_window_items);
            offset += bytes;
            sequence += 1;
        }

        let sent = response_encoder.finish();
        let received = request.finish(sent.bytes, &sent.sha256).unwrap();
        assert_eq!(received.bytes, limits.max_request_body_bytes as u64);
        assert_eq!(received.items, consumed_items);
        assert_eq!(received.sha256, sent.sha256);
        assert!(body.recv().await.unwrap().is_none());
    }

    #[test]
    fn response_control_messages_preserve_ordered_headers_and_integrity() {
        let stream_id = Uuid::new_v4();
        let headers = vec![
            ("set-cookie".to_string(), "a=1".to_string()),
            ("set-cookie".to_string(), "b=2".to_string()),
        ];
        let start = response_start_payload(stream_id, 201, &headers, None);
        assert_eq!(start["headers"][0], json!(["set-cookie", "a=1"]));
        assert_eq!(start["headers"][1], json!(["set-cookie", "b=2"]));

        let mut encoder = StreamEncoder::new(stream_id, limits());
        encoder.encode(b"response").unwrap();
        let end = encoder.finish().end_payload(stream_id);
        assert_eq!(end["total_bytes"], 8);
        assert_eq!(end["sha256"], digest_hex(&Sha256::digest(b"response")));
    }
}
