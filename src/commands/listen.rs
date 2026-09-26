//! `listen` command: stream an endpoint to a local target in the TUI or as JSON lines.

use anyhow::{Result, anyhow};
use std::time::Duration;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tracing::error;

use crate::api::ApiClient;
use crate::app::{App, AppState};
use crate::commands::{WorkerCompletion, supervise_json_worker};
use crate::credentials::{ensure_valid_token, refreshed_access_token_rx, resolve_tunnel_org};
use crate::receipts::{
    LISTEN_EVENT_SCHEMA, command_event_receipt, listen_event_receipt, listen_session_resource_uri,
    listen_started_receipt, reconnect_failure_reason,
};
use crate::render::print_json_line;
use crate::tui::{restore_terminal, run_app, setup_terminal};
use crate::tunnel::TunnelEvent;
use crate::{api, config, logo, output, target_policy, tunnel};

pub async fn execute(
    endpoint_slug: String,
    target: String,
    ws_url: String,
    allow_non_loopback: bool,
    insecure_tls: bool,
    json: bool,
    update_handle: &mut Option<JoinHandle<Option<String>>>,
) -> Result<()> {
    let mut config = config::Config::load()?;
    let access_token = ensure_valid_token(&mut config).await?;
    let selected_organization_id = resolve_tunnel_org(None, &config);
    let access_token_rx = refreshed_access_token_rx(access_token, config);

    if json {
        run_listen_json(
            access_token_rx,
            endpoint_slug,
            target,
            ws_url,
            selected_organization_id,
            allow_non_loopback,
            insecure_tls,
        )
        .await?;
    } else {
        let target_policy =
            target_policy::TargetPolicy::resolve(&target, allow_non_loopback, insecure_tls).await?;
        let target = target_policy.display_url();

        // Setup TUI for listen command
        let mut terminal = setup_terminal()?;
        let mut app = App::new()?;
        app.monochrome = !output::styles_enabled();

        // Set app state to listening
        app.state = AppState::Listening;
        app.listening_endpoint = endpoint_slug.clone();
        app.listening_target = target.clone();

        // Create channel for tunnel events
        let (event_tx, event_rx) = mpsc::channel(tunnel::PRESENTATION_QUEUE_CAPACITY);

        // Create and spawn tunnel client
        let tunnel_client = tunnel::TunnelClient::new(
            access_token_rx,
            endpoint_slug.clone(),
            target_policy,
            Some(ws_url),
            event_tx,
        );

        tokio::spawn(async move {
            if let Err(e) = tunnel_client
                .connect_with_reconnect(tunnel::ReconnectConfig::default())
                .await
            {
                error!("Tunnel client error: {}", e);
            }
        });

        let logo_rx = logo::spawn_logo_animation();
        let res = run_app(
            &mut terminal,
            &mut app,
            event_rx,
            None,
            None,
            Some(logo_rx),
            update_handle,
        )
        .await;

        restore_terminal(&mut terminal)?;

        if let Err(err) = res {
            error!(error = %err, "Application terminated with error");
            return Err(err);
        }
    }
    Ok(())
}

pub fn effective_listen_ws_url(ws_url: Option<&str>) -> Result<String> {
    let ws_url = ws_url
        .map(str::to_string)
        .or_else(|| std::env::var("HOOKLISTENER_WS_URL").ok())
        .unwrap_or_else(|| "wss://api.hooklistener.com".to_string());
    api::validate_websocket_base_url(&ws_url)?;
    Ok(ws_url)
}

pub async fn resolve_listen_endpoint(
    access_token: &str,
    organization_id: Option<String>,
    endpoint_slug: &str,
) -> Option<api::DebugEndpointSummary> {
    let client = ApiClient::with_organization(access_token.to_string(), organization_id).ok()?;
    let endpoints = tokio::time::timeout(Duration::from_secs(2), client.list_endpoints())
        .await
        .ok()?
        .ok()?;

    endpoints
        .into_iter()
        .find(|endpoint| endpoint.slug == endpoint_slug || endpoint.id == endpoint_slug)
}

pub async fn stream_listen_json_events(
    mut event_rx: mpsc::Receiver<TunnelEvent>,
    endpoint_slug: &str,
    target_url: &str,
    endpoint: Option<&api::DebugEndpointSummary>,
) -> Result<()> {
    let mut sequence = 1_u64;
    loop {
        tokio::select! {
            maybe_event = event_rx.recv() => {
                let Some(event) = maybe_event else {
                    return Ok(());
                };
                let failure_reason = reconnect_failure_reason(&event);

                let mut receipt =
                    listen_event_receipt(&event, endpoint_slug, target_url, endpoint);
                receipt["sequence"] = serde_json::json!(sequence);
                sequence = sequence.saturating_add(1);
                print_json_line(&receipt)?;

                if let Some(reason) = failure_reason {
                    return Err(anyhow!("Connection lost: {reason}"));
                }
            }
            signal = tokio::signal::ctrl_c() => {
                signal?;
                let mut receipt = command_event_receipt(
                    LISTEN_EVENT_SCHEMA,
                    "listen",
                    "listen_endpoint",
                    "stopped",
                    "stopped",
                );
                receipt["sequence"] = serde_json::json!(sequence);
                receipt["endpoint_slug"] = serde_json::json!(endpoint_slug);
                receipt["target_url"] = serde_json::json!(target_url);
                receipt["resource_uri"] =
                    serde_json::json!(listen_session_resource_uri(endpoint_slug));
                print_json_line(&receipt)?;
                return Ok(());
            }
        }
    }
}

pub async fn run_listen_json(
    access_token_rx: watch::Receiver<String>,
    endpoint_slug: String,
    target_url: String,
    ws_url: String,
    organization_id: Option<String>,
    allow_non_loopback: bool,
    insecure_tls: bool,
) -> Result<()> {
    let target =
        target_policy::TargetPolicy::resolve(&target_url, allow_non_loopback, insecure_tls).await?;
    let target_url = target.display_url();
    let access_token = access_token_rx.borrow().clone();
    let endpoint = resolve_listen_endpoint(&access_token, organization_id, &endpoint_slug).await;

    print_json_line(&listen_started_receipt(
        &endpoint_slug,
        &target_url,
        &ws_url,
        endpoint.as_ref(),
    ))?;

    let (event_tx, event_rx) = mpsc::channel(tunnel::PRESENTATION_QUEUE_CAPACITY);
    let tunnel_client = tunnel::TunnelClient::new(
        access_token_rx,
        endpoint_slug.clone(),
        target,
        Some(ws_url),
        event_tx,
    );

    let worker = tokio::spawn(async move {
        if let Err(e) = tunnel_client
            .connect_with_reconnect(tunnel::ReconnectConfig::default())
            .await
        {
            error!("Tunnel client error: {}", e);
        }
    });

    match supervise_json_worker(
        worker,
        stream_listen_json_events(event_rx, &endpoint_slug, &target_url, endpoint.as_ref()),
        std::future::pending(),
    )
    .await?
    {
        WorkerCompletion::Stream(result) => result,
        WorkerCompletion::Shutdown => unreachable!("shutdown is handled by the event stream"),
    }
}
