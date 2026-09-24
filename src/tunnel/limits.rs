//! Size, count, and byte budgets for tunnel traffic, plus the limits negotiated at join.

use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

// Protocol v2 carries logical JSON streams as bounded 64 KiB WebSocket frames.
// This must match limits.max_stream_bytes in the service join contract.
pub(crate) const TUNNEL_MAX_STREAM_BYTES: usize = 67_108_864;

pub(crate) const TUNNEL_PROTOCOL_VERSION: u64 = 2;

pub(crate) const TUNNEL_MAX_FRAME_BYTES: usize = 65_536;

pub(crate) const TUNNEL_MAX_RAW_CHUNK_BYTES: usize = 47_000;

pub(crate) const TUNNEL_MAX_JSON_DEPTH: usize = 16;

pub(crate) const TUNNEL_MAX_JSON_STRUCTURAL_NODES: usize = 65_536;

pub(crate) const TUNNEL_MAX_JSON_PRIMITIVE_BYTES: usize = 256;

pub(crate) const TUNNEL_MAX_RESPONSE_HEADERS: usize = 4_096;

pub(crate) const LEGACY_MAX_REQUEST_BODY_BYTES: usize = 10_485_760;

pub(crate) const LEGACY_MAX_RESPONSE_BODY_BYTES: usize = 7_000_000;

pub(crate) const LEGACY_MAX_RESPONSE_HEADER_BYTES: usize = 1_048_576;

pub(crate) const MAX_RAW_BODY_BYTES: usize = 1_048_576;

pub(crate) const UI_BODY_PREVIEW_BYTES: usize = 65_536;

pub(crate) const LOCAL_MAX_TUNNEL_BODY_BYTES: usize = 16 * 1024 * 1024;

pub(crate) const LOCAL_MAX_RESPONSE_HEADER_BYTES: usize = 1024 * 1024;

pub(crate) const LOCAL_WORK_MAX_COUNT: usize = 8;

pub(crate) const LOCAL_WORK_MAX_BYTES: usize = LOCAL_MAX_TUNNEL_BODY_BYTES;

pub(crate) const INBOUND_STREAM_MAX_COUNT: usize = 16;

pub(crate) const INBOUND_STREAM_MAX_BYTES: usize = 32 * 1024 * 1024;

pub(crate) const RESPONSE_BUFFER_MAX_BYTES: usize = LOCAL_MAX_TUNNEL_BODY_BYTES;

const _: () = assert!(INBOUND_STREAM_MAX_BYTES < TUNNEL_MAX_STREAM_BYTES);

pub(crate) const OUTBOUND_CONTROL_MAX_COUNT: usize = 256;

pub(crate) const OUTBOUND_RESPONSE_MAX_COUNT: usize = 256;

pub(crate) const OUTBOUND_MAX_BYTES: usize = 4 * 1024 * 1024;

pub(crate) const LOG_TEXT_MAX_BYTES: usize = 4 * 1024;

pub const PRESENTATION_QUEUE_CAPACITY: usize = 100;

pub(crate) const PRESENTATION_MAX_EVENT_BYTES: usize = 256 * 1024;

pub(crate) fn bounded_budget_permits(bytes: usize, capacity: usize) -> Option<u32> {
    if bytes > capacity {
        return None;
    }

    bytes.max(1).try_into().ok()
}

pub(crate) fn inbound_stream_fits_budget(reserved_bytes: usize, stream_bytes: usize) -> bool {
    stream_bytes <= INBOUND_STREAM_MAX_BYTES
        && reserved_bytes.saturating_add(stream_bytes) <= INBOUND_STREAM_MAX_BYTES
}

#[derive(Clone)]
pub(crate) struct LocalWorkBudget {
    count: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
}

pub(crate) struct LocalWorkPermit {
    _count: OwnedSemaphorePermit,
    _bytes: OwnedSemaphorePermit,
}

#[derive(Clone)]
pub(crate) struct ResponseBufferBudget {
    bytes: Arc<Semaphore>,
}

pub(crate) struct ResponseBufferReservation {
    budget: ResponseBufferBudget,
    permits: Vec<OwnedSemaphorePermit>,
    reserved_bytes: usize,
}

impl ResponseBufferBudget {
    pub(crate) fn new() -> Self {
        Self {
            bytes: Arc::new(Semaphore::new(RESPONSE_BUFFER_MAX_BYTES)),
        }
    }

    pub(crate) fn try_reserve(
        &self,
        expected_bytes: Option<usize>,
    ) -> Option<ResponseBufferReservation> {
        let mut reservation = ResponseBufferReservation {
            budget: self.clone(),
            permits: Vec::new(),
            reserved_bytes: 0,
        };

        reservation
            .try_grow_to(expected_bytes.unwrap_or(0))
            .then_some(reservation)
    }

    fn try_acquire(&self, bytes: usize) -> Option<OwnedSemaphorePermit> {
        let bytes = bounded_budget_permits(bytes, RESPONSE_BUFFER_MAX_BYTES)?;
        self.bytes.clone().try_acquire_many_owned(bytes).ok()
    }

    #[cfg(test)]
    pub(crate) fn available_bytes(&self) -> usize {
        self.bytes.available_permits()
    }
}

impl ResponseBufferReservation {
    pub(crate) fn try_grow_to(&mut self, total_bytes: usize) -> bool {
        if total_bytes > RESPONSE_BUFFER_MAX_BYTES {
            return false;
        }
        if total_bytes <= self.reserved_bytes {
            return true;
        }

        let additional_permits = total_bytes.saturating_sub(self.reserved_bytes);

        if additional_permits > 0 {
            let Some(permit) = self.budget.try_acquire(additional_permits) else {
                return false;
            };
            self.permits.push(permit);
        }

        self.reserved_bytes = total_bytes;
        true
    }
}

impl LocalWorkBudget {
    pub(crate) fn new() -> Self {
        Self {
            count: Arc::new(Semaphore::new(LOCAL_WORK_MAX_COUNT)),
            bytes: Arc::new(Semaphore::new(LOCAL_WORK_MAX_BYTES)),
        }
    }

    pub(crate) fn try_acquire(&self, bytes: usize) -> Option<LocalWorkPermit> {
        let bytes = bounded_budget_permits(bytes, LOCAL_WORK_MAX_BYTES)?;
        let count = self.count.clone().try_acquire_owned().ok()?;
        let bytes = self.bytes.clone().try_acquire_many_owned(bytes).ok()?;
        Some(LocalWorkPermit {
            _count: count,
            _bytes: bytes,
        })
    }

    #[cfg(test)]
    pub(crate) fn available_count(&self) -> usize {
        self.count.available_permits()
    }

    #[cfg(test)]
    pub(crate) fn available_bytes(&self) -> usize {
        self.bytes.available_permits()
    }
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct TunnelLimits {
    pub(crate) max_request_body_bytes: usize,
    pub(crate) max_response_body_bytes: usize,
    pub(crate) max_response_header_bytes: usize,
    pub(crate) max_response_header_items: usize,
    pub(crate) ordered_response_headers: bool,
}

impl TunnelLimits {
    pub(crate) fn from_join_response(response: &serde_json::Value) -> Self {
        let limits = response.get("limits");
        let framing = response.get("framing");
        let advertised_body_limit = json_limit(limits, "max_body_bytes");
        let body_limit = advertised_body_limit
            .unwrap_or(LEGACY_MAX_REQUEST_BODY_BYTES)
            .min(LOCAL_MAX_TUNNEL_BODY_BYTES);
        let response_header_items = [
            json_limit(limits, "max_response_header_items"),
            json_limit(framing, "max_response_headers"),
        ]
        .into_iter()
        .flatten()
        .min()
        .unwrap_or(TUNNEL_MAX_RESPONSE_HEADERS)
        .min(TUNNEL_MAX_RESPONSE_HEADERS);

        Self {
            max_request_body_bytes: body_limit,
            max_response_body_bytes: advertised_body_limit
                .unwrap_or(LEGACY_MAX_RESPONSE_BODY_BYTES)
                .min(LOCAL_MAX_TUNNEL_BODY_BYTES),
            max_response_header_bytes: json_limit(limits, "max_response_header_bytes")
                .unwrap_or(LEGACY_MAX_RESPONSE_HEADER_BYTES)
                .min(LOCAL_MAX_RESPONSE_HEADER_BYTES),
            max_response_header_items: response_header_items,
            ordered_response_headers: limits
                .and_then(|limits| limits.get("response_headers_format"))
                .and_then(|format| format.as_str())
                == Some("ordered_pairs"),
        }
    }
}

pub(crate) fn json_limit(limits: Option<&serde_json::Value>, key: &str) -> Option<usize> {
    limits?.get(key)?.as_u64()?.try_into().ok()
}

pub(crate) fn optional_positive_limit_at_most(
    contract: &serde_json::Value,
    key: &str,
    local_max: usize,
) -> bool {
    contract.get(key).is_none_or(|value| {
        value
            .as_u64()
            .is_some_and(|limit| limit > 0 && limit <= local_max as u64)
    })
}

pub(crate) fn tunnel_websocket_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(TUNNEL_MAX_FRAME_BYTES))
        .max_frame_size(Some(TUNNEL_MAX_FRAME_BYTES))
}
