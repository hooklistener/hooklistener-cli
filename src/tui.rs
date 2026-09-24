//! Interactive TUI event loop, tunnel forwarder tasks, and terminal setup and restore.

use anyhow::Result;
use crossterm::{
    cursor::Show,
    event::{self, Event, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io;
use std::time::Duration;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
};
use tracing::error;

use crate::app::{App, AppState, FeedbackKind};
use crate::tunnel::TunnelEvent;
use crate::{api, app, target_policy, tunnel, ui, updater};

#[allow(clippy::too_many_arguments)]
pub fn spawn_tunnel_forwarder_manager(
    access_token_rx: watch::Receiver<String>,
    host: String,
    port: u16,
    org: Option<String>,
    slug: Option<String>,
    target: target_policy::TargetPolicy,
    event_tx: mpsc::Sender<TunnelEvent>,
    replay_buffered: bool,
    anonymous_route: Option<(String, String, Option<api::RelayTicket>)>,
) -> mpsc::UnboundedSender<()> {
    let (reconnect_tx, mut reconnect_rx) = mpsc::unbounded_channel::<()>();

    tokio::spawn(async move {
        let reconnect_anonymous_route = anonymous_route
            .as_ref()
            .map(|(id, token, _ticket)| (id.clone(), token.clone(), None));
        let mut worker = tokio::spawn(run_tunnel_forwarder_connection(
            access_token_rx.clone(),
            host.clone(),
            port,
            org.clone(),
            slug.clone(),
            target.clone(),
            event_tx.clone(),
            replay_buffered,
            anonymous_route.clone(),
        ));

        while reconnect_rx.recv().await.is_some() {
            worker.abort();
            let _ = worker.await;

            // Collapse bursty manual reconnect presses into a single restart.
            while reconnect_rx.try_recv().is_ok() {}

            worker = tokio::spawn(run_tunnel_forwarder_connection(
                access_token_rx.clone(),
                host.clone(),
                port,
                org.clone(),
                slug.clone(),
                target.clone(),
                event_tx.clone(),
                replay_buffered,
                reconnect_anonymous_route.clone(),
            ));
        }

        worker.abort();
        let _ = worker.await;
    });

    reconnect_tx
}

#[allow(clippy::too_many_arguments)]
pub async fn run_tunnel_forwarder_connection(
    access_token_rx: watch::Receiver<String>,
    host: String,
    port: u16,
    org: Option<String>,
    slug: Option<String>,
    target: target_policy::TargetPolicy,
    event_tx: mpsc::Sender<TunnelEvent>,
    replay_buffered: bool,
    anonymous_route: Option<(String, String, Option<api::RelayTicket>)>,
) {
    let mut tunnel_forwarder = tunnel::TunnelForwarder::new(
        access_token_rx,
        host,
        port,
        target,
        org,
        slug,
        event_tx,
        replay_buffered,
    );

    if let Some((route_id, route_token, initial_ticket)) = anonymous_route {
        tunnel_forwarder =
            tunnel_forwarder.with_anonymous_route(route_id, route_token, initial_ticket);
    }

    if let Err(e) = tunnel_forwarder
        .connect_with_reconnect(tunnel::ReconnectConfig::default())
        .await
    {
        error!("Tunnel forwarder error: {}", e);
    }
}

pub fn spawn_tunnel_replay(
    replay_request: app::TunnelReplayRequest,
    event_tx: mpsc::Sender<TunnelEvent>,
) {
    tokio::spawn(async move {
        let request_id = replay_request.request_id.clone();
        let replay_result = replay_request.send().await;
        let event = match replay_result {
            Ok(outcome) => TunnelEvent::ReplayCompleted {
                request_id,
                status: outcome.status,
                duration_ms: outcome.duration_ms,
            },
            Err(error) => TunnelEvent::ReplayFailed {
                request_id,
                error: error.to_string(),
            },
        };
        let _ = event_tx.send(event).await;
    });
}

pub async fn run_app<B: ratatui::backend::Backend + Send>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    mut tunnel_rx: mpsc::Receiver<TunnelEvent>,
    tunnel_reconnect_tx: Option<mpsc::UnboundedSender<()>>,
    tunnel_event_tx: Option<mpsc::Sender<TunnelEvent>>,
    mut logo_rx: Option<watch::Receiver<String>>,
    update_handle: &mut Option<JoinHandle<Option<String>>>,
) -> Result<()>
where
    <B as ratatui::backend::Backend>::Error: std::error::Error + Send + Sync + 'static,
{
    // Ensure proper terminal cleanup on any exit
    let _cleanup = TerminalCleanup;

    loop {
        if update_handle.as_ref().is_some_and(JoinHandle::is_finished)
            && let Some(handle) = update_handle.take()
            && let Ok(Some(new_version)) = handle.await
        {
            updater::persist_check_result(Some(&new_version));
            app.available_update = Some(new_version);
        }

        if let Some(logo_rx) = logo_rx.as_mut() {
            let should_update_logo =
                app.logo_frame.is_none() || logo_rx.has_changed().unwrap_or(false);
            if should_update_logo {
                app.logo_frame = Some(logo_rx.borrow_and_update().clone());
            }
        }

        let terminal_width = terminal.size()?.width;
        app.set_detail_content_width(terminal_width.saturating_sub(4) as usize);
        terminal.draw(|frame| ui::draw(frame, app))?;

        // Update animations
        app.tick();

        if app.should_quit {
            break;
        }

        // Handle tunnel events
        while let Ok(event) = tunnel_rx.try_recv() {
            match event {
                TunnelEvent::Connecting => {
                    // Update UI to show connecting state
                }
                TunnelEvent::Connected => {
                    app.listening_connected = true;
                    app.listening_error = None;
                    app.tunnel_connected = true;
                    app.tunnel_connected_at = Some(std::time::Instant::now());
                }
                TunnelEvent::TunnelEstablished {
                    subdomain,
                    tunnel_id,
                    is_static,
                } => {
                    app.tunnel_subdomain = Some(subdomain);
                    app.tunnel_id = Some(tunnel_id);
                    app.tunnel_is_static = is_static;
                    app.tunnel_connected = true;
                    app.tunnel_connected_at = Some(std::time::Instant::now());
                }
                TunnelEvent::ConnectionError(err) => {
                    app.listening_connected = false;
                    app.listening_error = Some(err.clone());
                    app.tunnel_connected = false;
                    app.tunnel_error = Some(err);
                }
                TunnelEvent::Disconnected => {
                    app.listening_connected = false;
                    app.tunnel_connected = false;
                }
                TunnelEvent::WebhookReceived(request) => {
                    app.push_listening_request(*request);
                }
                TunnelEvent::RequestReceived {
                    request_id,
                    method,
                    path,
                    headers,
                    body,
                    query_string,
                    replay: _,
                } => {
                    use std::time::Instant;
                    let tunnel_request = app::TunnelRequest {
                        request_id,
                        method,
                        path,
                        received_at: Instant::now(),
                        status: None,
                        completed_at: None,
                        error: None,
                        headers,
                        body: app::truncate_body(body),
                        query_string,
                        response_headers: None,
                        response_body: None,
                        pinned: false,
                    };
                    app.push_tunnel_request(tunnel_request);
                    app.tunnel_stats.total += 1;
                }
                TunnelEvent::RequestForwarded {
                    request_id,
                    status,
                    duration_ms,
                    response_headers,
                    response_body,
                } => {
                    // Update the request in the list
                    if let Some(req) = app
                        .tunnel_requests
                        .iter_mut()
                        .find(|r| r.request_id == request_id)
                    {
                        req.status = Some(status);
                        req.completed_at = Some(std::time::Instant::now());
                        req.response_headers = Some(response_headers);
                        req.response_body = app::truncate_body(response_body);
                    }
                    app.tunnel_stats.record_response(status, duration_ms);
                }
                TunnelEvent::RequestFailed { request_id, error } => {
                    // Update the request in the list
                    if let Some(req) = app
                        .tunnel_requests
                        .iter_mut()
                        .find(|r| r.request_id == request_id)
                    {
                        req.error = Some(error);
                        req.completed_at = Some(std::time::Instant::now());
                    }
                    app.tunnel_stats.failed += 1;
                }
                TunnelEvent::StreamGap { dropped_events } => {
                    app.set_tunnel_feedback(
                        FeedbackKind::Warning,
                        format!(
                            "Presentation skipped {dropped_events} event(s); relay delivery is unaffected"
                        ),
                    );
                }
                TunnelEvent::ReplayCompleted {
                    request_id,
                    status,
                    duration_ms,
                } => {
                    app.set_tunnel_feedback(
                        FeedbackKind::Success,
                        format!("Replayed {request_id} {status} {duration_ms}ms"),
                    );
                }
                TunnelEvent::ReplayFailed { request_id, error } => {
                    app.set_tunnel_feedback(
                        FeedbackKind::Error,
                        format!("Replay {request_id} failed: {error}"),
                    );
                }
                TunnelEvent::BufferedSummary {
                    count,
                    oldest_captured_at: _,
                } => {
                    if count > 0 {
                        app.set_tunnel_feedback(
                            FeedbackKind::Info,
                            format!(
                                "{count} request{} buffered while offline",
                                if count == 1 { "" } else { "s" }
                            ),
                        );
                    }
                }
                TunnelEvent::BufferedReplayed { capture_id, status } => {
                    app.set_tunnel_feedback(
                        FeedbackKind::Success,
                        format!("Replayed buffered {capture_id} → {status}"),
                    );
                }
                TunnelEvent::BufferedReplayFailed { capture_id, reason } => {
                    app.set_tunnel_feedback(
                        FeedbackKind::Error,
                        format!("Buffered replay {capture_id} failed: {reason}"),
                    );
                }
                TunnelEvent::ForwardSuccess { .. } => {
                    app.listening_stats.successful_forwards += 1;
                }
                TunnelEvent::ForwardError { .. } => {
                    app.listening_stats.failed_forwards += 1;
                }
                TunnelEvent::Reconnecting {
                    attempt,
                    max_attempts,
                    next_retry_in_secs,
                } => {
                    let msg = format!(
                        "Reconnecting (attempt {}/{})... next retry in {}s",
                        attempt, max_attempts, next_retry_in_secs
                    );
                    app.listening_connected = false;
                    app.listening_error = Some(msg.clone());
                    app.tunnel_connected = false;
                    app.tunnel_error = Some(msg);
                }
                TunnelEvent::ReconnectFailed { reason } => {
                    let msg = format!("Connection lost: {}", reason);
                    app.listening_connected = false;
                    app.listening_error = Some(msg.clone());
                    app.tunnel_connected = false;
                    app.tunnel_error = Some(msg);
                }
            }
        }

        // Handle async states that don't require user input
        if matches!(app.state, AppState::ForwardingRequest) {
            app.forward_request().await?;
            continue;
        }

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            app.handle_key_event(key)?;

            if app.take_tunnel_reconnect_request()
                && let Some(tx) = tunnel_reconnect_tx.as_ref()
                && tx.send(()).is_err()
            {
                app.tunnel_error = Some("Failed to request tunnel reconnect".to_string());
            }

            if let Some(replay_request) = app.take_tunnel_replay_request() {
                if let Some(tx) = tunnel_event_tx.as_ref() {
                    spawn_tunnel_replay(replay_request, tx.clone());
                } else {
                    app.set_tunnel_feedback(FeedbackKind::Warning, "Replay unavailable");
                }
            }

            if matches!(app.state, AppState::ForwardingRequest) {
                app.forward_request().await?;
            }
        }
    }

    Ok(())
}

pub fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut cleanup = TerminalInitCleanup::raw_mode_enabled();
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    cleanup.alternate_screen = true;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    cleanup.disarm();
    Ok(terminal)
}

#[derive(Debug, Default, PartialEq, Eq)]
pub struct TerminalInitCleanup {
    pub raw_mode: bool,
    pub alternate_screen: bool,
}

impl TerminalInitCleanup {
    pub fn raw_mode_enabled() -> Self {
        Self {
            raw_mode: true,
            alternate_screen: false,
        }
    }
    pub fn disarm(&mut self) {
        self.raw_mode = false;
        self.alternate_screen = false;
    }
}

impl Drop for TerminalInitCleanup {
    fn drop(&mut self) {
        if self.alternate_screen {
            let _ = execute!(io::stdout(), LeaveAlternateScreen, Show);
        }
        if self.raw_mode {
            let _ = disable_raw_mode();
        }
    }
}

pub struct TerminalCleanup;

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        // Ensure terminal is always restored, even on panic
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        let _ = execute!(io::stdout(), Show);
    }
}

pub fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}
