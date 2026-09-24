//! `tunnel` commands: activation and the tunnel session lifecycle.

use anyhow::{Result, anyhow};
use clap::{Args, Subcommand};
use std::time::Duration;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::sleep,
};

use crate::api::ApiClient;
use crate::app::{App, AppState};
use crate::cli::{MILLIS_PER_SECOND, parse_duration, resolve_millis_flag};
use crate::commands::{WorkerCompletion, supervise_json_worker};
use crate::credentials::{ensure_valid_token, refreshed_access_token_rx, require_organization};
use crate::output::Stylize;
use crate::receipts::{
    TUNNEL_EVENT_SCHEMA, TUNNEL_RECEIPT_SCHEMA, command_event_receipt, emitted_at,
    local_tunnel_target_url, reconnect_failure_reason, tunnel_event_receipt,
    tunnel_session_resource_uri, tunnel_started_receipt,
};
use crate::render::{
    OutputStatus, TerminalTextLayout, new_table, print_context, print_empty_state, print_field,
    print_json, print_json_line, print_status, sanitize_terminal, sanitize_terminal_display,
};
use crate::tui::{
    restore_terminal, run_app, run_tunnel_forwarder_connection, setup_terminal,
    spawn_tunnel_forwarder_manager,
};
use crate::tunnel::TunnelEvent;
use crate::{api, config, errors, logo, output, target_policy, tunnel};

#[derive(Args, Clone, Default)]
pub struct TunnelTargetArgs {
    // `port` and `host` stay `Option` so `merge()` can distinguish "not given"
    // from "given the default"; the default is written into the doc comment
    // in clap's own `[default: ...]` rendering style.
    /// Local port to forward requests to [default: 3000]
    #[arg(short, long)]
    pub port: Option<u16>,

    /// Local host to forward to [default: localhost]
    #[arg(long)]
    host: Option<String>,

    /// Organization ID (overrides the configured default)
    #[arg(short = 'o', long, value_name = "ORG_ID")]
    org: Option<String>,

    /// Static tunnel slug to attach (from `static-tunnel create`)
    #[arg(short, long)]
    slug: Option<String>,

    /// Allow a host that resolves outside loopback
    #[arg(long)]
    allow_non_loopback: bool,

    /// Do not replay requests buffered while the tunnel was offline
    #[arg(long)]
    no_replay_buffered: bool,
}

impl TunnelTargetArgs {
    pub fn merge(self, action_target: Self) -> Self {
        Self {
            port: action_target.port.or(self.port),
            host: action_target.host.or(self.host),
            org: action_target.org.or(self.org),
            slug: action_target.slug.or(self.slug),
            allow_non_loopback: self.allow_non_loopback || action_target.allow_non_loopback,
            no_replay_buffered: self.no_replay_buffered || action_target.no_replay_buffered,
        }
    }

    pub fn resolve(self) -> TunnelTarget {
        TunnelTarget {
            port: self.port.unwrap_or(3000),
            host: self.host.unwrap_or_else(|| "localhost".to_string()),
            org: self.org,
            slug: self.slug,
            allow_non_loopback: self.allow_non_loopback,
            no_replay_buffered: self.no_replay_buffered,
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct TunnelTarget {
    pub port: u16,
    pub host: String,
    pub org: Option<String>,
    pub slug: Option<String>,
    pub allow_non_loopback: bool,
    pub no_replay_buffered: bool,
}

#[derive(Subcommand)]
pub enum TunnelAction {
    /// Validate authentication, schema compatibility, and the activation plan without connecting
    Prepare(TunnelTargetArgs),
    /// Prepare and activate a relay (the default when no subcommand is given)
    #[command(alias = "activate")]
    Start(TunnelTargetArgs),
    /// List tunnel sessions
    List {
        /// Maximum number of sessions to return
        #[arg(long, value_name = "N", default_value = "50", value_parser = clap::value_parser!(u16).range(1..=100))]
        limit: u16,
        /// Only sessions in this status, such as active or stopped
        #[arg(long, value_name = "STATUS")]
        status: Option<String>,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Show a tunnel session
    Status {
        /// Tunnel session ID (from `tunnel list`)
        #[arg(value_name = "SESSION_ID")]
        session_id: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// List lifecycle events, optionally following new ones
    Events {
        /// Resume after this cursor from an earlier events receipt
        #[arg(long, value_name = "CURSOR")]
        cursor: Option<String>,
        /// Maximum number of events per page
        #[arg(long, value_name = "N", default_value = "50", value_parser = clap::value_parser!(u16).range(1..=100))]
        limit: u16,
        /// Only events for this capture
        #[arg(long, value_name = "CAPTURE_ID")]
        capture_id: Option<String>,
        /// Only events for this delivery attempt
        #[arg(long, value_name = "ATTEMPT_ID")]
        attempt_id: Option<String>,
        /// Keep polling for new events until interrupted
        #[arg(long)]
        follow: bool,
        /// Poll interval for --follow, such as 500ms, 1s, or 5s
        #[arg(long, value_name = "DURATION", default_value = "1s", value_parser = parse_duration)]
        interval: Duration,
        #[arg(long, hide = true, value_name = "MS", conflicts_with = "interval")]
        interval_ms: Option<u64>,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Show a redacted capture
    Capture {
        /// Capture ID (from `tunnel events`)
        #[arg(value_name = "CAPTURE_ID")]
        capture_id: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Show a delivery attempt
    Attempt {
        /// Delivery attempt ID (from `tunnel events`)
        #[arg(value_name = "ATTEMPT_ID")]
        attempt_id: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Stop a tunnel session, even if its CLI process is gone
    Stop {
        /// Tunnel session ID (from `tunnel list`)
        #[arg(value_name = "SESSION_ID")]
        session_id: String,
        /// Reason recorded in the session's lifecycle events
        #[arg(long, value_name = "TEXT")]
        reason: Option<String>,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Detach the current owner and keep the route for recovery
    Detach {
        /// Tunnel session ID (from `tunnel list`)
        #[arg(value_name = "SESSION_ID")]
        session_id: String,
        /// Reason recorded in the session's lifecycle events
        #[arg(long, value_name = "TEXT")]
        reason: Option<String>,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
}

pub async fn stream_tunnel_json_events(
    mut event_rx: mpsc::Receiver<TunnelEvent>,
    host: &str,
    port: u16,
    organization_id: Option<&str>,
    requested_slug: Option<&str>,
) -> Result<()> {
    let mut sequence = 1_u64;
    loop {
        tokio::select! {
            maybe_event = event_rx.recv() => {
                let Some(event) = maybe_event else {
                    return Ok(());
                };
                let failure_reason = reconnect_failure_reason(&event);

                let mut receipt = tunnel_event_receipt(
                    &event,
                    host,
                    port,
                    organization_id,
                    requested_slug,
                );
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
                    TUNNEL_EVENT_SCHEMA,
                    "tunnel",
                    "start_local_tunnel",
                    "stopped",
                    "stopped",
                );
                receipt["sequence"] = serde_json::json!(sequence);
                receipt["resource_uri"] =
                    serde_json::json!(tunnel_session_resource_uri(host, port));
                receipt["local_target_url"] =
                    serde_json::json!(local_tunnel_target_url(host, port));
                print_json_line(&receipt)?;
                return Ok(());
            }
        }
    }
}

#[allow(clippy::too_many_arguments)]
pub async fn run_tunnel_json(
    access_token_rx: watch::Receiver<String>,
    host: String,
    port: u16,
    organization_id: Option<String>,
    slug: Option<String>,
    target: target_policy::TargetPolicy,
    replay_buffered: bool,
    anonymous_route: Option<(String, String, Option<api::RelayTicket>)>,
) -> Result<()> {
    print_json_line(&tunnel_started_receipt(
        &host,
        port,
        organization_id.as_deref(),
        slug.as_deref(),
    ))?;

    let (event_tx, event_rx) = mpsc::channel(tunnel::PRESENTATION_QUEUE_CAPACITY);
    let worker = tokio::spawn(run_tunnel_forwarder_connection(
        access_token_rx,
        host.clone(),
        port,
        organization_id.clone(),
        slug.clone(),
        target,
        event_tx,
        replay_buffered,
        anonymous_route,
    ));

    match supervise_json_worker(
        worker,
        stream_tunnel_json_events(
            event_rx,
            &host,
            port,
            organization_id.as_deref(),
            slug.as_deref(),
        ),
        std::future::pending(),
    )
    .await?
    {
        WorkerCompletion::Stream(result) => result,
        WorkerCompletion::Shutdown => unreachable!("shutdown is handled by the event stream"),
    }
}

pub const SUPPORTED_TUNNEL_SCHEMA_MAJOR: u64 = 1;

pub struct TunnelLifecycleContext {
    config: config::Config,
    access_token: String,
    organization_id: String,
    client: ApiClient,
    contract: api::TunnelLifecycleContract,
}

pub struct TunnelEventOptions {
    cursor: Option<String>,
    limit: u16,
    capture_id: Option<String>,
    attempt_id: Option<String>,
    follow: bool,
    interval_ms: u64,
}

pub async fn tunnel_lifecycle_context(org: Option<String>) -> Result<TunnelLifecycleContext> {
    let mut config = config::Config::load()?;
    let organization_id = require_organization(org, &config)?;
    let access_token = ensure_valid_token(&mut config).await?;
    let client = ApiClient::with_organization(access_token.clone(), Some(organization_id.clone()))?;
    let contract = client.tunnel_lifecycle_contract().await?;
    validate_tunnel_schema(&contract)?;

    Ok(TunnelLifecycleContext {
        config,
        access_token,
        organization_id,
        client,
        contract,
    })
}

pub fn validate_tunnel_schema(contract: &api::TunnelLifecycleContract) -> Result<()> {
    if contract.schema.major == SUPPORTED_TUNNEL_SCHEMA_MAJOR {
        return Ok(());
    }

    Err(errors::TunnelLifecycleError::IncompatibleSchema {
        supported: SUPPORTED_TUNNEL_SCHEMA_MAJOR,
        actual: contract.schema.major,
    }
    .into())
}

pub fn tunnel_lifecycle_receipt(
    operation: &str,
    status: &str,
    organization_id: &str,
    resource_uri: Option<&str>,
    data: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "$schema": TUNNEL_RECEIPT_SCHEMA,
        "schema_version": 1,
        "type": "receipt",
        "event_id": uuid::Uuid::new_v4(),
        "sequence": 0,
        "command": "tunnel",
        "operation": operation,
        "status": status,
        "emitted_at": emitted_at(),
        "organization_id": organization_id,
        "resource_uri": resource_uri,
        "data": data,
    })
}

pub fn tunnel_lifecycle_event_envelope(event: &api::TunnelLifecycleEvent) -> serde_json::Value {
    serde_json::json!({
        "$schema": TUNNEL_EVENT_SCHEMA,
        "schema_version": 1,
        "type": "event",
        "event": event.event_type,
        "event_id": event.id,
        "position": event.position,
        "sequence": event.sequence,
        "cursor": event.cursor,
        "emitted_at": event.created_at,
        "organization_id": event.organization_id,
        "capture_id": event.capture_id,
        "attempt_id": event.delivery_id,
        "fence": event.fence,
        "metadata": safe_tunnel_event_metadata(&event.metadata),
        "resources": {
            "capture": format!("hooklistener://tunnel/captures/{}", event.capture_id),
            "attempt": event.delivery_id.as_ref().map(|id| format!("hooklistener://tunnel/attempts/{id}")),
            "event": format!("hooklistener://tunnel/events/{}", event.id),
        }
    })
}

pub fn safe_tunnel_event_metadata(metadata: &serde_json::Value) -> serde_json::Value {
    let Some(metadata) = metadata.as_object() else {
        return serde_json::json!({});
    };
    let mut safe = serde_json::Map::new();

    for key in [
        "source",
        "source_delivery_id",
        "session_id",
        "route_id",
        "delivery_id",
        "error_code",
    ] {
        if let Some(value) = metadata.get(key).filter(|value| value.is_string()) {
            safe.insert(key.to_string(), value.clone());
        }
    }
    for key in ["status_code", "duration_ms"] {
        if let Some(value) = metadata.get(key).filter(|value| value.is_number()) {
            safe.insert(key.to_string(), value.clone());
        }
    }

    serde_json::Value::Object(safe)
}

pub async fn run_tunnel_lifecycle_command(
    action: Option<TunnelAction>,
    default_target: TunnelTargetArgs,
    json: bool,
    update_handle: &mut Option<JoinHandle<Option<String>>>,
) -> Result<()> {
    match action {
        None => run_tunnel_activation(default_target.resolve(), json, update_handle).await,
        Some(TunnelAction::Start(target)) => {
            run_tunnel_activation(default_target.merge(target).resolve(), json, update_handle).await
        }
        Some(TunnelAction::Prepare(target)) => {
            let target = default_target.merge(target).resolve();
            validate_tunnel_target(&target)?;
            let local_target_url = local_tunnel_target_url(&target.host, target.port);
            let target_policy = target_policy::TargetPolicy::resolve(
                &local_target_url,
                target.allow_non_loopback,
                false,
            )
            .await?;
            let replay_buffered = !target.no_replay_buffered;
            let context = tunnel_lifecycle_context(target.org.clone()).await?;
            let receipt = tunnel_lifecycle_receipt(
                "prepare",
                "prepared",
                &context.organization_id,
                None,
                serde_json::json!({
                    "contract": {
                        "id": context.contract.id,
                        "version": context.contract.version,
                        "schema": context.contract.schema,
                    },
                    "activation": {
                        "local_target_url": local_target_url,
                        "requested_slug": target.slug,
                        "target": target_policy.plan(),
                        "replay_buffered": replay_buffered,
                    }
                }),
            );
            if json {
                print_json_line(&receipt)
            } else {
                print_status(OutputStatus::Ok, "TUNNEL PREPARED");
                println!();
                print_field("TARGET", local_target_url);
                print_field("ORGANIZATION", context.organization_id);
                print_field("CONTRACT", context.contract.version);
                if let Some(slug) = target.slug {
                    print_field("SLUG", slug);
                }
                Ok(())
            }
        }
        Some(TunnelAction::List { limit, status, org }) => {
            let context = tunnel_lifecycle_context(org).await?;
            let sessions = context
                .client
                .list_tunnel_sessions(limit, status.as_deref())
                .await?;
            let receipt = tunnel_lifecycle_receipt(
                "list",
                "succeeded",
                &context.organization_id,
                Some("hooklistener://tunnel/sessions"),
                serde_json::to_value(&sessions)?,
            );
            if json {
                print_json_line(&receipt)
            } else {
                print_tunnel_sessions(&sessions.data, &context.organization_id);
                Ok(())
            }
        }
        Some(TunnelAction::Status { session_id, org }) => {
            let context = tunnel_lifecycle_context(org).await?;
            let session = context.client.get_tunnel_session(&session_id).await?;
            print_tunnel_lifecycle_resource(
                json,
                "status",
                "hooklistener://tunnel/sessions",
                &session.id,
                &context.organization_id,
                &session,
            )
        }
        Some(TunnelAction::Capture { capture_id, org }) => {
            let context = tunnel_lifecycle_context(org).await?;
            let capture = context.client.get_tunnel_capture(&capture_id).await?;
            print_tunnel_lifecycle_resource(
                json,
                "capture",
                "hooklistener://tunnel/captures",
                &capture.id,
                &context.organization_id,
                &capture,
            )
        }
        Some(TunnelAction::Attempt { attempt_id, org }) => {
            let context = tunnel_lifecycle_context(org).await?;
            let attempt = context.client.get_tunnel_attempt(&attempt_id).await?;
            print_tunnel_lifecycle_resource(
                json,
                "attempt",
                "hooklistener://tunnel/attempts",
                &attempt.id,
                &context.organization_id,
                &attempt,
            )
        }
        Some(TunnelAction::Stop {
            session_id,
            reason,
            org,
        }) => {
            let context = tunnel_lifecycle_context(org).await?;
            let session = context
                .client
                .stop_tunnel_session(&session_id, reason.as_deref())
                .await?;
            print_tunnel_lifecycle_resource(
                json,
                "stop",
                "hooklistener://tunnel/sessions",
                &session.id,
                &context.organization_id,
                &session,
            )
        }
        Some(TunnelAction::Detach {
            session_id,
            reason,
            org,
        }) => {
            let context = tunnel_lifecycle_context(org).await?;
            let session = context
                .client
                .detach_tunnel_session(&session_id, reason.as_deref())
                .await?;
            print_tunnel_lifecycle_resource(
                json,
                "detach",
                "hooklistener://tunnel/sessions",
                &session.id,
                &context.organization_id,
                &session,
            )
        }
        Some(TunnelAction::Events {
            cursor,
            limit,
            capture_id,
            attempt_id,
            follow,
            interval,
            interval_ms,
            org,
        }) => {
            let interval_ms =
                resolve_millis_flag(Some(interval), interval_ms, "interval-ms", "interval")
                    .unwrap_or(MILLIS_PER_SECOND);
            let context = tunnel_lifecycle_context(org).await?;
            run_tunnel_lifecycle_events(
                &context,
                TunnelEventOptions {
                    cursor,
                    limit,
                    capture_id,
                    attempt_id,
                    follow,
                    interval_ms,
                },
                json,
            )
            .await
        }
    }
}

pub async fn run_tunnel_activation(
    target: TunnelTarget,
    json: bool,
    update_handle: &mut Option<JoinHandle<Option<String>>>,
) -> Result<()> {
    validate_tunnel_target(&target)?;
    let local_target_url = local_tunnel_target_url(&target.host, target.port);
    let target_policy =
        target_policy::TargetPolicy::resolve(&local_target_url, target.allow_non_loopback, false)
            .await?;
    let replay_buffered = !target.no_replay_buffered;
    let context = tunnel_lifecycle_context(target.org.clone()).await?;
    let access_token_rx = refreshed_access_token_rx(context.access_token, context.config);
    let selected_org = Some(context.organization_id);

    if json {
        run_tunnel_json(
            access_token_rx,
            target.host,
            target.port,
            selected_org,
            target.slug,
            target_policy,
            replay_buffered,
            None,
        )
        .await
    } else {
        let mut terminal = setup_terminal()?;
        let mut app = App::new()?;
        app.monochrome = !output::styles_enabled();
        app.state = AppState::Tunneling;
        app.tunnel_local_host = target.host.clone();
        app.tunnel_local_port = target.port;
        app.tunnel_org_id = selected_org.clone();
        app.tunnel_requested_slug = target.slug.clone();

        let (event_tx, event_rx) = mpsc::channel(tunnel::PRESENTATION_QUEUE_CAPACITY);
        let reconnect_tx = spawn_tunnel_forwarder_manager(
            access_token_rx,
            target.host,
            target.port,
            selected_org,
            target.slug,
            target_policy,
            event_tx.clone(),
            replay_buffered,
            None,
        );
        let result = run_app(
            &mut terminal,
            &mut app,
            event_rx,
            Some(reconnect_tx),
            Some(event_tx),
            Some(logo::spawn_logo_animation()),
            update_handle,
        )
        .await;
        restore_terminal(&mut terminal)?;
        result
    }
}

pub async fn run_anonymous_tunnel_activation(
    host: String,
    port: u16,
    name: Option<String>,
    ttl: u64,
    allow_non_loopback: bool,
    json: bool,
    update_handle: &mut Option<JoinHandle<Option<String>>>,
) -> Result<()> {
    let target = TunnelTarget {
        port,
        host: host.clone(),
        org: None,
        slug: name.clone(),
        allow_non_loopback,
        no_replay_buffered: true,
    };
    validate_tunnel_target(&target)?;
    let local_target_url = local_tunnel_target_url(&host, port);
    let target_policy =
        target_policy::TargetPolicy::resolve(&local_target_url, allow_non_loopback, false).await?;
    let client = ApiClient::unauthenticated()?;
    let target_plan = serde_json::to_value(target_policy.plan())?;
    let created = client
        .create_anonymous_tunnel_route(&target_plan, name.as_deref(), ttl)
        .await?;

    if json {
        print_json_line(&serde_json::json!({
            "schema": "hooklistener.tunnel.anonymous-route/1",
            "operation": "create_anonymous_tunnel",
            "status": "created",
            "route": {
                "id": created.id,
                "slug": created.slug,
                "url": created.url,
                "stable_name": created.stable_name,
                "expires_at": created.expires_at,
                "limits": created.limits,
            },
            "credentials": {
                "route_token": created.route_token,
                "claim_token": created.claim_token,
            },
            "privacy": {
                "claim_transfers_captures": false,
            }
        }))?;
    } else {
        print_status(OutputStatus::Ok, "ANONYMOUS TUNNEL CREATED");
        println!();
        print_field("ROUTE", &created.id);
        print_field("PUBLIC URL", created.url.as_str().underlined());
        print_field("EXPIRES AT", created.expires_at.as_str().dim());
        println!();
        print_field("ROUTE TOKEN", created.route_token.as_str().yellow());
        print_field("CLAIM TOKEN", created.claim_token.as_str().yellow());
        print_field(
            "ACTION",
            format!(
                "Save both tokens. Claim later with `hooklistener anon claim {} --token <claim-token>`.",
                created.id
            )
            .dim(),
        );
    }

    let anonymous_route = Some((
        created.id.clone(),
        created.route_token.clone(),
        Some(created.relay_ticket.clone()),
    ));
    let (_token_tx, token_rx) = watch::channel(String::new());

    if json {
        run_tunnel_json(
            token_rx,
            host,
            port,
            None,
            Some(created.slug),
            target_policy,
            false,
            anonymous_route,
        )
        .await
    } else {
        let mut terminal = setup_terminal()?;
        let mut app = App::new()?;
        app.monochrome = !output::styles_enabled();
        app.state = AppState::Tunneling;
        app.tunnel_local_host = host.clone();
        app.tunnel_local_port = port;
        app.tunnel_org_id = None;
        app.tunnel_requested_slug = Some(created.slug.clone());

        let (event_tx, event_rx) = mpsc::channel(tunnel::PRESENTATION_QUEUE_CAPACITY);
        let reconnect_tx = spawn_tunnel_forwarder_manager(
            token_rx,
            host,
            port,
            None,
            Some(created.slug),
            target_policy,
            event_tx.clone(),
            false,
            anonymous_route,
        );
        let result = run_app(
            &mut terminal,
            &mut app,
            event_rx,
            Some(reconnect_tx),
            Some(event_tx),
            Some(logo::spawn_logo_animation()),
            update_handle,
        )
        .await;
        restore_terminal(&mut terminal)?;
        result
    }
}

pub fn validate_tunnel_target(target: &TunnelTarget) -> Result<()> {
    if target.port == 0 {
        return Err(anyhow!("Tunnel target port must be between 1 and 65535"));
    }
    if target.host.trim().is_empty() {
        return Err(anyhow!("Tunnel target host cannot be empty"));
    }
    Ok(())
}

pub fn print_tunnel_lifecycle_resource<T: serde::Serialize>(
    json: bool,
    operation: &str,
    collection_uri: &str,
    id: &str,
    organization_id: &str,
    resource: &T,
) -> Result<()> {
    let resource_uri = format!("{collection_uri}/{id}");
    let receipt = tunnel_lifecycle_receipt(
        operation,
        "succeeded",
        organization_id,
        Some(&resource_uri),
        serde_json::to_value(resource)?,
    );
    if json {
        print_json_line(&receipt)
    } else {
        print_status(
            OutputStatus::Ok,
            &format!("TUNNEL {}", operation.to_uppercase()),
        );
        println!();
        print_field(
            "RESOURCE",
            sanitize_terminal(&resource_uri, TerminalTextLayout::Inline),
        );
        print_field(
            "ORGANIZATION",
            sanitize_terminal(organization_id, TerminalTextLayout::Inline),
        );
        print_json(resource)
    }
}

pub fn print_tunnel_sessions(sessions: &[api::TunnelSessionResource], organization_id: &str) {
    print_context("Organization:", organization_id);
    if sessions.is_empty() {
        print_empty_state(
            "NO TUNNEL SESSIONS",
            "Run `hooklistener tunnel start --port 3000` to activate one.",
        );
        return;
    }

    let mut table = new_table(&["ID", "Status", "Slug", "Mode", "Updated"]);
    for session in sessions {
        table.add_row(vec![
            sanitize_terminal_display(&session.id),
            sanitize_terminal_display(&session.status),
            session
                .route
                .as_ref()
                .map(|route| sanitize_terminal_display(&route.slug))
                .unwrap_or_else(|| "-".to_string()),
            session
                .route
                .as_ref()
                .map(|route| sanitize_terminal_display(&route.mode))
                .unwrap_or_else(|| "-".to_string()),
            sanitize_terminal_display(&session.updated_at),
        ]);
    }
    println!("{table}");
}

pub fn format_tunnel_lifecycle_event(event: &api::TunnelLifecycleEvent) -> String {
    format!(
        "{}  {}  capture={}  attempt={}",
        event.position,
        sanitize_terminal(&event.event_type, TerminalTextLayout::Inline),
        sanitize_terminal(&event.capture_id, TerminalTextLayout::Inline),
        sanitize_terminal(
            event.delivery_id.as_deref().unwrap_or("-"),
            TerminalTextLayout::Inline,
        )
    )
}

pub async fn run_tunnel_lifecycle_events(
    context: &TunnelLifecycleContext,
    mut options: TunnelEventOptions,
    json: bool,
) -> Result<()> {
    loop {
        let page = context
            .client
            .list_tunnel_events(
                options.cursor.as_deref(),
                options.limit,
                options.capture_id.as_deref(),
                options.attempt_id.as_deref(),
            )
            .await?;

        for event in &page.data {
            if json {
                print_json_line(&tunnel_lifecycle_event_envelope(event))?;
            } else {
                println!("{}", format_tunnel_lifecycle_event(event));
            }
        }

        options.cursor = Some(page.meta.cursor.clone());
        if json {
            print_json_line(&tunnel_lifecycle_receipt(
                "events",
                if options.follow {
                    "following"
                } else {
                    "succeeded"
                },
                &context.organization_id,
                Some("hooklistener://tunnel/events"),
                serde_json::json!({
                    "cursor": page.meta.cursor,
                    "has_more": page.meta.has_more,
                    "retention_days": page.meta.retention_days,
                    "resync": page.meta.resync,
                    "event_count": page.data.len(),
                }),
            ))?;
        }

        if !options.follow {
            return Ok(());
        }
        if page.meta.has_more {
            continue;
        }

        tokio::select! {
            signal = tokio::signal::ctrl_c() => {
                signal?;
                if json {
                    print_json_line(&tunnel_lifecycle_receipt(
                        "events",
                        "stopped",
                        &context.organization_id,
                        Some("hooklistener://tunnel/events"),
                        serde_json::json!({"cursor": options.cursor}),
                    ))?;
                }
                return Ok(());
            }
            _ = sleep(Duration::from_millis(options.interval_ms.max(100))) => {}
        }
    }
}
