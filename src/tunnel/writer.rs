//! Priority WebSocket writer: control frames go out ahead of response data.

use anyhow::{Result, anyhow};
use futures_util::SinkExt;
use std::sync::Arc;
use tokio::sync::{OwnedSemaphorePermit, Semaphore, mpsc};
use tokio_tungstenite::tungstenite::{Bytes, Message};

use crate::tunnel::WsWrite;
use crate::tunnel::framing::ChannelMessage;
use crate::tunnel::limits::{
    OUTBOUND_CONTROL_MAX_COUNT, OUTBOUND_MAX_BYTES, OUTBOUND_RESPONSE_MAX_COUNT,
    TUNNEL_MAX_FRAME_BYTES,
};
use crate::tunnel_v3;

pub(crate) struct OutboundItem {
    pub(crate) message: Message,
    _bytes: OwnedSemaphorePermit,
}

#[derive(Clone)]
pub(crate) struct PriorityWriter {
    pub(crate) control_tx: mpsc::Sender<OutboundItem>,
    pub(crate) response_tx: mpsc::Sender<OutboundItem>,
    pub(crate) bytes: Arc<Semaphore>,
}

impl PriorityWriter {
    pub(crate) fn spawn(write: WsWrite) -> (Self, tokio::task::JoinHandle<Result<()>>) {
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

    pub(crate) async fn control(&self, message: ChannelMessage) -> Result<()> {
        self.enqueue_channel(message, true).await
    }

    pub(crate) async fn response(&self, message: ChannelMessage) -> Result<()> {
        self.enqueue_channel(message, false).await
    }

    pub(crate) async fn control_v3(&self, message: tunnel_v3::ControlMessage) -> Result<()> {
        self.enqueue_v3_control(message, true).await
    }

    pub(crate) fn try_control_v3(&self, message: tunnel_v3::ControlMessage) -> Result<()> {
        let encoded = tunnel_v3::encode_client_control(&message, TUNNEL_MAX_FRAME_BYTES)?;
        self.try_enqueue(Message::Text(encoded.into()), true)
    }

    pub(crate) async fn response_control_v3(
        &self,
        message: tunnel_v3::ControlMessage,
    ) -> Result<()> {
        self.enqueue_v3_control(message, false).await
    }

    pub(crate) async fn binary_response(&self, message: Vec<u8>) -> Result<()> {
        self.enqueue(Message::Binary(message.into()), false).await
    }

    pub(crate) async fn pong(&self, data: Bytes) -> Result<()> {
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
    pub(crate) fn available_bytes(&self) -> usize {
        self.bytes.available_permits()
    }
}

pub(crate) async fn run_priority_writer(
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
