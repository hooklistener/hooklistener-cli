use super::forwarder::*;
use super::framing::*;
use super::http::*;
use super::limits::*;
use super::preview::*;
use super::relay::*;
use super::v3_relay::*;
use super::writer::*;
use super::*;
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use futures_util::StreamExt;
use std::collections::HashMap;
use std::io::Cursor;
use std::sync::{
    Arc,
    atomic::{AtomicU8, Ordering},
};
use std::time::Duration;
use tokio::sync::{Semaphore, mpsc, watch};
use tokio::task::JoinSet;
use tokio_tungstenite::{connect_async, tungstenite::Message};
use uuid::Uuid;

use crate::target_policy::TargetPolicy;
use crate::tunnel_v3;
use flate2::{Compression, write::DeflateEncoder, write::GzEncoder, write::ZlibEncoder};
use sha2::{Digest, Sha256};
use std::io::Write;

mod client;
mod delivery;
mod framing;
mod http;
mod limits;
mod preview;
mod relay;

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
