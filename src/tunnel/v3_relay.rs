//! Protocol v3 delivery runtime: response windows and request body bridging.

use anyhow::{Context, Result, anyhow};
use std::collections::HashMap;
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
use tokio::sync::{Semaphore, mpsc, watch};
use tokio::task::{AbortHandle, JoinSet};
use tokio_tungstenite::tungstenite::Bytes;
use tracing::error;
use uuid::Uuid;

use crate::tunnel::limits::LOCAL_WORK_MAX_COUNT;
use crate::tunnel::preview::bounded_control_neutral_log_text;
use crate::tunnel::relay::cancellation_classification;
use crate::tunnel::writer::PriorityWriter;
use crate::tunnel_v3;

pub(crate) struct ActiveV3Delivery {
    pub(crate) abort: AbortHandle,
    pub(crate) phase: Arc<AtomicU8>,
    pub(crate) response_tx: mpsc::Sender<tunnel_v3::WindowUpdate>,
}

pub(crate) struct V3RelayRuntime {
    pub(crate) requests: tunnel_v3::RequestStreams,
    pub(crate) response_window: tunnel_v3::ConnectionSendWindow,
    pub(crate) work_count: Arc<Semaphore>,
    pub(crate) workers: JoinSet<(Uuid, Result<()>)>,
    pub(crate) active_deliveries: HashMap<Uuid, ActiveV3Delivery>,
    pub(crate) auto_drain_requested: bool,
}

impl V3RelayRuntime {
    pub(crate) fn new(topic: String, limits: tunnel_v3::StreamingLimits) -> Result<Self> {
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

pub(crate) async fn complete_v3_worker(
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

pub(crate) async fn reap_ready_v3_workers(
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

pub(crate) fn v3_control_message(
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

pub(crate) fn v3_abort_message(
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

pub(crate) fn v3_payload_stream_id(payload: &serde_json::Value) -> Result<Uuid> {
    payload
        .get("stream_id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow!("Protocol v3 event is missing stream_id"))?
        .parse()
        .context("Protocol v3 event has an invalid stream_id")
}

pub(crate) fn admit_v3_response_window(
    sender: &mpsc::Sender<tunnel_v3::WindowUpdate>,
    update: tunnel_v3::WindowUpdate,
) -> std::result::Result<(), &'static str> {
    match sender.try_send(update) {
        Ok(()) => Ok(()),
        Err(mpsc::error::TrySendError::Full(_)) => Err("response_window_overflow"),
        Err(mpsc::error::TrySendError::Closed(_)) => Err("response_window_receiver_closed"),
    }
}

pub(crate) async fn enqueue_v3_response_chunk(
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

pub(crate) async fn acknowledge_v3_response_window(
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

pub(crate) struct V3RequestBodyBridge {
    pub(crate) request_body: reqwest::Body,
    pub(crate) response_started: watch::Sender<bool>,
    pub(crate) pump: tokio::task::JoinHandle<Result<()>>,
}

pub(crate) fn spawn_v3_request_body_bridge(
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

pub(crate) async fn run_v3_request_body_pump(
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
