use crate::api::ApiClient;
use anyhow::{Context, Result, anyhow};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use brotli::Decompressor as BrotliDecoder;
use encoding_rs::{Encoding, UTF_8};
use flate2::read::{DeflateDecoder, MultiGzDecoder, ZlibDecoder};
use futures_util::{SinkExt, StreamExt};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::io::{BufRead, BufReader, Cursor, Read};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicU8, AtomicUsize, Ordering},
};
use std::time::{Duration, SystemTime};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc, watch};
use tokio::task::{AbortHandle, JoinSet};
use tokio_tungstenite::{
    connect_async, connect_async_with_config,
    tungstenite::{
        Bytes, Message, error::Error as WsError, http::StatusCode, protocol::WebSocketConfig,
    },
};
use tracing::{debug, error, info, warn};
use uuid::Uuid;

use crate::api;
use crate::target_policy::TargetPolicy;
use crate::tunnel_v3;

type WsStream =
    tokio_tungstenite::WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>;
type WsWrite = futures_util::stream::SplitSink<WsStream, Message>;
type WsRead = futures_util::stream::SplitStream<WsStream>;

// Protocol v2 carries logical JSON streams as bounded 64 KiB WebSocket frames.
// This must match limits.max_stream_bytes in the service join contract.
const TUNNEL_MAX_STREAM_BYTES: usize = 67_108_864;
const TUNNEL_PROTOCOL_VERSION: u64 = 2;
const TUNNEL_MAX_FRAME_BYTES: usize = 65_536;
const TUNNEL_MAX_RAW_CHUNK_BYTES: usize = 47_000;
const TUNNEL_MAX_JSON_DEPTH: usize = 16;
const TUNNEL_MAX_JSON_STRUCTURAL_NODES: usize = 65_536;
const TUNNEL_MAX_JSON_PRIMITIVE_BYTES: usize = 256;
const TUNNEL_MAX_RESPONSE_HEADERS: usize = 4_096;
const LEGACY_MAX_REQUEST_BODY_BYTES: usize = 10_485_760;
const LEGACY_MAX_RESPONSE_BODY_BYTES: usize = 7_000_000;
const LEGACY_MAX_RESPONSE_HEADER_BYTES: usize = 1_048_576;
const MAX_RAW_BODY_BYTES: usize = 1_048_576;
const UI_BODY_PREVIEW_BYTES: usize = 65_536;
const REDACTED_SECRET: &str = "[REDACTED]";
const LOCAL_MAX_TUNNEL_BODY_BYTES: usize = 16 * 1024 * 1024;
const LOCAL_MAX_RESPONSE_HEADER_BYTES: usize = 1024 * 1024;
const LOCAL_WORK_MAX_COUNT: usize = 8;
const LOCAL_WORK_MAX_BYTES: usize = LOCAL_MAX_TUNNEL_BODY_BYTES;
const INBOUND_STREAM_MAX_COUNT: usize = 16;
const INBOUND_STREAM_MAX_BYTES: usize = 32 * 1024 * 1024;
const RESPONSE_BUFFER_MAX_BYTES: usize = LOCAL_MAX_TUNNEL_BODY_BYTES;
const _: () = assert!(INBOUND_STREAM_MAX_BYTES < TUNNEL_MAX_STREAM_BYTES);
const OUTBOUND_CONTROL_MAX_COUNT: usize = 256;
const OUTBOUND_RESPONSE_MAX_COUNT: usize = 256;
const OUTBOUND_MAX_BYTES: usize = 4 * 1024 * 1024;
const LOG_TEXT_MAX_BYTES: usize = 4 * 1024;
pub const PRESENTATION_QUEUE_CAPACITY: usize = 100;
const PRESENTATION_MAX_EVENT_BYTES: usize = 256 * 1024;

fn bounded_budget_permits(bytes: usize, capacity: usize) -> Option<u32> {
    if bytes > capacity {
        return None;
    }

    bytes.max(1).try_into().ok()
}

fn inbound_stream_fits_budget(reserved_bytes: usize, stream_bytes: usize) -> bool {
    stream_bytes <= INBOUND_STREAM_MAX_BYTES
        && reserved_bytes.saturating_add(stream_bytes) <= INBOUND_STREAM_MAX_BYTES
}

const DELIVERY_QUEUED: u8 = 0;
const DELIVERY_STARTED: u8 = 1;
const DELIVERY_TERMINAL: u8 = 2;

struct OutboundItem {
    message: Message,
    _bytes: OwnedSemaphorePermit,
}

#[derive(Clone)]
struct PriorityWriter {
    control_tx: mpsc::Sender<OutboundItem>,
    response_tx: mpsc::Sender<OutboundItem>,
    bytes: Arc<Semaphore>,
}

impl PriorityWriter {
    fn spawn(write: WsWrite) -> (Self, tokio::task::JoinHandle<Result<()>>) {
        let (control_tx, control_rx) = mpsc::channel(OUTBOUND_CONTROL_MAX_COUNT);
        let (response_tx, response_rx) = mpsc::channel(OUTBOUND_RESPONSE_MAX_COUNT);
        let writer = Self {
            control_tx,
            response_tx,
            bytes: Arc::new(Semaphore::new(OUTBOUND_MAX_BYTES)),
        };
        let task = tokio::spawn(run_priority_writer(write, control_rx, response_rx));
        (writer, task)
    }

    async fn control(&self, message: ChannelMessage) -> Result<()> {
        self.enqueue_channel(message, true).await
    }

    async fn response(&self, message: ChannelMessage) -> Result<()> {
        self.enqueue_channel(message, false).await
    }

    async fn control_v3(&self, message: tunnel_v3::ControlMessage) -> Result<()> {
        self.enqueue_v3_control(message, true).await
    }

    fn try_control_v3(&self, message: tunnel_v3::ControlMessage) -> Result<()> {
        let encoded = tunnel_v3::encode_client_control(&message, TUNNEL_MAX_FRAME_BYTES)?;
        self.try_enqueue(Message::Text(encoded.into()), true)
    }

    async fn response_control_v3(&self, message: tunnel_v3::ControlMessage) -> Result<()> {
        self.enqueue_v3_control(message, false).await
    }

    async fn binary_response(&self, message: Vec<u8>) -> Result<()> {
        self.enqueue(Message::Binary(message.into()), false).await
    }

    async fn pong(&self, data: Bytes) -> Result<()> {
        self.enqueue(Message::Pong(data), true).await
    }

    async fn enqueue_channel(&self, message: ChannelMessage, control: bool) -> Result<()> {
        let json = serde_json::to_vec(&message)?;
        if json.len() > TUNNEL_MAX_FRAME_BYTES {
            return Err(anyhow!("Tunnel channel message exceeds 64 KiB"));
        }
        self.enqueue(Message::Text(String::from_utf8(json)?.into()), control)
            .await
    }

    async fn enqueue_v3_control(
        &self,
        message: tunnel_v3::ControlMessage,
        control: bool,
    ) -> Result<()> {
        let encoded = tunnel_v3::encode_client_control(&message, TUNNEL_MAX_FRAME_BYTES)?;
        self.enqueue(Message::Text(encoded.into()), control).await
    }

    async fn enqueue(&self, message: Message, control: bool) -> Result<()> {
        let size = message.len().max(1);
        let permits: u32 = size
            .try_into()
            .map_err(|_| anyhow!("Outbound message byte count is invalid"))?;
        let bytes = self.bytes.clone().acquire_many_owned(permits).await?;
        let item = OutboundItem {
            message,
            _bytes: bytes,
        };
        let sender = if control {
            &self.control_tx
        } else {
            &self.response_tx
        };
        sender
            .send(item)
            .await
            .map_err(|_| anyhow!("Tunnel writer stopped"))
    }

    fn try_enqueue(&self, message: Message, control: bool) -> Result<()> {
        let size = message.len().max(1);
        let permits: u32 = size
            .try_into()
            .map_err(|_| anyhow!("Outbound message byte count is invalid"))?;
        let bytes = self
            .bytes
            .clone()
            .try_acquire_many_owned(permits)
            .map_err(|_| anyhow!("Tunnel writer byte queue is full"))?;
        let item = OutboundItem {
            message,
            _bytes: bytes,
        };
        let sender = if control {
            &self.control_tx
        } else {
            &self.response_tx
        };
        sender
            .try_send(item)
            .map_err(|_| anyhow!("Tunnel writer control queue is unavailable"))
    }

    #[cfg(test)]
    fn available_bytes(&self) -> usize {
        self.bytes.available_permits()
    }
}

async fn run_priority_writer(
    mut write: WsWrite,
    mut control_rx: mpsc::Receiver<OutboundItem>,
    mut response_rx: mpsc::Receiver<OutboundItem>,
) -> Result<()> {
    loop {
        let item = tokio::select! {
            biased;
            item = control_rx.recv() => item,
            item = response_rx.recv() => item,
            else => None,
        };

        match item {
            Some(item) => write.send(item.message).await?,
            None => return Ok(()),
        }
    }
}

#[derive(Clone)]
struct LocalWorkBudget {
    count: Arc<Semaphore>,
    bytes: Arc<Semaphore>,
}

struct LocalWorkPermit {
    _count: OwnedSemaphorePermit,
    _bytes: OwnedSemaphorePermit,
}

#[derive(Clone)]
struct ResponseBufferBudget {
    bytes: Arc<Semaphore>,
}

struct ResponseBufferReservation {
    budget: ResponseBufferBudget,
    permits: Vec<OwnedSemaphorePermit>,
    reserved_bytes: usize,
}

impl ResponseBufferBudget {
    fn new() -> Self {
        Self {
            bytes: Arc::new(Semaphore::new(RESPONSE_BUFFER_MAX_BYTES)),
        }
    }

    fn try_reserve(&self, expected_bytes: Option<usize>) -> Option<ResponseBufferReservation> {
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
    fn available_bytes(&self) -> usize {
        self.bytes.available_permits()
    }
}

impl ResponseBufferReservation {
    fn try_grow_to(&mut self, total_bytes: usize) -> bool {
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
    fn new() -> Self {
        Self {
            count: Arc::new(Semaphore::new(LOCAL_WORK_MAX_COUNT)),
            bytes: Arc::new(Semaphore::new(LOCAL_WORK_MAX_BYTES)),
        }
    }

    fn try_acquire(&self, bytes: usize) -> Option<LocalWorkPermit> {
        let bytes = bounded_budget_permits(bytes, LOCAL_WORK_MAX_BYTES)?;
        let count = self.count.clone().try_acquire_owned().ok()?;
        let bytes = self.bytes.clone().try_acquire_many_owned(bytes).ok()?;
        Some(LocalWorkPermit {
            _count: count,
            _bytes: bytes,
        })
    }

    #[cfg(test)]
    fn available_count(&self) -> usize {
        self.count.available_permits()
    }

    #[cfg(test)]
    fn available_bytes(&self) -> usize {
        self.bytes.available_permits()
    }
}

struct ActiveDelivery {
    abort: AbortHandle,
    phase: Arc<AtomicU8>,
}

struct RelayRuntime {
    inbound_streams: HashMap<String, TunnelStreamAssembler>,
    inbound_reserved_bytes: usize,
    work_budget: LocalWorkBudget,
    response_budget: ResponseBufferBudget,
    workers: JoinSet<(String, Result<()>)>,
    active_deliveries: HashMap<String, ActiveDelivery>,
    auto_drain_requested: bool,
}

impl RelayRuntime {
    fn new() -> Self {
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

fn complete_tunnel_worker(
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

fn reap_ready_tunnel_workers(runtime: &mut RelayRuntime) -> usize {
    let mut reaped = 0;

    while let Some(completion) = runtime.workers.try_join_next_with_id() {
        complete_tunnel_worker(completion, runtime);
        reaped += 1;
    }

    reaped
}

struct ActiveV3Delivery {
    abort: AbortHandle,
    phase: Arc<AtomicU8>,
    response_tx: mpsc::Sender<tunnel_v3::WindowUpdate>,
}

struct V3RelayRuntime {
    requests: tunnel_v3::RequestStreams,
    response_window: tunnel_v3::ConnectionSendWindow,
    work_count: Arc<Semaphore>,
    workers: JoinSet<(Uuid, Result<()>)>,
    active_deliveries: HashMap<Uuid, ActiveV3Delivery>,
    auto_drain_requested: bool,
}

impl V3RelayRuntime {
    fn new(topic: String, limits: tunnel_v3::StreamingLimits) -> Result<Self> {
        Ok(Self {
            requests: tunnel_v3::RequestStreams::new(topic, limits)?,
            response_window: tunnel_v3::ConnectionSendWindow::new(
                limits.connection_window_bytes,
                limits.connection_window_items,
            ),
            work_count: Arc::new(Semaphore::new(
                limits.max_active_local_forwards.min(LOCAL_WORK_MAX_COUNT),
            )),
            workers: JoinSet::new(),
            active_deliveries: HashMap::new(),
            auto_drain_requested: false,
        })
    }
}

async fn complete_v3_worker(
    completion: std::result::Result<(tokio::task::Id, (Uuid, Result<()>)), tokio::task::JoinError>,
    runtime: &mut V3RelayRuntime,
    writer: &PriorityWriter,
    tunnel_topic: &str,
) -> Result<()> {
    match completion {
        Ok((_task_id, (stream_id, result))) => {
            let active = runtime.active_deliveries.remove(&stream_id);
            if runtime.requests.contains(stream_id) {
                runtime.requests.cancel(stream_id, "local_delivery_ended");
                let outcome = active
                    .as_ref()
                    .map(|active| active.phase.load(Ordering::Acquire))
                    .and_then(cancellation_classification)
                    .map(|(_code, outcome)| outcome)
                    .unwrap_or("outcome_unknown");
                writer
                    .control_v3(v3_abort_message(
                        tunnel_topic,
                        stream_id,
                        "request",
                        "local_delivery_ended",
                        outcome,
                    ))
                    .await?;
            }
            if let Err(error) = result {
                let error = bounded_control_neutral_log_text(&error.to_string());
                error!(stream_id = %stream_id, error = %error, "Protocol v3 local delivery failed");
            }
        }
        Err(error) => {
            let task_id = error.id();
            let stream_id = runtime
                .active_deliveries
                .iter()
                .find_map(|(stream_id, active)| {
                    (active.abort.id() == task_id).then_some(*stream_id)
                });

            if let Some(stream_id) = stream_id {
                let active = runtime
                    .active_deliveries
                    .remove(&stream_id)
                    .expect("matched active protocol v3 delivery");

                if runtime.requests.contains(stream_id) {
                    let code = if error.is_cancelled() {
                        "local_delivery_cancelled"
                    } else {
                        "local_delivery_panicked"
                    };
                    let outcome = cancellation_classification(active.phase.load(Ordering::Acquire))
                        .map(|(_code, outcome)| outcome)
                        .unwrap_or("outcome_unknown");
                    runtime.requests.cancel(stream_id, code);
                    writer
                        .control_v3(v3_abort_message(
                            tunnel_topic,
                            stream_id,
                            "request",
                            code,
                            outcome,
                        ))
                        .await?;
                }
            }

            if !error.is_cancelled() {
                error!(error = %error, "Protocol v3 local delivery panicked");
            }
        }
    }

    Ok(())
}

async fn reap_ready_v3_workers(
    runtime: &mut V3RelayRuntime,
    writer: &PriorityWriter,
    tunnel_topic: &str,
) -> Result<usize> {
    let mut reaped = 0;

    while let Some(completion) = runtime.workers.try_join_next_with_id() {
        complete_v3_worker(completion, runtime, writer, tunnel_topic).await?;
        reaped += 1;
    }

    Ok(reaped)
}

struct LocalDelivery {
    request_id: String,
    method: String,
    path: String,
    query_string: String,
    headers: Vec<(String, String)>,
    body: Vec<u8>,
    deadline_unix_ms: u64,
    replay: bool,
}

impl LocalDelivery {
    fn retained_bytes(&self) -> usize {
        // This semaphore is the retained body budget. Request count is bounded
        // separately, while request metadata has protocol and HTTP ingress
        // limits of its own. Including metadata here would reject a body that
        // is exactly at the server-advertised limit.
        self.body.len()
    }

    fn received_event(&self) -> TunnelEvent {
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

fn stream_error_message(topic: &str, stream_id: &str, code: &str) -> ChannelMessage {
    ChannelMessage {
        topic: topic.to_string(),
        event: "tunnel_stream_error".to_string(),
        payload: serde_json::json!({"stream_id": stream_id, "code": code}),
        reference: None,
    }
}

fn v3_control_message(
    topic: &str,
    event: &str,
    payload: serde_json::Value,
) -> tunnel_v3::ControlMessage {
    tunnel_v3::ControlMessage {
        join_ref: Some("1".to_string()),
        reference: None,
        topic: topic.to_string(),
        event: event.to_string(),
        payload,
    }
}

fn v3_abort_message(
    topic: &str,
    stream_id: Uuid,
    direction: &str,
    code: &str,
    outcome: &str,
) -> tunnel_v3::ControlMessage {
    v3_control_message(
        topic,
        "tunnel_stream_abort",
        serde_json::json!({
            "stream_id": stream_id,
            "direction": direction,
            "code": code,
            "outcome": outcome,
        }),
    )
}

fn v3_payload_stream_id(payload: &serde_json::Value) -> Result<Uuid> {
    payload
        .get("stream_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("Protocol v3 event is missing stream_id"))?
        .parse()
        .context("Protocol v3 event has an invalid stream_id")
}

fn admit_v3_response_window(
    sender: &mpsc::Sender<tunnel_v3::WindowUpdate>,
    update: tunnel_v3::WindowUpdate,
) -> std::result::Result<(), &'static str> {
    match sender.try_send(update) {
        Ok(()) => Ok(()),
        Err(mpsc::error::TrySendError::Full(_)) => Err("response_window_overflow"),
        Err(mpsc::error::TrySendError::Closed(_)) => Err("response_window_receiver_closed"),
    }
}

async fn enqueue_v3_response_chunk(
    stream: &mut tunnel_v3::ResponseStream,
    writer: &PriorityWriter,
    response_rx: &mut mpsc::Receiver<tunnel_v3::WindowUpdate>,
    data: &[u8],
    local_deadline: tokio::time::Instant,
) -> Result<()> {
    loop {
        let connection_credit = stream.connection_credit_notifier();
        let connection_credit_available = connection_credit.notified();
        tokio::pin!(connection_credit_available);
        connection_credit_available.as_mut().enable();

        match stream.try_encode_chunk(data) {
            Ok(encoded) => return writer.binary_response(encoded).await,
            Err(error)
                if matches!(
                    error.downcast_ref::<tunnel_v3::WindowError>(),
                    Some(tunnel_v3::WindowError::Backpressured)
                ) =>
            {
                let remaining =
                    local_deadline.saturating_duration_since(tokio::time::Instant::now());
                if remaining.is_zero() {
                    return Err(anyhow!("deadline_exceeded"));
                }

                tokio::select! {
                    update = response_rx.recv() => {
                        let update = update.ok_or_else(|| anyhow!("Protocol v3 response stream stopped"))?;
                        stream.acknowledge(update)?;
                    }
                    _ = &mut connection_credit_available => {}
                    _ = tokio::time::sleep_until(local_deadline) => {
                        return Err(anyhow!("deadline_exceeded"));
                    }
                }
            }
            Err(error) => return Err(error),
        }
    }
}

async fn acknowledge_v3_response_window(
    stream: &mut tunnel_v3::ResponseStream,
    response_rx: &mut mpsc::Receiver<tunnel_v3::WindowUpdate>,
    local_deadline: tokio::time::Instant,
) -> Result<()> {
    let remaining = local_deadline.saturating_duration_since(tokio::time::Instant::now());
    if remaining.is_zero() {
        return Err(anyhow!("deadline_exceeded"));
    }
    let update = tokio::time::timeout(remaining, response_rx.recv())
        .await
        .map_err(|_| anyhow!("deadline_exceeded"))?
        .ok_or_else(|| anyhow!("Protocol v3 response stream stopped"))?;
    stream.acknowledge(update)
}

struct V3RequestBodyBridge {
    request_body: reqwest::Body,
    response_started: watch::Sender<bool>,
    pump: tokio::task::JoinHandle<Result<()>>,
}

fn spawn_v3_request_body_bridge(
    source: tunnel_v3::BodyPipeReceiver,
    consumption_tx: mpsc::Sender<tunnel_v3::WindowUpdate>,
) -> V3RequestBodyBridge {
    // The extra handoff contains at most one negotiated chunk. The protocol
    // pipe remains bounded by its byte/item windows, while this queue lets the
    // pump retain ownership and drain safely if the local server responds
    // before reqwest consumes the complete request body.
    let (local_tx, local_rx) = mpsc::channel::<std::result::Result<Bytes, std::io::Error>>(1);
    let (response_started, response_started_rx) = watch::channel(false);
    let pump = tokio::spawn(run_v3_request_body_pump(
        source,
        consumption_tx,
        local_tx,
        response_started_rx,
    ));
    let stream = futures_util::stream::unfold(local_rx, |mut receiver| async move {
        receiver.recv().await.map(|item| (item, receiver))
    });

    V3RequestBodyBridge {
        request_body: reqwest::Body::wrap_stream(stream),
        response_started,
        pump,
    }
}

async fn run_v3_request_body_pump(
    mut source: tunnel_v3::BodyPipeReceiver,
    consumption_tx: mpsc::Sender<tunnel_v3::WindowUpdate>,
    local_tx: mpsc::Sender<std::result::Result<Bytes, std::io::Error>>,
    mut response_started: watch::Receiver<bool>,
) -> Result<()> {
    let mut local_tx = Some(local_tx);

    loop {
        let chunk = match source.recv().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) => return Ok(()),
            Err(reason) => {
                if let Some(sender) = local_tx.take() {
                    let _ = sender.try_send(Err(std::io::Error::new(
                        std::io::ErrorKind::InvalidData,
                        reason.clone(),
                    )));
                }
                return Err(anyhow!(reason));
            }
        };

        if let Some(sender) = local_tx.take()
            && !*response_started.borrow()
        {
            let sent = tokio::select! {
                biased;
                _ = response_started.changed() => false,
                result = sender.send(Ok(chunk.data)) => result.is_ok(),
            };
            if sent {
                local_tx = Some(sender);
            }
        }

        consumption_tx
            .send(chunk.window)
            .await
            .map_err(|_| anyhow!("Protocol v3 request consumption reporter stopped"))?;
    }
}

fn delivery_error_message(
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

fn cancellation_classification(phase: u8) -> Option<(&'static str, &'static str)> {
    match phase {
        DELIVERY_QUEUED => Some(("cancelled_before_forward", "known_not_executed")),
        DELIVERY_STARTED => Some(("cancelled_after_forward_started", "outcome_unknown")),
        DELIVERY_TERMINAL => None,
        _ => Some(("cancelled_after_forward_started", "outcome_unknown")),
    }
}

struct DeliveryFailure {
    code: &'static str,
    outcome: &'static str,
    error: String,
}

impl DeliveryFailure {
    fn known(code: &'static str, error: impl Into<String>) -> Self {
        Self {
            code,
            outcome: "known_not_executed",
            error: error.into(),
        }
    }

    fn unknown(code: &'static str, error: impl Into<String>) -> Self {
        Self {
            code,
            outcome: "outcome_unknown",
            error: error.into(),
        }
    }
}

#[derive(Clone, Copy, Debug)]
struct TunnelLimits {
    max_request_body_bytes: usize,
    max_response_body_bytes: usize,
    max_response_header_bytes: usize,
    max_response_header_items: usize,
    ordered_response_headers: bool,
}

impl TunnelLimits {
    fn from_join_response(response: &serde_json::Value) -> Self {
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

fn json_limit(limits: Option<&serde_json::Value>, key: &str) -> Option<usize> {
    limits?.get(key)?.as_u64()?.try_into().ok()
}

fn optional_positive_limit_at_most(
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

fn decode_request_body_limited(
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

fn supported_tunnel_method(method: &str) -> Option<reqwest::Method> {
    reqwest::Method::from_bytes(method.as_bytes()).ok()
}

fn response_body_forbidden(method: &str, status: u16) -> bool {
    method.eq_ignore_ascii_case("HEAD") || matches!(status, 204 | 205 | 304)
}

fn ensure_response_chunk_allowed(method: &str, status: u16) -> Result<()> {
    if response_body_forbidden(method, status) {
        Err(anyhow!(
            "Local response included a body where HTTP forbids one"
        ))
    } else {
        Ok(())
    }
}

fn relayed_response_content_length(
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

fn valid_reset_content_length(headers: &reqwest::header::HeaderMap) -> bool {
    let mut values = headers.get_all(reqwest::header::CONTENT_LENGTH).iter();

    match (values.next(), values.next()) {
        (None, None) => true,
        (Some(value), None) => value.as_bytes() == b"0",
        _ => false,
    }
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

fn response_header_limit_error(
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

async fn read_response_body_limited(
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

fn bounded_presentation_headers<I>(headers: I) -> HashMap<String, String>
where
    I: IntoIterator<Item = (String, String)>,
{
    let mut retained = HashMap::new();
    let mut bytes = 0usize;

    for (name, value) in headers {
        let pair_bytes = name.len().saturating_add(value.len());
        if bytes.saturating_add(pair_bytes) > PRESENTATION_MAX_EVENT_BYTES / 2 {
            break;
        }
        bytes += pair_bytes;
        retained.insert(name, value);
    }

    retained
}

fn bounded_presentation_text(value: &str) -> String {
    const MAX_TEXT_BYTES: usize = PRESENTATION_MAX_EVENT_BYTES / 8;

    if value.len() <= MAX_TEXT_BYTES {
        return value.to_string();
    }

    let mut end = MAX_TEXT_BYTES;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    format!("{}… [truncated]", &value[..end])
}

fn bounded_control_neutral_log_text(value: &str) -> String {
    let mut output = String::with_capacity(value.len().min(LOG_TEXT_MAX_BYTES));
    let mut truncated = false;

    for character in value.chars() {
        let rendered = if character.is_control() {
            character.escape_default().collect::<String>()
        } else {
            character.to_string()
        };

        if output.len().saturating_add(rendered.len()) > LOG_TEXT_MAX_BYTES - 3 {
            truncated = true;
            break;
        }
        output.push_str(&rendered);
    }

    if truncated {
        output.push_str("...");
    }
    output
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

fn decode_channel_message(text: &str, protocol_v3: bool) -> Result<ChannelMessage> {
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

fn validate_framing_contract(response: &serde_json::Value) -> Result<()> {
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

fn listen_join_payload() -> serde_json::Value {
    serde_json::json!({"mode": CAPTURE_FORWARD_MODE})
}

#[cfg(test)]
fn tunnel_join_payload(
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

fn tunnel_join_payload_for_version(
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

fn selected_tunnel_protocol(supported_protocol_versions: &[u64]) -> Result<u64> {
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

fn validate_join_mode(response: &serde_json::Value, expected: &str) -> Result<()> {
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

/// Webhook request received from the server (Tunnel format)
#[derive(Debug, Clone, Deserialize)]
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
    StreamGap {
        dropped_events: usize,
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

/// Build a one-time ticket-authenticated WebSocket URL from a base HTTP(S) URL.
pub fn build_ws_url(base_url: &str, ticket: &str, path: &str) -> String {
    format!("{}?ticket={ticket}", websocket_endpoint(base_url, path))
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
        || lower.contains("relay handshake rejected")
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
    target: TargetPolicy,
    base_url: String,
    event_tx: mpsc::Sender<TunnelEvent>,
}

impl TunnelClient {
    pub fn new(
        access_token_rx: watch::Receiver<String>,
        endpoint_slug: String,
        target: TargetPolicy,
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
            target,
            base_url,
            event_tx,
        }
    }

    /// Connect to WebSocket and start listening for webhook events
    pub async fn connect_and_listen(&self) -> Result<()> {
        api::validate_websocket_base_url(&self.base_url)?;
        info!(
            endpoint = %self.endpoint_slug,
            target = %self.target.display_url(),
            "Connecting to WebSocket tunnel"
        );

        // Exchange the long-lived HTTP credential for a one-time, scoped handshake ticket.
        let access_token = self.access_token_rx.borrow().clone();
        let plan = serde_json::json!({
            "mode": CAPTURE_FORWARD_MODE,
            "route": {"endpoint_slug": &self.endpoint_slug},
            "target": self.target.plan(),
        });
        let relay_ticket = api::issue_relay_ticket(&access_token, &self.base_url, &plan).await?;
        if relay_ticket.scope != "relay:listen" {
            return Err(anyhow!(
                "Relay handshake rejected: ticket returned an incompatible scope"
            ));
        }
        if relay_ticket.plan_fingerprint.is_empty() || relay_ticket.expires_at.is_empty() {
            return Err(anyhow!(
                "Relay handshake rejected: ticket receipt was incomplete"
            ));
        }

        let ws_url = build_ws_url(&self.base_url, &relay_ticket.ticket, "cli-tunnel/websocket");

        debug!(
            endpoint = %websocket_endpoint(&self.base_url, "cli-tunnel/websocket"),
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
                    let detail = redact_access_token(&e.to_string(), &relay_ticket.ticket);
                    let detail = redact_access_token(&detail, &access_token);
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
                                let reason = bounded_control_neutral_log_text(reason);
                                let _ = self
                                    .event_tx
                                    .send(TunnelEvent::ConnectionError(reason.clone()))
                                    .await;
                                return Err(anyhow!("Channel join failed: {}", reason));
                            }
                        }
                    }
                    Ok(Message::Ping(data)) => {
                        write.send(Message::Pong(data)).await?;
                    }
                    Ok(Message::Close(frame)) => {
                        let frame = bounded_control_neutral_log_text(&format!("{frame:?}"));
                        return Err(anyhow!("WebSocket closed during join: {frame}"));
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
                            let error = bounded_control_neutral_log_text(&e.to_string());
                            error!(error = %error, "Error handling message");
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        let frame = bounded_control_neutral_log_text(&format!("{frame:?}"));
                        info!(frame = %frame, "WebSocket closed");
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
        let log_topic = bounded_control_neutral_log_text(&msg.topic);
        let log_event = bounded_control_neutral_log_text(&msg.event);

        debug!(
            topic = %log_topic,
            event = %log_event,
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
                            let err_msg = format!("Invalid webhook payload: {e}");
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
                debug!(event = %log_event, "Unhandled event");
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

        let mut target_with_query = self.target.request_url(&request.path, None)?;
        if !request.query_params.is_empty() {
            let mut query = target_with_query.query_pairs_mut();
            for (key, value) in &request.query_params {
                query.append_pair(key, &json_value_to_string(value));
            }
        }

        // The client is pinned to the addresses resolved during activation and ignores proxies.
        let client = self.target.http_client()?;

        // Build request with method
        let mut req_builder = match request.method.as_str() {
            "GET" => client.get(target_with_query.clone()),
            "POST" => client.post(target_with_query.clone()),
            "PUT" => client.put(target_with_query.clone()),
            "DELETE" => client.delete(target_with_query.clone()),
            "PATCH" => client.patch(target_with_query.clone()),
            "HEAD" => client.head(target_with_query.clone()),
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
            Ok(mut response) => {
                let status = response.status();
                let status_code = status.as_u16();
                let response_headers = response_headers_to_map(response.headers());
                let response_bytes =
                    match read_response_body_limited(&mut response, LEGACY_MAX_RESPONSE_BODY_BYTES)
                        .await
                    {
                        Ok(response_bytes) => response_bytes,
                        Err(error) => {
                            let duration_ms = start_time.elapsed().as_millis() as u64;
                            let error_message = error.to_string();

                            error!(
                                request_id = %request.id,
                                error = %error_message,
                                "Failed to read local response"
                            );

                            let _ = self
                                .event_tx
                                .send(TunnelEvent::ForwardError {
                                    request_id: request.id.clone(),
                                    target_url: target_with_query.to_string(),
                                    error: error_message.clone(),
                                    duration_ms,
                                })
                                .await;

                            let payload = with_forward_id(
                                serde_json::json!({
                                    "request_id": &request.id,
                                    "status": "error",
                                    "proxied_to": target_with_query.as_str(),
                                    "error": error_message,
                                    "duration_ms": duration_ms,
                                }),
                                request.forward_id.as_deref(),
                            );

                            self.send_request_ack(write, payload).await?;
                            return Ok(());
                        }
                    };
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
                        target_url: target_with_query.to_string(),
                        status: status_code,
                        duration_ms,
                    })
                    .await;

                // Send acknowledgment back to server
                let payload = with_forward_id(
                    serde_json::json!({
                        "request_id": &request.id,
                        "status": "proxied",
                        "proxied_to": target_with_query.as_str(),
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

                let error_message = e.without_url().to_string();

                error!(
                    request_id = %request.id,
                    error = %error_message,
                    "Failed to forward request"
                );

                let _ = self
                    .event_tx
                    .send(TunnelEvent::ForwardError {
                        request_id: request.id.clone(),
                        target_url: target_with_query.to_string(),
                        error: error_message.clone(),
                        duration_ms,
                    })
                    .await;

                // Send error acknowledgment
                let payload = with_forward_id(
                    serde_json::json!({
                        "request_id": &request.id,
                        "status": "error",
                        "proxied_to": target_with_query.as_str(),
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
                    let log_error = bounded_control_neutral_log_text(&err_msg);
                    warn!(error = %log_error, "Tunnel connection attempt failed");
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
    target: TargetPolicy,
    org_id: Option<String>,
    slug: Option<String>,
    base_url: String,
    event_tx: mpsc::Sender<TunnelEvent>,
    replay_buffered: bool,
    presentation_drops: Arc<AtomicUsize>,
    resume_session_id: Arc<Mutex<Option<String>>>,
    anonymous_route: Option<AnonymousRouteCredential>,
}

#[derive(Clone)]
pub struct AnonymousRouteCredential {
    route_id: String,
    route_token: String,
    initial_ticket: Arc<Mutex<Option<api::RelayTicket>>>,
}

impl TunnelForwarder {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        access_token_rx: watch::Receiver<String>,
        local_host: String,
        local_port: u16,
        target: TargetPolicy,
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
            target,
            org_id,
            slug,
            base_url,
            event_tx,
            replay_buffered,
            presentation_drops: Arc::new(AtomicUsize::new(0)),
            resume_session_id: Arc::new(Mutex::new(None)),
            anonymous_route: None,
        }
    }

    pub fn with_anonymous_route(
        mut self,
        route_id: String,
        route_token: String,
        initial_ticket: Option<api::RelayTicket>,
    ) -> Self {
        self.anonymous_route = Some(AnonymousRouteCredential {
            route_id,
            route_token,
            initial_ticket: Arc::new(Mutex::new(initial_ticket)),
        });
        self
    }

    fn emit_presentation(&self, event: TunnelEvent) {
        let dropped = self.presentation_drops.swap(0, Ordering::AcqRel);
        if dropped > 0
            && self
                .event_tx
                .try_send(TunnelEvent::StreamGap {
                    dropped_events: dropped,
                })
                .is_err()
        {
            self.presentation_drops.fetch_add(dropped, Ordering::AcqRel);
        }

        if self.event_tx.try_send(event).is_err() {
            self.presentation_drops.fetch_add(1, Ordering::AcqRel);
        }
    }

    async fn resume_token(&self, access_token: &str) -> Result<Option<String>> {
        if self.anonymous_route.is_some() {
            return Ok(None);
        }

        let session_id = self
            .resume_session_id
            .lock()
            .map_err(|_| anyhow!("Tunnel resume state is unavailable"))?
            .clone();
        let Some(session_id) = session_id else {
            return Ok(None);
        };
        let organization_id = self
            .org_id
            .clone()
            .ok_or_else(|| anyhow!("Tunnel resume requires an organization"))?;
        let client = ApiClient::with_base_url(
            access_token.to_string(),
            self.base_url.clone(),
            Some(organization_id),
        )?;
        let descriptor = client.reconnect_tunnel_session(&session_id).await?;
        Ok(Some(descriptor.resume_token))
    }

    pub async fn connect_and_forward(&self) -> Result<()> {
        api::validate_api_base_url(&self.base_url)?;
        info!(
            local_host = %self.local_host,
            local_port = %self.local_port,
            "Starting HTTP tunnel"
        );

        let _ = self.event_tx.send(TunnelEvent::Connecting).await;

        // Exchange the long-lived HTTP credential for a one-time, scoped handshake ticket.
        let access_token = self.access_token_rx.borrow().clone();
        let resume_token = self.resume_token(&access_token).await?;
        let relay_ticket = if let Some(anonymous_route) = &self.anonymous_route {
            let initial_ticket = anonymous_route
                .initial_ticket
                .lock()
                .map_err(|_| anyhow!("Anonymous tunnel credential state is unavailable"))?
                .take();

            match initial_ticket {
                Some(ticket) => ticket,
                None => {
                    ApiClient::unauthenticated_at(self.base_url.clone())?
                        .issue_anonymous_tunnel_ticket(
                            &anonymous_route.route_id,
                            &anonymous_route.route_token,
                        )
                        .await?
                }
            }
        } else {
            let plan = serde_json::json!({
                "mode": DIRECT_RESPONSE_MODE,
                "route": {"organization_id": &self.org_id, "slug": &self.slug},
                "target": self.target.plan(),
            });
            api::issue_relay_ticket(&access_token, &self.base_url, &plan).await?
        };
        if relay_ticket.scope != "relay:tunnel" {
            return Err(anyhow!(
                "Relay handshake rejected: ticket returned an incompatible scope"
            ));
        }
        if relay_ticket.plan_fingerprint.is_empty() || relay_ticket.expires_at.is_empty() {
            return Err(anyhow!(
                "Relay handshake rejected: ticket receipt was incomplete"
            ));
        }

        let protocol_version = selected_tunnel_protocol(&relay_ticket.supported_protocol_versions)?;
        let protocol_v3 = protocol_version == u64::from(tunnel_v3::VERSION);
        let mut ws_url = build_ws_url(&self.base_url, &relay_ticket.ticket, "tunnel/websocket");
        if protocol_v3 {
            ws_url.push_str("&vsn=2.0.0");
        }

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
                let detail = redact_access_token(&e.to_string(), &relay_ticket.ticket);
                let detail = redact_access_token(&detail, &access_token);
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
        let join_payload = tunnel_join_payload_for_version(
            self.local_port,
            self.org_id.as_deref(),
            self.slug.as_deref(),
            resume_token.as_deref(),
            protocol_version,
        );

        if resume_token.is_some() {
            info!("Resuming canonical tunnel session");
        } else if let Some(slug) = &self.slug {
            info!(slug = %slug, "Requesting static tunnel");
        }

        let join_json = if protocol_v3 {
            tunnel_v3::encode_client_control(
                &tunnel_v3::ControlMessage {
                    join_ref: None,
                    reference: Some("1".to_string()),
                    topic: "tunnel:connect".to_string(),
                    event: "phx_join".to_string(),
                    payload: join_payload,
                },
                TUNNEL_MAX_FRAME_BYTES,
            )?
        } else {
            serde_json::to_string(&ChannelMessage {
                topic: "tunnel:connect".to_string(),
                event: "phx_join".to_string(),
                payload: join_payload,
                reference: Some("1".to_string()),
            })?
        };
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
            max_response_header_items: TUNNEL_MAX_RESPONSE_HEADERS,
            ordered_response_headers: false,
        };
        let mut streaming_limits = None;

        while !joined {
            match tokio::time::timeout(Duration::from_secs(10), read.next()).await {
                Ok(Some(msg_result)) => match msg_result {
                    Ok(Message::Text(text)) => {
                        let msg = decode_channel_message(&text, protocol_v3)?;
                        if msg.event == "phx_reply"
                            && msg.reference.as_deref() == Some("1")
                            && let Some(status) = msg.payload.get("status")
                        {
                            if status == "ok" {
                                // Extract the negotiated contract and canonical session identity.
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

                                if protocol_v3 {
                                    streaming_limits = Some(
                                        tunnel_v3::StreamingLimits::from_join_response(response)?,
                                    );
                                } else {
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
                                }

                                let session_id = response
                                    .get("session_id")
                                    .and_then(|id| id.as_str())
                                    .ok_or_else(|| {
                                        anyhow!(
                                            "Tunnel service did not return a canonical session id"
                                        )
                                    })?
                                    .to_string();
                                *self
                                    .resume_session_id
                                    .lock()
                                    .map_err(|_| anyhow!("Tunnel resume state is unavailable"))? =
                                    Some(session_id);

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
                                let log_subdomain = bounded_control_neutral_log_text(&subdomain);
                                let log_tunnel_id = bounded_control_neutral_log_text(&tunnel_id);
                                info!(
                                    subdomain = %log_subdomain,
                                    tunnel_id = %log_tunnel_id,
                                    tunnel_type = %tunnel_type,
                                    protocol_version = protocol_version,
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
                                let reason = bounded_control_neutral_log_text(reason);
                                let _ = self
                                    .event_tx
                                    .send(TunnelEvent::ConnectionError(reason.clone()))
                                    .await;
                                return Err(anyhow!("Tunnel join failed: {}", reason));
                            }
                        }
                    }
                    Ok(Message::Ping(data)) => {
                        write.send(Message::Pong(data)).await?;
                    }
                    Ok(Message::Close(frame)) => {
                        let frame = bounded_control_neutral_log_text(&format!("{frame:?}"));
                        return Err(anyhow!("WebSocket closed during join: {frame}"));
                    }
                    Err(e) => return Err(anyhow!("WebSocket error during join: {}", e)),
                    _ => {}
                },
                Ok(None) => return Err(anyhow!("WebSocket stream ended during join")),
                Err(_) => return Err(anyhow!("Timeout waiting for tunnel join response")),
            }
        }

        let (writer, mut writer_task) = PriorityWriter::spawn(write);
        if let Some(streaming_limits) = streaming_limits {
            return self
                .run_v3_tunnel(read, writer, writer_task, tunnel_topic, streaming_limits)
                .await;
        }

        let mut ping_interval = tokio::time::interval(Duration::from_secs(30));
        ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ping_interval.tick().await;
        let mut ping_counter = 2;
        let mut runtime = RelayRuntime::new();

        // Listen for tunnel_request events
        loop {
            // Completed tasks have already returned their local-forward
            // permits. Remove their JoinSet results and delivery indexes
            // before admitting more messages from a continuously readable
            // socket.
            reap_ready_tunnel_workers(&mut runtime);

            tokio::select! {
                biased;
                writer_result = &mut writer_task => {
                    match writer_result {
                        Ok(Ok(())) => return Err(anyhow!("Tunnel writer stopped")),
                        Ok(Err(error)) => return Err(error.context("Tunnel writer failed")),
                        Err(error) => return Err(anyhow!("Tunnel writer task failed: {error}")),
                    }
                }
                _ = ping_interval.tick() => {
                    writer.control(ChannelMessage {
                        topic: tunnel_topic.clone(),
                        event: "ping".to_string(),
                        payload: serde_json::json!({}),
                        reference: Some(ping_counter.to_string()),
                    }).await?;
                    ping_counter += 1;
                },
                completed = runtime.workers.join_next_with_id(), if !runtime.workers.is_empty() => {
                    if let Some(completion) = completed {
                        complete_tunnel_worker(completion, &mut runtime);
                    }
                },
                maybe_msg = read.next() => match maybe_msg {
                    Some(msg) => match msg {
                    Ok(Message::Text(text)) => {
                        if let Err(e) = self
                            .handle_tunnel_message(
                                &text,
                                &writer,
                                &tunnel_topic,
                                tunnel_limits,
                                &mut runtime,
                            )
                            .await
                        {
                            let error = bounded_control_neutral_log_text(&e.to_string());
                            error!(error = %error, "Error handling tunnel message");
                        }
                    }
                    Ok(Message::Close(frame)) => {
                        let frame = bounded_control_neutral_log_text(&format!("{frame:?}"));
                        info!(frame = %frame, "Tunnel WebSocket closed");
                        self.emit_presentation(TunnelEvent::Disconnected);
                        break;
                    }
                    Ok(Message::Ping(data)) => {
                        if let Err(e) = writer.pong(data).await {
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
                    None => {
                    warn!("Tunnel WebSocket stream ended");
                    self.emit_presentation(TunnelEvent::Disconnected);
                    break;
                    }
                },
            }
        }

        runtime.workers.abort_all();
        while runtime.workers.join_next().await.is_some() {}
        writer_task.abort();
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn forward_v3_request(
        &self,
        incoming: tunnel_v3::IncomingRequest,
        phase: Arc<AtomicU8>,
        writer: PriorityWriter,
        tunnel_topic: String,
        limits: tunnel_v3::StreamingLimits,
        connection_window: tunnel_v3::ConnectionSendWindow,
        mut response_rx: mpsc::Receiver<tunnel_v3::WindowUpdate>,
        _work_permit: OwnedSemaphorePermit,
    ) -> Result<()> {
        let tunnel_v3::IncomingRequest { start, body } = incoming;
        let stream_id = start.stream_id;
        let started_at = std::time::Instant::now();
        let local_deadline = tokio::time::Instant::now() + Duration::from_millis(start.timeout_ms);

        let target = match self.target.request_url(
            &start.path,
            (!start.query_string.is_empty()).then_some(start.query_string.as_str()),
        ) {
            Ok(target) => target,
            Err(error) => {
                return self
                    .report_v3_failure(
                        stream_id,
                        error.code(),
                        error.to_string(),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        };

        let remaining = local_deadline.saturating_duration_since(tokio::time::Instant::now());

        let method = match supported_tunnel_method(&start.method) {
            Some(method) => method,
            None => {
                return self
                    .report_v3_failure(
                        stream_id,
                        "invalid_method_before_forward",
                        format!("Unsupported HTTP method: {}", start.method),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        };
        let client = match self.target.http_client_with_timeout(remaining) {
            Ok(client) => client,
            Err(error) => {
                return self
                    .report_v3_failure(
                        stream_id,
                        "local_client_error_before_forward",
                        error.to_string(),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        };

        let mut request = client.request(method, target);
        for (name, value) in &start.headers {
            if should_forward_request_header(name)
                && let Ok(name) = reqwest::header::HeaderName::from_bytes(name.as_bytes())
                && let Ok(value) = reqwest::header::HeaderValue::from_str(value)
            {
                request = request.header(name, value);
            }
        }
        if let Some(content_length) = start.content_length {
            request = request.header(reqwest::header::CONTENT_LENGTH, content_length);
        }

        let (consumption_tx, mut consumption_rx) =
            mpsc::channel(limits.stream_window_items.saturating_add(1));
        let V3RequestBodyBridge {
            request_body,
            response_started,
            pump: request_body_pump,
        } = spawn_v3_request_body_bridge(body, consumption_tx);
        request = request.body(request_body);

        let reporter_writer = writer.clone();
        let reporter_topic = tunnel_topic.clone();
        let consumption_reporter = tokio::spawn(async move {
            while let Some(update) = consumption_rx.recv().await {
                reporter_writer
                    .control_v3(v3_control_message(
                        &reporter_topic,
                        "tunnel_stream_window",
                        serde_json::json!({
                            "stream_id": stream_id,
                            "direction": "request",
                            "consumed_bytes": update.consumed_bytes,
                            "consumed_items": update.consumed_items,
                        }),
                    ))
                    .await?;
            }
            Ok::<(), anyhow::Error>(())
        });

        phase.store(DELIVERY_STARTED, Ordering::Release);
        let mut response = match request.send().await {
            Ok(response) => {
                // A final local response transfers ownership away from
                // reqwest. Keep draining the bounded protocol source so the
                // service can finish ingress without waiting on a local
                // server that has already stopped reading the upload.
                let _ = response_started.send(true);
                response
            }
            Err(error) => {
                let _ = response_started.send(true);
                request_body_pump.abort();
                consumption_reporter.abort();
                return self
                    .report_v3_failure(
                        stream_id,
                        "local_forward_failed",
                        format!("Failed to forward request: {}", error.without_url()),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        };

        match request_body_pump.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                consumption_reporter.abort();
                return self
                    .report_v3_failure(
                        stream_id,
                        "request_body_drain_failed",
                        error.to_string(),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
            Err(error) => {
                consumption_reporter.abort();
                return self
                    .report_v3_failure(
                        stream_id,
                        "request_body_pump_failed",
                        error.to_string(),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        }

        match consumption_reporter.await {
            Ok(Ok(())) => {}
            Ok(Err(error)) => {
                return self
                    .report_v3_failure(
                        stream_id,
                        "request_window_delivery_failed",
                        error.to_string(),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
            Err(error) => {
                return self
                    .report_v3_failure(
                        stream_id,
                        "request_window_reporter_failed",
                        error.to_string(),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        }

        let status = response.status().as_u16();
        if !(200..=599).contains(&status) || status == 101 {
            return self
                .report_v3_failure(
                    stream_id,
                    "invalid_local_response_status",
                    format!("Local response status {status} cannot be relayed"),
                    &writer,
                    &tunnel_topic,
                    &phase,
                )
                .await;
        }
        if response_header_bytes(response.headers()) > LOCAL_MAX_RESPONSE_HEADER_BYTES
            || response
                .headers()
                .iter()
                .any(|(_name, value)| value.to_str().is_err())
        {
            return self
                .report_v3_failure(
                    stream_id,
                    "response_headers_too_large",
                    "Local response headers cannot be relayed safely".to_string(),
                    &writer,
                    &tunnel_topic,
                    &phase,
                )
                .await;
        }
        if status == 205 && !valid_reset_content_length(response.headers()) {
            return self
                .report_v3_failure(
                    stream_id,
                    "body_not_allowed",
                    "A 205 Reset Content response may only declare Content-Length: 0".to_string(),
                    &writer,
                    &tunnel_topic,
                    &phase,
                )
                .await;
        }

        let response_header_pairs = response_headers_to_ordered_pairs(response.headers());
        let response_headers = response_headers_to_map(response.headers());
        let response_content_length =
            relayed_response_content_length(&start.method, status, response.content_length());
        if response_content_length
            .is_some_and(|length| length > limits.max_response_body_bytes as u64)
        {
            return self
                .report_v3_failure(
                    stream_id,
                    "response_body_too_large",
                    format!(
                        "Local response body exceeds tunnel limit (max {} bytes)",
                        limits.max_response_body_bytes
                    ),
                    &writer,
                    &tunnel_topic,
                    &phase,
                )
                .await;
        }

        let response_start = tunnel_v3::response_start_payload(
            stream_id,
            status,
            &response_header_pairs,
            response_content_length,
        );

        if let Err(error) =
            tunnel_v3::validate_control_payload(&response_start, limits.max_control_payload_bytes)
        {
            return self
                .report_v3_failure(
                    stream_id,
                    "response_headers_too_large",
                    error.to_string(),
                    &writer,
                    &tunnel_topic,
                    &phase,
                )
                .await;
        }

        if let Err(error) = writer
            .response_control_v3(v3_control_message(
                &tunnel_topic,
                "tunnel_response_start",
                response_start,
            ))
            .await
        {
            return self
                .report_v3_failure(
                    stream_id,
                    "response_start_delivery_failed",
                    error.to_string(),
                    &writer,
                    &tunnel_topic,
                    &phase,
                )
                .await;
        }

        let mut response_stream = match tunnel_v3::ResponseStream::new(
            stream_id,
            tunnel_topic.clone(),
            "1".to_string(),
            limits,
            connection_window,
        ) {
            Ok(stream) => stream,
            Err(error) => {
                return self
                    .report_v3_failure(
                        stream_id,
                        "response_stream_initialization_failed",
                        error.to_string(),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        };
        let mut preview = Vec::with_capacity(UI_BODY_PREVIEW_BYTES.saturating_add(1));
        let mut streamed_response_bytes = 0u64;

        loop {
            match response.chunk().await {
                Ok(Some(chunk)) => {
                    if let Err(error) = ensure_response_chunk_allowed(&start.method, status) {
                        return self
                            .report_v3_failure(
                                stream_id,
                                "body_not_allowed",
                                error.to_string(),
                                &writer,
                                &tunnel_topic,
                                &phase,
                            )
                            .await;
                    }

                    let preview_remaining = UI_BODY_PREVIEW_BYTES
                        .saturating_add(1)
                        .saturating_sub(preview.len());
                    preview.extend_from_slice(&chunk[..chunk.len().min(preview_remaining)]);

                    for data in chunk.chunks(limits.max_chunk_bytes) {
                        let next_bytes = streamed_response_bytes.saturating_add(data.len() as u64);
                        if response_content_length
                            .is_some_and(|content_length| next_bytes > content_length)
                        {
                            return self
                                .report_v3_failure(
                                    stream_id,
                                    "response_content_length_mismatch",
                                    "Local response exceeded its declared Content-Length"
                                        .to_string(),
                                    &writer,
                                    &tunnel_topic,
                                    &phase,
                                )
                                .await;
                        }
                        if let Err(error) = enqueue_v3_response_chunk(
                            &mut response_stream,
                            &writer,
                            &mut response_rx,
                            data,
                            local_deadline,
                        )
                        .await
                        {
                            return self
                                .report_v3_failure(
                                    stream_id,
                                    "response_delivery_failed",
                                    error.to_string(),
                                    &writer,
                                    &tunnel_topic,
                                    &phase,
                                )
                                .await;
                        }
                        streamed_response_bytes = next_bytes;
                    }
                }
                Ok(None) => break,
                Err(error) => {
                    return self
                        .report_v3_failure(
                            stream_id,
                            "response_read_failed",
                            format!("Failed to read local response: {}", error.without_url()),
                            &writer,
                            &tunnel_topic,
                            &phase,
                        )
                        .await;
                }
            }
        }
        drop(response);

        if response_content_length
            .is_some_and(|content_length| streamed_response_bytes != content_length)
        {
            return self
                .report_v3_failure(
                    stream_id,
                    "response_content_length_mismatch",
                    "Local response did not match its declared Content-Length".to_string(),
                    &writer,
                    &tunnel_topic,
                    &phase,
                )
                .await;
        }

        let evidence = match response_stream.finish() {
            Ok(evidence) => evidence,
            Err(error) => {
                return self
                    .report_v3_failure(
                        stream_id,
                        "response_integrity_failed",
                        error.to_string(),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        };
        if let Err(error) = writer
            .response_control_v3(v3_control_message(
                &tunnel_topic,
                "tunnel_response_end",
                evidence.end_payload(stream_id),
            ))
            .await
        {
            return self
                .report_v3_failure(
                    stream_id,
                    "response_end_delivery_failed",
                    error.to_string(),
                    &writer,
                    &tunnel_topic,
                    &phase,
                )
                .await;
        }

        while !response_stream.fully_consumed() {
            if let Err(error) = acknowledge_v3_response_window(
                &mut response_stream,
                &mut response_rx,
                local_deadline,
            )
            .await
            {
                return self
                    .report_v3_failure(
                        stream_id,
                        "response_consumption_failed",
                        error.to_string(),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        }

        phase.store(DELIVERY_TERMINAL, Ordering::Release);
        let mut response_body = body_preview(&preview, &response_headers);
        if evidence.bytes > preview.len() as u64
            && let Some(body) = &mut response_body
        {
            body.push_str(&format!("\n… [{} bytes total]", evidence.bytes));
        }
        self.emit_presentation(TunnelEvent::RequestForwarded {
            request_id: stream_id.to_string(),
            status,
            duration_ms: started_at.elapsed().as_millis() as u64,
            response_headers: bounded_presentation_headers(response_header_pairs),
            response_body,
        });
        Ok(())
    }

    async fn report_v3_failure(
        &self,
        stream_id: Uuid,
        code: &str,
        message: String,
        writer: &PriorityWriter,
        tunnel_topic: &str,
        phase: &AtomicU8,
    ) -> Result<()> {
        let previous_phase = phase.swap(DELIVERY_TERMINAL, Ordering::AcqRel);
        let outcome = cancellation_classification(previous_phase)
            .map(|(_code, outcome)| outcome)
            .unwrap_or("outcome_unknown");
        writer
            .response_control_v3(v3_abort_message(
                tunnel_topic,
                stream_id,
                "response",
                code,
                outcome,
            ))
            .await?;
        self.emit_presentation(TunnelEvent::RequestFailed {
            request_id: stream_id.to_string(),
            error: bounded_presentation_text(&message),
        });
        Err(anyhow!(message))
    }

    async fn run_v3_tunnel(
        &self,
        mut read: WsRead,
        writer: PriorityWriter,
        mut writer_task: tokio::task::JoinHandle<Result<()>>,
        tunnel_topic: String,
        limits: tunnel_v3::StreamingLimits,
    ) -> Result<()> {
        let mut runtime = V3RelayRuntime::new(tunnel_topic.clone(), limits)?;
        let mut ping_interval = tokio::time::interval(Duration::from_secs(30));
        ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        ping_interval.tick().await;
        let mut ping_counter = 2u64;

        loop {
            // A completed task has already released its local-forward permit.
            // Reap every ready result before accepting more ingress so the
            // JoinSet and active-delivery index cannot grow while the socket is
            // continuously readable.
            reap_ready_v3_workers(&mut runtime, &writer, &tunnel_topic).await?;

            tokio::select! {
                biased;
                writer_result = &mut writer_task => {
                    match writer_result {
                        Ok(Ok(())) => return Err(anyhow!("Tunnel writer stopped")),
                        Ok(Err(error)) => return Err(error.context("Tunnel writer failed")),
                        Err(error) => return Err(anyhow!("Tunnel writer task failed: {error}")),
                    }
                }
                _ = ping_interval.tick() => {
                    let mut message = v3_control_message(
                        &tunnel_topic,
                        "ping",
                        serde_json::json!({}),
                    );
                    message.reference = Some(ping_counter.to_string());
                    writer.control_v3(message).await?;
                    ping_counter = ping_counter.saturating_add(1);
                }
                completed = runtime.workers.join_next_with_id(), if !runtime.workers.is_empty() => {
                    if let Some(completion) = completed {
                        complete_v3_worker(
                            completion,
                            &mut runtime,
                            &writer,
                            &tunnel_topic,
                        ).await?;
                    }
                },
                maybe_message = read.next() => match maybe_message {
                    Some(Ok(Message::Text(text))) => {
                        let message = tunnel_v3::decode_server_control(&text)?;
                        if matches!(message.event.as_str(), "phx_error" | "phx_close") {
                            return Err(anyhow!("Protocol v3 channel closed"));
                        }
                        if let Err(error) = self
                            .handle_v3_control(
                                message,
                                &writer,
                                &tunnel_topic,
                                limits,
                                &mut runtime,
                            )
                            .await
                        {
                            let error = bounded_control_neutral_log_text(&error.to_string());
                            warn!(error = %error, "Rejected protocol v3 control event");
                        }
                    }
                    Some(Ok(Message::Binary(encoded))) => {
                        self.handle_v3_binary(
                            &encoded,
                            &writer,
                            &tunnel_topic,
                            limits,
                            &mut runtime,
                        ).await?;
                    }
                    Some(Ok(Message::Ping(data))) => writer.pong(data).await?,
                    Some(Ok(Message::Close(frame))) => {
                        let frame = bounded_control_neutral_log_text(&format!("{frame:?}"));
                        info!(frame = %frame, "Tunnel WebSocket closed");
                        self.emit_presentation(TunnelEvent::Disconnected);
                        break;
                    }
                    Some(Ok(_)) => {}
                    Some(Err(error)) => {
                        let error = bounded_control_neutral_log_text(&error.to_string());
                        return Err(anyhow!("Tunnel WebSocket error: {error}"));
                    }
                    None => {
                        self.emit_presentation(TunnelEvent::Disconnected);
                        break;
                    }
                },
            }
        }

        runtime.workers.abort_all();
        while runtime.workers.join_next().await.is_some() {}
        writer_task.abort();
        Ok(())
    }

    async fn handle_v3_control(
        &self,
        message: tunnel_v3::ControlMessage,
        writer: &PriorityWriter,
        tunnel_topic: &str,
        limits: tunnel_v3::StreamingLimits,
        runtime: &mut V3RelayRuntime,
    ) -> Result<()> {
        if message.topic != tunnel_topic && message.topic != "phoenix" {
            return Err(anyhow!("Protocol v3 control event has an unexpected topic"));
        }
        tunnel_v3::validate_control_payload(&message.payload, limits.max_control_payload_bytes)?;

        match message.event.as_str() {
            "tunnel_request_start" => {
                let stream_id = v3_payload_stream_id(&message.payload)?;
                if runtime.active_deliveries.contains_key(&stream_id) {
                    writer
                        .control_v3(v3_abort_message(
                            tunnel_topic,
                            stream_id,
                            "request",
                            "duplicate_stream",
                            "known_not_executed",
                        ))
                        .await?;
                    return Ok(());
                }

                let incoming = match runtime.requests.start(&message.payload) {
                    Ok(incoming) => incoming,
                    Err(error) => {
                        writer
                            .control_v3(v3_abort_message(
                                tunnel_topic,
                                stream_id,
                                "request",
                                "invalid_request_start",
                                "known_not_executed",
                            ))
                            .await?;
                        return Err(error);
                    }
                };

                let Some(work_permit) = runtime.work_count.clone().try_acquire_owned().ok() else {
                    runtime
                        .requests
                        .cancel(stream_id, "relay_overloaded_before_forward");
                    writer
                        .control_v3(v3_abort_message(
                            tunnel_topic,
                            stream_id,
                            "request",
                            "relay_overloaded_before_forward",
                            "known_not_executed",
                        ))
                        .await?;
                    return Ok(());
                };

                let headers = bounded_presentation_headers(incoming.start.headers.iter().cloned());
                self.emit_presentation(TunnelEvent::RequestReceived {
                    request_id: stream_id.to_string(),
                    method: incoming.start.method.clone(),
                    path: bounded_presentation_text(&incoming.start.path),
                    headers,
                    body: None,
                    query_string: bounded_presentation_text(&incoming.start.query_string),
                    replay: incoming.start.replay,
                });

                let (response_tx, response_rx) =
                    mpsc::channel(limits.stream_window_items.saturating_add(1));
                let phase = Arc::new(AtomicU8::new(DELIVERY_QUEUED));
                let task_phase = phase.clone();
                let worker = self.clone();
                let task_writer = writer.clone();
                let task_topic = tunnel_topic.to_string();
                let response_window = runtime.response_window.clone();
                let abort = runtime.workers.spawn(async move {
                    let result = worker
                        .forward_v3_request(
                            incoming,
                            task_phase,
                            task_writer,
                            task_topic,
                            limits,
                            response_window,
                            response_rx,
                            work_permit,
                        )
                        .await;
                    (stream_id, result)
                });

                runtime.active_deliveries.insert(
                    stream_id,
                    ActiveV3Delivery {
                        abort,
                        phase,
                        response_tx,
                    },
                );
            }
            "tunnel_request_end" => {
                let stream_id = v3_payload_stream_id(&message.payload)?;
                if let Err(error) = runtime.requests.finish(&message.payload) {
                    if let Some(active) = runtime.active_deliveries.remove(&stream_id) {
                        active.abort.abort();
                    }
                    writer
                        .control_v3(v3_abort_message(
                            tunnel_topic,
                            stream_id,
                            "request",
                            "invalid_request_end",
                            "outcome_unknown",
                        ))
                        .await?;
                    return Err(error);
                }
            }
            "tunnel_stream_window" => {
                if message
                    .payload
                    .get("direction")
                    .and_then(serde_json::Value::as_str)
                    != Some("response")
                {
                    return Err(anyhow!("Protocol v3 window has an unexpected direction"));
                }
                let stream_id = v3_payload_stream_id(&message.payload)?;
                let update = tunnel_v3::WindowUpdate {
                    consumed_bytes: message
                        .payload
                        .get("consumed_bytes")
                        .and_then(serde_json::Value::as_u64)
                        .ok_or_else(|| anyhow!("Protocol v3 window has invalid bytes"))?,
                    consumed_items: message
                        .payload
                        .get("consumed_items")
                        .and_then(serde_json::Value::as_u64)
                        .ok_or_else(|| anyhow!("Protocol v3 window has invalid items"))?,
                };
                let admission_error = runtime
                    .active_deliveries
                    .get(&stream_id)
                    .and_then(|active| admit_v3_response_window(&active.response_tx, update).err());

                if let Some(code) = admission_error
                    && let Some(active) = runtime.active_deliveries.remove(&stream_id)
                {
                    let phase = active.phase.load(Ordering::Acquire);
                    let outcome = cancellation_classification(phase)
                        .map(|(_code, outcome)| outcome)
                        .unwrap_or("outcome_unknown");

                    active.abort.abort();
                    runtime.requests.cancel(stream_id, code);
                    writer.try_control_v3(v3_abort_message(
                        tunnel_topic,
                        stream_id,
                        "response",
                        code,
                        outcome,
                    ))?;
                }
            }
            "tunnel_stream_abort" => {
                let stream_id = v3_payload_stream_id(&message.payload)?;
                let direction = message
                    .payload
                    .get("direction")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| anyhow!("Protocol v3 abort has no direction"))?;
                if !matches!(direction, "request" | "response") {
                    return Err(anyhow!("Protocol v3 abort has an invalid direction"));
                }

                if runtime.requests.contains(stream_id) {
                    runtime.requests.cancel(stream_id, "remote_abort");
                }
                if let Some(active) = runtime.active_deliveries.remove(&stream_id) {
                    active.abort.abort();
                }
            }
            "buffered_summary" => {
                let count = message
                    .payload
                    .get("count")
                    .and_then(serde_json::Value::as_u64)
                    .unwrap_or(0);
                let oldest_captured_at = message
                    .payload
                    .get("oldest_captured_at")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);

                info!(count = count, "Buffered requests waiting on server");
                let _ = self
                    .event_tx
                    .send(TunnelEvent::BufferedSummary {
                        count,
                        oldest_captured_at,
                    })
                    .await;

                if self.replay_buffered && count > 0 && !runtime.auto_drain_requested {
                    runtime.auto_drain_requested = true;
                    let mut replay =
                        v3_control_message(tunnel_topic, "buffered:replay", serde_json::json!({}));
                    replay.reference = Some("buffered-replay".to_string());
                    writer
                        .control_v3(replay)
                        .await
                        .context("Failed to send buffered replay request")?;
                }
            }
            "buffered_replayed" => {
                let capture_id = message
                    .payload
                    .get("capture_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let status = message
                    .payload
                    .get("status")
                    .and_then(serde_json::Value::as_u64)
                    .and_then(|status| u16::try_from(status).ok())
                    .unwrap_or(0);

                let _ = self
                    .event_tx
                    .send(TunnelEvent::BufferedReplayed { capture_id, status })
                    .await;
            }
            "buffered_replay_failed" => {
                let capture_id = message
                    .payload
                    .get("capture_id")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();
                let reason = message
                    .payload
                    .get("reason")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("unknown")
                    .to_string();

                let _ = self
                    .event_tx
                    .send(TunnelEvent::BufferedReplayFailed { capture_id, reason })
                    .await;
            }
            "phx_reply" => {
                if message.reference.as_deref() == Some("buffered-replay") {
                    let status = message
                        .payload
                        .get("status")
                        .and_then(serde_json::Value::as_str)
                        .unwrap_or("");

                    if status != "ok" {
                        let reason = message
                            .payload
                            .get("response")
                            .and_then(|response| response.get("reason"))
                            .and_then(serde_json::Value::as_str)
                            .unwrap_or("unknown")
                            .to_string();

                        if reason != "buffer_empty" {
                            let log_reason = bounded_control_neutral_log_text(&reason);
                            warn!(reason = %log_reason, "Buffered replay request rejected");
                            let _ = self
                                .event_tx
                                .send(TunnelEvent::BufferedReplayFailed {
                                    capture_id: "unknown".to_string(),
                                    reason,
                                })
                                .await;
                        }
                    }
                }
            }
            "phx_error" | "phx_close" => {
                return Err(anyhow!("Protocol v3 channel closed"));
            }
            event => debug!(event = %event, "Unhandled protocol v3 tunnel event"),
        }

        Ok(())
    }

    async fn handle_v3_binary(
        &self,
        encoded: &[u8],
        writer: &PriorityWriter,
        tunnel_topic: &str,
        limits: tunnel_v3::StreamingLimits,
        runtime: &mut V3RelayRuntime,
    ) -> Result<()> {
        let push = tunnel_v3::decode_server_binary_push(encoded)?;
        if push.topic != tunnel_topic || push.event != "tunnel_request_chunk" {
            return Err(anyhow!("Unexpected protocol v3 binary channel event"));
        }
        let chunk = tunnel_v3::decode_chunk(push.payload, limits.max_chunk_bytes)?;
        let stream_id = chunk.stream_id;

        if let Err(error) = runtime.requests.accept_binary_push(encoded) {
            runtime.requests.cancel(stream_id, "invalid_request_chunk");
            if let Some(active) = runtime.active_deliveries.remove(&stream_id) {
                active.abort.abort();
            }
            writer
                .control_v3(v3_abort_message(
                    tunnel_topic,
                    stream_id,
                    "request",
                    "invalid_request_chunk",
                    "outcome_unknown",
                ))
                .await?;
            let error = bounded_control_neutral_log_text(&error.to_string());
            warn!(stream_id = %stream_id, error = %error, "Rejected protocol v3 request chunk");
        }

        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    async fn handle_tunnel_message(
        &self,
        text: &str,
        writer: &PriorityWriter,
        tunnel_topic: &str,
        tunnel_limits: TunnelLimits,
        runtime: &mut RelayRuntime,
    ) -> Result<()> {
        let msg: ChannelMessage = serde_json::from_str(text)?;
        let log_topic = bounded_control_neutral_log_text(&msg.topic);
        let log_event = bounded_control_neutral_log_text(&msg.event);

        debug!(
            topic = %log_topic,
            event = %log_event,
            "Received tunnel message"
        );

        match msg.event.as_str() {
            "tunnel_stream_start" => {
                let assembler = TunnelStreamAssembler::from_start(&msg.payload, "request")?;
                if runtime.inbound_streams.len() >= INBOUND_STREAM_MAX_COUNT
                    || !inbound_stream_fits_budget(
                        runtime.inbound_reserved_bytes,
                        assembler.total_bytes,
                    )
                {
                    writer
                        .control(stream_error_message(
                            tunnel_topic,
                            &assembler.stream_id,
                            "relay_inbound_overloaded",
                        ))
                        .await?;
                    return Ok(());
                }
                if runtime.inbound_streams.contains_key(&assembler.stream_id) {
                    writer
                        .control(stream_error_message(
                            tunnel_topic,
                            &assembler.stream_id,
                            "duplicate_stream",
                        ))
                        .await?;
                    return Ok(());
                }

                runtime.inbound_reserved_bytes += assembler.total_bytes;
                runtime
                    .inbound_streams
                    .insert(assembler.stream_id.clone(), assembler);
            }
            "tunnel_stream_frame" => {
                let stream_id = msg
                    .payload
                    .get("stream_id")
                    .and_then(serde_json::Value::as_str)
                    .ok_or_else(|| anyhow!("Tunnel frame is missing its stream id"))?
                    .to_string();
                let sequence = json_usize(&msg.payload, "sequence")?;

                let completed = match runtime.inbound_streams.get_mut(&stream_id) {
                    Some(assembler) => match assembler.append(&msg.payload) {
                        Ok(completed) => completed,
                        Err(error) => {
                            let assembler = runtime
                                .inbound_streams
                                .remove(&stream_id)
                                .expect("invalid stream must still be reserved");
                            runtime.inbound_reserved_bytes = runtime
                                .inbound_reserved_bytes
                                .saturating_sub(assembler.total_bytes);
                            writer
                                .control(stream_error_message(
                                    tunnel_topic,
                                    &stream_id,
                                    "invalid_stream_frame",
                                ))
                                .await?;
                            let log_stream_id = bounded_control_neutral_log_text(&stream_id);
                            let error = bounded_control_neutral_log_text(&error.to_string());
                            warn!(stream_id = %log_stream_id, error = %error, "Rejected invalid tunnel frame");
                            return Ok(());
                        }
                    },
                    None => {
                        writer
                            .control(stream_error_message(
                                tunnel_topic,
                                &stream_id,
                                "unknown_stream",
                            ))
                            .await?;
                        return Ok(());
                    }
                };

                writer
                    .control(ChannelMessage {
                        topic: tunnel_topic.to_string(),
                        event: "tunnel_stream_ack".to_string(),
                        payload: serde_json::json!({
                            "stream_id": stream_id,
                            "sequence": sequence,
                        }),
                        reference: None,
                    })
                    .await?;

                if let Some(payload) = completed {
                    let assembler = runtime
                        .inbound_streams
                        .remove(&stream_id)
                        .ok_or_else(|| anyhow!("Completed tunnel stream disappeared"))?;
                    runtime.inbound_reserved_bytes = runtime
                        .inbound_reserved_bytes
                        .saturating_sub(assembler.total_bytes);
                    let deadline_unix_ms = assembler.deadline_unix_ms;
                    drop(assembler);

                    let delivery = match self.parse_local_delivery(
                        &payload,
                        deadline_unix_ms,
                        tunnel_limits,
                    ) {
                        Ok(delivery) => delivery,
                        Err(error) => {
                            let request_id = payload
                                .get("request_id")
                                .and_then(serde_json::Value::as_str)
                                .unwrap_or(&stream_id)
                                .to_string();
                            let error = error.to_string();
                            drop(payload);
                            writer
                                .control(delivery_error_message(
                                    tunnel_topic,
                                    &request_id,
                                    "invalid_relay_request",
                                    "known_not_executed",
                                    &error,
                                ))
                                .await?;
                            return Ok(());
                        }
                    };
                    // Parsing clones the validated metadata and decodes the body into
                    // LocalDelivery, so the larger serialized JSON representation is
                    // no longer needed while the request waits for local capacity.
                    drop(payload);

                    if runtime.active_deliveries.contains_key(&delivery.request_id) {
                        writer
                            .control(delivery_error_message(
                                tunnel_topic,
                                &delivery.request_id,
                                "duplicate_delivery",
                                "known_not_executed",
                                "Delivery is already active",
                            ))
                            .await?;
                        return Ok(());
                    }

                    let Some(permit) = runtime.work_budget.try_acquire(delivery.retained_bytes())
                    else {
                        writer
                            .control(delivery_error_message(
                                tunnel_topic,
                                &delivery.request_id,
                                "relay_overloaded_before_forward",
                                "known_not_executed",
                                "Local relay capacity is full",
                            ))
                            .await?;
                        self.emit_presentation(TunnelEvent::RequestFailed {
                            request_id: delivery.request_id,
                            error: "Local relay capacity is full".to_string(),
                        });
                        return Ok(());
                    };

                    self.emit_presentation(delivery.received_event());
                    let request_id = delivery.request_id.clone();
                    let task_request_id = request_id.clone();
                    let phase = Arc::new(AtomicU8::new(DELIVERY_QUEUED));
                    let task_phase = phase.clone();
                    let worker = self.clone();
                    let task_writer = writer.clone();
                    let task_response_budget = runtime.response_budget.clone();
                    let topic = tunnel_topic.to_string();
                    let abort = runtime.workers.spawn(async move {
                        let _permit = permit;
                        let result = worker
                            .forward_tunnel_request(
                                delivery,
                                task_phase,
                                task_writer,
                                topic,
                                tunnel_limits,
                                task_response_budget,
                            )
                            .await;
                        (task_request_id, result)
                    });
                    runtime
                        .active_deliveries
                        .insert(request_id, ActiveDelivery { abort, phase });
                }
            }
            "tunnel_cancel" => {
                let request_id = required_string(&msg.payload, "request_id")?;
                if let Some(active) = runtime.active_deliveries.remove(&request_id) {
                    let phase = active.phase.load(Ordering::Acquire);
                    if let Some((code, outcome)) = cancellation_classification(phase) {
                        active.abort.abort();
                        writer
                            .control(delivery_error_message(
                                tunnel_topic,
                                &request_id,
                                code,
                                outcome,
                                "Delivery was cancelled",
                            ))
                            .await?;
                        self.emit_presentation(TunnelEvent::RequestFailed {
                            request_id,
                            error: outcome.to_string(),
                        });
                    }
                }
            }
            "tunnel_stream_error" => {
                let stream_id = msg
                    .payload
                    .get("stream_id")
                    .and_then(serde_json::Value::as_str)
                    .map(str::to_string);
                let code = msg
                    .payload
                    .get("code")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or("stream_error");

                if let Some(stream_id) = stream_id.as_deref() {
                    if let Some(assembler) = runtime.inbound_streams.remove(stream_id) {
                        runtime.inbound_reserved_bytes = runtime
                            .inbound_reserved_bytes
                            .saturating_sub(assembler.total_bytes);
                    }

                    if let Some(active) = runtime.active_deliveries.remove(stream_id) {
                        active.abort.abort();
                        self.emit_presentation(TunnelEvent::RequestFailed {
                            request_id: stream_id.to_string(),
                            error: bounded_presentation_text(code),
                        });
                    }
                }

                let code = bounded_control_neutral_log_text(code);
                warn!(code = %code, "Tunnel stream failed");
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

                if self.replay_buffered && count > 0 && !runtime.auto_drain_requested {
                    runtime.auto_drain_requested = true;

                    let replay_msg = ChannelMessage {
                        topic: tunnel_topic.to_string(),
                        event: "buffered:replay".to_string(),
                        payload: serde_json::json!({}),
                        reference: Some("buffered-replay".to_string()),
                    };

                    writer
                        .control(replay_msg)
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
                            let log_reason = bounded_control_neutral_log_text(&reason);
                            warn!(reason = %log_reason, "Buffered replay request rejected");
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
                debug!(event = %log_event, "Unhandled tunnel event");
            }
        }

        Ok(())
    }

    fn parse_local_delivery(
        &self,
        payload: &serde_json::Value,
        deadline_unix_ms: u64,
        tunnel_limits: TunnelLimits,
    ) -> Result<LocalDelivery> {
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
        let body = decode_request_body_limited(
            body_encoding,
            raw_body,
            tunnel_limits.max_request_body_bytes,
        )?;

        Ok(LocalDelivery {
            request_id,
            method,
            path,
            query_string,
            headers,
            body,
            deadline_unix_ms,
            replay,
        })
    }

    async fn forward_tunnel_request(
        &self,
        delivery: LocalDelivery,
        phase: Arc<AtomicU8>,
        writer: PriorityWriter,
        tunnel_topic: String,
        tunnel_limits: TunnelLimits,
        response_budget: ResponseBufferBudget,
    ) -> Result<()> {
        let LocalDelivery {
            request_id,
            method,
            path,
            query_string,
            headers,
            body,
            deadline_unix_ms,
            replay: _,
        } = delivery;
        let start_time = tokio::time::Instant::now();

        info!(
            request_id = %request_id,
            method = %method,
            path = %path,
            "Forwarding tunnel request to local server"
        );

        let target = match self.target.request_url(
            &path,
            (!query_string.is_empty()).then_some(query_string.as_str()),
        ) {
            Ok(target) => target,
            Err(error) => {
                return self
                    .report_tunnel_failure(
                        &request_id,
                        DeliveryFailure::known(error.code(), error.to_string()),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        };

        let remaining_ms = deadline_unix_ms.saturating_sub(unix_time_ms());
        if remaining_ms == 0 {
            return self
                .report_tunnel_failure(
                    &request_id,
                    DeliveryFailure::known(
                        "deadline_exceeded_before_forward",
                        "Delivery deadline elapsed before local forwarding",
                    ),
                    &writer,
                    &tunnel_topic,
                    &phase,
                )
                .await;
        }

        // One end-to-end deadline covers a proxy-free, address-pinned local request
        // and its response stream.
        let client = match self
            .target
            .http_client_with_timeout(Duration::from_millis(remaining_ms))
        {
            Ok(client) => client,
            Err(error) => {
                return self
                    .report_tunnel_failure(
                        &request_id,
                        DeliveryFailure::known(
                            "local_client_error_before_forward",
                            error.to_string(),
                        ),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        };

        let request_method = match supported_tunnel_method(&method) {
            Some(method) => method,
            None => {
                return self
                    .report_tunnel_failure(
                        &request_id,
                        DeliveryFailure::known(
                            "invalid_method_before_forward",
                            format!("Unsupported HTTP method: {method}"),
                        ),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        };
        let mut req_builder = client.request(request_method, target);

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

        // After this point the local server may have observed the request. Every
        // failure must therefore report an unknown outcome.
        phase.store(DELIVERY_STARTED, Ordering::Release);

        // Send request and handle response
        match req_builder.send().await {
            Ok(mut response) => {
                let status = response.status().as_u16();

                if let Some(error_msg) =
                    response_header_limit_error(response.headers(), tunnel_limits)
                {
                    return self
                        .report_tunnel_failure(
                            &request_id,
                            DeliveryFailure::unknown("response_headers_too_large", error_msg),
                            &writer,
                            &tunnel_topic,
                            &phase,
                        )
                        .await;
                }

                let response_header_pairs = response_headers_to_ordered_pairs(response.headers());
                let response_headers = response_headers_to_map(response.headers());

                let response_content_length = response
                    .content_length()
                    .and_then(|length| usize::try_from(length).ok());

                if response_content_length
                    .is_some_and(|length| length > tunnel_limits.max_response_body_bytes)
                {
                    let error_msg = format!(
                        "Local response body exceeds tunnel limit (max {} bytes)",
                        tunnel_limits.max_response_body_bytes
                    );

                    return self
                        .report_tunnel_failure(
                            &request_id,
                            DeliveryFailure::unknown("response_body_too_large", error_msg),
                            &writer,
                            &tunnel_topic,
                            &phase,
                        )
                        .await;
                }

                let Some(mut response_reservation) =
                    response_budget.try_reserve(response_content_length)
                else {
                    return self
                        .report_tunnel_failure(
                            &request_id,
                            DeliveryFailure::unknown(
                                "relay_response_overloaded",
                                "Concurrent local responses exceed the relay buffer budget",
                            ),
                            &writer,
                            &tunnel_topic,
                            &phase,
                        )
                        .await;
                };

                let initial_capacity = response_content_length
                    .unwrap_or(0)
                    .min(TUNNEL_MAX_FRAME_BYTES);
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
                                        DeliveryFailure::unknown(
                                            "response_body_too_large",
                                            error_msg,
                                        ),
                                        &writer,
                                        &tunnel_topic,
                                        &phase,
                                    )
                                    .await;
                            }

                            if !response_reservation
                                .try_grow_to(response_bytes.len().saturating_add(chunk.len()))
                            {
                                return self
                                    .report_tunnel_failure(
                                        &request_id,
                                        DeliveryFailure::unknown(
                                            "relay_response_overloaded",
                                            "Concurrent local responses exceed the relay buffer budget",
                                        ),
                                        &writer,
                                        &tunnel_topic,
                                        &phase,
                                    )
                                    .await;
                            }

                            response_bytes.extend_from_slice(&chunk);
                        }
                        Ok(None) => break,
                        Err(error) => {
                            let error_msg =
                                format!("Failed to read local response: {}", error.without_url());

                            return self
                                .report_tunnel_failure(
                                    &request_id,
                                    DeliveryFailure::unknown("response_read_failed", error_msg),
                                    &writer,
                                    &tunnel_topic,
                                    &phase,
                                )
                                .await;
                        }
                    }
                }

                drop(response);
                let response_body_preview = body_preview(&response_bytes, &response_headers);
                drop(response_headers);
                let presentation_response_headers =
                    bounded_presentation_headers(response_header_pairs.iter().cloned());
                let (response_body, body_encoding) = encode_response_body(&response_bytes);
                drop(response_bytes);
                let duration_ms = start_time.elapsed().as_millis() as u64;

                info!(
                    request_id = %request_id,
                    status = %status,
                    duration_ms = %duration_ms,
                    body_encoding = %body_encoding,
                    "Request forwarded successfully"
                );

                let response_result = self
                    .send_framed_tunnel_response(
                        &writer,
                        &tunnel_topic,
                        &request_id,
                        deadline_unix_ms,
                        serde_json::json!({
                            "request_id": request_id.clone(),
                            "status": status,
                            "headers": response_header_pairs,
                            "body": response_body,
                            "body_encoding": body_encoding,
                        }),
                    )
                    .await;
                drop(response_reservation);

                if let Err(error) = response_result {
                    return self
                        .report_tunnel_failure(
                            &request_id,
                            DeliveryFailure::unknown("response_delivery_failed", error.to_string()),
                            &writer,
                            &tunnel_topic,
                            &phase,
                        )
                        .await;
                }

                phase.store(DELIVERY_TERMINAL, Ordering::Release);
                self.emit_presentation(TunnelEvent::RequestForwarded {
                    request_id,
                    status,
                    duration_ms,
                    response_headers: presentation_response_headers,
                    response_body: response_body_preview,
                });
            }
            Err(error) => {
                let duration_ms = start_time.elapsed().as_millis() as u64;
                let error_msg = format!("Failed to forward request: {}", error.without_url());

                error!(
                    request_id = %request_id,
                    error = %error_msg,
                    duration_ms = %duration_ms,
                    "Request forwarding failed"
                );

                return self
                    .report_tunnel_failure(
                        &request_id,
                        DeliveryFailure::unknown("local_forward_failed", error_msg),
                        &writer,
                        &tunnel_topic,
                        &phase,
                    )
                    .await;
            }
        }

        Ok(())
    }

    async fn send_framed_tunnel_response(
        &self,
        writer: &PriorityWriter,
        tunnel_topic: &str,
        request_id: &str,
        deadline_unix_ms: u64,
        payload: serde_json::Value,
    ) -> Result<()> {
        let mut stream =
            OutboundTunnelStream::new(request_id, "response", deadline_unix_ms, &payload)?;
        drop(payload);

        writer.response(stream.start_message(tunnel_topic)).await?;

        while let Some(frame) = stream.next_frame(tunnel_topic)? {
            if unix_time_ms() >= deadline_unix_ms {
                return Err(anyhow!("deadline_exceeded"));
            }

            writer.response(frame).await?;
        }

        Ok(())
    }

    async fn report_tunnel_failure(
        &self,
        request_id: &str,
        failure: DeliveryFailure,
        writer: &PriorityWriter,
        tunnel_topic: &str,
        phase: &AtomicU8,
    ) -> Result<()> {
        let DeliveryFailure {
            code,
            outcome,
            error,
        } = failure;
        phase.store(DELIVERY_TERMINAL, Ordering::Release);
        error!(request_id = %request_id, code, outcome, error = %error, "Tunnel request failed");

        self.emit_presentation(TunnelEvent::RequestFailed {
            request_id: request_id.to_string(),
            error: error.clone(),
        });

        writer
            .control(delivery_error_message(
                tunnel_topic,
                request_id,
                code,
                outcome,
                &error,
            ))
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
                    let log_error = bounded_control_neutral_log_text(&err_msg);
                    warn!(error = %log_error, "Tunnel connection attempt failed");
                    if is_fatal_error(&err_msg) {
                        let _ = self
                            .event_tx
                            .send(TunnelEvent::ReconnectFailed { reason: err_msg })
                            .await;
                        return result;
                    }

                    warn!(error = %log_error, "Tunnel connection attempt failed; retrying");

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
    use sha2::{Digest, Sha256};
    use std::io::Write;

    fn v2_join_contract() -> serde_json::Value {
        serde_json::json!({
            "limits": {
                "max_stream_bytes": TUNNEL_MAX_STREAM_BYTES,
            },
            "framing": serde_json::from_str::<serde_json::Value>(include_str!(
                "../fixtures/tunnel_framing_v2.json"
            ))
            .unwrap(),
        })
    }

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
    fn protocol_v3_response_window_admission_never_waits_for_worker_capacity() {
        let (sender, mut receiver) = mpsc::channel(1);
        let first = tunnel_v3::WindowUpdate {
            consumed_bytes: 64_000,
            consumed_items: 1,
        };
        let second = tunnel_v3::WindowUpdate {
            consumed_bytes: 128_000,
            consumed_items: 2,
        };

        assert_eq!(admit_v3_response_window(&sender, first), Ok(()));
        assert_eq!(
            admit_v3_response_window(&sender, second),
            Err("response_window_overflow")
        );
        assert_eq!(receiver.try_recv().unwrap(), first);

        drop(receiver);
        assert_eq!(
            admit_v3_response_window(&sender, second),
            Err("response_window_receiver_closed")
        );
    }

    #[tokio::test]
    async fn protocol_v3_response_window_overflow_aborts_only_the_affected_stream() {
        let address = "127.0.0.1:9".parse().unwrap();
        let (forwarder, _event_rx) = forwarder_for_local_address(address).await;
        let (writer, mut control_rx, _response_rx) = test_priority_writer();
        let limits = test_streaming_limits();
        let mut runtime = V3RelayRuntime::new("tunnel:connect".to_string(), limits).unwrap();
        let stream_id = Uuid::new_v4();
        let (response_tx, _response_rx) = mpsc::channel(1);
        response_tx
            .try_send(tunnel_v3::WindowUpdate {
                consumed_bytes: 1,
                consumed_items: 1,
            })
            .unwrap();
        let phase = Arc::new(AtomicU8::new(DELIVERY_STARTED));
        let abort = runtime.workers.spawn(async move {
            std::future::pending::<()>().await;
            (stream_id, Ok(()))
        });
        runtime.active_deliveries.insert(
            stream_id,
            ActiveV3Delivery {
                abort,
                phase,
                response_tx,
            },
        );

        forwarder
            .handle_v3_control(
                v3_control_message(
                    "tunnel:connect",
                    "tunnel_stream_window",
                    serde_json::json!({
                        "stream_id": stream_id,
                        "direction": "response",
                        "consumed_bytes": 2,
                        "consumed_items": 2,
                    }),
                ),
                &writer,
                "tunnel:connect",
                limits,
                &mut runtime,
            )
            .await
            .unwrap();

        assert!(!runtime.active_deliveries.contains_key(&stream_id));
        let abort = protocol_v3_control(control_rx.recv().await.unwrap());
        assert_eq!(abort.event, "tunnel_stream_abort");
        assert_eq!(abort.payload["stream_id"], stream_id.to_string());
        assert_eq!(abort.payload["direction"], "response");
        assert_eq!(abort.payload["code"], "response_window_overflow");
        assert_eq!(abort.payload["outcome"], "outcome_unknown");
        assert!(
            runtime
                .workers
                .join_next()
                .await
                .unwrap()
                .unwrap_err()
                .is_cancelled()
        );
    }

    #[tokio::test]
    async fn protocol_v3_ninth_start_is_explicitly_rejected_before_local_forwarding() {
        let address = "127.0.0.1:9".parse().unwrap();
        let (forwarder, _event_rx) = forwarder_for_local_address(address).await;
        let (writer, mut control_rx, _response_rx) = test_priority_writer();
        let mut limits = test_streaming_limits();
        limits.max_concurrent_streams = 128;
        limits.max_active_local_forwards = LOCAL_WORK_MAX_COUNT;
        let mut runtime = V3RelayRuntime::new("tunnel:connect".to_string(), limits).unwrap();
        let occupied = (0..LOCAL_WORK_MAX_COUNT)
            .map(|_| {
                runtime
                    .work_count
                    .clone()
                    .try_acquire_owned()
                    .expect("one of the eight local forwarding slots")
            })
            .collect::<Vec<_>>();
        let ninth_stream_id = Uuid::new_v4();

        forwarder
            .handle_v3_control(
                v3_control_message(
                    "tunnel:connect",
                    "tunnel_request_start",
                    serde_json::json!({
                        "stream_id": ninth_stream_id,
                        "deadline_unix_ms": 1,
                        "timeout_ms": 30_000,
                        "method": "POST",
                        "path": "/overload",
                        "query_string": "",
                        "headers": [],
                        "content_length": 0,
                        "replay": false,
                    }),
                ),
                &writer,
                "tunnel:connect",
                limits,
                &mut runtime,
            )
            .await
            .unwrap();

        assert!(!runtime.active_deliveries.contains_key(&ninth_stream_id));
        assert!(!runtime.requests.contains(ninth_stream_id));
        assert_eq!(runtime.work_count.available_permits(), 0);

        let abort = protocol_v3_control(
            tokio::time::timeout(Duration::from_secs(1), control_rx.recv())
                .await
                .expect("ninth start was silently dropped")
                .unwrap(),
        );
        assert_eq!(abort.event, "tunnel_stream_abort");
        assert_eq!(abort.payload["stream_id"], ninth_stream_id.to_string());
        assert_eq!(abort.payload["direction"], "request");
        assert_eq!(abort.payload["code"], "relay_overloaded_before_forward");
        assert_eq!(abort.payload["outcome"], "known_not_executed");
        assert!(control_rx.try_recv().is_err());

        drop(occupied);
        assert_eq!(runtime.work_count.available_permits(), LOCAL_WORK_MAX_COUNT);
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
    fn protocol_v3_205_reset_content_forbids_body_and_nonzero_content_length() {
        assert!(response_body_forbidden("GET", 205));
        assert!(ensure_response_chunk_allowed("GET", 205).is_err());
        assert_eq!(
            relayed_response_content_length("GET", 205, Some(123)),
            Some(0)
        );

        let mut headers = reqwest::header::HeaderMap::new();
        assert!(valid_reset_content_length(&headers));

        headers.insert(
            reqwest::header::CONTENT_LENGTH,
            reqwest::header::HeaderValue::from_static("0"),
        );
        assert!(valid_reset_content_length(&headers));

        headers.insert(
            reqwest::header::CONTENT_LENGTH,
            reqwest::header::HeaderValue::from_static("1"),
        );
        assert!(!valid_reset_content_length(&headers));

        headers.append(
            reqwest::header::CONTENT_LENGTH,
            reqwest::header::HeaderValue::from_static("0"),
        );
        assert!(!valid_reset_content_length(&headers));
    }

    #[test]
    fn test_supported_tunnel_method_accepts_token_valid_extension_methods() {
        assert_eq!(supported_tunnel_method("GET"), Some(reqwest::Method::GET));
        assert_eq!(
            supported_tunnel_method("OPTIONS"),
            Some(reqwest::Method::OPTIONS)
        );
        assert_eq!(
            supported_tunnel_method("PURGE"),
            Some(reqwest::Method::from_bytes(b"PURGE").unwrap())
        );
        assert_eq!(supported_tunnel_method("BAD METHOD"), None);
    }

    #[test]
    fn test_tunnel_websocket_config_accepts_advertised_messages() {
        let config = tunnel_websocket_config();

        assert_eq!(config.max_message_size, Some(TUNNEL_MAX_FRAME_BYTES));
        assert_eq!(config.max_frame_size, Some(TUNNEL_MAX_FRAME_BYTES));
    }

    #[tokio::test]
    async fn test_read_response_body_limited_rejects_large_content_length() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/large")
            .with_body("too large")
            .create_async()
            .await;
        let mut response = reqwest::Client::new()
            .get(format!("{}/large", server.url()))
            .send()
            .await
            .unwrap();

        let error = read_response_body_limited(&mut response, 4)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("max 4 bytes"));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_read_response_body_limited_rejects_large_chunked_body() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/chunked")
            .with_chunked_body(|writer| writer.write_all(b"too large"))
            .create_async()
            .await;
        let mut response = reqwest::Client::new()
            .get(format!("{}/chunked", server.url()))
            .send()
            .await
            .unwrap();

        let error = read_response_body_limited(&mut response, 4)
            .await
            .unwrap_err();

        assert!(error.to_string().contains("max 4 bytes"));
        mock.assert_async().await;
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
            serde_json::from_str(include_str!("../fixtures/tunnel_framing_v2.json")).unwrap();

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

    #[test]
    fn test_v2_response_header_item_limit_counts_repeated_values() {
        let mut headers = reqwest::header::HeaderMap::new();
        headers.append("set-cookie", "first=1".parse().unwrap());
        headers.append("set-cookie", "second=2".parse().unwrap());
        let mut limits = TunnelLimits::from_join_response(&serde_json::json!({
            "limits": {"max_response_header_items": 1}
        }));
        limits.max_response_header_bytes = usize::MAX;

        assert!(
            response_header_limit_error(&headers, limits)
                .unwrap()
                .contains("item limit")
        );

        limits.max_response_header_items = 2;
        assert!(response_header_limit_error(&headers, limits).is_none());
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

    #[test]
    fn test_tunnel_limits_clamp_join_response_contract_to_local_maxima() {
        let response = serde_json::json!({
            "limits": {
                "max_body_bytes": u64::MAX,
                "max_response_header_bytes": 16_777_216,
                "max_response_header_items": 2_048,
                "response_headers_format": "ordered_pairs"
            }
        });

        let limits = TunnelLimits::from_join_response(&response);

        assert_eq!(limits.max_request_body_bytes, LOCAL_MAX_TUNNEL_BODY_BYTES);
        assert_eq!(limits.max_response_body_bytes, LOCAL_MAX_TUNNEL_BODY_BYTES);
        assert_eq!(
            limits.max_response_header_bytes,
            LOCAL_MAX_RESPONSE_HEADER_BYTES
        );
        assert_eq!(limits.max_response_header_items, 2_048);
        assert!(limits.ordered_response_headers);
    }

    #[test]
    fn test_tunnel_limits_clamp_untrusted_body_limit_to_local_maximum() {
        let response = serde_json::json!({
            "limits": {
                "max_body_bytes": u64::MAX,
                "max_response_header_bytes": u64::MAX,
                "max_response_header_items": u64::MAX
            }
        });

        let limits = TunnelLimits::from_join_response(&response);

        assert_eq!(
            (
                limits.max_request_body_bytes,
                limits.max_response_body_bytes
            ),
            (LOCAL_MAX_TUNNEL_BODY_BYTES, LOCAL_MAX_TUNNEL_BODY_BYTES)
        );
        assert_eq!(
            limits.max_response_header_bytes,
            LOCAL_MAX_RESPONSE_HEADER_BYTES
        );
        assert_eq!(
            limits.max_response_header_items,
            TUNNEL_MAX_RESPONSE_HEADERS
        );
    }

    #[test]
    fn test_tunnel_limits_keep_legacy_response_header_shape_by_default() {
        let limits = TunnelLimits::from_join_response(&serde_json::json!({}));

        assert_eq!(limits.max_request_body_bytes, LEGACY_MAX_REQUEST_BODY_BYTES);
        assert_eq!(
            limits.max_response_body_bytes,
            LEGACY_MAX_RESPONSE_BODY_BYTES
        );
        assert_eq!(
            limits.max_response_header_bytes,
            LEGACY_MAX_RESPONSE_HEADER_BYTES
        );
        assert_eq!(
            limits.max_response_header_items,
            TUNNEL_MAX_RESPONSE_HEADERS
        );
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

    #[test]
    fn test_log_text_is_control_neutral_and_bounded() {
        let input = format!(
            "relay\u{1b}]52;c;owned\u{7}\n{}",
            "a".repeat(LOG_TEXT_MAX_BYTES)
        );

        let sanitized = bounded_control_neutral_log_text(&input);

        assert!(sanitized.len() <= LOG_TEXT_MAX_BYTES);
        assert!(!sanitized.chars().any(char::is_control));
        assert!(sanitized.contains("\\u{1b}"));
        assert!(sanitized.contains("\\n"));
        assert!(sanitized.ends_with("..."));
    }

    #[test]
    fn test_local_work_budget_prevents_concurrent_ten_megabyte_deliveries() {
        let budget = LocalWorkBudget::new();
        let ten_mib = 10 * 1024 * 1024;
        let permit = budget.try_acquire(ten_mib).unwrap();

        assert_eq!(budget.available_count(), LOCAL_WORK_MAX_COUNT - 1);
        assert_eq!(budget.available_bytes(), 6 * 1024 * 1024);
        assert!(budget.try_acquire(ten_mib).is_none());

        drop(permit);
        assert_eq!(budget.available_count(), LOCAL_WORK_MAX_COUNT);
        assert_eq!(budget.available_bytes(), LOCAL_WORK_MAX_BYTES);
    }

    #[test]
    fn test_local_work_budget_allows_one_advertised_limit_body() {
        let budget = LocalWorkBudget::new();
        let delivery = LocalDelivery {
            request_id: "request-at-limit".to_string(),
            method: "POST".to_string(),
            path: "/metadata-does-not-reduce-the-body-budget".to_string(),
            query_string: "source=conformance".to_string(),
            headers: vec![(
                "content-type".to_string(),
                "application/octet-stream".to_string(),
            )],
            body: Vec::new(),
            deadline_unix_ms: unix_time_ms() + 1_000,
            replay: false,
        };
        assert_eq!(delivery.retained_bytes(), 0);

        let permit = budget.try_acquire(LOCAL_WORK_MAX_BYTES).unwrap();

        assert_eq!(budget.available_bytes(), 0);
        assert!(budget.try_acquire(1).is_none());

        drop(permit);
        assert_eq!(budget.available_bytes(), LOCAL_WORK_MAX_BYTES);
    }

    #[test]
    fn test_local_work_budget_enforces_count_independently_of_bytes() {
        let budget = LocalWorkBudget::new();
        let permits = (0..LOCAL_WORK_MAX_COUNT)
            .map(|_| budget.try_acquire(1).unwrap())
            .collect::<Vec<_>>();

        assert_eq!(budget.available_count(), 0);
        assert!(budget.try_acquire(1).is_none());
        assert!(budget.available_bytes() > LOCAL_WORK_MAX_BYTES - 1024);

        drop(permits);
        assert_eq!(budget.available_count(), LOCAL_WORK_MAX_COUNT);
    }

    #[test]
    fn test_local_work_budget_rejects_oversized_delivery() {
        let budget = LocalWorkBudget::new();

        assert!(budget.try_acquire(LOCAL_WORK_MAX_BYTES + 1).is_none());
        assert_eq!(budget.available_count(), LOCAL_WORK_MAX_COUNT);
        assert_eq!(budget.available_bytes(), LOCAL_WORK_MAX_BYTES);
    }

    #[test]
    fn test_response_buffer_budget_prevents_concurrent_ten_megabyte_responses() {
        let budget = ResponseBufferBudget::new();
        let ten_mib = 10 * 1024 * 1024;
        let reservation = budget.try_reserve(Some(ten_mib)).unwrap();

        assert_eq!(budget.available_bytes(), 6 * 1024 * 1024);
        assert!(budget.try_reserve(Some(ten_mib)).is_none());

        drop(reservation);
        assert_eq!(budget.available_bytes(), RESPONSE_BUFFER_MAX_BYTES);
    }

    #[test]
    fn test_response_buffer_allows_one_advertised_limit_body() {
        let budget = ResponseBufferBudget::new();
        let permit = budget.try_reserve(Some(RESPONSE_BUFFER_MAX_BYTES)).unwrap();

        assert_eq!(budget.available_bytes(), 0);
        assert!(budget.try_reserve(Some(1)).is_none());

        drop(permit);
        assert_eq!(budget.available_bytes(), RESPONSE_BUFFER_MAX_BYTES);
    }

    #[test]
    fn test_response_buffer_budget_rejects_oversized_response() {
        let budget = ResponseBufferBudget::new();

        assert!(
            budget
                .try_reserve(Some(RESPONSE_BUFFER_MAX_BYTES + 1))
                .is_none()
        );
        assert_eq!(budget.available_bytes(), RESPONSE_BUFFER_MAX_BYTES);
    }

    #[test]
    fn test_unknown_response_length_cannot_grow_past_budget() {
        let budget = ResponseBufferBudget::new();
        let mut reservation = budget.try_reserve(None).unwrap();

        assert!(reservation.try_grow_to(RESPONSE_BUFFER_MAX_BYTES / 2));
        assert!(!reservation.try_grow_to(RESPONSE_BUFFER_MAX_BYTES + 1));
        assert_eq!(budget.available_bytes(), RESPONSE_BUFFER_MAX_BYTES / 2);

        drop(reservation);
        assert_eq!(budget.available_bytes(), RESPONSE_BUFFER_MAX_BYTES);
    }

    #[test]
    fn test_inbound_stream_budget_rejects_oversized_stream() {
        let oversized = INBOUND_STREAM_MAX_BYTES.saturating_add(1);
        let encoded_body_bytes = base64::encoded_len(LOCAL_MAX_TUNNEL_BODY_BYTES, false).unwrap();

        assert_eq!(INBOUND_STREAM_MAX_BYTES, 32 * 1024 * 1024);
        assert!(
            encoded_body_bytes.saturating_add(TUNNEL_MAX_FRAME_BYTES) < INBOUND_STREAM_MAX_BYTES
        );
        assert!(!inbound_stream_fits_budget(0, oversized));
        assert!(inbound_stream_fits_budget(INBOUND_STREAM_MAX_BYTES - 1, 1));
    }

    #[test]
    fn test_cancellation_classifies_execution_boundary() {
        assert_eq!(
            cancellation_classification(DELIVERY_QUEUED),
            Some(("cancelled_before_forward", "known_not_executed"))
        );
        assert_eq!(
            cancellation_classification(DELIVERY_STARTED),
            Some(("cancelled_after_forward_started", "outcome_unknown"))
        );
        assert_eq!(cancellation_classification(DELIVERY_TERMINAL), None);
    }

    #[tokio::test]
    async fn test_saturated_presentation_emits_stream_gap_after_recovery() {
        let (_token_tx, token_rx) = watch::channel("token".to_string());
        let (event_tx, mut event_rx) = mpsc::channel(2);
        let target = TargetPolicy::resolve("http://127.0.0.1:3000", false, false)
            .await
            .unwrap();
        let forwarder = TunnelForwarder::new(
            token_rx,
            "127.0.0.1".to_string(),
            3000,
            target,
            None,
            None,
            event_tx,
            false,
        );

        forwarder.emit_presentation(TunnelEvent::Connecting);
        forwarder.emit_presentation(TunnelEvent::Connected);
        forwarder.emit_presentation(TunnelEvent::Disconnected);

        assert!(matches!(event_rx.try_recv(), Ok(TunnelEvent::Connecting)));
        assert!(matches!(event_rx.try_recv(), Ok(TunnelEvent::Connected)));

        forwarder.emit_presentation(TunnelEvent::Disconnected);

        assert!(matches!(
            event_rx.try_recv(),
            Ok(TunnelEvent::StreamGap { dropped_events: 1 })
        ));
        assert!(matches!(event_rx.try_recv(), Ok(TunnelEvent::Disconnected)));
    }

    #[tokio::test]
    async fn test_closed_presentation_consumer_does_not_block_delivery_path() {
        let (_token_tx, token_rx) = watch::channel("token".to_string());
        let (event_tx, event_rx) = mpsc::channel(1);
        drop(event_rx);
        let target = TargetPolicy::resolve("http://127.0.0.1:3000", false, false)
            .await
            .unwrap();
        let forwarder = TunnelForwarder::new(
            token_rx,
            "127.0.0.1".to_string(),
            3000,
            target,
            None,
            None,
            event_tx,
            false,
        );

        forwarder.emit_presentation(TunnelEvent::Disconnected);

        assert_eq!(forwarder.presentation_drops.load(Ordering::Acquire), 1);
    }

    #[tokio::test]
    async fn test_resume_token_uses_configured_api_base_url() {
        let mut server = mockito::Server::new_async().await;
        let reconnect = server
            .mock("POST", "/api/v1/tunnel/sessions/session-123/reconnect")
            .match_header("authorization", "Bearer test-token")
            .match_header("x-organization-id", "org-123")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"data":{"session":{"id":"session-123","organization_id":"org-123","status":"active","fence":1,"created_at":"2026-07-14T20:00:00Z","updated_at":"2026-07-14T20:00:01Z","route":null},"topic":"tunnel:connect","resume_token":"short-lived-resume-token","resume_token_expires_in":300,"cursor":"opaque","ownership":"lost"}}"#,
            )
            .create_async()
            .await;
        let (_token_tx, token_rx) = watch::channel("test-token".to_string());
        let (event_tx, _event_rx) = mpsc::channel(1);
        let target = TargetPolicy::resolve("http://127.0.0.1:3000", false, false)
            .await
            .unwrap();
        let mut forwarder = TunnelForwarder::new(
            token_rx,
            "127.0.0.1".to_string(),
            3000,
            target,
            Some("org-123".to_string()),
            None,
            event_tx,
            false,
        );
        forwarder.base_url = server.url();
        *forwarder.resume_session_id.lock().unwrap() = Some("session-123".to_string());

        assert_eq!(
            forwarder.resume_token("test-token").await.unwrap(),
            Some("short-lived-resume-token".to_string())
        );
        reconnect.assert_async().await;
    }

    #[tokio::test]
    async fn test_interrupted_delivery_releases_local_work_capacity() {
        let budget = LocalWorkBudget::new();
        let permit = budget.try_acquire(10 * 1024 * 1024).unwrap();
        let mut workers = JoinSet::new();
        let abort = workers.spawn(async move {
            let _permit = permit;
            std::future::pending::<()>().await;
        });

        abort.abort();
        assert!(
            workers
                .join_next()
                .await
                .unwrap()
                .unwrap_err()
                .is_cancelled()
        );
        assert_eq!(budget.available_count(), LOCAL_WORK_MAX_COUNT);
        assert_eq!(budget.available_bytes(), LOCAL_WORK_MAX_BYTES);
    }

    #[tokio::test]
    async fn test_priority_writer_sends_control_before_queued_response() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (socket, _) = listener.accept().await.unwrap();
            let mut websocket = tokio_tungstenite::accept_async(socket).await.unwrap();
            let mut events = Vec::new();
            for _ in 0..2 {
                let message = websocket.next().await.unwrap().unwrap();
                let Message::Text(text) = message else {
                    panic!("expected a text channel message");
                };
                let message: ChannelMessage = serde_json::from_str(&text).unwrap();
                events.push(message.event);
            }
            events
        });

        let (websocket, _) = connect_async(format!("ws://{address}")).await.unwrap();
        let (write, _) = websocket.split();
        let (control_tx, control_rx) = mpsc::channel(OUTBOUND_CONTROL_MAX_COUNT);
        let (response_tx, response_rx) = mpsc::channel(OUTBOUND_RESPONSE_MAX_COUNT);
        let writer = PriorityWriter {
            control_tx,
            response_tx,
            bytes: Arc::new(Semaphore::new(OUTBOUND_MAX_BYTES)),
        };

        writer
            .response(ChannelMessage {
                topic: "cli:tunnel:test".to_string(),
                event: "response".to_string(),
                payload: serde_json::json!({}),
                reference: None,
            })
            .await
            .unwrap();
        writer
            .control(ChannelMessage {
                topic: "cli:tunnel:test".to_string(),
                event: "control".to_string(),
                payload: serde_json::json!({}),
                reference: None,
            })
            .await
            .unwrap();
        assert!(writer.available_bytes() < OUTBOUND_MAX_BYTES);

        let writer_task = tokio::spawn(run_priority_writer(write, control_rx, response_rx));
        let events = server.await.unwrap();
        assert_eq!(events, vec!["control", "response"]);

        drop(writer);
        writer_task.await.unwrap().unwrap();
    }

    #[tokio::test]
    async fn protocol_v3_reaps_completed_workers_before_sustained_ingress() {
        let limits = test_streaming_limits();
        let capacity = limits.max_active_local_forwards;
        let mut runtime = V3RelayRuntime::new("tunnel:connect".to_string(), limits).unwrap();
        let (writer, _control_rx, _response_rx) = test_priority_writer();
        let (ingress_tx, mut ingress_rx) = mpsc::channel(1);
        ingress_tx.send(0usize).await.unwrap();
        let mut peak_active_deliveries = 0;

        for wave in 0..256usize {
            for _worker in 0..capacity {
                let stream_id = Uuid::new_v4();
                let work_permit = runtime
                    .work_count
                    .clone()
                    .try_acquire_owned()
                    .expect("bounded local-forward capacity");
                let (response_tx, _response_rx) = mpsc::channel(1);
                let phase = Arc::new(AtomicU8::new(DELIVERY_TERMINAL));
                let abort = runtime.workers.spawn(async move {
                    let _work_permit = work_permit;
                    tokio::task::yield_now().await;
                    (stream_id, Ok(()))
                });

                runtime.active_deliveries.insert(
                    stream_id,
                    ActiveV3Delivery {
                        abort,
                        phase,
                        response_tx,
                    },
                );
            }

            peak_active_deliveries = peak_active_deliveries.max(runtime.active_deliveries.len());
            assert_eq!(runtime.active_deliveries.len(), capacity);
            assert_eq!(
                ingress_rx.len(),
                1,
                "ingress must remain continuously ready"
            );

            while !runtime.workers.is_empty() {
                if reap_ready_v3_workers(&mut runtime, &writer, "tunnel:connect")
                    .await
                    .unwrap()
                    == 0
                {
                    tokio::task::yield_now().await;
                }
            }

            assert!(runtime.active_deliveries.is_empty());
            assert_eq!(ingress_rx.recv().await, Some(wave));
            ingress_tx.send(wave + 1).await.unwrap();
        }

        assert_eq!(peak_active_deliveries, capacity);
        assert_eq!(runtime.work_count.available_permits(), capacity);
    }

    #[tokio::test]
    async fn protocol_v2_reaps_completed_workers_before_sustained_ingress() {
        let mut runtime = RelayRuntime::new();
        let (ingress_tx, mut ingress_rx) = mpsc::channel(1);
        ingress_tx.send(0usize).await.unwrap();
        let mut peak_active_deliveries = 0;

        for wave in 0..256usize {
            for worker in 0..LOCAL_WORK_MAX_COUNT {
                let request_id = format!("{wave}-{worker}");
                let permit = runtime
                    .work_budget
                    .try_acquire(1)
                    .expect("bounded local-forward capacity");
                let task_request_id = request_id.clone();
                let phase = Arc::new(AtomicU8::new(DELIVERY_TERMINAL));
                let abort = runtime.workers.spawn(async move {
                    let _permit = permit;
                    tokio::task::yield_now().await;
                    (task_request_id, Ok(()))
                });

                runtime
                    .active_deliveries
                    .insert(request_id, ActiveDelivery { abort, phase });
            }

            peak_active_deliveries = peak_active_deliveries.max(runtime.active_deliveries.len());
            assert_eq!(runtime.active_deliveries.len(), LOCAL_WORK_MAX_COUNT);
            assert_eq!(
                ingress_rx.len(),
                1,
                "ingress must remain continuously ready"
            );

            while !runtime.workers.is_empty() {
                if reap_ready_tunnel_workers(&mut runtime) == 0 {
                    tokio::task::yield_now().await;
                }
            }

            assert!(runtime.active_deliveries.is_empty());
            assert_eq!(ingress_rx.recv().await, Some(wave));
            ingress_tx.send(wave + 1).await.unwrap();
        }

        assert_eq!(peak_active_deliveries, LOCAL_WORK_MAX_COUNT);
        assert_eq!(runtime.work_budget.available_count(), LOCAL_WORK_MAX_COUNT);
        assert_eq!(runtime.work_budget.available_bytes(), LOCAL_WORK_MAX_BYTES);
    }

    #[tokio::test]
    async fn protocol_v2_stream_error_aborts_active_delivery_and_releases_inbound_reservation() {
        let address = "127.0.0.1:9".parse().unwrap();
        let (forwarder, mut event_rx) = forwarder_for_local_address(address).await;
        let (writer, _control_rx, _response_rx) = test_priority_writer();
        let mut runtime = RelayRuntime::new();
        let request_id = "stream-error-delivery".to_string();
        let assembler = TunnelStreamAssembler::from_start(
            &serde_json::json!({
                "version": TUNNEL_PROTOCOL_VERSION,
                "direction": "request",
                "stream_id": request_id,
                "deadline_unix_ms": unix_time_ms() + 30_000,
                "total_bytes": 17,
                "frame_count": 1,
            }),
            "request",
        )
        .unwrap();
        runtime.inbound_reserved_bytes = assembler.total_bytes;
        runtime
            .inbound_streams
            .insert(request_id.clone(), assembler);

        let task_request_id = request_id.clone();
        let phase = Arc::new(AtomicU8::new(DELIVERY_STARTED));
        let abort = runtime.workers.spawn(async move {
            std::future::pending::<()>().await;
            (task_request_id, Ok(()))
        });
        runtime
            .active_deliveries
            .insert(request_id.clone(), ActiveDelivery { abort, phase });

        let message = serde_json::to_string(&ChannelMessage {
            topic: "tunnel:connect".to_string(),
            event: "tunnel_stream_error".to_string(),
            payload: serde_json::json!({
                "stream_id": request_id,
                "code": "deadline_exceeded",
            }),
            reference: None,
        })
        .unwrap();

        forwarder
            .handle_tunnel_message(
                &message,
                &writer,
                "tunnel:connect",
                TunnelLimits::from_join_response(&serde_json::json!({})),
                &mut runtime,
            )
            .await
            .unwrap();

        assert!(!runtime.active_deliveries.contains_key(&request_id));
        assert!(!runtime.inbound_streams.contains_key(&request_id));
        assert_eq!(runtime.inbound_reserved_bytes, 0);
        assert!(matches!(
            event_rx.recv().await,
            Some(TunnelEvent::RequestFailed {
                request_id: failed_request_id,
                error,
            }) if failed_request_id == request_id && error == "deadline_exceeded"
        ));
        assert!(
            runtime
                .workers
                .join_next()
                .await
                .unwrap()
                .unwrap_err()
                .is_cancelled()
        );
    }

    fn test_priority_writer() -> (
        PriorityWriter,
        mpsc::Receiver<OutboundItem>,
        mpsc::Receiver<OutboundItem>,
    ) {
        let (control_tx, control_rx) = mpsc::channel(OUTBOUND_CONTROL_MAX_COUNT);
        let (response_tx, response_rx) = mpsc::channel(OUTBOUND_RESPONSE_MAX_COUNT);
        (
            PriorityWriter {
                control_tx,
                response_tx,
                bytes: Arc::new(Semaphore::new(OUTBOUND_MAX_BYTES)),
            },
            control_rx,
            response_rx,
        )
    }

    fn channel_message(item: OutboundItem) -> ChannelMessage {
        let Message::Text(text) = item.message else {
            panic!("expected text channel message");
        };
        serde_json::from_str(&text).unwrap()
    }

    fn protocol_v3_control(item: OutboundItem) -> tunnel_v3::ControlMessage {
        let Message::Text(text) = item.message else {
            panic!("expected protocol v3 text control");
        };
        tunnel_v3::decode_server_control(&text).unwrap()
    }

    fn protocol_v3_client_chunk(item: OutboundItem, max_chunk_bytes: usize) -> (Uuid, Vec<u8>) {
        let Message::Binary(encoded) = item.message else {
            panic!("expected protocol v3 binary response chunk");
        };
        assert!(encoded.len() >= 5);
        assert_eq!(encoded[0], 0);
        let metadata_bytes = encoded[1..5]
            .iter()
            .map(|length| usize::from(*length))
            .sum::<usize>();
        let payload_offset = 5 + metadata_bytes;
        let chunk = tunnel_v3::decode_chunk(&encoded[payload_offset..], max_chunk_bytes).unwrap();
        (chunk.stream_id, chunk.data.to_vec())
    }

    fn test_streaming_limits() -> tunnel_v3::StreamingLimits {
        tunnel_v3::StreamingLimits {
            max_request_body_bytes: 256 * 1024,
            max_response_body_bytes: 256 * 1024,
            max_start_metadata_bytes: 55_000,
            max_control_payload_bytes: 63_000,
            max_websocket_frame_bytes: TUNNEL_MAX_FRAME_BYTES,
            max_chunk_bytes: 16 * 1024,
            stream_window_bytes: 32 * 1024,
            stream_window_items: 2,
            connection_window_bytes: 64 * 1024,
            connection_window_items: 4,
            websocket_admission_bytes: 128 * 1024,
            websocket_admission_items: 32,
            service_ingress_admission_bytes: 64 * 1024,
            service_ingress_admission_items: 4,
            max_concurrent_streams: 4,
            max_active_local_forwards: 4,
            request_timeout_ms: 80_000,
        }
    }

    async fn forwarder_for_local_address(
        address: std::net::SocketAddr,
    ) -> (TunnelForwarder, mpsc::Receiver<TunnelEvent>) {
        let (_token_tx, token_rx) = watch::channel("token".to_string());
        let (event_tx, event_rx) = mpsc::channel(PRESENTATION_QUEUE_CAPACITY);
        let target = TargetPolicy::resolve(
            &format!("http://127.0.0.1:{}", address.port()),
            false,
            false,
        )
        .await
        .unwrap();
        (
            TunnelForwarder::new(
                token_rx,
                "127.0.0.1".to_string(),
                address.port(),
                target,
                None,
                None,
                event_tx,
                false,
            ),
            event_rx,
        )
    }

    #[tokio::test]
    async fn protocol_v3_early_local_response_drains_large_request_and_relays_response() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let local_server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 1024];

            while !request.windows(4).any(|window| window == b"\r\n\r\n") {
                let read = socket.read(&mut buffer).await.unwrap();
                assert!(read > 0);
                request.extend_from_slice(&buffer[..read]);
            }

            socket
                .write_all(
                    b"HTTP/1.1 413 Payload Too Large\r\nContent-Length: 5\r\nConnection: close\r\n\r\nearly",
                )
                .await
                .unwrap();
            tokio::time::sleep(Duration::from_millis(500)).await;
            socket.shutdown().await.unwrap();
        });

        let (forwarder, mut event_rx) = forwarder_for_local_address(address).await;
        let (writer, mut control_rx, mut outbound_response_rx) = test_priority_writer();
        let limits = test_streaming_limits();
        let stream_id = Uuid::new_v4();
        let request_bytes = limits.stream_window_bytes * 4;
        let request_connection = tunnel_v3::ConnectionBudget::new(
            limits.connection_window_bytes,
            limits.connection_window_items,
        )
        .unwrap();
        let (mut request_stream, request_body) = tunnel_v3::RequestStreamReceiver::new(
            stream_id,
            Some(request_bytes as u64),
            limits,
            request_connection,
        )
        .unwrap();
        let incoming = tunnel_v3::IncomingRequest {
            start: tunnel_v3::RequestStart {
                stream_id,
                deadline_unix_ms: unix_time_ms() + 10_000,
                timeout_ms: 10_000,
                method: "POST".to_string(),
                path: "/early".to_string(),
                query_string: String::new(),
                headers: vec![(
                    "content-type".to_string(),
                    "application/octet-stream".to_string(),
                )],
                content_length: Some(request_bytes as u64),
                replay: false,
            },
            body: request_body,
        };
        let phase = Arc::new(AtomicU8::new(DELIVERY_QUEUED));
        let (response_window_tx, response_window_rx) =
            mpsc::channel(limits.stream_window_items + 1);
        let work_permit = Arc::new(Semaphore::new(1)).acquire_owned().await.unwrap();
        let forward_task = tokio::spawn({
            let forwarder = forwarder.clone();
            let writer = writer.clone();
            let phase = phase.clone();
            async move {
                forwarder
                    .forward_v3_request(
                        incoming,
                        phase,
                        writer,
                        "tunnel:connect".to_string(),
                        limits,
                        tunnel_v3::ConnectionSendWindow::new(
                            limits.connection_window_bytes,
                            limits.connection_window_items,
                        ),
                        response_window_rx,
                        work_permit,
                    )
                    .await
            }
        });

        let request_chunk = vec![0xa5; limits.max_chunk_bytes];
        let mut request_hasher = Sha256::new();
        let mut offset = 0usize;
        let mut sequence = 0u32;

        while offset < request_bytes {
            let bytes = (request_bytes - offset).min(request_chunk.len());
            let encoded = tunnel_v3::encode_chunk(
                stream_id,
                sequence,
                offset as u64,
                &request_chunk[..bytes],
                limits.max_chunk_bytes,
            )
            .unwrap();
            request_stream.accept_frame(&encoded).unwrap();
            request_hasher.update(&request_chunk[..bytes]);
            offset += bytes;
            sequence += 1;

            let window = protocol_v3_control(
                tokio::time::timeout(Duration::from_secs(2), control_rx.recv())
                    .await
                    .expect("request body did not keep draining after the early response")
                    .unwrap(),
            );
            assert_eq!(window.event, "tunnel_stream_window");
            assert_eq!(window.payload["direction"], "request");
            assert_eq!(window.payload["consumed_bytes"], offset as u64);
            assert_eq!(window.payload["consumed_items"], u64::from(sequence));
        }

        let checksum = request_hasher
            .finalize()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        request_stream
            .finish(request_bytes as u64, &checksum)
            .unwrap();

        let mut response_bytes = Vec::new();
        let mut response_items = 0u64;
        let mut saw_response_start = false;
        let mut saw_response_end = false;

        while !saw_response_end {
            let item = tokio::time::timeout(Duration::from_secs(2), outbound_response_rx.recv())
                .await
                .expect("early local response was not relayed")
                .unwrap();

            match &item.message {
                Message::Text(_) => {
                    let control = protocol_v3_control(item);
                    match control.event.as_str() {
                        "tunnel_response_start" => {
                            saw_response_start = true;
                            assert_eq!(control.payload["status"], 413);
                            assert_eq!(control.payload["content_length"], 5);
                        }
                        "tunnel_response_end" => {
                            saw_response_end = true;
                            assert_eq!(control.payload["total_bytes"], 5);
                        }
                        event => panic!("unexpected protocol v3 response control: {event}"),
                    }
                }
                Message::Binary(_) => {
                    let (chunk_stream_id, data) =
                        protocol_v3_client_chunk(item, limits.max_chunk_bytes);
                    assert_eq!(chunk_stream_id, stream_id);
                    response_bytes.extend_from_slice(&data);
                    response_items += 1;
                    response_window_tx
                        .send(tunnel_v3::WindowUpdate {
                            consumed_bytes: response_bytes.len() as u64,
                            consumed_items: response_items,
                        })
                        .await
                        .unwrap();
                }
                message => panic!("unexpected protocol v3 response message: {message:?}"),
            }
        }

        assert!(saw_response_start);
        assert_eq!(response_bytes, b"early");
        tokio::time::timeout(Duration::from_secs(2), forward_task)
            .await
            .expect("early response forwarding did not complete")
            .unwrap()
            .unwrap();
        assert_eq!(phase.load(Ordering::Acquire), DELIVERY_TERMINAL);
        assert!(matches!(
            event_rx.recv().await,
            Some(TunnelEvent::RequestForwarded { status: 413, .. })
        ));
        local_server.await.unwrap();
    }

    fn test_delivery(deadline_unix_ms: u64) -> LocalDelivery {
        LocalDelivery {
            request_id: "failure-contract".to_string(),
            method: "GET".to_string(),
            path: "/".to_string(),
            query_string: String::new(),
            headers: Vec::new(),
            body: Vec::new(),
            deadline_unix_ms,
            replay: false,
        }
    }

    fn assert_delivery_failure(message: ChannelMessage, code: &str, outcome: &str) {
        assert_eq!(message.event, "tunnel_error");
        assert_eq!(message.payload["code"], code);
        assert_eq!(message.payload["outcome"], outcome);
    }

    #[tokio::test]
    async fn test_deadline_before_forward_reports_known_not_executed() {
        let address = "127.0.0.1:9".parse().unwrap();
        let (forwarder, mut event_rx) = forwarder_for_local_address(address).await;
        let (writer, mut control_rx, _response_rx) = test_priority_writer();

        tokio::time::timeout(
            Duration::from_secs(10),
            forwarder.forward_tunnel_request(
                test_delivery(unix_time_ms()),
                Arc::new(AtomicU8::new(DELIVERY_QUEUED)),
                writer,
                "cli:tunnel:test".to_string(),
                TunnelLimits::from_join_response(&serde_json::json!({})),
                ResponseBufferBudget::new(),
            ),
        )
        .await
        .expect("forwarding an expired delivery timed out")
        .unwrap();

        assert_delivery_failure(
            channel_message(
                tokio::time::timeout(Duration::from_secs(5), control_rx.recv())
                    .await
                    .expect("timed out waiting for deadline failure")
                    .unwrap(),
            ),
            "deadline_exceeded_before_forward",
            "known_not_executed",
        );
        assert!(matches!(
            tokio::time::timeout(Duration::from_secs(5), event_rx.recv())
                .await
                .expect("timed out waiting for request failure event"),
            Some(TunnelEvent::RequestFailed { request_id, .. }) if request_id == "failure-contract"
        ));
    }

    #[tokio::test]
    async fn test_local_response_limit_and_read_failures_report_unknown_outcome() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let cases: &[(&[u8], TunnelLimits, &str)] = &[
            (
                b"HTTP/1.1 200 OK\r\nX-One: 1\r\nX-Two: 2\r\nContent-Length: 0\r\n\r\n",
                TunnelLimits {
                    max_request_body_bytes: 1024,
                    max_response_body_bytes: 1024,
                    max_response_header_bytes: 1024,
                    max_response_header_items: 1,
                    ordered_response_headers: true,
                },
                "response_headers_too_large",
            ),
            (
                b"HTTP/1.1 200 OK\r\nX-Large: 1234567890\r\nContent-Length: 0\r\n\r\n",
                TunnelLimits {
                    max_request_body_bytes: 1024,
                    max_response_body_bytes: 1024,
                    max_response_header_bytes: 8,
                    max_response_header_items: TUNNEL_MAX_RESPONSE_HEADERS,
                    ordered_response_headers: true,
                },
                "response_headers_too_large",
            ),
            (
                b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nabc\r\n3\r\ndef\r\n0\r\n\r\n",
                TunnelLimits {
                    max_request_body_bytes: 1024,
                    max_response_body_bytes: 4,
                    max_response_header_bytes: 1024,
                    max_response_header_items: TUNNEL_MAX_RESPONSE_HEADERS,
                    ordered_response_headers: true,
                },
                "response_body_too_large",
            ),
            (
                b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\n\r\nshort",
                TunnelLimits {
                    max_request_body_bytes: 1024,
                    max_response_body_bytes: 1024,
                    max_response_header_bytes: 1024,
                    max_response_header_items: TUNNEL_MAX_RESPONSE_HEADERS,
                    ordered_response_headers: true,
                },
                "response_read_failed",
            ),
        ];

        for (response, limits, expected_code) in cases {
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let response = response.to_vec();
            let server = tokio::spawn(async move {
                let (mut socket, _) = listener.accept().await.unwrap();
                let mut request = [0_u8; 1024];
                let _bytes_read = socket.read(&mut request).await.unwrap();
                socket.write_all(&response).await.unwrap();
            });
            let (forwarder, _event_rx) = forwarder_for_local_address(address).await;
            let (writer, mut control_rx, _response_rx) = test_priority_writer();

            tokio::time::timeout(
                Duration::from_secs(10),
                forwarder.forward_tunnel_request(
                    test_delivery(unix_time_ms() + 30_000),
                    Arc::new(AtomicU8::new(DELIVERY_QUEUED)),
                    writer,
                    "cli:tunnel:test".to_string(),
                    *limits,
                    ResponseBufferBudget::new(),
                ),
            )
            .await
            .expect("forwarding a malformed local response timed out")
            .unwrap();

            assert_delivery_failure(
                channel_message(
                    tokio::time::timeout(Duration::from_secs(5), control_rx.recv())
                        .await
                        .expect("timed out waiting for local response failure")
                        .unwrap(),
                ),
                expected_code,
                "outcome_unknown",
            );
            tokio::time::timeout(Duration::from_secs(5), server)
                .await
                .expect("local response server timed out")
                .unwrap();
        }
    }

    #[tokio::test]
    async fn test_response_buffer_overload_reports_unknown_outcome() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _bytes_read = socket.read(&mut request).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .unwrap();
        });
        let budget = ResponseBufferBudget::new();
        let _reservation = budget.try_reserve(Some(RESPONSE_BUFFER_MAX_BYTES)).unwrap();
        let (forwarder, _event_rx) = forwarder_for_local_address(address).await;
        let (writer, mut control_rx, _response_rx) = test_priority_writer();

        tokio::time::timeout(
            Duration::from_secs(10),
            forwarder.forward_tunnel_request(
                test_delivery(unix_time_ms() + 30_000),
                Arc::new(AtomicU8::new(DELIVERY_QUEUED)),
                writer,
                "cli:tunnel:test".to_string(),
                TunnelLimits::from_join_response(&serde_json::json!({})),
                budget,
            ),
        )
        .await
        .expect("forwarding an overloaded response timed out")
        .unwrap();

        assert_delivery_failure(
            channel_message(
                tokio::time::timeout(Duration::from_secs(5), control_rx.recv())
                    .await
                    .expect("timed out waiting for response overload failure")
                    .unwrap(),
            ),
            "relay_response_overloaded",
            "outcome_unknown",
        );
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("local response server timed out")
            .unwrap();
    }

    #[tokio::test]
    async fn test_response_writer_failure_after_delivery_started_reports_unknown_outcome() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut socket, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 1024];
            let _bytes_read = socket.read(&mut request).await.unwrap();
            socket
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                .await
                .unwrap();
        });
        let (forwarder, _event_rx) = forwarder_for_local_address(address).await;
        let (writer, mut control_rx, response_rx) = test_priority_writer();
        drop(response_rx);

        tokio::time::timeout(
            Duration::from_secs(10),
            forwarder.forward_tunnel_request(
                test_delivery(unix_time_ms() + 30_000),
                Arc::new(AtomicU8::new(DELIVERY_QUEUED)),
                writer,
                "cli:tunnel:test".to_string(),
                TunnelLimits::from_join_response(&serde_json::json!({})),
                ResponseBufferBudget::new(),
            ),
        )
        .await
        .expect("forwarding with a closed response writer timed out")
        .unwrap();

        assert_delivery_failure(
            channel_message(
                tokio::time::timeout(Duration::from_secs(5), control_rx.recv())
                    .await
                    .expect("timed out waiting for response delivery failure")
                    .unwrap(),
            ),
            "response_delivery_failed",
            "outcome_unknown",
        );
        tokio::time::timeout(Duration::from_secs(5), server)
            .await
            .expect("local response server timed out")
            .unwrap();
    }

    #[tokio::test]
    async fn test_slow_local_handler_does_not_delay_fast_delivery() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio::sync::oneshot;

        let local_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let local_address = local_listener.local_addr().unwrap();
        let (slow_started_tx, slow_started_rx) = oneshot::channel();
        let release_slow = Arc::new(tokio::sync::Notify::new());
        let server_release_slow = Arc::clone(&release_slow);
        let local_server = tokio::spawn(async move {
            let mut slow_started_tx = Some(slow_started_tx);
            let mut handlers = JoinSet::new();
            for _ in 0..2 {
                let (mut socket, _) = local_listener.accept().await.unwrap();
                let mut request = [0_u8; 1024];
                let read = socket.read(&mut request).await.unwrap();
                let slow = String::from_utf8_lossy(&request[..read]).contains("GET /slow ");
                if slow && let Some(sender) = slow_started_tx.take() {
                    let _ = sender.send(());
                }
                let release_slow = Arc::clone(&server_release_slow);
                handlers.spawn(async move {
                    if slow {
                        release_slow.notified().await;
                    }
                    socket
                        .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok")
                        .await
                        .unwrap();
                });
            }
            while handlers.join_next().await.is_some() {}
        });

        let websocket_listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let websocket_address = websocket_listener.local_addr().unwrap();
        let websocket_server = tokio::spawn(async move {
            let (socket, _) = websocket_listener.accept().await.unwrap();
            let mut websocket = tokio_tungstenite::accept_async(socket).await.unwrap();
            while websocket.next().await.is_some() {}
        });
        let (websocket, _) = connect_async(format!("ws://{websocket_address}"))
            .await
            .unwrap();
        let (write, read) = websocket.split();
        let (writer, writer_task) = PriorityWriter::spawn(write);

        let (_token_tx, token_rx) = watch::channel("token".to_string());
        let (event_tx, mut event_rx) = mpsc::channel(PRESENTATION_QUEUE_CAPACITY);
        let target = TargetPolicy::resolve(
            &format!("http://127.0.0.1:{}", local_address.port()),
            false,
            false,
        )
        .await
        .unwrap();
        let forwarder = TunnelForwarder::new(
            token_rx,
            "127.0.0.1".to_string(),
            local_address.port(),
            target,
            None,
            None,
            event_tx,
            false,
        );
        let limits = TunnelLimits {
            max_request_body_bytes: 1024,
            max_response_body_bytes: 1024,
            max_response_header_bytes: 1024,
            max_response_header_items: TUNNEL_MAX_RESPONSE_HEADERS,
            ordered_response_headers: true,
        };
        let response_budget = ResponseBufferBudget::new();
        let delivery = |request_id: &str, path: &str| LocalDelivery {
            request_id: request_id.to_string(),
            method: "GET".to_string(),
            path: path.to_string(),
            query_string: String::new(),
            headers: Vec::new(),
            body: Vec::new(),
            deadline_unix_ms: unix_time_ms() + 10_000,
            replay: false,
        };

        let slow = tokio::spawn({
            let forwarder = forwarder.clone();
            let writer = writer.clone();
            let response_budget = response_budget.clone();
            async move {
                forwarder
                    .forward_tunnel_request(
                        delivery("slow", "/slow"),
                        Arc::new(AtomicU8::new(DELIVERY_QUEUED)),
                        writer,
                        "cli:tunnel:test".to_string(),
                        limits,
                        response_budget,
                    )
                    .await
            }
        });
        slow_started_rx.await.unwrap();

        let fast = tokio::spawn({
            let forwarder = forwarder.clone();
            let writer = writer.clone();
            let response_budget = response_budget.clone();
            async move {
                forwarder
                    .forward_tunnel_request(
                        delivery("fast", "/fast"),
                        Arc::new(AtomicU8::new(DELIVERY_QUEUED)),
                        writer,
                        "cli:tunnel:test".to_string(),
                        limits,
                        response_budget,
                    )
                    .await
            }
        });

        let first = tokio::time::timeout(Duration::from_secs(1), event_rx.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(
            first,
            TunnelEvent::RequestForwarded { request_id, .. } if request_id == "fast"
        ));
        release_slow.notify_one();

        fast.await.unwrap().unwrap();
        slow.await.unwrap().unwrap();
        local_server.await.unwrap();
        drop(writer);
        drop(read);
        writer_task.await.unwrap().unwrap();
        websocket_server.abort();
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

    #[test]
    fn test_decode_request_body_limited_rejects_oversized_raw_body_before_copying() {
        let error = decode_request_body_limited("raw", "12345", 4).unwrap_err();

        assert!(error.to_string().contains("5 > 4 bytes"));
    }

    #[test]
    fn test_decode_request_body_limited_rejects_oversized_base64_before_decoding() {
        let encoded = URL_SAFE_NO_PAD.encode(b"12345");

        let error = decode_request_body_limited("base64", &encoded, 4).unwrap_err();

        assert!(error.to_string().contains("encoded body is too large"));
    }
}
