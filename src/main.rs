mod api;
mod app;
mod auth;
mod config;
mod errors;
mod logger;
mod logo;
mod models;
mod syntax;
mod tunnel;
mod ui;
mod updater;

use anyhow::{Result, anyhow};
use chrono::{Duration as ChronoDuration, Utc};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use comfy_table::{ContentArrangement, Table, presets::UTF8_FULL_CONDENSED};
use crossterm::{
    cursor::{MoveToColumn, Show},
    event::{self, Event, KeyEventKind},
    execute,
    style::Stylize,
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode,
    },
};
use ratatui::{Terminal, backend::CrosstermBackend};
use reqwest::Url;
use std::io::{self, Write};
use std::path::PathBuf;
use std::time::Duration;
use tokio::{
    sync::{mpsc, watch},
    time::sleep,
};
use tracing::error;

use api::ApiClient;
use app::{App, AppState};
use logger::{LogConfig, Logger};
use tunnel::TunnelEvent;

#[derive(Parser)]
#[command(name = "hooklistener")]
#[command(about = "A CLI tool for debugging webhooks")]
#[command(version)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Output command responses or event streams as JSON
    #[arg(long, global = true)]
    json: bool,

    /// Log level (trace, debug, info, warn, error)
    #[arg(long, default_value = "info", value_parser = validate_log_level)]
    log_level: String,

    /// Custom directory for log files
    #[arg(long)]
    log_dir: Option<PathBuf>,

    /// Output logs to stdout in addition to files (for debugging)
    #[arg(long)]
    log_stdout: bool,
}

#[derive(Subcommand)]
enum Commands {
    /// Authenticate with Hooklistener via the device flow
    Login {
        /// Start a new authentication even if a valid token already exists
        #[arg(long)]
        force: bool,
    },
    /// Start WebSocket tunnel to forward webhooks to local server
    Listen {
        /// Debug endpoint slug to listen to
        endpoint: String,

        /// Local URL to forward requests to
        #[arg(short, long, default_value = "http://localhost:3000")]
        target: String,

        /// WebSocket server URL (defaults to production)
        #[arg(long)]
        ws_url: Option<String>,
    },
    /// Generate a diagnostic bundle for support
    Diagnostics {
        /// Output directory for the diagnostic bundle
        #[arg(short, long, default_value = ".")]
        output: PathBuf,
    },
    /// Clean up old log files
    CleanLogs {
        /// Maximum number of log files to keep
        #[arg(short, long, default_value = "10")]
        keep: usize,
    },
    /// Manage CLI configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Sign out and clear locally stored token
    Logout,
    /// Organization helpers
    Org {
        #[command(subcommand)]
        action: OrgAction,
    },
    /// Debug endpoint helpers
    Endpoint {
        #[command(subcommand)]
        action: EndpointAction,
    },
    /// Saved endpoint case helpers
    Cases {
        #[command(subcommand)]
        action: CasesAction,
    },
    /// Static tunnel slug management
    StaticTunnel {
        #[command(subcommand)]
        action: StaticTunnelAction,
    },
    /// Anonymous (temporary) debug endpoints — no login required
    Anon {
        #[command(subcommand)]
        action: AnonAction,
    },
    /// Share captured requests via public links
    Share {
        #[command(subcommand)]
        action: ShareAction,
    },
    /// Uptime monitoring for your endpoints
    Monitor {
        #[command(subcommand)]
        action: MonitorAction,
    },
    /// Generate shell completion scripts
    Completions {
        /// Target shell
        #[arg(value_enum)]
        shell: CompletionShell,
    },
    /// Update hooklistener to the latest version
    Update,
    /// Start HTTP tunnel to forward requests to local server
    Tunnel {
        /// Local port to forward requests to
        #[arg(short, long, default_value = "3000")]
        port: u16,

        /// Local host to forward to
        #[arg(long, default_value = "localhost")]
        host: String,

        /// Organization ID (optional, uses default)
        #[arg(short, long)]
        org: Option<String>,

        /// Static tunnel slug (paid plans only, creates persistent subdomain)
        #[arg(short, long)]
        slug: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CompletionShell {
    Bash,
    Zsh,
    Fish,
    PowerShell,
    Elvish,
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Display current configuration
    Show,
    /// Set a configuration value
    Set {
        /// Configuration key (selected_organization_id)
        key: String,
        /// New value
        value: String,
    },
}

#[derive(Subcommand)]
enum OrgAction {
    /// List organizations available to your account
    List,
    /// Set the default organization used by CLI commands
    Use {
        /// Organization ID
        id: String,
    },
    /// Clear the default organization
    Clear,
}

#[derive(Subcommand)]
enum EndpointAction {
    /// Create a debug endpoint
    Create {
        /// Endpoint display name
        name: String,
        /// Optional custom slug
        #[arg(long)]
        slug: Option<String>,
        /// Organization ID override (falls back to configured default)
        #[arg(long)]
        org: Option<String>,
    },
    /// List debug endpoints for an organization
    List {
        /// Organization ID override (falls back to configured default)
        #[arg(long)]
        org: Option<String>,
    },
    /// Show a single debug endpoint by ID
    Show {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Organization ID override (falls back to configured default)
        #[arg(long)]
        org: Option<String>,
    },
    /// Delete a debug endpoint by ID
    Delete {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Organization ID override (falls back to configured default)
        #[arg(long)]
        org: Option<String>,
    },
    /// List captured requests for an endpoint
    Requests {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Page number
        #[arg(long, default_value = "1")]
        page: u32,
        /// Page size
        #[arg(long, default_value = "50")]
        page_size: u32,
        /// Organization ID override (falls back to configured default)
        #[arg(long)]
        org: Option<String>,
    },
    /// Show a single captured request
    Request {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Debug request ID
        request_id: String,
        /// Organization ID override (falls back to configured default)
        #[arg(long)]
        org: Option<String>,
    },
    /// Delete a captured request
    DeleteRequest {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Debug request ID
        request_id: String,
        /// Organization ID override (falls back to configured default)
        #[arg(long)]
        org: Option<String>,
    },
    /// Replay a captured request to a target URL
    ForwardRequest {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Debug request ID
        request_id: String,
        /// Target URL to replay to
        target_url: String,
        /// Optional HTTP method override (GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS)
        #[arg(long)]
        method: Option<String>,
        /// Validate scope and print the forward plan without queueing delivery
        #[arg(long)]
        dry_run: bool,
        /// Organization ID override (falls back to configured default)
        #[arg(long)]
        org: Option<String>,
    },
    /// List forwards created from a captured request
    Forwards {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Debug request ID
        request_id: String,
        /// Page number
        #[arg(long, default_value = "1")]
        page: u32,
        /// Page size
        #[arg(long, default_value = "50")]
        page_size: u32,
        /// Organization ID override (falls back to configured default)
        #[arg(long)]
        org: Option<String>,
    },
    /// Show a single forward attempt by ID
    Forward {
        /// Forward ID
        forward_id: String,
        /// Organization ID override (falls back to configured default)
        #[arg(long)]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
enum CasesAction {
    /// Run saved cases for an endpoint
    Run {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Target URL, saved target ID, or "cli"
        #[arg(long)]
        target: Option<String>,
        /// Explicit target URL to replay cases to
        #[arg(long)]
        target_url: Option<String>,
        /// Explicit saved replay target ID
        #[arg(long)]
        target_id: Option<String>,
        /// Optional display name for the target
        #[arg(long)]
        target_name: Option<String>,
        /// Wait for the run to complete before returning
        #[arg(long)]
        wait: bool,
        /// Timeout for --wait, such as 60, 60s, 2m, or 1h
        #[arg(long)]
        timeout: Option<String>,
        /// Timeout for --wait in milliseconds
        #[arg(long)]
        timeout_ms: Option<u64>,
        /// Poll interval for --wait in milliseconds
        #[arg(long)]
        interval_ms: Option<u64>,
        /// Organization ID override (falls back to configured default)
        #[arg(long)]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
enum StaticTunnelAction {
    /// List reserved static tunnel slugs
    List {
        /// Organization ID override (falls back to configured default)
        #[arg(long)]
        org: Option<String>,
    },
    /// Create a new static tunnel slug
    Create {
        /// Slug to reserve
        slug: String,
        /// Optional display name
        #[arg(long)]
        name: Option<String>,
        /// Organization ID override (falls back to configured default)
        #[arg(long)]
        org: Option<String>,
    },
    /// Delete a static tunnel slug by ID
    Delete {
        /// Static tunnel ID
        slug_id: String,
        /// Organization ID override (falls back to configured default)
        #[arg(long)]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
enum AnonAction {
    /// Create a temporary anonymous endpoint (no login required)
    Create {
        /// Time-to-live in seconds (default: 86400 = 24 hours)
        #[arg(long)]
        ttl: Option<u64>,
    },
    /// Show status of an anonymous endpoint
    Show {
        /// Anonymous endpoint ID
        id: String,
    },
    /// List captured events for an anonymous endpoint
    Events {
        /// Anonymous endpoint ID
        endpoint_id: String,
        /// Viewer token (returned when the endpoint was created)
        #[arg(long)]
        token: String,
        /// Page number
        #[arg(long, default_value = "1")]
        page: u32,
        /// Page size
        #[arg(long, default_value = "50")]
        page_size: u32,
    },
    /// Show a single captured event
    Event {
        /// Anonymous endpoint ID
        endpoint_id: String,
        /// Event ID
        event_id: String,
        /// Viewer token (returned when the endpoint was created)
        #[arg(long)]
        token: String,
    },
}

#[derive(Subcommand)]
enum ShareAction {
    /// Create a shareable link for a captured request
    Create {
        /// Debug request ID to share
        debug_request_id: String,
        /// Expiration in hours (e.g. 24)
        #[arg(long)]
        expires_in_hours: Option<u64>,
        /// Optional password to protect the share
        #[arg(long)]
        password: Option<String>,
        /// Include forwards in the shared view
        #[arg(long)]
        include_forwards: bool,
        /// Organization ID override (falls back to configured default)
        #[arg(long)]
        org: Option<String>,
    },
    /// List all shares for a captured request
    List {
        /// Debug request ID
        debug_request_id: String,
        /// Organization ID override (falls back to configured default)
        #[arg(long)]
        org: Option<String>,
    },
    /// View a shared request by its share token (public, no login required)
    Show {
        /// Share token
        token: String,
    },
    /// Revoke a shared request link
    Revoke {
        /// Share token to revoke
        token: String,
        /// Organization ID override (falls back to configured default)
        #[arg(long)]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
enum MonitorAction {
    /// Create a new uptime monitor
    Create {
        /// Monitor display name
        name: String,
        /// URL to monitor (must be http:// or https://)
        url: String,
        /// HTTP method (get, post, put, patch, delete, head)
        #[arg(long, default_value = "get")]
        method: String,
        /// Expected HTTP status code
        #[arg(long, default_value = "200")]
        expected_status: u16,
        /// Check interval in minutes (1, 5, 10, 30, 60)
        #[arg(long, default_value = "5")]
        interval: u32,
        /// String the response body must contain
        #[arg(long)]
        body_contains: Option<String>,
        /// Request body to send (for POST/PUT/PATCH)
        #[arg(long)]
        body: Option<String>,
        /// Number of consecutive failures before alerting
        #[arg(long, default_value = "2")]
        failure_threshold: u32,
        /// Enable email notifications
        #[arg(long, default_value = "true")]
        email: bool,
        /// Organization ID override (falls back to configured default)
        #[arg(long)]
        org: Option<String>,
    },
    /// List all uptime monitors
    List {
        /// Organization ID override (falls back to configured default)
        #[arg(long)]
        org: Option<String>,
    },
    /// Show details of an uptime monitor
    Show {
        /// Monitor ID
        id: String,
        /// Organization ID override (falls back to configured default)
        #[arg(long)]
        org: Option<String>,
    },
    /// Update an uptime monitor
    Update {
        /// Monitor ID
        id: String,
        /// New name
        #[arg(long)]
        name: Option<String>,
        /// New URL
        #[arg(long)]
        url: Option<String>,
        /// HTTP method
        #[arg(long)]
        method: Option<String>,
        /// Expected HTTP status code
        #[arg(long)]
        expected_status: Option<u16>,
        /// Check interval in minutes (1, 5, 10, 30, 60)
        #[arg(long)]
        interval: Option<u32>,
        /// Enable or disable the monitor
        #[arg(long)]
        enabled: Option<bool>,
        /// Number of consecutive failures before alerting
        #[arg(long)]
        failure_threshold: Option<u32>,
        /// Organization ID override (falls back to configured default)
        #[arg(long)]
        org: Option<String>,
    },
    /// Delete an uptime monitor
    Delete {
        /// Monitor ID
        id: String,
        /// Organization ID override (falls back to configured default)
        #[arg(long)]
        org: Option<String>,
    },
    /// Show recent check history for a monitor
    Checks {
        /// Monitor ID
        id: String,
        /// Page number
        #[arg(long, default_value = "1")]
        page: u32,
        /// Page size
        #[arg(long, default_value = "50")]
        page_size: u32,
        /// Organization ID override (falls back to configured default)
        #[arg(long)]
        org: Option<String>,
    },
}

fn validate_log_level(s: &str) -> Result<String, String> {
    match s.to_lowercase().as_str() {
        "trace" | "debug" | "info" | "warn" | "error" => Ok(s.to_string()),
        _ => Err(format!(
            "Invalid log level: {}. Valid levels are: trace, debug, info, warn, error",
            s
        )),
    }
}

fn normalize_http_method(method: Option<String>) -> Result<Option<String>> {
    let Some(method) = method else {
        return Ok(None);
    };

    let normalized = method.trim().to_ascii_uppercase();
    let valid = matches!(
        normalized.as_str(),
        "GET" | "POST" | "PUT" | "PATCH" | "DELETE" | "HEAD" | "OPTIONS"
    );

    if !valid {
        return Err(anyhow!(
            "Invalid HTTP method '{}'. Valid values: GET, POST, PUT, PATCH, DELETE, HEAD, OPTIONS",
            method
        ));
    }

    Ok(Some(normalized))
}

fn parse_timeout_ms(timeout: Option<String>, timeout_ms: Option<u64>) -> Result<Option<u64>> {
    match (timeout, timeout_ms) {
        (Some(_), Some(_)) => Err(anyhow!("Use either --timeout or --timeout-ms, not both.")),
        (None, None) => Ok(None),
        (None, Some(ms)) => Ok(Some(ms)),
        (Some(raw), None) => parse_duration_to_ms(&raw).map(Some),
    }
}

fn parse_duration_to_ms(raw: &str) -> Result<u64> {
    let value = raw.trim();
    if value.is_empty() {
        return Err(anyhow!("Timeout cannot be empty."));
    }

    let split_at = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(split_at);
    if number.is_empty() {
        return Err(anyhow!("Timeout must start with a number."));
    }
    let amount: u64 = number.parse()?;
    let multiplier = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "s" | "sec" | "secs" | "second" | "seconds" => 1_000,
        "ms" | "millisecond" | "milliseconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => 60_000,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3_600_000,
        other => {
            return Err(anyhow!(
                "Invalid timeout unit '{}'. Use ms, s, m, or h.",
                other
            ));
        }
    };
    amount
        .checked_mul(multiplier)
        .ok_or_else(|| anyhow!("Timeout is too large."))
}

struct CaseRunInput {
    target: Option<String>,
    target_url: Option<String>,
    target_id: Option<String>,
    target_name: Option<String>,
    wait: bool,
    timeout: Option<String>,
    timeout_ms: Option<u64>,
    interval_ms: Option<u64>,
}

fn build_case_run_params(input: CaseRunInput) -> Result<api::CaseRunParams> {
    let CaseRunInput {
        target,
        target_url,
        target_id,
        target_name,
        wait,
        timeout,
        timeout_ms,
        interval_ms,
    } = input;

    let explicit_targets = target_url.iter().count() + target_id.iter().count();
    if target.is_some() && explicit_targets > 0 {
        return Err(anyhow!(
            "Use --target by itself, or use one of --target-url/--target-id."
        ));
    }
    if explicit_targets > 1 {
        return Err(anyhow!("Use only one of --target-url or --target-id."));
    }

    let mut params = api::CaseRunParams {
        target_url: None,
        target_id: None,
        target: None,
        target_name,
        wait: wait.then_some(true),
        timeout_ms: parse_timeout_ms(timeout, timeout_ms)?,
        interval_ms,
    };

    if let Some(target_url) = target_url {
        params.target_url = Some(target_url);
    } else if let Some(target_id) = target_id {
        params.target_id = Some(target_id);
    } else if let Some(target) = target {
        let normalized = target.trim();
        if normalized.eq_ignore_ascii_case("cli") {
            params.target = Some("cli".to_string());
        } else if normalized.starts_with("http://") || normalized.starts_with("https://") {
            params.target_url = Some(target);
        } else {
            params.target_id = Some(target);
        }
    } else {
        return Err(anyhow!(
            "Target is required. Use --target, --target-url, or --target-id."
        ));
    }

    Ok(params)
}

fn case_run_failed(result: &api::CaseRunResult) -> bool {
    matches!(result.result_status.as_str(), "failed" | "timeout")
}

fn case_run_target_label(target: &api::CaseRunTarget) -> String {
    if target.r#type.as_deref() == Some("cli") {
        return "CLI listener".to_string();
    }

    target
        .name
        .as_deref()
        .or(target.url.as_deref())
        .or(target.id.as_deref())
        .unwrap_or("-")
        .to_string()
}

fn case_run_forward_target(forward: &api::CaseRunForward) -> String {
    forward.target_url.as_deref().unwrap_or("CLI").to_string()
}

fn has_case_run_error(error: Option<&serde_json::Value>) -> bool {
    match error {
        Some(serde_json::Value::Null) | None => false,
        Some(serde_json::Value::String(value)) => !value.is_empty(),
        Some(serde_json::Value::Object(value)) => !value.is_empty(),
        Some(_) => true,
    }
}

fn case_run_value_message(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(message) => message.clone(),
        serde_json::Value::Object(object) => {
            if let Some(detail) = object.get("detail").and_then(|value| value.as_str()) {
                detail.to_string()
            } else if let Some(error_type) = object.get("type").and_then(|value| value.as_str()) {
                error_type.replace('_', " ")
            } else {
                value.to_string()
            }
        }
        _ => value.to_string(),
    }
}

fn case_run_failure_reason(failure: &api::CaseRunFailure) -> String {
    case_run_value_message(&failure.reason)
}

fn validate_forward_target_url(target_url: &str) -> Result<()> {
    let parsed = Url::parse(target_url)
        .map_err(|err| anyhow!("Invalid target URL '{}': {}", target_url, err))?;

    match parsed.scheme() {
        "http" | "https" => Ok(()),
        scheme => Err(anyhow!(
            "Invalid target URL scheme '{}'. Use http or https.",
            scheme
        )),
    }
}

fn request_resource_uri(request_id: &str) -> String {
    format!("hooklistener://requests/{request_id}")
}

fn request_forwards_resource_uri(request_id: &str) -> String {
    format!("hooklistener://requests/{request_id}/forwards")
}

fn endpoint_resource_uri(endpoint_id: &str) -> String {
    format!("hooklistener://endpoints/{endpoint_id}")
}

fn endpoint_requests_resource_uri(endpoint_id: &str) -> String {
    format!("hooklistener://endpoints/{endpoint_id}/requests")
}

fn endpoint_slug_resource_uri(endpoint_slug: &str) -> String {
    format!("hooklistener://endpoints/by-slug/{endpoint_slug}")
}

fn forward_resource_uri(forward_id: &str) -> String {
    format!("hooklistener://forwards/{forward_id}")
}

fn tunnel_resource_uri(tunnel_id: &str) -> String {
    format!("hooklistener://tunnels/{tunnel_id}")
}

fn listen_session_resource_uri(endpoint_slug: &str) -> String {
    format!("hooklistener://cli/listen/{endpoint_slug}")
}

fn tunnel_session_resource_uri(host: &str, port: u16) -> String {
    format!("hooklistener://cli/tunnels/{host}:{port}")
}

fn forward_poll_path(forward_id: &str) -> String {
    format!("/api/v1/forwards/{forward_id}")
}

fn forward_poll_command(forward_id: &str) -> String {
    format!("hooklistener endpoint forward {forward_id}")
}

fn emitted_at() -> String {
    Utc::now().to_rfc3339()
}

fn effective_listen_ws_url(ws_url: Option<&str>) -> String {
    ws_url
        .map(str::to_string)
        .or_else(|| std::env::var("HOOKLISTENER_WS_URL").ok())
        .unwrap_or_else(|| "wss://api.hooklistener.com".to_string())
}

fn command_event_receipt(
    command: &str,
    operation: &str,
    event: &str,
    status: &str,
) -> serde_json::Value {
    serde_json::json!({
        "type": "event",
        "event": event,
        "status": status,
        "command": command,
        "operation": operation,
        "emitted_at": emitted_at()
    })
}

fn listen_event_base(
    event: &str,
    status: &str,
    endpoint_slug: &str,
    target_url: &str,
) -> serde_json::Value {
    let mut receipt = command_event_receipt("listen", "listen_endpoint", event, status);
    receipt["endpoint_slug"] = serde_json::json!(endpoint_slug);
    receipt["target_url"] = serde_json::json!(target_url);
    receipt["resource_uri"] = serde_json::json!(listen_session_resource_uri(endpoint_slug));
    receipt
}

fn tunnel_event_base(event: &str, status: &str) -> serde_json::Value {
    command_event_receipt("tunnel", "start_local_tunnel", event, status)
}

fn reconnect_failure_reason(event: &TunnelEvent) -> Option<&str> {
    match event {
        TunnelEvent::ReconnectFailed { reason } => Some(reason),
        _ => None,
    }
}

fn path_with_query(path: &str, query_string: &str) -> String {
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{path}")
    };

    let query_string = query_string.trim().trim_start_matches('?');
    if query_string.is_empty() {
        path
    } else {
        format!("{path}?{query_string}")
    }
}

fn public_tunnel_url(subdomain: &str) -> String {
    let subdomain = subdomain.trim().trim_end_matches('/');
    if subdomain.starts_with("http://") || subdomain.starts_with("https://") {
        subdomain.to_string()
    } else {
        format!("https://{subdomain}")
    }
}

fn local_tunnel_target_url(host: &str, port: u16) -> String {
    format!("http://{host}:{port}")
}

fn local_tunnel_request_target(host: &str, port: u16, path: &str, query_string: &str) -> String {
    format!(
        "{}{}",
        local_tunnel_target_url(host, port),
        path_with_query(path, query_string)
    )
}

fn endpoint_receipt_parts(
    endpoint_slug: &str,
    endpoint: Option<&api::DebugEndpointSummary>,
) -> (String, String, serde_json::Value) {
    match endpoint {
        Some(endpoint) => (
            endpoint_resource_uri(&endpoint.id),
            endpoint_requests_resource_uri(&endpoint.id),
            serde_json::json!({
                "id": &endpoint.id,
                "name": &endpoint.name,
                "slug": &endpoint.slug,
                "status": &endpoint.status,
                "webhook_url": &endpoint.webhook_url,
                "resource_uri": endpoint_resource_uri(&endpoint.id)
            }),
        ),
        None => {
            let endpoint_uri = endpoint_slug_resource_uri(endpoint_slug);
            (
                endpoint_uri.clone(),
                format!("{endpoint_uri}/requests"),
                serde_json::json!({
                    "slug": endpoint_slug,
                    "resource_uri": endpoint_uri,
                    "resolution": "unresolved"
                }),
            )
        }
    }
}

fn listen_started_receipt(
    endpoint_slug: &str,
    target_url: &str,
    ws_url: Option<&str>,
    endpoint: Option<&api::DebugEndpointSummary>,
) -> serde_json::Value {
    let (endpoint_resource_uri, requests_resource_uri, endpoint_value) =
        endpoint_receipt_parts(endpoint_slug, endpoint);
    let session_resource_uri = listen_session_resource_uri(endpoint_slug);
    let ws_url = effective_listen_ws_url(ws_url);
    let inspect_command = endpoint
        .map(|endpoint| format!("hooklistener endpoint requests {}", endpoint.id))
        .unwrap_or_else(|| "hooklistener endpoint list --json".to_string());

    serde_json::json!({
        "type": "receipt",
        "event": "listen_started",
        "status": "running",
        "command": "listen",
        "operation": "listen_endpoint",
        "emitted_at": emitted_at(),
        "resource_uri": &session_resource_uri,
        "endpoint_slug": endpoint_slug,
        "target_url": target_url,
        "ws_url": ws_url,
        "endpoint": endpoint_value,
        "resources": {
            "self": session_resource_uri,
            "endpoint": endpoint_resource_uri,
            "requests": requests_resource_uri
        },
        "next_actions": [
            format!("Send webhook traffic to endpoint slug `{endpoint_slug}`."),
            inspect_command
        ]
    })
}

fn listen_event_receipt(
    event: &TunnelEvent,
    endpoint_slug: &str,
    target_url: &str,
    endpoint: Option<&api::DebugEndpointSummary>,
) -> serde_json::Value {
    let (endpoint_resource_uri, requests_resource_uri, _) =
        endpoint_receipt_parts(endpoint_slug, endpoint);

    match event {
        TunnelEvent::Connecting => {
            listen_event_base("connecting", "connecting", endpoint_slug, target_url)
        }
        TunnelEvent::Connected => {
            let mut receipt =
                listen_event_base("connected", "connected", endpoint_slug, target_url);
            receipt["resources"] = serde_json::json!({
                "endpoint": endpoint_resource_uri,
                "requests": requests_resource_uri
            });
            receipt
        }
        TunnelEvent::WebhookReceived(request) => {
            let request_resource_uri = request_resource_uri(&request.id);
            let mut receipt =
                listen_event_base("webhook_received", "received", endpoint_slug, target_url);
            receipt["request_id"] = serde_json::json!(&request.id);
            receipt["resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["request_resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["endpoint_resource_uri"] = serde_json::json!(endpoint_resource_uri);
            receipt["request"] = serde_json::json!({
                "id": &request.id,
                "method": &request.method,
                "path": request.path.as_deref().unwrap_or(&request.url),
                "url": &request.url,
                "remote_addr": &request.remote_addr,
                "content_length": request.content_length,
                "created_at": &request.created_at,
                "resource_uri": request_resource_uri
            });
            receipt["next_actions"] = serde_json::json!([format!(
                "hooklistener endpoint request <endpoint-id> {}",
                request.id
            )]);
            receipt
        }
        TunnelEvent::ForwardSuccess {
            request_id,
            target_url: forward_target_url,
            status,
            duration_ms,
        } => {
            let request_resource_uri = request_resource_uri(request_id);
            let mut receipt =
                listen_event_base("forward_succeeded", "succeeded", endpoint_slug, target_url);
            receipt["request_id"] = serde_json::json!(request_id);
            receipt["resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["request_resource_uri"] = serde_json::json!(request_resource_uri);
            receipt["target_url"] = serde_json::json!(forward_target_url);
            receipt["status_code"] = serde_json::json!(status);
            receipt["duration_ms"] = serde_json::json!(duration_ms);
            receipt
        }
        TunnelEvent::ForwardError {
            request_id,
            target_url: forward_target_url,
            error,
            duration_ms,
        } => {
            let request_resource_uri = request_resource_uri(request_id);
            let mut receipt =
                listen_event_base("forward_failed", "failed", endpoint_slug, target_url);
            receipt["request_id"] = serde_json::json!(request_id);
            receipt["resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["request_resource_uri"] = serde_json::json!(request_resource_uri);
            receipt["target_url"] = serde_json::json!(forward_target_url);
            receipt["error"] = serde_json::json!(error);
            receipt["duration_ms"] = serde_json::json!(duration_ms);
            receipt["next_actions"] = serde_json::json!([
                "Check that the local target URL is reachable from this machine."
            ]);
            receipt
        }
        TunnelEvent::ConnectionError(error) => {
            let mut receipt =
                listen_event_base("connection_error", "error", endpoint_slug, target_url);
            receipt["error"] = serde_json::json!(error);
            receipt["retryable"] = serde_json::json!(true);
            receipt["next_actions"] =
                serde_json::json!(["Wait for automatic reconnect or restart the command."]);
            receipt
        }
        TunnelEvent::Disconnected => {
            listen_event_base("disconnected", "disconnected", endpoint_slug, target_url)
        }
        TunnelEvent::Reconnecting {
            attempt,
            max_attempts,
            next_retry_in_secs,
        } => {
            let mut receipt =
                listen_event_base("reconnecting", "reconnecting", endpoint_slug, target_url);
            receipt["attempt"] = serde_json::json!(attempt);
            receipt["max_attempts"] = serde_json::json!(max_attempts);
            receipt["next_retry_in_secs"] = serde_json::json!(next_retry_in_secs);
            receipt
        }
        TunnelEvent::ReconnectFailed { reason } => {
            let mut receipt =
                listen_event_base("reconnect_failed", "failed", endpoint_slug, target_url);
            receipt["error"] = serde_json::json!(reason);
            receipt["retryable"] = serde_json::json!(false);
            receipt["next_actions"] = serde_json::json!([
                "Verify authentication, endpoint slug, and network connectivity."
            ]);
            receipt
        }
        _ => listen_event_base("ignored", "ignored", endpoint_slug, target_url),
    }
}

fn tunnel_started_receipt(
    host: &str,
    port: u16,
    organization_id: Option<&str>,
    slug: Option<&str>,
) -> serde_json::Value {
    let session_resource_uri = tunnel_session_resource_uri(host, port);
    let local_target_url = local_tunnel_target_url(host, port);

    serde_json::json!({
        "type": "receipt",
        "event": "tunnel_started",
        "status": "starting",
        "command": "tunnel",
        "operation": "start_local_tunnel",
        "emitted_at": emitted_at(),
        "resource_uri": &session_resource_uri,
        "local_host": host,
        "local_port": port,
        "local_target_url": local_target_url,
        "organization_id": organization_id,
        "requested_slug": slug,
        "resources": {
            "self": session_resource_uri
        },
        "next_actions": ["Wait for a tunnel_established event before sending external traffic."]
    })
}

fn tunnel_event_receipt(
    event: &TunnelEvent,
    host: &str,
    port: u16,
    organization_id: Option<&str>,
    requested_slug: Option<&str>,
) -> serde_json::Value {
    match event {
        TunnelEvent::Connecting => {
            let mut receipt = tunnel_event_base("connecting", "connecting");
            receipt["resource_uri"] = serde_json::json!(tunnel_session_resource_uri(host, port));
            receipt["local_target_url"] = serde_json::json!(local_tunnel_target_url(host, port));
            receipt["organization_id"] = serde_json::json!(organization_id);
            receipt["requested_slug"] = serde_json::json!(requested_slug);
            receipt
        }
        TunnelEvent::TunnelEstablished {
            subdomain,
            tunnel_id,
            is_static,
        } => {
            let tunnel_resource_uri = tunnel_resource_uri(tunnel_id);
            let public_url = public_tunnel_url(subdomain);
            let mut receipt = tunnel_event_base("tunnel_established", "running");
            receipt["type"] = serde_json::json!("receipt");
            receipt["resource_uri"] = serde_json::json!(&tunnel_resource_uri);
            receipt["tunnel_id"] = serde_json::json!(tunnel_id);
            receipt["tunnel_resource_uri"] = serde_json::json!(&tunnel_resource_uri);
            receipt["subdomain"] = serde_json::json!(subdomain);
            receipt["public_url"] = serde_json::json!(public_url);
            receipt["local_host"] = serde_json::json!(host);
            receipt["local_port"] = serde_json::json!(port);
            receipt["local_target_url"] = serde_json::json!(local_tunnel_target_url(host, port));
            receipt["organization_id"] = serde_json::json!(organization_id);
            receipt["requested_slug"] = serde_json::json!(requested_slug);
            receipt["is_static"] = serde_json::json!(is_static);
            receipt["resources"] = serde_json::json!({
                "self": tunnel_resource_uri,
                "session": tunnel_session_resource_uri(host, port)
            });
            receipt["next_actions"] = serde_json::json!([
                format!("Send external traffic to {public_url}."),
                "Watch subsequent request_received and request_forwarded events."
            ]);
            receipt
        }
        TunnelEvent::RequestReceived {
            request_id,
            method,
            path,
            headers,
            body,
            query_string,
        } => {
            let request_resource_uri = request_resource_uri(request_id);
            let mut receipt = tunnel_event_base("request_received", "received");
            receipt["request_id"] = serde_json::json!(request_id);
            receipt["resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["request_resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["method"] = serde_json::json!(method);
            receipt["path"] = serde_json::json!(path);
            receipt["query_string"] = serde_json::json!(query_string);
            receipt["local_target_url"] =
                serde_json::json!(local_tunnel_request_target(host, port, path, query_string));
            receipt["headers"] = serde_json::json!(headers);
            receipt["body_size"] = serde_json::json!(body.as_ref().map(String::len).unwrap_or(0));
            receipt
        }
        TunnelEvent::RequestForwarded {
            request_id,
            status,
            duration_ms,
            response_headers,
            response_body,
        } => {
            let request_resource_uri = request_resource_uri(request_id);
            let mut receipt = tunnel_event_base("request_forwarded", "succeeded");
            receipt["request_id"] = serde_json::json!(request_id);
            receipt["resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["request_resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["status_code"] = serde_json::json!(status);
            receipt["duration_ms"] = serde_json::json!(duration_ms);
            receipt["response_headers"] = serde_json::json!(response_headers);
            receipt["response_body_size"] =
                serde_json::json!(response_body.as_ref().map(String::len).unwrap_or(0));
            receipt
        }
        TunnelEvent::RequestFailed { request_id, error } => {
            let request_resource_uri = request_resource_uri(request_id);
            let mut receipt = tunnel_event_base("request_failed", "failed");
            receipt["request_id"] = serde_json::json!(request_id);
            receipt["resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["request_resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["error"] = serde_json::json!(error);
            receipt["next_actions"] =
                serde_json::json!(["Check that the local target is running and reachable."]);
            receipt
        }
        TunnelEvent::ConnectionError(error) => {
            let mut receipt = tunnel_event_base("connection_error", "error");
            receipt["resource_uri"] = serde_json::json!(tunnel_session_resource_uri(host, port));
            receipt["local_target_url"] = serde_json::json!(local_tunnel_target_url(host, port));
            receipt["error"] = serde_json::json!(error);
            receipt["retryable"] = serde_json::json!(true);
            receipt["next_actions"] =
                serde_json::json!(["Wait for automatic reconnect or restart the command."]);
            receipt
        }
        TunnelEvent::Disconnected => {
            let mut receipt = tunnel_event_base("disconnected", "disconnected");
            receipt["resource_uri"] = serde_json::json!(tunnel_session_resource_uri(host, port));
            receipt["local_target_url"] = serde_json::json!(local_tunnel_target_url(host, port));
            receipt
        }
        TunnelEvent::Reconnecting {
            attempt,
            max_attempts,
            next_retry_in_secs,
        } => {
            let mut receipt = tunnel_event_base("reconnecting", "reconnecting");
            receipt["resource_uri"] = serde_json::json!(tunnel_session_resource_uri(host, port));
            receipt["local_target_url"] = serde_json::json!(local_tunnel_target_url(host, port));
            receipt["attempt"] = serde_json::json!(attempt);
            receipt["max_attempts"] = serde_json::json!(max_attempts);
            receipt["next_retry_in_secs"] = serde_json::json!(next_retry_in_secs);
            receipt
        }
        TunnelEvent::ReconnectFailed { reason } => {
            let mut receipt = tunnel_event_base("reconnect_failed", "failed");
            receipt["resource_uri"] = serde_json::json!(tunnel_session_resource_uri(host, port));
            receipt["local_target_url"] = serde_json::json!(local_tunnel_target_url(host, port));
            receipt["error"] = serde_json::json!(reason);
            receipt["retryable"] = serde_json::json!(false);
            receipt["next_actions"] = serde_json::json!([
                "Verify authentication, organization scope, requested slug, and network connectivity."
            ]);
            receipt
        }
        _ => {
            let mut receipt = tunnel_event_base("ignored", "ignored");
            receipt["resource_uri"] = serde_json::json!(tunnel_session_resource_uri(host, port));
            receipt["local_target_url"] = serde_json::json!(local_tunnel_target_url(host, port));
            receipt
        }
    }
}

fn forward_method(method: Option<&str>, request: &api::DebugRequestDetail) -> String {
    method
        .map(str::to_string)
        .unwrap_or_else(|| request.method.clone())
}

fn forward_request_preview_receipt(
    organization_id: &str,
    endpoint_id: &str,
    request_id: &str,
    target_url: &str,
    method: Option<&str>,
    request: &api::DebugRequestDetail,
) -> serde_json::Value {
    let method = forward_method(method, request);
    let request_resource_uri = request_resource_uri(request_id);
    let next_action = format!(
        "Run `hooklistener endpoint forward-request {endpoint_id} {request_id} {target_url}` without --dry-run to queue the forward."
    );

    serde_json::json!({
        "dry_run": true,
        "status": "preview",
        "command": "endpoint forward-request",
        "operation": "forward_request",
        "would_create": "debug_request_forward",
        "risk_level": "external_side_effect",
        "required_confirmation": true,
        "organization_id": organization_id,
        "endpoint_id": endpoint_id,
        "request_id": request_id,
        "target_url": target_url,
        "method": method,
        "source_request": {
            "id": &request.id,
            "method": &request.method,
            "path": &request.path,
            "url": &request.url,
            "resource_uri": &request_resource_uri
        },
        "request_resource_uri": request_resource_uri,
        "receipt_resource_uri_template": "hooklistener://forwards/{forward_id}",
        "next_action": next_action
    })
}

fn forward_request_receipt(
    organization_id: &str,
    endpoint_id: &str,
    request_id: &str,
    response: &api::EndpointRequestForwardResponse,
) -> serde_json::Value {
    let forward_resource_uri = forward_resource_uri(&response.forward_id);
    let request_resource_uri = request_resource_uri(request_id);
    let request_forwards_resource_uri = request_forwards_resource_uri(request_id);
    let poll_url = forward_poll_path(&response.forward_id);
    let poll_command = forward_poll_command(&response.forward_id);
    let forwards_command = format!("hooklistener endpoint forwards {endpoint_id} {request_id}");

    serde_json::json!({
        "status": &response.status,
        "delivery_status": "queued",
        "command": "endpoint forward-request",
        "operation": "forward_request",
        "organization_id": organization_id,
        "endpoint_id": endpoint_id,
        "request_id": request_id,
        "forward_id": &response.forward_id,
        "resource_uri": &forward_resource_uri,
        "forward_resource_uri": &forward_resource_uri,
        "request_resource_uri": &request_resource_uri,
        "poll_url": poll_url,
        "resources": {
            "self": forward_resource_uri,
            "request": request_resource_uri,
            "request_forwards": request_forwards_resource_uri
        },
        "next_actions": [
            poll_command,
            forwards_command
        ],
        "forward": response
    })
}

fn print_forward_request_preview(
    organization_id: &str,
    endpoint_id: &str,
    request_id: &str,
    target_url: &str,
    method: Option<&str>,
    request: &api::DebugRequestDetail,
) {
    let method = forward_method(method, request);
    let request_resource_uri = request_resource_uri(request_id);

    print_status_block(
        OutputStatus::Info,
        "FORWARD PREVIEW",
        &[
            output_field("DRY RUN", "true"),
            output_field("WOULD CREATE", "debug_request_forward"),
            output_field("TARGET URL", target_url.underlined()),
            output_field("METHOD", method.bold()),
            output_field("REQUEST", request_id.dim()),
            output_field("RESOURCE", request_resource_uri.dim()),
            output_field("ENDPOINT", endpoint_id.dim()),
            output_field("ORGANIZATION", organization_id.dim()),
            output_field(
                "NEXT",
                "Run without --dry-run to queue the forward; receipt will be hooklistener://forwards/<forward_id>.",
            ),
        ],
    );
}

fn print_forward_request_accepted(
    organization_id: &str,
    endpoint_id: &str,
    request_id: &str,
    response: &api::EndpointRequestForwardResponse,
) {
    print_status_block(
        OutputStatus::Ok,
        "FORWARD ACCEPTED",
        &[
            output_field("FORWARD ID", response.forward_id.as_str().bold()),
            output_field("STATUS", response.status.as_str().bold()),
            output_field("TARGET URL", response.target_url.as_str().underlined()),
            output_field("RESOURCE", forward_resource_uri(&response.forward_id).dim()),
            output_field("POLL", forward_poll_command(&response.forward_id).dim()),
            output_field("REQUEST", request_id.dim()),
            output_field("ENDPOINT", endpoint_id.dim()),
            output_field("ORGANIZATION", organization_id.dim()),
        ],
    );
}

async fn run_endpoint_forward_request(
    endpoint_id: String,
    request_id: String,
    target_url: String,
    method: Option<String>,
    dry_run: bool,
    org: Option<String>,
    json: bool,
) -> Result<()> {
    let mut config = config::Config::load()?;
    let organization_id = require_organization(org, &config)?;
    let token = ensure_valid_token(&mut config).await?;
    let normalized_method = normalize_http_method(method)?;
    validate_forward_target_url(&target_url)?;
    let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;

    if dry_run {
        let request = client
            .get_endpoint_request(&endpoint_id, &request_id)
            .await?;

        if json {
            print_json(&forward_request_preview_receipt(
                &organization_id,
                &endpoint_id,
                &request_id,
                &target_url,
                normalized_method.as_deref(),
                &request,
            ))?;
        } else {
            print_forward_request_preview(
                &organization_id,
                &endpoint_id,
                &request_id,
                &target_url,
                normalized_method.as_deref(),
                &request,
            );
        }

        return Ok(());
    }

    let response = client
        .forward_endpoint_request(
            &endpoint_id,
            &request_id,
            &target_url,
            normalized_method.as_deref(),
        )
        .await?;

    if json {
        print_json(&forward_request_receipt(
            &organization_id,
            &endpoint_id,
            &request_id,
            &response,
        ))?;
    } else {
        print_forward_request_accepted(&organization_id, &endpoint_id, &request_id, &response);
    }

    Ok(())
}

pub(crate) fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

fn print_json_line<T: serde::Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    io::stdout().flush()?;
    Ok(())
}

async fn resolve_listen_endpoint(
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

async fn stream_listen_json_events(
    mut event_rx: mpsc::Receiver<TunnelEvent>,
    endpoint_slug: &str,
    target_url: &str,
    endpoint: Option<&api::DebugEndpointSummary>,
) -> Result<()> {
    loop {
        tokio::select! {
            maybe_event = event_rx.recv() => {
                let Some(event) = maybe_event else {
                    return Ok(());
                };
                let failure_reason = reconnect_failure_reason(&event);

                print_json_line(&listen_event_receipt(&event, endpoint_slug, target_url, endpoint))?;

                if let Some(reason) = failure_reason {
                    return Err(anyhow!("Connection lost: {reason}"));
                }
            }
            signal = tokio::signal::ctrl_c() => {
                signal?;
                print_json_line(&serde_json::json!({
                    "type": "event",
                    "event": "stopped",
                    "status": "stopped",
                    "command": "listen",
                    "operation": "listen_endpoint",
                    "emitted_at": emitted_at(),
                    "endpoint_slug": endpoint_slug,
                    "target_url": target_url,
                    "resource_uri": listen_session_resource_uri(endpoint_slug)
                }))?;
                return Ok(());
            }
        }
    }
}

async fn run_listen_json(
    access_token_rx: watch::Receiver<String>,
    endpoint_slug: String,
    target_url: String,
    ws_url: Option<String>,
    organization_id: Option<String>,
) -> Result<()> {
    let access_token = access_token_rx.borrow().clone();
    let endpoint = resolve_listen_endpoint(&access_token, organization_id, &endpoint_slug).await;

    print_json_line(&listen_started_receipt(
        &endpoint_slug,
        &target_url,
        ws_url.as_deref(),
        endpoint.as_ref(),
    ))?;

    let (event_tx, event_rx) = mpsc::channel(100);
    let tunnel_client = tunnel::TunnelClient::new(
        access_token_rx,
        endpoint_slug.clone(),
        target_url.clone(),
        ws_url,
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

    stream_listen_json_events(event_rx, &endpoint_slug, &target_url, endpoint.as_ref()).await
}

async fn stream_tunnel_json_events(
    mut event_rx: mpsc::Receiver<TunnelEvent>,
    host: &str,
    port: u16,
    organization_id: Option<&str>,
    requested_slug: Option<&str>,
) -> Result<()> {
    loop {
        tokio::select! {
            maybe_event = event_rx.recv() => {
                let Some(event) = maybe_event else {
                    return Ok(());
                };
                let failure_reason = reconnect_failure_reason(&event);

                print_json_line(&tunnel_event_receipt(
                    &event,
                    host,
                    port,
                    organization_id,
                    requested_slug,
                ))?;

                if let Some(reason) = failure_reason {
                    return Err(anyhow!("Connection lost: {reason}"));
                }
            }
            signal = tokio::signal::ctrl_c() => {
                signal?;
                print_json_line(&serde_json::json!({
                    "type": "event",
                    "event": "stopped",
                    "status": "stopped",
                    "command": "tunnel",
                    "operation": "start_local_tunnel",
                    "emitted_at": emitted_at(),
                    "resource_uri": tunnel_session_resource_uri(host, port),
                    "local_target_url": local_tunnel_target_url(host, port)
                }))?;
                return Ok(());
            }
        }
    }
}

async fn run_tunnel_json(
    access_token_rx: watch::Receiver<String>,
    host: String,
    port: u16,
    organization_id: Option<String>,
    slug: Option<String>,
) -> Result<()> {
    print_json_line(&tunnel_started_receipt(
        &host,
        port,
        organization_id.as_deref(),
        slug.as_deref(),
    ))?;

    let (event_tx, event_rx) = mpsc::channel(100);
    tokio::spawn(run_tunnel_forwarder_connection(
        access_token_rx,
        host.clone(),
        port,
        organization_id.clone(),
        slug.clone(),
        event_tx,
    ));

    stream_tunnel_json_events(
        event_rx,
        &host,
        port,
        organization_id.as_deref(),
        slug.as_deref(),
    )
    .await
}

const SESSION_TOKEN_VALIDITY_DAYS: i64 = 60;

#[tokio::main]
async fn main() -> Result<()> {
    let Cli {
        command,
        json,
        log_level,
        log_dir,
        log_stdout,
    } = Cli::parse();

    let Some(command) = command else {
        Cli::command().print_help()?;
        println!();
        return Ok(());
    };

    // Spawn background version check for non-interactive, non-update commands
    let update_handle =
        if !json && !matches!(command, Commands::Update | Commands::Completions { .. }) {
            config::Config::load()
                .ok()
                .and_then(|cfg| updater::spawn_version_check(&cfg))
        } else {
            None
        };

    match command {
        Commands::Login { force } => {
            let log_config = LogConfig {
                level: log_level.clone(),
                output_to_stdout: log_stdout,
                directory: log_dir
                    .clone()
                    .unwrap_or_else(|| LogConfig::default().directory),
                ..Default::default()
            };
            let _logger = Logger::new(log_config)?;
            run_login_flow(force).await?;
        }
        Commands::Listen {
            endpoint,
            target,
            ws_url,
        } => {
            // Initialize logging for tunnel
            let log_config = LogConfig {
                level: log_level.clone(),
                output_to_stdout: false, // Disable stdout logging for TUI
                directory: log_dir
                    .clone()
                    .unwrap_or_else(|| LogConfig::default().directory),
                ..Default::default()
            };
            let _logger = Logger::new(log_config)?;

            let mut config = config::Config::load()?;
            let access_token = ensure_valid_token(&mut config).await?;
            let selected_organization_id = config.selected_organization_id.clone();
            let access_token_rx = refreshed_access_token_rx(access_token, config);

            if json {
                run_listen_json(
                    access_token_rx,
                    endpoint,
                    target,
                    ws_url,
                    selected_organization_id,
                )
                .await?;
            } else {
                // Setup TUI for listen command
                let mut terminal = setup_terminal()?;
                let mut app = App::new()?;

                // Set app state to listening
                app.state = AppState::Listening;
                app.listening_endpoint = endpoint.clone();
                app.listening_target = target.clone();

                // Create channel for tunnel events
                let (event_tx, event_rx) = mpsc::channel(100);

                // Create and spawn tunnel client
                let tunnel_client = tunnel::TunnelClient::new(
                    access_token_rx,
                    endpoint.clone(),
                    target.clone(),
                    ws_url,
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
                let res =
                    run_app(&mut terminal, &mut app, event_rx, None, None, Some(logo_rx)).await;

                restore_terminal(&mut terminal)?;

                if let Err(err) = res {
                    error!(error = %err, "Application terminated with error");
                    display_error(&err);
                }
            }
        }
        Commands::Diagnostics { output } => {
            // Initialize minimal logging for diagnostics
            let log_config = LogConfig {
                level: "info".to_string(),
                // Keep machine-readable output clean when --json is enabled.
                output_to_stdout: !json,
                ..Default::default()
            };
            let logger = Logger::new(log_config)?;
            logger.create_diagnostic_bundle(&output)?;
            if json {
                print_json(&serde_json::json!({
                    "status": "ok",
                    "output": output.display().to_string()
                }))?;
            } else {
                print_status_block(
                    OutputStatus::Ok,
                    "DIAGNOSTIC BUNDLE CREATED",
                    &[output_field("OUTPUT", output.display().to_string().bold())],
                );
            }
        }
        Commands::CleanLogs { keep } => {
            let directory = log_dir
                .clone()
                .unwrap_or_else(|| LogConfig::default().directory);
            std::fs::create_dir_all(&directory)?;
            let removed = Logger::cleanup_old_logs(&directory, keep)?;
            if json {
                print_json(&serde_json::json!({
                    "removed": removed,
                    "directory": directory.display().to_string(),
                    "keep": keep
                }))?;
            } else {
                print_status_block(
                    OutputStatus::Ok,
                    "LOGS CLEANED",
                    &[
                        output_field("REMOVED", removed.to_string().bold()),
                        output_field("DIRECTORY", directory.display().to_string().dim()),
                        output_field("KEEP", keep.to_string().bold()),
                    ],
                );
            }
        }
        Commands::Config { action } => match action {
            ConfigAction::Show => {
                let config = config::Config::load()?;
                let config_path = config::Config::config_path()?;
                if json {
                    let token_status = if config.access_token.is_none() {
                        "none"
                    } else if config.is_token_valid() {
                        "valid"
                    } else {
                        "expired"
                    };

                    print_json(&serde_json::json!({
                        "config_path": config_path.display().to_string(),
                        "token": {
                            "present": config.access_token.is_some(),
                            "status": token_status
                        },
                        "organization_id": config.selected_organization_id
                    }))?;
                } else {
                    print_field("CONFIG FILE", config_path.display());
                    println!();
                    match &config.access_token {
                        Some(token) => {
                            let truncated = if token.len() > 8 {
                                format!("{}...", &token[..8])
                            } else {
                                token.clone()
                            };
                            if config.is_token_valid() {
                                print_field("TOKEN", format!("{truncated} {}", "(valid)".green()));
                            } else {
                                print_field("TOKEN", format!("{truncated} {}", "(expired)".red()));
                            }
                        }
                        None => print_field("TOKEN", "(none)".dim()),
                    }
                    match &config.selected_organization_id {
                        Some(org_id) => print_field("ORGANIZATION", org_id),
                        None => print_field("ORGANIZATION", "(none)".dim()),
                    }
                }
            }
            ConfigAction::Set { key, value } => match key.as_str() {
                "selected_organization_id" => {
                    let mut config = config::Config::load()?;
                    if value == "none" {
                        config.selected_organization_id = None;
                        config.save()?;
                        if json {
                            print_json(&serde_json::json!({
                                "status": "ok",
                                "key": "selected_organization_id",
                                "value": null
                            }))?;
                        } else {
                            print_status_block(
                                OutputStatus::Ok,
                                "CONFIG CLEARED",
                                &[
                                    output_field("KEY", "selected_organization_id"),
                                    output_field("VALUE", "(none)".dim()),
                                ],
                            );
                        }
                    } else {
                        config.selected_organization_id = Some(value);
                        config.save()?;
                        if json {
                            print_json(&serde_json::json!({
                                "status": "ok",
                                "key": "selected_organization_id",
                                "value": config.selected_organization_id
                            }))?;
                        } else {
                            print_status_block(
                                OutputStatus::Ok,
                                "CONFIG SET",
                                &[
                                    output_field("KEY", "selected_organization_id"),
                                    output_field(
                                        "VALUE",
                                        config.selected_organization_id.as_deref().unwrap().bold(),
                                    ),
                                ],
                            );
                        }
                    }
                }
                _ => {
                    eprint_status_block(
                        OutputStatus::Err,
                        "UNKNOWN CONFIG KEY",
                        &[
                            output_field("KEY", key),
                            output_field("AVAILABLE", "selected_organization_id"),
                        ],
                    );
                    std::process::exit(1);
                }
            },
        },
        Commands::Logout => {
            let mut config = config::Config::load()?;
            if config.access_token.is_none() && config.token_expires_at.is_none() {
                if json {
                    print_json(&serde_json::json!({
                        "status": "already_logged_out"
                    }))?;
                } else {
                    print_status(OutputStatus::Info, "ALREADY LOGGED OUT");
                }
            } else {
                // Best-effort revoke refresh token server-side
                if let Some(ref refresh_token) = config.refresh_token {
                    let _ = api::revoke_refresh_token(refresh_token).await;
                }
                config.clear_token();
                config.save()?;
                if json {
                    print_json(&serde_json::json!({
                        "status": "logged_out"
                    }))?;
                } else {
                    print_status(OutputStatus::Ok, "LOGGED OUT");
                }
            }
        }
        Commands::Org { action } => match action {
            OrgAction::List => {
                let mut config = config::Config::load()?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, None)?;
                let organizations = client.list_organizations().await?;
                if json {
                    print_json(&serde_json::json!({
                        "selected_organization_id": config.selected_organization_id,
                        "organizations": organizations
                    }))?;
                } else {
                    print_organizations(&organizations, config.selected_organization_id.as_deref());
                }
            }
            OrgAction::Use { id } => {
                let mut config = config::Config::load()?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, None)?;
                let organizations = client.list_organizations().await?;
                let organization_name = organizations
                    .iter()
                    .find(|org| org.id == id)
                    .map(|org| org.name.clone())
                    .ok_or_else(|| {
                        anyhow!("Organization not found or not accessible with id: {}", id)
                    })?;

                config.selected_organization_id = Some(id.clone());
                config.save()?;
                if json {
                    print_json(&serde_json::json!({
                        "status": "ok",
                        "selected_organization_id": id,
                        "organization_name": organization_name
                    }))?;
                } else {
                    print_status_block(
                        OutputStatus::Ok,
                        "ORGANIZATION SELECTED",
                        &[
                            output_field("NAME", organization_name.bold()),
                            output_field("ORGANIZATION", id.dim()),
                        ],
                    );
                }
            }
            OrgAction::Clear => {
                let mut config = config::Config::load()?;
                config.selected_organization_id = None;
                config.save()?;
                if json {
                    print_json(&serde_json::json!({
                        "status": "ok",
                        "selected_organization_id": null
                    }))?;
                } else {
                    print_status(OutputStatus::Ok, "ORGANIZATION CLEARED");
                }
            }
        },
        Commands::Endpoint { action } => match action {
            EndpointAction::Create { name, slug, org } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let endpoint = client.create_endpoint(&name, slug.as_deref()).await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "endpoint": endpoint
                    }))?;
                } else {
                    print_status(OutputStatus::Ok, "ENDPOINT CREATED");
                    println!();
                    print_endpoint_detail(&endpoint);
                    print_field("ORGANIZATION", &organization_id);
                    print_field(
                        "ACTION",
                        format!("Run `hooklistener listen {}`", endpoint.slug).dim(),
                    );
                }
            }
            EndpointAction::List { org } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let endpoints = client.list_endpoints().await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "endpoints": endpoints
                    }))?;
                } else {
                    print_context("Organization:", &organization_id);
                    print_endpoints(&endpoints);
                }
            }
            EndpointAction::Show { endpoint_id, org } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let endpoint = client.get_endpoint(&endpoint_id).await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "endpoint": endpoint
                    }))?;
                } else {
                    print_endpoint_detail(&endpoint);
                    print_field("ORGANIZATION", &organization_id);
                }
            }
            EndpointAction::Delete { endpoint_id, org } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                client.delete_endpoint(&endpoint_id).await?;
                if json {
                    print_json(&serde_json::json!({
                        "status": "deleted",
                        "organization_id": organization_id,
                        "endpoint_id": endpoint_id
                    }))?;
                } else {
                    print_status_block(
                        OutputStatus::Ok,
                        "ENDPOINT DELETED",
                        &[
                            output_field("ENDPOINT", endpoint_id.bold()),
                            output_field("ORGANIZATION", organization_id.dim()),
                        ],
                    );
                }
            }
            EndpointAction::Requests {
                endpoint_id,
                page,
                page_size,
                org,
            } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let requests = client
                    .list_endpoint_requests(&endpoint_id, page, page_size)
                    .await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "endpoint_id": endpoint_id,
                        "requests": requests
                    }))?;
                } else {
                    print_context("Organization:", &organization_id);
                    print_context("Endpoint:", &endpoint_id);
                    print_endpoint_requests(&requests);
                }
            }
            EndpointAction::Request {
                endpoint_id,
                request_id,
                org,
            } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let request = client
                    .get_endpoint_request(&endpoint_id, &request_id)
                    .await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "endpoint_id": endpoint_id,
                        "request": request
                    }))?;
                } else {
                    print_context("Organization:", &organization_id);
                    print_context("Endpoint:", &endpoint_id);
                    print_endpoint_request_detail(&request);
                }
            }
            EndpointAction::DeleteRequest {
                endpoint_id,
                request_id,
                org,
            } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                client
                    .delete_endpoint_request(&endpoint_id, &request_id)
                    .await?;
                if json {
                    print_json(&serde_json::json!({
                        "status": "deleted",
                        "organization_id": organization_id,
                        "endpoint_id": endpoint_id,
                        "request_id": request_id
                    }))?;
                } else {
                    print_status_block(
                        OutputStatus::Ok,
                        "REQUEST DELETED",
                        &[
                            output_field("REQUEST", request_id.bold()),
                            output_field("ENDPOINT", endpoint_id.dim()),
                            output_field("ORGANIZATION", organization_id.dim()),
                        ],
                    );
                }
            }
            EndpointAction::ForwardRequest {
                endpoint_id,
                request_id,
                target_url,
                method,
                dry_run,
                org,
            } => {
                run_endpoint_forward_request(
                    endpoint_id,
                    request_id,
                    target_url,
                    method,
                    dry_run,
                    org,
                    json,
                )
                .await?;
            }
            EndpointAction::Forwards {
                endpoint_id,
                request_id,
                page,
                page_size,
                org,
            } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let forwards = client
                    .list_endpoint_request_forwards(&endpoint_id, &request_id, page, page_size)
                    .await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "endpoint_id": endpoint_id,
                        "request_id": request_id,
                        "forwards": forwards
                    }))?;
                } else {
                    print_context("Organization:", &organization_id);
                    print_context("Endpoint:", &endpoint_id);
                    print_context("Request:", &request_id);
                    print_endpoint_request_forwards(&forwards);
                }
            }
            EndpointAction::Forward { forward_id, org } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let forward = client.get_forward(&forward_id).await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "forward": forward
                    }))?;
                } else {
                    print_context("Organization:", &organization_id);
                    print_forward_detail(&forward);
                }
            }
        },
        Commands::Cases { action } => match action {
            CasesAction::Run {
                endpoint_id,
                target,
                target_url,
                target_id,
                target_name,
                wait,
                timeout,
                timeout_ms,
                interval_ms,
                org,
            } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let params = build_case_run_params(CaseRunInput {
                    target,
                    target_url,
                    target_id,
                    target_name,
                    wait,
                    timeout,
                    timeout_ms,
                    interval_ms,
                })?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let result = client.run_endpoint_cases(&endpoint_id, &params).await?;

                if json {
                    print_json(&result)?;
                } else {
                    print_context("Organization:", &organization_id);
                    print_case_run_result(&result);
                }

                if case_run_failed(&result) {
                    std::process::exit(1);
                }
            }
        },
        Commands::StaticTunnel { action } => match action {
            StaticTunnelAction::List { org } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let tunnels = client.list_static_tunnels(&organization_id).await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "tunnels": tunnels
                    }))?;
                } else {
                    print_context("Organization:", &organization_id);
                    print_static_tunnels(&tunnels);
                }
            }
            StaticTunnelAction::Create { slug, name, org } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let created = client
                    .create_static_tunnel(&organization_id, &slug, name.as_deref())
                    .await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "result": created
                    }))?;
                } else {
                    print_status(OutputStatus::Ok, "STATIC TUNNEL CREATED");
                    println!();
                    print_field("ID", &created.static_tunnel.id);
                    print_field("SLUG", created.static_tunnel.slug.as_str().bold());
                    if let Some(name) = created.static_tunnel.name.as_deref() {
                        print_field("NAME", name);
                    }
                    print_field("ORGANIZATION", organization_id.dim());
                    if let Some(message) = created.message {
                        print_field("MESSAGE", message.dim());
                    }
                }
            }
            StaticTunnelAction::Delete { slug_id, org } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let response = client
                    .delete_static_tunnel(&organization_id, &slug_id)
                    .await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "slug_id": slug_id,
                        "status": "deleted",
                        "message": response.message
                    }))?;
                } else {
                    print_status(OutputStatus::Ok, "STATIC TUNNEL DELETED");
                    println!();
                    print_field("SLUG/ID", slug_id);
                    print_field("ORGANIZATION", organization_id.dim());
                    if let Some(message) = response.message {
                        print_field("MESSAGE", message.dim());
                    }
                }
            }
        },
        Commands::Anon { action } => match action {
            AnonAction::Create { ttl } => {
                let client = ApiClient::unauthenticated()?;
                let endpoint = client.create_anon_endpoint(ttl).await?;
                if json {
                    print_json(&endpoint)?;
                } else {
                    print_status(OutputStatus::Ok, "ANONYMOUS ENDPOINT CREATED");
                    println!();
                    print_field("ID", endpoint.id);
                    print_field("WEBHOOK URL", endpoint.webhook_url.as_str().underlined());
                    print_field("EXPIRES AT", endpoint.expires_at.dim());
                    println!();
                    print_field("VIEWER TOKEN", endpoint.viewer_token.as_str().yellow());
                    print_field(
                        "ACTION",
                        "Save this token. It is required to list captured events.".dim(),
                    );
                }
            }
            AnonAction::Show { id } => {
                let client = ApiClient::unauthenticated()?;
                let status = client.get_anon_endpoint(&id).await?;
                if json {
                    print_json(&status)?;
                } else {
                    print_status(OutputStatus::Info, "ANONYMOUS ENDPOINT");
                    println!();
                    print_field("ID", status.id);
                    let active_str = if status.active {
                        "active".green().to_string()
                    } else {
                        "expired".red().to_string()
                    };
                    print_field("STATUS", active_str);
                    if let Some(expires) = status.expires_at.as_deref() {
                        print_field("EXPIRES AT", expires.dim());
                    }
                    if let Some(url) = status.webhook_url.as_deref() {
                        print_field("WEBHOOK URL", url.underlined());
                    }
                    if let Some(error) = status.error.as_deref() {
                        print_field("ERROR", error);
                    }
                }
            }
            AnonAction::Events {
                endpoint_id,
                token,
                page,
                page_size,
            } => {
                let client = ApiClient::with_organization(token, None)?;
                let response = client
                    .list_anon_events(&endpoint_id, page, page_size)
                    .await?;
                if json {
                    print_json(&serde_json::json!({
                        "endpoint_id": endpoint_id,
                        "events": response
                    }))?;
                } else {
                    print_context("Endpoint:", &endpoint_id);
                    print_anon_events(&response);
                }
            }
            AnonAction::Event {
                endpoint_id,
                event_id,
                token,
            } => {
                let client = ApiClient::with_organization(token, None)?;
                let event = client.get_anon_event(&endpoint_id, &event_id).await?;
                if json {
                    print_json(&serde_json::json!({
                        "endpoint_id": endpoint_id,
                        "event": event
                    }))?;
                } else {
                    print_context("Endpoint:", &endpoint_id);
                    print_anon_event_detail(&event);
                }
            }
        },
        Commands::Share { action } => match action {
            ShareAction::Create {
                debug_request_id,
                expires_in_hours,
                password,
                include_forwards,
                org,
            } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let shared = client
                    .create_shared_request(
                        &debug_request_id,
                        expires_in_hours,
                        password.as_deref(),
                        include_forwards,
                    )
                    .await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "shared_request": shared
                    }))?;
                } else {
                    print_status(OutputStatus::Ok, "SHARE LINK CREATED");
                    println!();
                    print_field("SHARE TOKEN", shared.share_token);
                    if let Some(url) = shared.share_url.as_deref() {
                        print_field("SHARE URL", url.underlined());
                    }
                    print_field("REQUEST ID", shared.debug_request_id);
                    print_field("ORGANIZATION", &organization_id);
                    if shared.password_protected {
                        print_field("PASSWORD", "protected".yellow());
                    }
                    if let Some(expires) = shared.expires_at.as_deref() {
                        print_field("EXPIRES AT", expires.dim());
                    }
                    print_field("INCLUDE FWDS", shared.include_forwards);
                }
            }
            ShareAction::List {
                debug_request_id,
                org,
            } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let shares = client.list_shared_requests(&debug_request_id).await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "debug_request_id": debug_request_id,
                        "shares": shares
                    }))?;
                } else {
                    print_context("Organization:", &organization_id);
                    print_context("Request:", &debug_request_id);
                    print_shared_requests(&shares);
                }
            }
            ShareAction::Show { token } => {
                let client = ApiClient::unauthenticated()?;
                let data = client.get_shared_request(&token).await?;
                if json {
                    print_json(&data)?;
                } else {
                    // Check if it's a protected share
                    if data.get("protected").and_then(|v| v.as_bool()) == Some(true) {
                        print_status(OutputStatus::Info, "PROTECTED SHARE");
                        println!();
                        if let Some(expires) = data.get("expires_at").and_then(|v| v.as_str()) {
                            print_field("EXPIRES AT", expires.dim());
                        }
                        print_field(
                            "ACTION",
                            "Use the web UI or API to authenticate with the password.".dim(),
                        );
                    } else {
                        print_shared_request_full(&data);
                    }
                }
            }
            ShareAction::Revoke { token, org } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let access_token = ensure_valid_token(&mut config).await?;
                let client =
                    ApiClient::with_organization(access_token, Some(organization_id.clone()))?;
                client.revoke_shared_request(&token).await?;
                if json {
                    print_json(&serde_json::json!({
                        "status": "revoked",
                        "organization_id": organization_id,
                        "share_token": token
                    }))?;
                } else {
                    print_status_block(
                        OutputStatus::Ok,
                        "SHARE REVOKED",
                        &[
                            output_field("SHARE TOKEN", token.bold()),
                            output_field("ORGANIZATION", organization_id.dim()),
                        ],
                    );
                }
            }
        },
        Commands::Monitor { action } => match action {
            MonitorAction::Create {
                name,
                url,
                method,
                expected_status,
                interval,
                body_contains,
                body,
                failure_threshold,
                email,
                org,
            } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;

                let mut params = serde_json::json!({
                    "name": name,
                    "url": url,
                    "method": method.to_lowercase(),
                    "expected_status_code": expected_status,
                    "check_interval": interval,
                    "failure_threshold": failure_threshold,
                    "email_enabled": email,
                });
                if let Some(bc) = body_contains {
                    params["body_contains"] = serde_json::Value::String(bc);
                }
                if let Some(b) = body {
                    params["body"] = serde_json::Value::String(b);
                }

                let monitor = client.create_uptime_monitor(&params).await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "monitor": monitor
                    }))?;
                } else {
                    print_status(OutputStatus::Ok, "UPTIME MONITOR CREATED");
                    println!();
                    print_monitor_detail(&monitor);
                    print_field("ORGANIZATION", &organization_id);
                }
            }
            MonitorAction::List { org } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let monitors = client.list_uptime_monitors().await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "monitors": monitors
                    }))?;
                } else {
                    print_context("Organization:", &organization_id);
                    print_monitors(&monitors);
                }
            }
            MonitorAction::Show { id, org } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let monitor = client.get_uptime_monitor(&id).await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "monitor": monitor
                    }))?;
                } else {
                    print_context("Organization:", &organization_id);
                    print_monitor_detail(&monitor);
                }
            }
            MonitorAction::Update {
                id,
                name,
                url,
                method,
                expected_status,
                interval,
                enabled,
                failure_threshold,
                org,
            } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;

                let mut params = serde_json::Map::new();
                if let Some(v) = name {
                    params.insert("name".into(), serde_json::Value::String(v));
                }
                if let Some(v) = url {
                    params.insert("url".into(), serde_json::Value::String(v));
                }
                if let Some(v) = method {
                    params.insert("method".into(), serde_json::Value::String(v.to_lowercase()));
                }
                if let Some(v) = expected_status {
                    params.insert("expected_status_code".into(), v.into());
                }
                if let Some(v) = interval {
                    params.insert("check_interval".into(), v.into());
                }
                if let Some(v) = enabled {
                    params.insert("enabled".into(), serde_json::Value::Bool(v));
                }
                if let Some(v) = failure_threshold {
                    params.insert("failure_threshold".into(), v.into());
                }

                if params.is_empty() {
                    return Err(anyhow!(
                        "No fields to update. Use --name, --url, --method, --expected-status, --interval, --enabled, or --failure-threshold."
                    ));
                }

                let monitor = client
                    .update_uptime_monitor(&id, &serde_json::Value::Object(params))
                    .await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "monitor": monitor
                    }))?;
                } else {
                    print_status(OutputStatus::Ok, "MONITOR UPDATED");
                    println!();
                    print_monitor_detail(&monitor);
                    print_field("ORGANIZATION", &organization_id);
                }
            }
            MonitorAction::Delete { id, org } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                client.delete_uptime_monitor(&id).await?;
                if json {
                    print_json(&serde_json::json!({
                        "status": "deleted",
                        "organization_id": organization_id,
                        "monitor_id": id
                    }))?;
                } else {
                    print_status_block(
                        OutputStatus::Ok,
                        "MONITOR DELETED",
                        &[
                            output_field("MONITOR", id.bold()),
                            output_field("ORGANIZATION", organization_id.dim()),
                        ],
                    );
                }
            }
            MonitorAction::Checks {
                id,
                page,
                page_size,
                org,
            } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let response = client.list_uptime_checks(&id, page, page_size).await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "monitor_id": id,
                        "checks": response
                    }))?;
                } else {
                    print_context("Organization:", &organization_id);
                    print_context("Monitor:", &id);
                    print_uptime_checks(&response);
                }
            }
        },
        Commands::Completions { shell } => {
            use clap_complete::generate;
            use clap_complete::shells::{Bash, Elvish, Fish, PowerShell, Zsh};

            let mut command = Cli::command();
            let bin_name = command.get_name().to_string();
            let mut stdout = io::stdout();

            match shell {
                CompletionShell::Bash => generate(Bash, &mut command, bin_name, &mut stdout),
                CompletionShell::Zsh => generate(Zsh, &mut command, bin_name, &mut stdout),
                CompletionShell::Fish => generate(Fish, &mut command, bin_name, &mut stdout),
                CompletionShell::PowerShell => {
                    generate(PowerShell, &mut command, bin_name, &mut stdout)
                }
                CompletionShell::Elvish => generate(Elvish, &mut command, bin_name, &mut stdout),
            }
        }
        Commands::Update => {
            updater::run_self_update(json).await?;
        }
        Commands::Tunnel {
            port,
            host,
            org,
            slug,
        } => {
            // Initialize logging for tunnel
            let log_config = LogConfig {
                level: log_level.clone(),
                output_to_stdout: false, // Disable stdout logging for TUI
                directory: log_dir
                    .clone()
                    .unwrap_or_else(|| LogConfig::default().directory),
                ..Default::default()
            };
            let _logger = Logger::new(log_config)?;

            let mut config = config::Config::load()?;
            let selected_org = resolve_tunnel_org(org, &config);
            let access_token = ensure_valid_token(&mut config).await?;
            let access_token_rx = refreshed_access_token_rx(access_token, config);

            if json {
                run_tunnel_json(access_token_rx, host, port, selected_org, slug).await?;
            } else {
                // Setup TUI for tunnel command
                let mut terminal = setup_terminal()?;
                let mut app = App::new()?;

                // Set app state to tunneling
                app.state = AppState::Tunneling;
                app.tunnel_local_host = host.clone();
                app.tunnel_local_port = port;
                // Prefer explicit CLI org, then fall back to configured organization.
                app.tunnel_org_id = selected_org.clone();
                app.tunnel_requested_slug = slug.clone();

                // Create channel for tunnel events
                let (event_tx, event_rx) = mpsc::channel(100);

                // Create and spawn tunnel forwarder manager
                let reconnect_tx = spawn_tunnel_forwarder_manager(
                    access_token_rx,
                    host,
                    port,
                    selected_org,
                    slug,
                    event_tx.clone(),
                );

                let res = run_app(
                    &mut terminal,
                    &mut app,
                    event_rx,
                    Some(reconnect_tx),
                    Some(event_tx),
                    Some(logo::spawn_logo_animation()),
                )
                .await;

                restore_terminal(&mut terminal)?;

                if let Err(err) = res {
                    error!(error = %err, "Application terminated with error");
                    display_error(&err);
                }
            }
        }
    }

    // Await background version check and show notification if update is available
    if let Some(handle) = update_handle
        && let Ok(Ok(Some(new_version))) =
            tokio::time::timeout(Duration::from_millis(500), handle).await
    {
        updater::persist_check_result(Some(&new_version));
        updater::print_update_notification(&new_version);
    }

    Ok(())
}

async fn run_login_flow(force_reauth: bool) -> Result<()> {
    let mut config = config::Config::load()?;

    if config.is_token_valid() && !force_reauth {
        println!();
        print_status(OutputStatus::Ok, "AUTHENTICATED");
        println!();
        print_field(
            "ACTION",
            format!(
                "Run {} to start forwarding webhooks.",
                "hooklistener listen <endpoint>".bold()
            ),
        );
        print_field(
            "ACTION",
            format!(
                "Use {} to re-authenticate.",
                "hooklistener login --force".bold()
            ),
        );
        println!();
        return Ok(());
    }

    if force_reauth {
        config.clear_token();
        config.save()?;
    }

    let mut device_flow = auth::DeviceCodeFlow::new(api::default_base_url());

    let user_code = device_flow.initiate_device_flow().await?;
    let display_code = device_flow
        .format_user_code()
        .unwrap_or_else(|| user_code.clone());
    let portal_url = device_portal_url();

    let clipboard_ok = arboard::Clipboard::new()
        .and_then(|mut cb| cb.set_text(&display_code))
        .is_ok();

    println!("\n{}\n", "HOOKLISTENER LOGIN".bold());
    print_field("OPEN", portal_url.as_str().underlined());
    print!("{} {}", output_label("CODE").bold(), display_code.bold());
    if clipboard_ok {
        print!(" {}", "(copied to clipboard)".dim());
    }
    println!("\n");

    let spinner_chars = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
    let mut spinner_idx: usize = 0;
    let mut stdout = io::stdout();

    let mut poll_interval = tokio::time::interval(Duration::from_secs(5));
    poll_interval.tick().await; // consume the immediate first tick

    loop {
        // Poll the API
        match device_flow.poll_for_authorization().await {
            Ok(Some(token_response)) => {
                execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
                let access_expires_at = match token_response.expires_in {
                    Some(secs) => Utc::now() + ChronoDuration::seconds(secs as i64),
                    None => Utc::now() + ChronoDuration::days(SESSION_TOKEN_VALIDITY_DAYS),
                };
                let refresh_expires_at = token_response
                    .refresh_expires_in
                    .map(|secs| Utc::now() + ChronoDuration::seconds(secs as i64));
                config.set_tokens(
                    token_response.access_token,
                    access_expires_at,
                    token_response.refresh_token,
                    refresh_expires_at,
                );
                config.save()?;
                print_status(OutputStatus::Ok, "AUTHENTICATION COMPLETE");
                println!();
                print_field(
                    "ACTION",
                    format!(
                        "Run {} to forward webhooks.",
                        "hooklistener listen <endpoint>".bold()
                    ),
                );
                println!();
                return Ok(());
            }
            Ok(None) => {
                if let Some(remaining) = device_flow.time_remaining()
                    && remaining == ChronoDuration::zero()
                {
                    execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
                    return Err(anyhow!(
                        "Device code expired before authorization completed. Please run `hooklistener login` again."
                    ));
                }
            }
            Err(err) => {
                execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
                return Err(anyhow!("Authentication failed: {}", err));
            }
        }

        // Animate spinner until next poll
        loop {
            let spinner = spinner_chars[spinner_idx % spinner_chars.len()];
            spinner_idx = (spinner_idx + 1) % spinner_chars.len();

            let status = if let Some(remaining) = device_flow.time_remaining() {
                let minutes = remaining.num_minutes();
                let seconds = remaining.num_seconds() % 60;
                let timer = if minutes > 0 {
                    format!("{minutes}m {seconds:02}s")
                } else {
                    format!("{seconds}s")
                };
                format!("  {spinner} Waiting for approval... {}", timer.dim())
            } else {
                format!("  {spinner} Waiting for approval...")
            };

            execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
            print!("{status}");
            stdout.flush()?;

            tokio::select! {
                _ = sleep(Duration::from_millis(80)) => continue,
                _ = poll_interval.tick() => break,
            }
        }
    }
}

fn device_portal_url() -> String {
    std::env::var("HOOKLISTENER_DEVICE_PORTAL_URL")
        .unwrap_or_else(|_| "https://app.hooklistener.com/device-codes".to_string())
}

fn resolve_tunnel_org(cli_org: Option<String>, config: &config::Config) -> Option<String> {
    cli_org.or_else(|| config.selected_organization_id.clone())
}

const ACCESS_TOKEN_REFRESH_SKEW_SECONDS: i64 = 60;
const ACCESS_TOKEN_REFRESH_RETRY_SECONDS: u64 = 30;

fn refreshed_access_token_rx(
    access_token: String,
    config: config::Config,
) -> watch::Receiver<String> {
    let (token_tx, token_rx) = watch::channel(access_token);

    if config.is_refresh_token_valid() {
        tokio::spawn(refresh_access_token_loop(config, token_tx));
    }

    token_rx
}

async fn refresh_access_token_loop(mut config: config::Config, token_tx: watch::Sender<String>) {
    loop {
        if config.refresh_token.is_none() || !config.is_refresh_token_valid() {
            return;
        }

        sleep(access_token_refresh_delay(&config)).await;

        match refresh_access_token_from_config(&mut config).await {
            Ok(access_token) => {
                if token_tx.send(access_token).is_err() {
                    return;
                }
            }
            Err(err) => {
                error!(error = %err, "Failed to refresh CLI access token");
                sleep(Duration::from_secs(ACCESS_TOKEN_REFRESH_RETRY_SECONDS)).await;
            }
        }
    }
}

fn access_token_refresh_delay(config: &config::Config) -> Duration {
    let Some(expires_at) = config.token_expires_at.as_ref() else {
        return Duration::from_secs(0);
    };

    let duration_until_refresh = expires_at.signed_duration_since(Utc::now())
        - ChronoDuration::seconds(ACCESS_TOKEN_REFRESH_SKEW_SECONDS);

    duration_until_refresh
        .to_std()
        .unwrap_or_else(|_| Duration::from_secs(0))
}

async fn ensure_valid_token(config: &mut config::Config) -> Result<String> {
    // 1. If access token is still valid, return it
    if config.is_token_valid() {
        return config
            .access_token
            .clone()
            .ok_or_else(|| anyhow!("No access token found. Please run `hooklistener login`."));
    }

    // 2. If refresh token is valid, try refreshing
    if config.refresh_token.is_some() && config.is_refresh_token_valid() {
        return refresh_access_token_from_config(config)
            .await
            .map_err(|err| {
                anyhow!(
                    "Session expired. Please run `hooklistener login` to re-authenticate. ({err})"
                )
            });
    }

    // 3. No valid tokens
    Err(anyhow!(
        "Session expired. Please run `hooklistener login` to re-authenticate."
    ))
}

async fn refresh_access_token_from_config(config: &mut config::Config) -> Result<String> {
    let refresh_token = config
        .refresh_token
        .clone()
        .ok_or_else(|| anyhow!("No refresh token found"))?;

    if !config.is_refresh_token_valid() {
        return Err(anyhow!("Refresh token expired"));
    }

    let response = api::refresh_access_token(&refresh_token).await?;
    let expires_at = Utc::now() + ChronoDuration::seconds(response.expires_in as i64);

    config.set_tokens(
        response.access_token.clone(),
        expires_at,
        Some(refresh_token),
        config.refresh_token_expires_at,
    );
    config.save()?;

    Ok(response.access_token)
}

fn require_organization(cli_org: Option<String>, config: &config::Config) -> Result<String> {
    resolve_tunnel_org(cli_org, config).ok_or_else(|| {
        anyhow!(
            "No organization selected. Use `hooklistener org use <organization-id>` or pass --org."
        )
    })
}

const FIELD_LABEL_WIDTH: usize = 14;

#[derive(Clone, Copy)]
pub(crate) enum OutputStatus {
    Ok,
    Err,
    Info,
}

impl OutputStatus {
    fn token(self) -> &'static str {
        match self {
            Self::Ok => "[OK]",
            Self::Err => "[ERR]",
            Self::Info => "[INFO]",
        }
    }
}

pub(crate) struct OutputField {
    label: &'static str,
    value: String,
}

pub(crate) fn output_field(label: &'static str, value: impl std::fmt::Display) -> OutputField {
    OutputField {
        label,
        value: value.to_string(),
    }
}

fn output_label(label: &str) -> String {
    let normalized = label.trim().trim_end_matches(':').to_ascii_uppercase();
    format!("{normalized:<width$}", width = FIELD_LABEL_WIDTH)
}

fn output_title(title: &str) -> String {
    title.trim().trim_end_matches(':').to_ascii_uppercase()
}

pub(crate) fn format_status_line(status: OutputStatus, title: &str) -> String {
    format!("{} {}", status.token(), output_title(title))
}

pub(crate) fn format_field_line(label: &str, value: impl std::fmt::Display) -> String {
    format!("{} {}", output_label(label), value)
}

fn styled_status_line(status: OutputStatus, title: &str) -> String {
    let line = format_status_line(status, title);
    match status {
        OutputStatus::Ok => line.green().bold().to_string(),
        OutputStatus::Err => line.red().bold().to_string(),
        OutputStatus::Info => line.blue().bold().to_string(),
    }
}

fn format_pagination_line(p: &api::Pagination) -> String {
    format!(
        "PAGE {}/{}  PAGE SIZE {}  TOTAL {}",
        p.page, p.total_pages, p.page_size, p.total_count
    )
}

fn value_or_dash(value: Option<&str>) -> &str {
    value.unwrap_or("-")
}

fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[cfg(test)]
pub(crate) fn render_status_block(
    status: OutputStatus,
    title: &str,
    fields: &[OutputField],
) -> String {
    let mut output = format_status_line(status, title);
    if !fields.is_empty() {
        output.push('\n');
        output.push('\n');
    }
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        output.push_str(&format_field_line(field.label, &field.value));
    }
    output.push('\n');
    output
}

fn print_status(status: OutputStatus, title: &str) {
    println!("{}", styled_status_line(status, title));
}

fn print_status_block(status: OutputStatus, title: &str, fields: &[OutputField]) {
    print_status(status, title);
    if !fields.is_empty() {
        println!();
    }
    for field in fields {
        print_field(field.label, &field.value);
    }
}

fn eprint_field(label: &str, value: impl std::fmt::Display) {
    eprintln!("{} {}", output_label(label).bold(), value);
}

fn eprint_status(status: OutputStatus, title: &str) {
    eprintln!("{}", styled_status_line(status, title));
}

fn eprint_status_block(status: OutputStatus, title: &str, fields: &[OutputField]) {
    eprint_status(status, title);
    if !fields.is_empty() {
        eprintln!();
    }
    for field in fields {
        eprint_field(field.label, &field.value);
    }
}

fn print_field(label: &str, value: impl std::fmt::Display) {
    println!("{} {}", output_label(label).bold(), value);
}

fn print_section(label: &str) {
    println!("{}", output_title(label).bold());
}

/// Print a dim context line like "ORGANIZATION abc123".
fn print_context(label: &str, value: &str) {
    println!("{} {}", output_label(label).dim(), value.dim());
}

/// Print a pagination footer.
fn print_pagination(p: &api::Pagination) {
    println!("{}", format_pagination_line(p).dim());
}

/// Print a key-value map (headers, query params) with a bold section label.
fn print_key_value_map(
    label: &str,
    map: &std::collections::HashMap<String, serde_json::Value>,
    separator: &str,
) {
    if map.is_empty() {
        print_field(label, "(none)".dim());
    } else {
        print_section(label);
        for (key, value) in map {
            println!("  {}{}{}", key.as_str().dim(), separator, value);
        }
    }
}

/// Print a body section, showing "(empty)" when the body is absent or blank.
fn print_body_section(label: &str, body: Option<&str>) {
    match body {
        Some(body) if !body.is_empty() => {
            print_section(label);
            println!("{}", body);
        }
        _ => print_field(label, "(empty)".dim()),
    }
}

/// Create a pre-configured table with the standard preset and dynamic content arrangement.
fn new_table(headers: &[&str]) -> Table {
    let headers = headers
        .iter()
        .map(|header| output_title(header))
        .collect::<Vec<_>>();
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL_CONDENSED)
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(headers);
    table
}

fn print_organizations(organizations: &[api::Organization], selected_org: Option<&str>) {
    if organizations.is_empty() {
        print_status(OutputStatus::Info, "NO ORGANIZATIONS FOUND");
        return;
    }

    let mut table = new_table(&["", "ID", "Name"]);
    for org in organizations {
        let marker = if selected_org.is_some_and(|id| id == org.id) {
            "*"
        } else {
            ""
        };
        table.add_row(vec![marker, &org.id, &org.name]);
    }
    println!("{table}");
}

fn print_endpoints(endpoints: &[api::DebugEndpointSummary]) {
    if endpoints.is_empty() {
        print_status(OutputStatus::Info, "NO DEBUG ENDPOINTS FOUND");
        return;
    }

    let mut table = new_table(&["ID", "Slug", "Status", "Webhook URL", "Name"]);
    for endpoint in endpoints {
        table.add_row(vec![
            &endpoint.id,
            &endpoint.slug,
            &endpoint.status,
            &endpoint.webhook_url,
            &endpoint.name,
        ]);
    }
    println!("{table}");
}

fn print_endpoint_detail(endpoint: &api::DebugEndpointSummary) {
    print_field("ID", &endpoint.id);
    print_field("SLUG", &endpoint.slug);
    print_field("STATUS", &endpoint.status);
    print_field("WEBHOOK URL", endpoint.webhook_url.as_str().underlined());
    print_field("NAME", &endpoint.name);
    if let Some(created_at) = endpoint.created_at.as_deref() {
        print_field("CREATED AT", created_at.dim());
    }
}

fn print_endpoint_requests(response: &api::EndpointRequestsResponse) {
    if response.data.is_empty() {
        print_status(OutputStatus::Info, "NO REQUESTS FOUND");
        return;
    }

    let mut table = new_table(&["ID", "Method", "URL", "Remote"]);
    for request in &response.data {
        table.add_row(vec![
            &request.id,
            &request.method,
            &request.url,
            &request.remote_addr,
        ]);
    }
    println!("{table}");
    print_pagination(&response.pagination);
}

fn print_endpoint_request_detail(request: &api::DebugRequestDetail) {
    print_field("REQUEST ID", &request.id);
    print_field("METHOD", request.method.as_str().bold());
    if let Some(path) = request.path.as_deref() {
        print_field("PATH", path);
    }
    print_field("URL", &request.url);

    if let Some(status_remote) = request.remote_addr.as_deref() {
        print_field("REMOTE", status_remote);
    }
    if let Some(content_length) = request.content_length {
        print_field("CONTENT LEN", content_length);
    }
    if let Some(created_at) = request.created_at.as_deref() {
        print_field("CREATED AT", created_at.dim());
    }

    println!();
    print_key_value_map("Headers:", &request.headers, ": ");

    println!();
    print_key_value_map("Query Params:", &request.query_params, "=");

    println!();
    print_body_section(
        "Body:",
        request.body.as_deref().or(request.body_preview.as_deref()),
    );
}

fn print_endpoint_request_forwards(response: &api::EndpointRequestForwardsResponse) {
    if response.data.is_empty() {
        print_status(OutputStatus::Info, "NO FORWARDS FOUND");
        return;
    }

    let mut table = new_table(&["ID", "Method", "Status", "Duration", "Target"]);
    for forward in &response.data {
        let status = forward
            .status_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "-".into());
        let duration = forward
            .duration_ms
            .map(|ms| format!("{ms}ms"))
            .unwrap_or_else(|| "-".into());
        let target = match forward.error_message.as_deref() {
            Some(err) => format!("{}\n  [ERR] {err}", forward.target_url),
            None => forward.target_url.clone(),
        };
        table.add_row(vec![
            forward.id.as_str(),
            &forward.method,
            &status,
            &duration,
            &target,
        ]);
    }
    println!("{table}");
    print_pagination(&response.pagination);
}

fn print_forward_detail(forward: &api::DebugRequestForwardDetail) {
    print_field("FORWARD ID", &forward.id);
    print_field("REQUEST ID", &forward.debug_request_id);
    print_field("TARGET URL", &forward.target_url);
    print_field("METHOD", forward.method.as_str().bold());
    if let Some(status_code) = forward.status_code {
        print_field("STATUS", style_status_code(status_code));
    } else {
        print_field("STATUS", "(pending)".yellow());
    }
    if let Some(duration_ms) = forward.duration_ms {
        print_field("DURATION", format!("{duration_ms}ms"));
    }
    if let Some(attempted_at) = forward.attempted_at.as_deref() {
        print_field("ATTEMPTED AT", attempted_at.dim());
    }
    if let Some(error_message) = forward.error_message.as_deref() {
        print_field("ERROR", error_message);
    }

    println!();
    print_key_value_map("Request Headers:", &forward.request_headers, ": ");

    if forward
        .request_body
        .as_deref()
        .is_some_and(|b| !b.is_empty())
    {
        println!();
        print_body_section("Request Body:", forward.request_body.as_deref());
    }

    println!();
    print_key_value_map("Response Headers:", &forward.response_headers, ": ");

    if forward
        .response_body
        .as_deref()
        .is_some_and(|b| !b.is_empty())
    {
        println!();
        print_body_section("Response Body:", forward.response_body.as_deref());
    }
}

fn print_case_run_result(result: &api::CaseRunResult) {
    let status = if case_run_failed(result) {
        OutputStatus::Err
    } else if matches!(result.result_status.as_str(), "pending" | "completed") {
        OutputStatus::Info
    } else {
        OutputStatus::Ok
    };

    print_status(status, "CASE RUN");
    println!();
    if let Some(run_id) = result.case_suite_run_id.as_deref().or(result.id.as_deref()) {
        print_field("RUN ID", run_id);
    }
    if let Some(report_url) = result.case_suite_run_url.as_deref() {
        print_field("REPORT", report_url);
    }
    print_field("RESULT", result.result_status.as_str().bold());
    print_field("STATUS", &result.status);
    print_field("ENDPOINT", &result.endpoint_id);
    print_field("TARGET", case_run_target_label(&result.target));
    if let Some(source) = result.source.as_deref() {
        print_field("SOURCE", source.to_uppercase());
    }
    print_field("ASYNC", yes_no(result.async_run));
    if let Some(waited) = result.waited {
        print_field("WAITED", yes_no(waited));
    }
    if result.timed_out == Some(true) {
        print_field("TIMED OUT", "yes".red());
    }
    print_field(
        "COUNTS",
        format!(
            "total={} queued={} failed={}",
            result.total_count, result.queued_count, result.failed_count
        ),
    );

    if result.waited == Some(true) || result.completed_count > 0 {
        print_field(
            "RESULTS",
            format!(
                "completed={} waiting={} passed={} assertions_failed={} assertions_error={} not_configured={}",
                result.completed_count,
                result.waiting_count,
                result.passed_count,
                result.assertion_failed_count,
                result.assertion_error_count,
                result.not_configured_count
            ),
        );
        print_field(
            "FAILURES",
            format!(
                "queue={} delivery={}",
                result.queue_failed_count, result.delivery_failed_count
            ),
        );
    }

    let problem_forwards = result
        .forwards
        .iter()
        .filter(|forward| {
            has_case_run_error(forward.error_message.as_ref())
                || forward.status_code.is_some_and(|code| code >= 400)
                || matches!(
                    forward.assertion_status.as_deref(),
                    Some("failed" | "error" | "timeout")
                )
        })
        .collect::<Vec<_>>();

    if !problem_forwards.is_empty() {
        println!();
        print_section("Failed Forwards");
        let mut table = new_table(&[
            "ID",
            "Request",
            "Case",
            "Target",
            "HTTP",
            "Assertion",
            "Error",
        ]);
        for forward in problem_forwards {
            let status = forward
                .status_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "-".to_string());
            let assertion = forward
                .assertion_status
                .as_deref()
                .unwrap_or("-")
                .to_string();
            let error = forward
                .error_message
                .as_ref()
                .map(case_run_value_message)
                .or_else(|| forward.poll_url.clone())
                .unwrap_or_else(|| "-".to_string());
            table.add_row(vec![
                forward.id.as_str().to_string(),
                forward.debug_request_id.clone(),
                value_or_dash(forward.debug_request_case_id.as_deref()).to_string(),
                case_run_forward_target(forward),
                status,
                assertion,
                error,
            ]);
        }
        println!("{table}");
    }

    if !result.failures.is_empty() {
        println!();
        print_section("Queue Failures");
        let mut table = new_table(&["Case", "Reason"]);
        for failure in &result.failures {
            table.add_row(vec![
                failure.case_id.as_str().to_string(),
                case_run_failure_reason(failure),
            ]);
        }
        println!("{table}");
    }
}

fn print_static_tunnels(response: &api::StaticTunnelsResponse) {
    if response.static_tunnels.is_empty() {
        print_status(OutputStatus::Info, "NO STATIC TUNNELS FOUND");
    } else {
        let mut table = new_table(&["ID", "Slug", "Name"]);
        for tunnel in &response.static_tunnels {
            let name = tunnel.name.as_deref().unwrap_or("");
            table.add_row(vec![tunnel.id.as_str(), &tunnel.slug, name]);
        }
        println!("{table}");
    }

    print_field(
        "USED",
        format!("{}/{}", response.used, response.limit).dim(),
    );
}

fn print_anon_events(response: &api::AnonEventsResponse) {
    if response.data.is_empty() {
        print_status(OutputStatus::Info, "NO EVENTS CAPTURED");
    } else {
        let mut table = new_table(&["ID", "Method", "Received At"]);
        for event in &response.data {
            table.add_row(vec![
                event.id.as_str(),
                event.method.as_str(),
                value_or_dash(event.inserted_at.as_deref()),
            ]);
        }
        println!("{table}");
    }
    print_pagination(&response.pagination);
}

fn print_anon_event_detail(event: &api::AnonEvent) {
    print_field("EVENT ID", &event.id);
    print_field("ENDPOINT ID", &event.endpoint_id);
    print_field("METHOD", event.method.as_str().bold());
    if let Some(status) = event.status.as_deref() {
        print_field("STATUS", status);
    }
    if let Some(inserted_at) = event.inserted_at.as_deref() {
        print_field("RECEIVED AT", inserted_at.dim());
    }

    println!();
    print_key_value_map("Headers:", &event.headers, ": ");

    println!();
    print_body_section("Body:", event.body.as_deref());
}

fn print_shared_requests(shares: &[api::SharedRequestSummary]) {
    if shares.is_empty() {
        print_status(OutputStatus::Info, "NO SHARES FOUND");
        return;
    }

    let mut table = new_table(&["ID", "Token", "Fwds", "Views", "Protected", "Expires At"]);
    for share in shares {
        table.add_row(vec![
            share.id.as_str().to_string(),
            share.share_token.clone(),
            yes_no(share.include_forwards).to_string(),
            share.view_count.to_string(),
            yes_no(share.password_protected).to_string(),
            value_or_dash(share.expires_at.as_deref()).to_string(),
        ]);
    }
    println!("{table}");
}

fn print_shared_request_full(data: &serde_json::Value) {
    if let Some(token) = data.get("share_token").and_then(|v| v.as_str()) {
        print_field("SHARE TOKEN", token);
    }
    if let Some(expires) = data.get("expires_at").and_then(|v| v.as_str()) {
        print_field("EXPIRES AT", expires.dim());
    }
    if let Some(views) = data.get("view_count").and_then(|v| v.as_u64()) {
        print_field("VIEWS", views);
    }

    if let Some(request) = data.get("debug_request") {
        println!();
        print_section("DEBUG REQUEST");
        if let Some(id) = request.get("id").and_then(|v| v.as_str()) {
            print_field("ID", id);
        }
        if let Some(method) = request.get("method").and_then(|v| v.as_str()) {
            print_field("METHOD", method.bold());
        }
        if let Some(url) = request.get("url").and_then(|v| v.as_str()) {
            print_field("URL", url);
        }
        if let Some(remote) = request.get("remote_addr").and_then(|v| v.as_str()) {
            print_field("REMOTE", remote.dim());
        }
        if let Some(created) = request.get("created_at").and_then(|v| v.as_str()) {
            print_field("CREATED AT", created.dim());
        }

        if let Some(headers) = request.get("headers").and_then(|v| v.as_object())
            && !headers.is_empty()
        {
            println!();
            print_section("HEADERS");
            for (key, value) in headers {
                println!("  {}: {}", key.as_str().dim(), value);
            }
        }

        let body = request
            .get("body")
            .and_then(|v| v.as_str())
            .or_else(|| request.get("body_preview").and_then(|v| v.as_str()));
        if let Some(body) = body
            && !body.is_empty()
        {
            println!();
            print_section("BODY");
            println!("{}", body);
        }
    }

    if let Some(forwards) = data.get("forwards").and_then(|v| v.as_array())
        && !forwards.is_empty()
    {
        println!();
        print_section("FORWARDS");
        for fwd in forwards {
            let target = fwd
                .get("target_url")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let method = fwd.get("method").and_then(|v| v.as_str()).unwrap_or("-");
            let status = fwd
                .get("status_code")
                .and_then(|v| v.as_u64())
                .map(|c| style_status_code(c as u16))
                .unwrap_or_else(|| "-".to_string());
            let duration = fwd
                .get("duration_ms")
                .and_then(|v| v.as_u64())
                .map(|ms| format!("{}ms", ms))
                .unwrap_or_else(|| "-".to_string());
            println!("  {} {} → {} ({})", method.bold(), status, target, duration);
        }
    }
}

fn monitor_status_label(status: Option<&str>) -> &str {
    status.unwrap_or("pending")
}

fn style_monitor_status(status: Option<&str>) -> String {
    match monitor_status_label(status) {
        "up" => "up".green().to_string(),
        "down" => "down".red().to_string(),
        s => s.yellow().to_string(),
    }
}

fn print_monitors(monitors: &[api::UptimeMonitor]) {
    if monitors.is_empty() {
        print_status(OutputStatus::Info, "NO UPTIME MONITORS FOUND");
        return;
    }

    let mut table = new_table(&["ID", "Status", "Method", "Int", "URL", "Name"]);
    for m in monitors {
        let status = monitor_status_label(m.current_status.as_deref());
        let interval = m
            .check_interval
            .map(|i| format!("{}m", i))
            .unwrap_or_default();
        let url_display = if m.url.len() > 24 {
            format!("{}…", &m.url[..23])
        } else {
            m.url.clone()
        };
        let name = if m.enabled {
            m.name.clone()
        } else {
            format!("{} (disabled)", m.name)
        };
        table.add_row(vec![
            m.id.clone(),
            status.to_string(),
            m.method.to_uppercase(),
            interval,
            url_display,
            name,
        ]);
    }
    println!("{table}");
}

fn print_monitor_detail(m: &api::UptimeMonitor) {
    print_field("ID", &m.id);
    print_field("NAME", &m.name);
    print_field("URL", m.url.as_str().underlined());
    print_field("METHOD", m.method.to_uppercase().bold());

    let status = style_monitor_status(m.current_status.as_deref());
    let enabled = if m.enabled {
        "yes".green().to_string()
    } else {
        "no (paused)".yellow().to_string()
    };
    print_field("STATUS", status);
    print_field("ENABLED", enabled);

    if let Some(code) = m.expected_status_code {
        print_field("EXPECTED", code);
    }
    if let Some(ref bc) = m.body_contains {
        print_field("BODY MATCH", bc);
    }
    if let Some(interval) = m.check_interval {
        print_field("INTERVAL", format!("{interval}m"));
    }
    if let Some(threshold) = m.failure_threshold {
        print_field("THRESHOLD", threshold);
    }
    if let Some(failures) = m.consecutive_failures {
        print_field("FAILURES", failures);
    }

    print_field(
        "NOTIFY",
        format!(
            "email={}, slack={}",
            if m.email_enabled { "on" } else { "off" },
            if m.slack_enabled { "on" } else { "off" }
        ),
    );

    if let Some(ref checked) = m.last_checked_at {
        print_field("LAST CHECK", checked.as_str().dim());
    }
    if let Some(ref changed) = m.last_status_change_at {
        print_field("STATUS CHANGE", changed.as_str().dim());
    }
    if let Some(ref created) = m.created_at {
        print_field("CREATED AT", created.as_str().dim());
    }
}

fn print_uptime_checks(response: &api::UptimeChecksResponse) {
    if let Some(ref stats) = response.stats {
        let uptime = stats
            .uptime_percentage
            .map(|p| {
                let s = format!("{:.2}%", p);
                if p >= 99.9 {
                    s.green().to_string()
                } else if p >= 95.0 {
                    s.yellow().to_string()
                } else {
                    s.red().to_string()
                }
            })
            .unwrap_or_else(|| "-".to_string());
        let avg_rt = stats
            .avg_response_time_ms
            .map(|ms| format!("{:.0}ms", ms))
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{} {}  {} {}",
            "UPTIME".bold(),
            uptime,
            "AVG RESPONSE".bold(),
            avg_rt
        );
        println!();
    }

    if response.data.is_empty() {
        print_status(OutputStatus::Info, "NO CHECKS RECORDED");
    } else {
        let mut table = new_table(&["ID", "Status", "Code", "Response", "Checked At", "Error"]);
        for check in &response.data {
            let code = check
                .status_code
                .map(status_code_label)
                .unwrap_or_else(|| "-".to_string());
            let rt = check
                .response_time_ms
                .map(|ms| format!("{}ms", ms))
                .unwrap_or_else(|| "-".to_string());
            let checked = value_or_dash(check.checked_at.as_deref());
            let error = value_or_dash(check.error_message.as_deref());

            table.add_row(vec![
                check.id.clone(),
                check.status.clone(),
                code,
                rt,
                checked.to_string(),
                error.to_string(),
            ]);
        }
        println!("{table}");
    }

    print_pagination(&response.pagination);
}

fn spawn_tunnel_forwarder_manager(
    access_token_rx: watch::Receiver<String>,
    host: String,
    port: u16,
    org: Option<String>,
    slug: Option<String>,
    event_tx: mpsc::Sender<TunnelEvent>,
) -> mpsc::UnboundedSender<()> {
    let (reconnect_tx, mut reconnect_rx) = mpsc::unbounded_channel::<()>();

    tokio::spawn(async move {
        let mut worker = tokio::spawn(run_tunnel_forwarder_connection(
            access_token_rx.clone(),
            host.clone(),
            port,
            org.clone(),
            slug.clone(),
            event_tx.clone(),
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
                event_tx.clone(),
            ));
        }

        worker.abort();
        let _ = worker.await;
    });

    reconnect_tx
}

async fn run_tunnel_forwarder_connection(
    access_token_rx: watch::Receiver<String>,
    host: String,
    port: u16,
    org: Option<String>,
    slug: Option<String>,
    event_tx: mpsc::Sender<TunnelEvent>,
) {
    let tunnel_forwarder =
        tunnel::TunnelForwarder::new(access_token_rx, host, port, org, slug, event_tx);

    if let Err(e) = tunnel_forwarder
        .connect_with_reconnect(tunnel::ReconnectConfig::default())
        .await
    {
        error!("Tunnel forwarder error: {}", e);
    }
}

fn spawn_tunnel_replay(
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

async fn run_app<B: ratatui::backend::Backend + Send>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    mut tunnel_rx: mpsc::Receiver<TunnelEvent>,
    tunnel_reconnect_tx: Option<mpsc::UnboundedSender<()>>,
    tunnel_event_tx: Option<mpsc::Sender<TunnelEvent>>,
    mut logo_rx: Option<watch::Receiver<String>>,
) -> Result<()>
where
    <B as ratatui::backend::Backend>::Error: std::error::Error + Send + Sync + 'static,
{
    // Ensure proper terminal cleanup on any exit
    let _cleanup = TerminalCleanup;

    loop {
        if let Some(logo_rx) = logo_rx.as_mut() {
            let should_update_logo =
                app.logo_frame.is_none() || logo_rx.has_changed().unwrap_or(false);
            if should_update_logo {
                app.logo_frame = Some(logo_rx.borrow_and_update().clone());
            }
        }

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
                    app.listening_requests.push(*request);
                    app.listening_stats.total_requests += 1;
                }
                TunnelEvent::RequestReceived {
                    request_id,
                    method,
                    path,
                    headers,
                    body,
                    query_string,
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
                TunnelEvent::ReplayCompleted {
                    request_id,
                    status,
                    duration_ms,
                } => {
                    app.tunnel_status_message = Some((
                        format!("Replayed {} {} {}ms", request_id, status, duration_ms),
                        std::time::Instant::now(),
                    ));
                }
                TunnelEvent::ReplayFailed { request_id, error } => {
                    app.tunnel_status_message = Some((
                        format!("Replay {} failed: {}", request_id, error),
                        std::time::Instant::now(),
                    ));
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
                    app.tunnel_status_message =
                        Some(("Replay unavailable".to_string(), std::time::Instant::now()));
                }
            }

            if matches!(app.state, AppState::ForwardingRequest) {
                app.forward_request().await?;
            }
        }
    }

    Ok(())
}

fn status_code_label(code: u16) -> String {
    code.to_string()
}

fn style_status_code(code: u16) -> String {
    let s = status_code_label(code);
    match code {
        200..=299 => s.green().to_string(),
        300..=399 => s.yellow().to_string(),
        400..=599 => s.red().to_string(),
        _ => s,
    }
}

fn error_hint(err: &anyhow::Error) -> Option<&str> {
    if let Some(e) = err.downcast_ref::<errors::ApiError>() {
        return e.hint();
    }
    if let Some(e) = err.downcast_ref::<errors::TunnelError>() {
        return e.hint();
    }
    if let Some(e) = err.downcast_ref::<errors::ConfigError>() {
        return e.hint();
    }
    if let Some(e) = err.downcast_ref::<errors::UpdateError>() {
        return e.hint();
    }
    None
}

fn display_error(err: &anyhow::Error) {
    eprint_status(OutputStatus::Err, "COMMAND FAILED");
    eprintln!();
    eprint_field("MESSAGE", err);
    if let Some(hint) = error_hint(err) {
        eprint_field("HINT", hint);
    }
    for cause in err.chain().skip(1) {
        eprint_field("CAUSE", cause);
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen)?;
    let backend = CrosstermBackend::new(stdout);
    let terminal = Terminal::new(backend)?;
    Ok(terminal)
}

struct TerminalCleanup;

impl Drop for TerminalCleanup {
    fn drop(&mut self) {
        // Ensure terminal is always restored, even on panic
        let _ = disable_raw_mode();
        let _ = execute!(io::stdout(), LeaveAlternateScreen);
        let _ = execute!(io::stdout(), Show);
    }
}

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn assert_no_emoji(output: &str) {
        assert!(
            !output.chars().any(|ch| {
                let code = ch as u32;
                (0x1F300..=0x1FAFF).contains(&code) || (0x2600..=0x27BF).contains(&code)
            }),
            "output contains emoji-like glyphs: {output}"
        );
    }

    fn assert_no_ansi_escape(output: &str) {
        assert!(
            !output.contains("\u{1b}["),
            "output contains ANSI escape sequences: {output:?}"
        );
    }

    fn render_table<'a, const C: usize>(
        headers: &[&str],
        rows: impl IntoIterator<Item = [&'a str; C]>,
    ) -> String {
        let mut table = new_table(headers);
        for row in rows {
            table.add_row(row);
        }
        table.to_string()
    }

    fn render_field_block(fields: &[OutputField]) -> String {
        let mut output = String::new();
        for (index, field) in fields.iter().enumerate() {
            if index > 0 {
                output.push('\n');
            }
            output.push_str(&format_field_line(field.label, &field.value));
        }
        output.push('\n');
        output
    }

    fn render_snapshot_sections(sections: Vec<(&str, String)>) -> String {
        let mut output = String::new();
        for (index, (title, body)) in sections.into_iter().enumerate() {
            if index > 0 {
                output.push('\n');
                output.push('\n');
            }
            output.push_str(&output_title(title));
            output.push('\n');
            output.push_str(body.trim_end());
            output.push('\n');
        }
        output
    }

    fn render_empty_status(title: &str) -> String {
        format!("{}\n", format_status_line(OutputStatus::Info, title))
    }

    fn make_config(selected_org: Option<&str>) -> config::Config {
        config::Config {
            selected_organization_id: selected_org.map(String::from),
            ..config::Config::default()
        }
    }

    #[test]
    fn command_output_endpoint_created_snapshot() {
        let output = render_status_block(
            OutputStatus::Ok,
            "endpoint created",
            &[
                output_field("ID", "ep_123"),
                output_field("SLUG", "github-webhooks"),
                output_field(
                    "WEBHOOK URL",
                    "https://example.hooklistener.dev/github-webhooks",
                ),
                output_field("ORGANIZATION", "org_123"),
                output_field("ACTION", "Run `hooklistener listen github-webhooks`"),
            ],
        );

        assert_no_emoji(&output);
        assert!(!output.contains("ID:"));
        insta::assert_snapshot!("command_output_endpoint_created", output);
    }

    #[test]
    fn command_output_request_deleted_snapshot() {
        let output = render_status_block(
            OutputStatus::Ok,
            "request deleted",
            &[
                output_field("REQUEST", "req_123"),
                output_field("ENDPOINT", "ep_123"),
                output_field("ORGANIZATION", "org_123"),
            ],
        );

        assert_no_emoji(&output);
        assert!(output.starts_with("[OK] REQUEST DELETED\n\n"));
        insta::assert_snapshot!("command_output_request_deleted", output);
    }

    #[test]
    fn command_output_error_block_snapshot() {
        let output = render_status_block(
            OutputStatus::Err,
            "command failed",
            &[
                output_field("MESSAGE", "Not authenticated"),
                output_field("ACTION", "Run `hooklistener login`"),
            ],
        );

        assert_no_emoji(&output);
        assert!(output.starts_with("[ERR] COMMAND FAILED\n\n"));
        insta::assert_snapshot!("command_output_error_block", output);
    }

    #[test]
    fn command_output_table_headers_snapshot() {
        let output = render_table(
            &["ID", "Method", "Status", "Checked At"],
            [["req_123", "POST", "200", "2026-05-29T12:00:00Z"]],
        );

        assert!(output.contains("METHOD"));
        assert!(output.contains("CHECKED AT"));
        assert!(!output.contains("Method"));
        assert_no_ansi_escape(&output);
        insta::assert_snapshot!("command_output_table_headers", output);
    }

    #[test]
    fn cases_run_target_shorthand_maps_to_expected_body_fields() {
        let url = build_case_run_params(CaseRunInput {
            target: Some("https://example.test/webhooks".to_string()),
            target_url: None,
            target_id: None,
            target_name: None,
            wait: true,
            timeout: Some("60s".to_string()),
            timeout_ms: None,
            interval_ms: None,
        })
        .unwrap();
        assert_eq!(
            url.target_url.as_deref(),
            Some("https://example.test/webhooks")
        );
        assert_eq!(url.wait, Some(true));
        assert_eq!(url.timeout_ms, Some(60_000));

        let cli = build_case_run_params(CaseRunInput {
            target: Some("cli".to_string()),
            target_url: None,
            target_id: None,
            target_name: Some("Local CLI".to_string()),
            wait: false,
            timeout: None,
            timeout_ms: None,
            interval_ms: Some(500),
        })
        .unwrap();
        assert_eq!(cli.target.as_deref(), Some("cli"));
        assert_eq!(cli.target_name.as_deref(), Some("Local CLI"));
        assert_eq!(cli.interval_ms, Some(500));

        let saved = build_case_run_params(CaseRunInput {
            target: Some("rt_123".to_string()),
            target_url: None,
            target_id: None,
            target_name: None,
            wait: false,
            timeout: None,
            timeout_ms: None,
            interval_ms: None,
        })
        .unwrap();
        assert_eq!(saved.target_id.as_deref(), Some("rt_123"));
    }

    #[test]
    fn cases_run_rejects_ambiguous_targets() {
        let err = build_case_run_params(CaseRunInput {
            target: Some("cli".to_string()),
            target_url: Some("http://localhost:3000".to_string()),
            target_id: None,
            target_name: None,
            wait: false,
            timeout: None,
            timeout_ms: None,
            interval_ms: None,
        })
        .unwrap_err();
        assert!(err.to_string().contains("Use --target by itself"));

        let err = build_case_run_params(CaseRunInput {
            target: None,
            target_url: None,
            target_id: None,
            target_name: None,
            wait: false,
            timeout: None,
            timeout_ms: None,
            interval_ms: None,
        })
        .unwrap_err();
        assert!(err.to_string().contains("Target is required"));
    }

    #[test]
    fn cases_run_timeout_parser_accepts_seconds_and_units() {
        assert_eq!(parse_duration_to_ms("60").unwrap(), 60_000);
        assert_eq!(parse_duration_to_ms("60s").unwrap(), 60_000);
        assert_eq!(parse_duration_to_ms("2m").unwrap(), 120_000);
        assert_eq!(parse_duration_to_ms("1500ms").unwrap(), 1_500);
        assert!(parse_timeout_ms(Some("1s".to_string()), Some(1_000)).is_err());
    }

    #[test]
    fn cases_run_clap_shape_matches_expected_command() {
        let cli = Cli::try_parse_from([
            "hooklistener",
            "cases",
            "run",
            "ep_123",
            "--target",
            "cli",
            "--wait",
            "--timeout",
            "60s",
            "--json",
        ])
        .unwrap();

        assert!(cli.json);
        match cli.command.unwrap() {
            Commands::Cases {
                action:
                    CasesAction::Run {
                        endpoint_id,
                        target,
                        wait,
                        timeout,
                        ..
                    },
            } => {
                assert_eq!(endpoint_id, "ep_123");
                assert_eq!(target.as_deref(), Some("cli"));
                assert!(wait);
                assert_eq!(timeout.as_deref(), Some("60s"));
            }
            _ => panic!("expected cases run command"),
        }
    }

    #[test]
    fn command_output_monitor_table_snapshot() {
        let output = render_table(
            &["ID", "Status", "Method", "Int", "URL", "Name"],
            [[
                "mon_123",
                monitor_status_label(Some("up")),
                "GET",
                "5m",
                "https://serpgoblin.com",
                "SerpGoblin",
            ]],
        );

        assert_no_emoji(&output);
        assert_no_ansi_escape(&output);
        insta::assert_snapshot!("command_output_monitor_table", output);
    }

    #[test]
    fn command_output_table_family_snapshot() {
        let output = render_snapshot_sections(vec![
            (
                "org list",
                render_table(
                    &["", "ID", "Name"],
                    [
                        ["*", "org_123", "Hooklistener Labs"],
                        ["", "org_456", "Platform Team"],
                    ],
                ),
            ),
            (
                "endpoint list",
                render_table(
                    &["ID", "Slug", "Status", "Webhook URL", "Name"],
                    [[
                        "ep_123",
                        "github-webhooks",
                        "active",
                        "https://hooks.example.dev/github-webhooks",
                        "GitHub",
                    ]],
                ),
            ),
            (
                "endpoint request list",
                render_table(
                    &["ID", "Method", "URL", "Remote"],
                    [[
                        "req_123",
                        "POST",
                        "/webhooks/github/push?delivery=1c07ce58",
                        "172.71.190.83",
                    ]],
                ),
            ),
            (
                "request forwards",
                render_table(
                    &["ID", "Method", "Status", "Duration", "Target"],
                    [
                        [
                            "fwd_123",
                            "POST",
                            "200",
                            "42ms",
                            "http://localhost:3000/webhooks",
                        ],
                        [
                            "fwd_456",
                            "POST",
                            "-",
                            "-",
                            "http://localhost:3001/webhooks\n  [ERR] connection refused",
                        ],
                    ],
                ),
            ),
            (
                "static tunnel list",
                render_table(
                    &["ID", "Slug", "Name"],
                    [["tun_123", "acme-dev", "Development tunnel"]],
                ),
            ),
            (
                "anon events",
                render_table(
                    &["ID", "Method", "Received At"],
                    [["evt_123", "POST", "2026-05-29T12:00:00Z"]],
                ),
            ),
            (
                "share list",
                render_table(
                    &["ID", "Token", "Fwds", "Views", "Protected", "Expires At"],
                    [[
                        "shr_123",
                        "share_abcdef123456",
                        "yes",
                        "14",
                        "no",
                        "2026-06-05T12:00:00Z",
                    ]],
                ),
            ),
            (
                "monitor checks",
                render_table(
                    &["ID", "Status", "Code", "Response", "Checked At", "Error"],
                    [
                        ["chk_123", "up", "200", "184ms", "2026-05-29T12:00:00Z", "-"],
                        [
                            "chk_456",
                            "down",
                            "500",
                            "901ms",
                            "2026-05-29T12:05:00Z",
                            "timeout",
                        ],
                    ],
                ),
            ),
        ]);

        insta::assert_snapshot!("command_output_table_family", output);
    }

    #[test]
    fn command_output_detail_blocks_snapshot() {
        let output = render_snapshot_sections(vec![
            (
                "endpoint show",
                render_field_block(&[
                    output_field("ID", "ep_123"),
                    output_field("SLUG", "github-webhooks"),
                    output_field("STATUS", "active"),
                    output_field("WEBHOOK URL", "https://hooks.example.dev/github-webhooks"),
                    output_field("NAME", "GitHub"),
                    output_field("CREATED AT", "2026-05-29T12:00:00Z"),
                ]),
            ),
            (
                "request show",
                format!(
                    "{}\nHEADERS\n  content-type: \"application/json\"\n  x-github-delivery: \"1c07ce58\"\n\nQUERY PARAMS\n  delivery=\"1c07ce58\"\n\nBODY\n{}",
                    render_field_block(&[
                        output_field("REQUEST ID", "req_123"),
                        output_field("METHOD", "POST"),
                        output_field("PATH", "/webhooks/github/push"),
                        output_field("URL", "/webhooks/github/push?delivery=1c07ce58"),
                        output_field("REMOTE", "172.71.190.83"),
                        output_field("CONTENT LEN", "10354"),
                        output_field("CREATED AT", "2026-05-29T12:00:00Z"),
                    ])
                    .trim_end(),
                    r#"{"ref":"refs/heads/main","repository":"hooklistener"}"#
                ),
            ),
            (
                "forward show",
                format!(
                    "{}\nREQUEST HEADERS\n  content-type: \"application/json\"\n\nRESPONSE HEADERS\n  server: \"local-dev\"\n\nRESPONSE BODY\n{}",
                    render_field_block(&[
                        output_field("FORWARD ID", "fwd_123"),
                        output_field("REQUEST ID", "req_123"),
                        output_field("TARGET URL", "http://localhost:3000/webhooks"),
                        output_field("METHOD", "POST"),
                        output_field("STATUS", "200"),
                        output_field("DURATION", "42ms"),
                        output_field("ATTEMPTED AT", "2026-05-29T12:00:01Z"),
                    ])
                    .trim_end(),
                    r#"{"ok":true}"#
                ),
            ),
            (
                "monitor show",
                render_field_block(&[
                    output_field("ID", "mon_123"),
                    output_field("NAME", "SerpGoblin"),
                    output_field("URL", "https://serpgoblin.com"),
                    output_field("METHOD", "GET"),
                    output_field("STATUS", "up"),
                    output_field("ENABLED", "yes"),
                    output_field("EXPECTED", "200"),
                    output_field("INTERVAL", "5m"),
                    output_field("THRESHOLD", "3"),
                    output_field("FAILURES", "0"),
                    output_field("NOTIFY", "email=on, slack=off"),
                    output_field("LAST CHECK", "2026-05-29T12:00:00Z"),
                ]),
            ),
        ]);

        insta::assert_snapshot!("command_output_detail_blocks", output);
    }

    #[test]
    fn command_output_empty_states_snapshot() {
        let output = render_snapshot_sections(vec![
            (
                "endpoint list",
                render_empty_status("NO DEBUG ENDPOINTS FOUND"),
            ),
            ("request list", render_empty_status("NO REQUESTS FOUND")),
            ("forward list", render_empty_status("NO FORWARDS FOUND")),
            ("share list", render_empty_status("NO SHARES FOUND")),
            (
                "monitor list",
                render_empty_status("NO UPTIME MONITORS FOUND"),
            ),
            ("monitor checks", render_empty_status("NO CHECKS RECORDED")),
            (
                "static tunnel list",
                render_empty_status("NO STATIC TUNNELS FOUND"),
            ),
            ("anon events", render_empty_status("NO EVENTS CAPTURED")),
        ]);

        insta::assert_snapshot!("command_output_empty_states", output);
    }

    #[test]
    fn command_output_pagination_snapshot() {
        let output = format_pagination_line(&api::Pagination {
            page: 2,
            page_size: 25,
            total_count: 120,
            total_pages: 5,
        });

        assert_eq!(output, "PAGE 2/5  PAGE SIZE 25  TOTAL 120");
        insta::assert_snapshot!("command_output_pagination", output);
    }

    #[test]
    fn resolve_tunnel_org_prefers_cli_arg() {
        let config = make_config(Some("org-config"));
        let resolved = resolve_tunnel_org(Some("org-cli".to_string()), &config);
        assert_eq!(resolved.as_deref(), Some("org-cli"));
    }

    #[test]
    fn resolve_tunnel_org_falls_back_to_config() {
        let config = make_config(Some("org-config"));
        let resolved = resolve_tunnel_org(None, &config);
        assert_eq!(resolved.as_deref(), Some("org-config"));
    }

    #[test]
    fn resolve_tunnel_org_none_when_not_set() {
        let config = make_config(None);
        let resolved = resolve_tunnel_org(None, &config);
        assert!(resolved.is_none());
    }

    #[test]
    fn require_organization_uses_cli_value() {
        let config = make_config(Some("org-config"));
        let org = require_organization(Some("org-cli".to_string()), &config).unwrap();
        assert_eq!(org, "org-cli");
    }

    #[test]
    fn require_organization_errors_when_missing() {
        let config = make_config(None);
        let err = require_organization(None, &config).unwrap_err();
        assert!(
            err.to_string().contains("No organization selected"),
            "unexpected error: {}",
            err
        );
    }

    #[tokio::test]
    async fn ensure_valid_token_returns_error_when_expired() {
        let mut config = make_config(Some("org-config"));
        let err = ensure_valid_token(&mut config).await.unwrap_err();
        assert!(
            err.to_string().contains("Session expired"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn forward_request_receipt_includes_agent_links() {
        let response = api::EndpointRequestForwardResponse {
            forward_id: "fwd_123".to_string(),
            debug_request_id: "req_123".to_string(),
            target_url: "https://example.com/webhook".to_string(),
            status: "pending".to_string(),
        };

        let receipt = forward_request_receipt("org_123", "ep_123", "req_123", &response);

        assert_eq!(receipt["status"], "pending");
        assert_eq!(receipt["delivery_status"], "queued");
        assert_eq!(receipt["resource_uri"], "hooklistener://forwards/fwd_123");
        assert_eq!(
            receipt["request_resource_uri"],
            "hooklistener://requests/req_123"
        );
        assert_eq!(receipt["poll_url"], "/api/v1/forwards/fwd_123");
        assert_eq!(
            receipt["resources"]["request_forwards"],
            "hooklistener://requests/req_123/forwards"
        );
        assert_eq!(
            receipt["next_actions"][0],
            "hooklistener endpoint forward fwd_123"
        );
    }

    #[test]
    fn listen_started_receipt_includes_endpoint_resources() {
        let endpoint = api::DebugEndpointSummary {
            id: "ep_123".to_string(),
            name: "GitHub".to_string(),
            slug: "github-webhooks".to_string(),
            status: "active".to_string(),
            webhook_url: "https://hooks.example.dev/github-webhooks".to_string(),
            created_at: None,
            updated_at: None,
        };

        let receipt = listen_started_receipt(
            "github-webhooks",
            "http://localhost:3000/webhooks",
            Some("wss://api.example.dev/socket/websocket"),
            Some(&endpoint),
        );

        assert_eq!(receipt["type"], "receipt");
        assert_eq!(receipt["event"], "listen_started");
        assert_eq!(receipt["status"], "running");
        assert_eq!(
            receipt["resource_uri"],
            "hooklistener://cli/listen/github-webhooks"
        );
        assert_eq!(receipt["endpoint"]["id"], "ep_123");
        assert_eq!(
            receipt["resources"]["endpoint"],
            "hooklistener://endpoints/ep_123"
        );
        assert_eq!(
            receipt["resources"]["requests"],
            "hooklistener://endpoints/ep_123/requests"
        );
    }

    #[test]
    fn listen_webhook_event_includes_request_resource_uri() {
        let request = models::WebhookRequest {
            id: "req_123".to_string(),
            timestamp: 1_781_000_000,
            remote_addr: "Tunnel".to_string(),
            headers: std::collections::HashMap::new(),
            content_length: 42,
            method: "POST".to_string(),
            url: "/webhooks/github".to_string(),
            path: Some("/webhooks/github".to_string()),
            query_params: std::collections::HashMap::new(),
            created_at: "2026-07-08T10:00:00Z".to_string(),
            body_preview: None,
            body: None,
        };
        let event = TunnelEvent::WebhookReceived(Box::new(request));
        let receipt = listen_event_receipt(
            &event,
            "github-webhooks",
            "http://localhost:3000/webhooks",
            None,
        );

        assert_eq!(receipt["event"], "webhook_received");
        assert_eq!(receipt["request_id"], "req_123");
        assert_eq!(receipt["resource_uri"], "hooklistener://requests/req_123");
        assert_eq!(
            receipt["endpoint_resource_uri"],
            "hooklistener://endpoints/by-slug/github-webhooks"
        );
        assert_eq!(receipt["request"]["method"], "POST");
    }

    #[test]
    fn listen_forward_success_event_includes_delivery_receipt() {
        let event = TunnelEvent::ForwardSuccess {
            request_id: "req_123".to_string(),
            target_url: "http://localhost:3000/webhooks/github".to_string(),
            status: 204,
            duration_ms: 37,
        };
        let receipt = listen_event_receipt(
            &event,
            "github-webhooks",
            "http://localhost:3000/webhooks",
            None,
        );

        assert_eq!(receipt["event"], "forward_succeeded");
        assert_eq!(receipt["status"], "succeeded");
        assert_eq!(receipt["status_code"], 204);
        assert_eq!(receipt["duration_ms"], 37);
        assert_eq!(
            receipt["request_resource_uri"],
            "hooklistener://requests/req_123"
        );
    }

    #[test]
    fn tunnel_established_event_includes_public_url_and_resource() {
        let event = TunnelEvent::TunnelEstablished {
            subdomain: "plant-07.hook.events".to_string(),
            tunnel_id: "tun_123".to_string(),
            is_static: false,
        };
        let receipt = tunnel_event_receipt(&event, "localhost", 3000, Some("org_123"), None);

        assert_eq!(receipt["type"], "receipt");
        assert_eq!(receipt["event"], "tunnel_established");
        assert_eq!(receipt["resource_uri"], "hooklistener://tunnels/tun_123");
        assert_eq!(receipt["public_url"], "https://plant-07.hook.events");
        assert_eq!(receipt["local_target_url"], "http://localhost:3000");
        assert_eq!(receipt["organization_id"], "org_123");
    }

    #[test]
    fn tunnel_request_event_includes_local_target_and_request_resource() {
        let mut headers = std::collections::HashMap::new();
        headers.insert("content-type".to_string(), "application/json".to_string());
        let event = TunnelEvent::RequestReceived {
            request_id: "req_123".to_string(),
            method: "POST".to_string(),
            path: "webhooks/github".to_string(),
            headers,
            body: Some("{\"ok\":true}".to_string()),
            query_string: "delivery=abc".to_string(),
        };
        let receipt = tunnel_event_receipt(&event, "127.0.0.1", 8080, None, Some("dev"));

        assert_eq!(receipt["event"], "request_received");
        assert_eq!(receipt["resource_uri"], "hooklistener://requests/req_123");
        assert_eq!(
            receipt["local_target_url"],
            "http://127.0.0.1:8080/webhooks/github?delivery=abc"
        );
        assert_eq!(receipt["body_size"], 11);
        assert_eq!(receipt["headers"]["content-type"], "application/json");
    }

    #[test]
    fn validate_forward_target_url_requires_http_or_https() {
        assert!(validate_forward_target_url("http://localhost:3000/webhook").is_ok());
        assert!(validate_forward_target_url("https://example.com/webhook").is_ok());

        let err = validate_forward_target_url("ftp://example.com/webhook").unwrap_err();
        assert!(
            err.to_string().contains("Use http or https"),
            "unexpected error: {}",
            err
        );
    }

    #[test]
    fn normalize_http_method_accepts_lowercase() {
        let method = normalize_http_method(Some("post".to_string())).unwrap();
        assert_eq!(method.as_deref(), Some("POST"));
    }

    #[test]
    fn normalize_http_method_rejects_invalid_values() {
        let err = normalize_http_method(Some("TRACE".to_string())).unwrap_err();
        assert!(
            err.to_string().contains("Invalid HTTP method"),
            "unexpected error: {}",
            err
        );
    }
}
