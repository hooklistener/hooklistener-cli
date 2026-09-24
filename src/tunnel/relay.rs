//! Bookkeeping for in-flight protocol v2 deliveries and their failure reporting.

use anyhow::Result;
use std::collections::HashMap;
use std::sync::{Arc, atomic::AtomicU8};
use tokio::task::{AbortHandle, JoinSet};
use tracing::error;

use crate::tunnel::TunnelEvent;
use crate::tunnel::framing::{ChannelMessage, TunnelStreamAssembler};
use crate::tunnel::limits::{LocalWorkBudget, ResponseBufferBudget};
use crate::tunnel::preview::{
    body_preview, bounded_presentation_headers, bounded_presentation_text,
};

pub(crate) const DELIVERY_QUEUED: u8 = 0;

pub(crate) const DELIVERY_STARTED: u8 = 1;

pub(crate) const DELIVERY_TERMINAL: u8 = 2;

pub(crate) struct ActiveDelivery {
    pub(crate) abort: AbortHandle,
    pub(crate) phase: Arc<AtomicU8>,
}

pub(crate) struct RelayRuntime {
    pub(crate) inbound_streams: HashMap<String, TunnelStreamAssembler>,
    pub(crate) inbound_reserved_bytes: usize,
    pub(crate) work_budget: LocalWorkBudget,
    pub(crate) response_budget: ResponseBufferBudget,
    pub(crate) workers: JoinSet<(String, Result<()>)>,
    pub(crate) active_deliveries: HashMap<String, ActiveDelivery>,
    pub(crate) auto_drain_requested: bool,
}

impl RelayRuntime {
    pub(crate) fn new() -> Self {
        Self {
            inbound_streams: HashMap::new(),
            inbound_reserved_bytes: 0,
            work_budget: LocalWorkBudget::new(),
            response_budget: ResponseBufferBudget::new(),
            workers: JoinSet::new(),
            active_deliveries: HashMap::new(),
            auto_drain_requested: false,
        }
    }
}

pub(crate) fn complete_tunnel_worker(
    completion: std::result::Result<
        (tokio::task::Id, (String, Result<()>)),
        tokio::task::JoinError,
    >,
    runtime: &mut RelayRuntime,
) {
    match completion {
        Ok((_task_id, (request_id, result))) => {
            runtime.active_deliveries.remove(&request_id);
            if let Err(error) = result {
                error!(request_id = %request_id, error = %error, "Local delivery task failed");
            }
        }
        Err(error) => {
            let task_id = error.id();
            let request_id = runtime
                .active_deliveries
                .iter()
                .find_map(|(request_id, active)| {
                    (active.abort.id() == task_id).then(|| request_id.clone())
                });

            if let Some(request_id) = request_id {
                runtime.active_deliveries.remove(&request_id);
                if !error.is_cancelled() {
                    error!(request_id = %request_id, error = %error, "Local delivery task panicked");
                }
            } else if !error.is_cancelled() {
                error!(error = %error, "Local delivery task panicked");
            }
        }
    }
}

pub(crate) fn reap_ready_tunnel_workers(runtime: &mut RelayRuntime) -> usize {
    let mut reaped = 0;

    while let Some(completion) = runtime.workers.try_join_next_with_id() {
        complete_tunnel_worker(completion, runtime);
        reaped += 1;
    }

    reaped
}

pub(crate) struct LocalDelivery {
    pub(crate) request_id: String,
    pub(crate) method: String,
    pub(crate) path: String,
    pub(crate) query_string: String,
    pub(crate) headers: Vec<(String, String)>,
    pub(crate) body: Vec<u8>,
    pub(crate) deadline_unix_ms: u64,
    pub(crate) replay: bool,
}

impl LocalDelivery {
    pub(crate) fn retained_bytes(&self) -> usize {
        // This semaphore is the retained body budget. Request count is bounded
        // separately, while request metadata has protocol and HTTP ingress
        // limits of its own. Including metadata here would reject a body that
        // is exactly at the server-advertised limit.
        self.body.len()
    }

    pub(crate) fn received_event(&self) -> TunnelEvent {
        let headers = bounded_presentation_headers(self.headers.iter().cloned());
        TunnelEvent::RequestReceived {
            request_id: self.request_id.clone(),
            method: self.method.clone(),
            path: bounded_presentation_text(&self.path),
            body: body_preview(&self.body, &headers),
            headers,
            query_string: bounded_presentation_text(&self.query_string),
            replay: self.replay,
        }
    }
}

pub(crate) fn delivery_error_message(
    topic: &str,
    request_id: &str,
    code: &str,
    outcome: &str,
    error: &str,
) -> ChannelMessage {
    ChannelMessage {
        topic: topic.to_string(),
        event: "tunnel_error".to_string(),
        payload: serde_json::json!({
            "request_id": request_id,
            "code": code,
            "outcome": outcome,
            "error": error,
        }),
        reference: None,
    }
}

pub(crate) fn cancellation_classification(phase: u8) -> Option<(&'static str, &'static str)> {
    match phase {
        DELIVERY_QUEUED => Some(("cancelled_before_forward", "known_not_executed")),
        DELIVERY_STARTED => Some(("cancelled_after_forward_started", "outcome_unknown")),
        DELIVERY_TERMINAL => None,
        _ => Some(("cancelled_after_forward_started", "outcome_unknown")),
    }
}

pub(crate) struct DeliveryFailure {
    pub(crate) code: &'static str,
    pub(crate) outcome: &'static str,
    pub(crate) error: String,
}

impl DeliveryFailure {
    pub(crate) fn known(code: &'static str, error: impl Into<String>) -> Self {
        Self {
            code,
            outcome: "known_not_executed",
            error: error.into(),
        }
    }

    pub(crate) fn unknown(code: &'static str, error: impl Into<String>) -> Self {
        Self {
            code,
            outcome: "outcome_unknown",
            error: error.into(),
        }
    }
}
