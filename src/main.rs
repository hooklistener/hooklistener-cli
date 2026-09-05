mod api;
mod app;
mod auth;
mod config;
mod errors;
mod logger;
mod logo;
mod models;
mod output;
mod syntax;
mod target_policy;
mod theme;
mod tunnel;
mod tunnel_v3;
mod ui;
mod updater;

use anyhow::{Result, anyhow};
use chrono::{Duration as ChronoDuration, Utc};
use clap::{ArgAction, Args, CommandFactory, Parser, Subcommand, ValueEnum};
use comfy_table::{ContentArrangement, Table, presets::UTF8_FULL_CONDENSED};
use crossterm::{
    cursor::{MoveToColumn, Show},
    event::{self, Event, KeyEventKind},
    execute,
    terminal::{
        Clear, ClearType, EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode,
        enable_raw_mode,
    },
};
use ratatui::{Terminal, backend::CrosstermBackend};
use reqwest::Url;
use std::future::Future;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;
use std::time::Duration;
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::sleep,
};
use tracing::{error, warn};

use api::ApiClient;
use app::{App, AppState, FeedbackKind};
use logger::{LogConfig, Logger};
use output::{ColorMode, Stylize};
use tunnel::TunnelEvent;

#[derive(Parser)]
#[command(name = "hooklistener")]
#[command(about = "Inspect webhooks, replay failures, and expose localhost from your terminal")]
#[command(version)]
#[command(
    after_help = "COMMAND GROUPS:\n  Capture and delivery: listen, tunnel, endpoint, static-tunnel, anon\n  Review and automation: cases, share, monitor\n  Account and settings: login, logout, org, config\n  Maintenance: diagnostics, clean-logs, completions, update\n\nCOMMON WORKFLOWS:\n  Sign in:\n    hooklistener login\n\n  Forward an existing debug endpoint:\n    hooklistener listen <endpoint-slug> --target http://localhost:3000\n\n  Expose a local HTTP server:\n    hooklistener tunnel --port 3000\n\n  Create and inspect hosted captures:\n    hooklistener endpoint create <name>\n    hooklistener endpoint list-requests <endpoint-id>"
)]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,

    /// Output supported command responses or event streams as JSON
    #[arg(long, global = true, help_heading = "Global options")]
    json: bool,

    /// Styling policy for human output
    #[arg(
        long,
        global = true,
        value_enum,
        default_value_t,
        help_heading = "Global options"
    )]
    color: ColorMode,

    /// Confirm destructive commands without an interactive prompt
    #[arg(long, global = true, help_heading = "Global options")]
    yes: bool,

    /// Log level
    #[arg(
        long,
        global = true,
        value_enum,
        ignore_case = true,
        default_value_t = LogLevel::Info,
        value_name = "LEVEL",
        help_heading = "Global options"
    )]
    log_level: LogLevel,

    /// Directory for log files
    #[arg(
        long,
        global = true,
        value_name = "DIR",
        help_heading = "Global options"
    )]
    log_dir: Option<PathBuf>,

    /// Also write logs to stdout
    #[arg(long, global = true, help_heading = "Global options")]
    log_stdout: bool,

    /// Allow a non-loopback cleartext Hooklistener server (development only)
    #[arg(long, global = true, hide = true)]
    allow_insecure_dev_server: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
enum LogLevel {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

impl LogLevel {
    fn as_str(self) -> &'static str {
        match self {
            LogLevel::Trace => "trace",
            LogLevel::Debug => "debug",
            LogLevel::Info => "info",
            LogLevel::Warn => "warn",
            LogLevel::Error => "error",
        }
    }
}

#[derive(Subcommand)]
enum Commands {
    /// Forward events from a debug endpoint to a local URL
    Listen {
        /// Debug endpoint slug (from `endpoint list`)
        endpoint_slug: String,

        /// Local URL to forward requests to
        #[arg(
            short,
            long,
            value_name = "URL",
            default_value = "http://localhost:3000"
        )]
        target: String,

        /// WebSocket server URL; requires wss except for approved development use
        #[arg(long, value_name = "URL")]
        ws_url: Option<String>,

        /// Allow forwarding to a target that resolves outside loopback
        #[arg(long)]
        allow_non_loopback: bool,

        /// Disable target TLS certificate verification (visible and plan-bound)
        #[arg(long)]
        insecure_tls: bool,
    },
    /// Expose a local HTTP server on a public Hooklistener URL
    #[command(
        after_help = "TARGET FLAGS:\n  Flags given before a subcommand set defaults; the same flag on the subcommand overrides them.\n  hooklistener tunnel --port 3000          same as: hooklistener tunnel start --port 3000"
    )]
    Tunnel {
        #[command(subcommand)]
        action: Option<TunnelAction>,

        #[command(flatten)]
        target: TunnelTargetArgs,
    },
    /// Manage debug endpoints and their captured requests
    Endpoint {
        #[command(subcommand)]
        action: EndpointAction,
    },
    /// Reserve and manage static tunnel slugs
    StaticTunnel {
        #[command(subcommand)]
        action: StaticTunnelAction,
    },
    /// Create temporary endpoints and tunnels (no login required)
    Anon {
        #[command(subcommand)]
        action: AnonAction,
    },
    /// Run saved replay cases against a target
    Cases {
        #[command(subcommand)]
        action: CasesAction,
    },
    /// Create, inspect, and revoke public links to captured requests
    Share {
        #[command(subcommand)]
        action: ShareAction,
    },
    /// Create and inspect uptime monitors
    Monitor {
        #[command(subcommand)]
        action: MonitorAction,
    },
    /// Sign in with the device flow
    Login {
        /// Start a new sign-in even if a valid token exists
        #[arg(long)]
        force: bool,
    },
    /// Sign out and delete the stored token
    Logout,
    /// List organizations and set the default one
    Org {
        #[command(subcommand)]
        action: OrgAction,
    },
    /// Show and set CLI configuration
    Config {
        #[command(subcommand)]
        action: ConfigAction,
    },
    /// Write a diagnostic bundle for support
    Diagnostics {
        /// Directory for the diagnostic bundle
        #[arg(short, long, value_name = "DIR", default_value = ".")]
        output: PathBuf,
    },
    /// Delete old log files
    CleanLogs {
        /// Number of log files to keep
        #[arg(short, long, value_name = "N", default_value = "10")]
        keep: usize,
    },
    /// Print a shell completion script
    Completions {
        /// Shell to generate completions for
        #[arg(value_enum, ignore_case = true, value_name = "SHELL")]
        shell: CompletionShell,
    },
    /// Update hooklistener to the latest release
    Update,
}

#[derive(Args, Clone, Default)]
struct TunnelTargetArgs {
    // `port` and `host` stay `Option` so `merge()` can distinguish "not given"
    // from "given the default"; the default is written into the doc comment
    // in clap's own `[default: ...]` rendering style.
    /// Local port to forward requests to [default: 3000]
    #[arg(short, long)]
    port: Option<u16>,

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
    fn merge(self, action_target: Self) -> Self {
        Self {
            port: action_target.port.or(self.port),
            host: action_target.host.or(self.host),
            org: action_target.org.or(self.org),
            slug: action_target.slug.or(self.slug),
            allow_non_loopback: self.allow_non_loopback || action_target.allow_non_loopback,
            no_replay_buffered: self.no_replay_buffered || action_target.no_replay_buffered,
        }
    }

    fn resolve(self) -> TunnelTarget {
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
struct TunnelTarget {
    port: u16,
    host: String,
    org: Option<String>,
    slug: Option<String>,
    allow_non_loopback: bool,
    no_replay_buffered: bool,
}

#[derive(Subcommand)]
enum TunnelAction {
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
        /// Poll interval while following, such as 500ms, 2s, or 1m
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

#[derive(Clone, Copy, Debug, ValueEnum)]
enum CompletionShell {
    Bash,
    Zsh,
    Fish,
    #[value(name = "powershell", alias = "power-shell")]
    PowerShell,
    Elvish,
}

/// Generate the completion script for `shell` into `out`.
///
/// The script is rendered into memory first so that a single `write_all`
/// carries it to `out`; a closed pipe then surfaces as one `io::Error`
/// instead of a panic inside `clap_complete`.
fn write_completions(shell: CompletionShell, out: &mut dyn io::Write) -> io::Result<()> {
    use clap_complete::generate;
    use clap_complete::shells::{Bash, Elvish, Fish, PowerShell, Zsh};

    let mut command = Cli::command();
    let bin_name = command.get_name().to_string();
    let mut buf: Vec<u8> = Vec::new();

    match shell {
        CompletionShell::Bash => generate(Bash, &mut command, bin_name, &mut buf),
        CompletionShell::Zsh => generate(Zsh, &mut command, bin_name, &mut buf),
        CompletionShell::Fish => generate(Fish, &mut command, bin_name, &mut buf),
        CompletionShell::PowerShell => generate(PowerShell, &mut command, bin_name, &mut buf),
        CompletionShell::Elvish => generate(Elvish, &mut command, bin_name, &mut buf),
    }

    out.write_all(&buf).and_then(|_| out.flush())
}

/// Write the completion script for `shell` to `out`, treating a closed pipe
/// (for example `completions bash | head -1`) as success.
fn print_completions(shell: CompletionShell, out: &mut dyn io::Write) -> io::Result<()> {
    match write_completions(shell, out) {
        Err(err) if err.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        result => result,
    }
}

#[derive(Subcommand)]
enum ConfigAction {
    /// Show the current configuration
    Show,
    /// Set a configuration value
    Set {
        /// Configuration key
        #[arg(value_enum)]
        key: ConfigKey,
        /// New value
        value: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum ConfigKey {
    #[value(name = "selected_organization_id")]
    SelectedOrganizationId,
}

#[derive(Subcommand)]
enum OrgAction {
    /// List organizations available to your account
    List,
    /// Set the default organization
    Use {
        /// Organization ID (from `org list`)
        org_id: String,
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
        /// Custom slug
        #[arg(long)]
        slug: Option<String>,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// List debug endpoints for an organization
    List {
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Show a single debug endpoint by ID
    Show {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Delete a debug endpoint by ID
    Delete {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// List captured requests for an endpoint
    #[command(name = "list-requests", visible_alias = "requests")]
    ListRequests {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Page number
        #[arg(long, value_name = "N", default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
        page: u32,
        /// Results per page
        #[arg(long, value_name = "N", default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..))]
        page_size: u32,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Show a captured request
    #[command(name = "show-request", visible_alias = "request")]
    ShowRequest {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Captured request ID
        request_id: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Delete a captured request
    DeleteRequest {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Captured request ID
        request_id: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Replay a captured request to a target URL
    ForwardRequest {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Captured request ID
        request_id: String,
        /// URL to replay the request to
        #[arg(value_name = "URL")]
        target_url: String,
        /// HTTP method override
        #[arg(long, value_enum, ignore_case = true)]
        method: Option<HttpMethod>,
        /// Validate scope and print the forward plan without queueing delivery
        #[arg(long)]
        dry_run: bool,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// List forwards of a captured request
    #[command(name = "list-forwards", visible_alias = "forwards")]
    ListForwards {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Captured request ID
        request_id: String,
        /// Page number
        #[arg(long, value_name = "N", default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
        page: u32,
        /// Results per page
        #[arg(long, value_name = "N", default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..))]
        page_size: u32,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Show a forward by ID
    #[command(name = "show-forward", visible_alias = "forward")]
    ShowForward {
        /// Forward ID
        forward_id: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
enum CasesAction {
    /// Run saved cases for an endpoint
    Run {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Target: a URL, a saved target ID, or cli for the running listen session
        #[arg(long, value_name = "TARGET", conflicts_with_all = ["target_url", "target_id"])]
        target: Option<String>,
        #[arg(long, hide = true, value_name = "URL", conflicts_with_all = ["target", "target_id"])]
        target_url: Option<String>,
        #[arg(long, hide = true, value_name = "TARGET_ID", conflicts_with_all = ["target", "target_url"])]
        target_id: Option<String>,
        /// Display name recorded for the target
        #[arg(long, value_name = "NAME")]
        target_name: Option<String>,
        /// Wait for the run to complete before returning
        #[arg(long)]
        wait: bool,
        /// Timeout for --wait, such as 60, 2m, or 1h
        #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
        timeout: Option<Duration>,
        #[arg(long, hide = true, value_name = "MS", conflicts_with = "timeout")]
        timeout_ms: Option<u64>,
        /// Poll interval for --wait, such as 500ms, 2s, or 10s
        #[arg(long, value_name = "DURATION", value_parser = parse_duration)]
        interval: Option<Duration>,
        #[arg(long, hide = true, value_name = "MS", conflicts_with = "interval")]
        interval_ms: Option<u64>,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
enum StaticTunnelAction {
    /// List reserved static tunnel slugs
    List {
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Reserve a static tunnel slug
    Create {
        /// Slug to reserve
        slug: String,
        /// Display name
        #[arg(long)]
        name: Option<String>,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Release a static tunnel slug
    Delete {
        /// Static tunnel ID (from `static-tunnel list`)
        static_tunnel_id: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
enum AnonAction {
    /// Create a temporary anonymous endpoint
    Create {
        /// Endpoint lifetime, such as 3600, 1h, or 24h
        #[arg(long, value_name = "DURATION", default_value = "24h", value_parser = parse_duration)]
        ttl: Duration,
    },
    /// Show an anonymous endpoint
    Show {
        /// Anonymous endpoint ID (from `anon create`)
        endpoint_id: String,
    },
    /// List captured events for an anonymous endpoint
    #[command(name = "list-events", visible_alias = "events")]
    ListEvents {
        /// Anonymous endpoint ID (from `anon create`)
        endpoint_id: String,
        /// Viewer token (from `anon create`)
        #[arg(long, value_name = "VIEWER_TOKEN")]
        token: String,
        /// Page number
        #[arg(long, value_name = "N", default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
        page: u32,
        /// Results per page
        #[arg(long, value_name = "N", default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..))]
        page_size: u32,
    },
    /// Show a captured event
    #[command(name = "show-event", visible_alias = "event")]
    ShowEvent {
        /// Anonymous endpoint ID (from `anon create`)
        endpoint_id: String,
        /// Event ID (from `anon events`)
        event_id: String,
        /// Viewer token (from `anon create`)
        #[arg(long, value_name = "VIEWER_TOKEN")]
        token: String,
    },
    /// Expose a local HTTP server without signing in
    Tunnel {
        /// Local port to forward requests to
        #[arg(short, long, default_value = "3000")]
        port: u16,
        /// Local host to forward to
        #[arg(long, default_value = "localhost")]
        host: String,
        /// Stable public route name
        #[arg(long)]
        name: Option<String>,
        /// Route lifetime, such as 300, 10m, or 30m
        #[arg(
            long,
            value_name = "DURATION",
            default_value = "15m",
            value_parser = parse_duration_within(Duration::from_secs(60), Duration::from_secs(1800))
        )]
        ttl: Duration,
        /// Allow forwarding to a host that resolves outside loopback
        #[arg(long)]
        allow_non_loopback: bool,
    },
    /// Claim an anonymous route into an organization
    Claim {
        /// Anonymous route ID (from `anon tunnel`)
        #[arg(value_name = "ROUTE_ID")]
        route_id: String,
        /// Claim token (from `anon tunnel`)
        #[arg(long, value_name = "CLAIM_TOKEN")]
        token: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
enum ShareAction {
    /// Create a public link for a captured request
    Create {
        /// Captured request ID (from `endpoint list-requests`)
        request_id: String,
        /// Link lifetime in whole hours, such as 24h, 86400, or 7d
        #[arg(long, value_name = "DURATION", value_parser = parse_whole_hours)]
        expires_in: Option<Duration>,
        #[arg(long, hide = true, value_name = "HOURS", conflicts_with = "expires_in")]
        expires_in_hours: Option<u64>,
        /// Password required to open the link
        #[arg(long)]
        password: Option<String>,
        /// Include forwards in the shared view
        #[arg(long)]
        include_forwards: bool,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// List links for a captured request
    List {
        /// Captured request ID (from `endpoint list-requests`)
        request_id: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Show a shared request by token (no login required)
    Show {
        /// Share token (from `share create`)
        share_token: String,
    },
    /// Revoke a public link
    Revoke {
        /// Share token (from `share create`)
        share_token: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
}

#[derive(Subcommand)]
enum MonitorAction {
    /// Create an uptime monitor
    Create {
        /// Monitor display name
        name: String,
        /// URL to check (http:// or https://)
        url: String,
        /// HTTP method
        #[arg(long, value_enum, ignore_case = true, default_value_t = MonitorMethod::Get)]
        method: MonitorMethod,
        /// Expected HTTP status code
        #[arg(long, value_name = "CODE", default_value_t = 200, value_parser = clap::value_parser!(u16).range(100..=599))]
        expected_status: u16,
        /// Check interval
        #[arg(long, value_enum, default_value = "5m", ignore_case = true)]
        interval: MonitorInterval,
        /// Text the response body must contain
        #[arg(long, value_name = "TEXT")]
        body_contains: Option<String>,
        /// Request body to send with POST, PUT, or PATCH
        #[arg(long)]
        body: Option<String>,
        /// Consecutive failures before alerting
        #[arg(long, value_name = "N", default_value_t = 2, value_parser = clap::value_parser!(u32).range(1..))]
        failure_threshold: u32,
        /// Disable email notifications for this monitor
        #[arg(long, conflicts_with = "email")]
        no_email: bool,
        #[arg(
            long,
            hide = true,
            default_value_t = true,
            action = ArgAction::Set,
            num_args = 0..=1,
            default_missing_value = "true",
            conflicts_with = "no_email"
        )]
        email: bool,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// List uptime monitors
    List {
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Show an uptime monitor
    Show {
        /// Monitor ID (from `monitor list`)
        monitor_id: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Update an uptime monitor
    Update {
        /// Monitor ID (from `monitor list`)
        monitor_id: String,
        /// New name
        #[arg(long)]
        name: Option<String>,
        /// New URL
        #[arg(long)]
        url: Option<String>,
        /// HTTP method
        #[arg(long, value_enum, ignore_case = true)]
        method: Option<MonitorMethod>,
        /// Expected HTTP status code
        #[arg(long, value_name = "CODE", value_parser = clap::value_parser!(u16).range(100..=599))]
        expected_status: Option<u16>,
        /// Check interval
        #[arg(long, value_enum, ignore_case = true)]
        interval: Option<MonitorInterval>,
        /// Resume checks for this monitor
        #[arg(long, conflicts_with_all = ["disable", "enabled"])]
        enable: bool,
        /// Pause checks for this monitor
        #[arg(long, conflicts_with_all = ["enable", "enabled"])]
        disable: bool,
        #[arg(long, hide = true, value_name = "BOOL")]
        enabled: Option<bool>,
        /// Consecutive failures before alerting
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
        failure_threshold: Option<u32>,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Delete an uptime monitor
    Delete {
        /// Monitor ID (from `monitor list`)
        monitor_id: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// List recent checks for a monitor
    Checks {
        /// Monitor ID (from `monitor list`)
        monitor_id: String,
        /// Page number
        #[arg(long, value_name = "N", default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
        page: u32,
        /// Results per page
        #[arg(long, value_name = "N", default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..))]
        page_size: u32,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
}

/// HTTP method for `endpoint forward-request` (sent uppercase on the wire).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum HttpMethod {
    #[value(name = "GET")]
    Get,
    #[value(name = "POST")]
    Post,
    #[value(name = "PUT")]
    Put,
    #[value(name = "PATCH")]
    Patch,
    #[value(name = "DELETE")]
    Delete,
    #[value(name = "HEAD")]
    Head,
    #[value(name = "OPTIONS")]
    Options,
}

impl HttpMethod {
    fn as_uppercase(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
            Self::Patch => "PATCH",
            Self::Delete => "DELETE",
            Self::Head => "HEAD",
            Self::Options => "OPTIONS",
        }
    }
}

/// HTTP method for uptime monitors (sent lowercase on the wire; no OPTIONS).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum MonitorMethod {
    #[value(name = "GET")]
    Get,
    #[value(name = "POST")]
    Post,
    #[value(name = "PUT")]
    Put,
    #[value(name = "PATCH")]
    Patch,
    #[value(name = "DELETE")]
    Delete,
    #[value(name = "HEAD")]
    Head,
}

impl MonitorMethod {
    fn as_lowercase(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Post => "post",
            Self::Put => "put",
            Self::Patch => "patch",
            Self::Delete => "delete",
            Self::Head => "head",
        }
    }
}

// Required by `default_value_t`; renders the help default as `[default: GET]`.
impl std::fmt::Display for MonitorMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.to_possible_value().unwrap().get_name())
    }
}

/// Check interval accepted by the monitor API, in minutes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
enum MonitorInterval {
    #[value(name = "1m", alias = "1")]
    M1,
    #[value(name = "5m", alias = "5")]
    M5,
    #[value(name = "10m", alias = "10")]
    M10,
    #[value(name = "30m", alias = "30")]
    M30,
    #[value(name = "60m", alias = "60", alias = "1h")]
    M60,
}

impl MonitorInterval {
    /// Wire value for `check_interval`.
    fn minutes(self) -> u32 {
        match self {
            MonitorInterval::M1 => 1,
            MonitorInterval::M5 => 5,
            MonitorInterval::M10 => 10,
            MonitorInterval::M30 => 30,
            MonitorInterval::M60 => 60,
        }
    }
}

const MILLIS_PER_SECOND: u64 = 1_000;
const MILLIS_PER_MINUTE: u64 = 60 * MILLIS_PER_SECOND;
const MILLIS_PER_HOUR: u64 = 60 * MILLIS_PER_MINUTE;
const MILLIS_PER_DAY: u64 = 24 * MILLIS_PER_HOUR;

/// Parses a duration such as `60`, `1500ms`, `2m`, `1h`, or `7d`.
///
/// A bare integer is seconds. Unit names are case-insensitive and may be
/// separated from the number by whitespace.
fn parse_duration(raw: &str) -> std::result::Result<Duration, String> {
    let value = raw.trim();
    if value.is_empty() {
        return Err("Duration cannot be empty.".to_string());
    }

    let split_at = value
        .find(|c: char| !c.is_ascii_digit())
        .unwrap_or(value.len());
    let (number, unit) = value.split_at(split_at);
    if number.is_empty() {
        return Err("Duration must start with a number.".to_string());
    }
    let amount: u64 = number
        .parse()
        .map_err(|_| "Duration is too large.".to_string())?;
    let millis_per_unit = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "s" | "sec" | "secs" | "second" | "seconds" => MILLIS_PER_SECOND,
        "ms" | "millisecond" | "milliseconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => MILLIS_PER_MINUTE,
        "h" | "hr" | "hrs" | "hour" | "hours" => MILLIS_PER_HOUR,
        "d" | "day" | "days" => MILLIS_PER_DAY,
        other => {
            return Err(format!(
                "Invalid duration unit '{other}'. Use ms, s, m, h, or d."
            ));
        }
    };
    amount
        .checked_mul(millis_per_unit)
        .map(Duration::from_millis)
        .ok_or_else(|| "Duration is too large.".to_string())
}

/// Value parser for a duration flag that must fall within `min..=max`.
fn parse_duration_within(
    min: Duration,
    max: Duration,
) -> impl clap::builder::TypedValueParser<Value = Duration> {
    move |raw: &str| -> std::result::Result<Duration, String> {
        let duration = parse_duration(raw)?;
        if duration < min || duration > max {
            return Err(format!(
                "must be between {} and {}",
                format_duration(min),
                format_duration(max)
            ));
        }
        Ok(duration)
    }
}

/// Value parser for a duration flag whose API field counts whole hours.
fn parse_whole_hours(raw: &str) -> std::result::Result<Duration, String> {
    let duration = parse_duration(raw)?;
    let millis = duration_millis(duration);
    if millis == 0 || !millis.is_multiple_of(MILLIS_PER_HOUR) {
        return Err("must be a whole number of hours, such as 24h, 86400, or 7d".to_string());
    }
    Ok(duration)
}

fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn duration_hours(duration: Duration) -> u64 {
    duration.as_secs() / (MILLIS_PER_HOUR / MILLIS_PER_SECOND)
}

/// Renders a duration in the largest unit that expresses it exactly.
fn format_duration(duration: Duration) -> String {
    let millis = duration_millis(duration);
    if millis == 0 {
        return "0s".to_string();
    }
    for (unit, per_unit) in [
        ("d", MILLIS_PER_DAY),
        ("h", MILLIS_PER_HOUR),
        ("m", MILLIS_PER_MINUTE),
        ("s", MILLIS_PER_SECOND),
    ] {
        if millis.is_multiple_of(per_unit) {
            return format!("{}{unit}", millis / per_unit);
        }
    }
    format!("{millis}ms")
}

/// Prints the single stderr notice for a hidden compatibility flag.
fn warn_deprecated_flag(old: &str, new: &str, example: &str) {
    eprintln!("warning: --{old} is deprecated; use --{new} {example}");
}

/// Resolves a canonical duration flag against its hidden `--<flag>-ms` twin.
///
/// The hidden flag wins when given (clap already rejects supplying both
/// explicitly; a defaulted canonical flag must not mask it) and prints a
/// deprecation notice.
fn resolve_millis_flag(
    duration: Option<Duration>,
    legacy_ms: Option<u64>,
    old_flag: &str,
    new_flag: &str,
) -> Option<u64> {
    if let Some(ms) = legacy_ms {
        warn_deprecated_flag(
            old_flag,
            new_flag,
            &format_duration(Duration::from_millis(ms)),
        );
        return Some(ms);
    }
    duration.map(duration_millis)
}

struct CaseRunInput {
    target: Option<String>,
    target_url: Option<String>,
    target_id: Option<String>,
    target_name: Option<String>,
    wait: bool,
    timeout: Option<Duration>,
    timeout_ms: Option<u64>,
    interval: Option<Duration>,
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
        interval,
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
        timeout_ms: timeout.map(duration_millis).or(timeout_ms),
        interval_ms: interval.map(duration_millis).or(interval_ms),
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
            "Target is required. Use --target <URL|TARGET_ID|cli>."
        ));
    }

    Ok(params)
}

/// Effective email setting for `monitor create`: `--no-email` wins over the
/// hidden `--email <BOOL>` shape (clap already rejects supplying both).
fn monitor_email_enabled(email: bool, no_email: bool) -> bool {
    email && !no_email
}

/// Effective `enabled` update for `monitor update`: `--enable` / `--disable`
/// take precedence over the hidden `--enabled <BOOL>` shape.
fn monitor_enabled_update(enable: bool, disable: bool, enabled: Option<bool>) -> Option<bool> {
    if enable {
        Some(true)
    } else if disable {
        Some(false)
    } else {
        enabled
    }
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
    format!("hooklistener endpoint show-forward {forward_id}")
}

fn emitted_at() -> String {
    Utc::now().to_rfc3339()
}

fn effective_listen_ws_url(ws_url: Option<&str>) -> Result<String> {
    let ws_url = ws_url
        .map(str::to_string)
        .or_else(|| std::env::var("HOOKLISTENER_WS_URL").ok())
        .unwrap_or_else(|| "wss://api.hooklistener.com".to_string());
    api::validate_websocket_base_url(&ws_url)?;
    Ok(ws_url)
}

fn command_event_receipt(
    schema: &str,
    command: &str,
    operation: &str,
    event: &str,
    status: &str,
) -> serde_json::Value {
    serde_json::json!({
        "$schema": schema,
        "schema_version": 1,
        "type": "event",
        "event_id": uuid::Uuid::new_v4(),
        "sequence": 0,
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
    let mut receipt = command_event_receipt(
        LISTEN_EVENT_SCHEMA,
        "listen",
        "listen_endpoint",
        event,
        status,
    );
    receipt["endpoint_slug"] = serde_json::json!(endpoint_slug);
    receipt["target_url"] = serde_json::json!(target_url);
    receipt["resource_uri"] = serde_json::json!(listen_session_resource_uri(endpoint_slug));
    receipt
}

fn tunnel_event_base(event: &str, status: &str) -> serde_json::Value {
    command_event_receipt(
        TUNNEL_EVENT_SCHEMA,
        "tunnel",
        "start_local_tunnel",
        event,
        status,
    )
}

fn redact_tunnel_headers(
    headers: &std::collections::HashMap<String, String>,
) -> std::collections::HashMap<String, String> {
    headers
        .iter()
        .map(|(name, value)| {
            let value = if sensitive_header_name(name) {
                "[REDACTED]".to_string()
            } else {
                value.clone()
            };

            (name.clone(), value)
        })
        .collect()
}

fn sensitive_header_name(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase().replace('_', "-");

    matches!(
        normalized.as_str(),
        "authorization" | "proxy-authorization" | "cookie" | "set-cookie"
    ) || normalized.contains("token")
        || normalized.contains("secret")
        || normalized.contains("api-key")
        || normalized.ends_with("-key")
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
    ws_url: &str,
    endpoint: Option<&api::DebugEndpointSummary>,
) -> serde_json::Value {
    let (endpoint_resource_uri, requests_resource_uri, endpoint_value) =
        endpoint_receipt_parts(endpoint_slug, endpoint);
    let session_resource_uri = listen_session_resource_uri(endpoint_slug);
    let inspect_command = endpoint
        .map(|endpoint| format!("hooklistener endpoint list-requests {}", endpoint.id))
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
                "hooklistener endpoint show-request <endpoint-id> {}",
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
        "$schema": TUNNEL_RECEIPT_SCHEMA,
        "schema_version": 1,
        "type": "receipt",
        "event_id": uuid::Uuid::new_v4(),
        "sequence": 0,
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
            replay,
        } => {
            let request_resource_uri = request_resource_uri(request_id);
            let mut receipt = tunnel_event_base("request_received", "received");
            receipt["request_id"] = serde_json::json!(request_id);
            receipt["replay"] = serde_json::json!(replay);
            receipt["resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["request_resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["method"] = serde_json::json!(method);
            receipt["path"] = serde_json::json!(path);
            receipt["query_string"] = serde_json::json!(query_string);
            receipt["local_target_url"] =
                serde_json::json!(local_tunnel_request_target(host, port, path, query_string));
            receipt["headers"] = serde_json::json!(redact_tunnel_headers(headers));
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
            receipt["response_headers"] =
                serde_json::json!(redact_tunnel_headers(response_headers));
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
        TunnelEvent::StreamGap { dropped_events } => {
            let mut receipt = tunnel_event_base("stream_gap", "recoverable");
            receipt["resource_uri"] = serde_json::json!(tunnel_session_resource_uri(host, port));
            receipt["dropped_events"] = serde_json::json!(dropped_events);
            receipt["delivery_affected"] = serde_json::json!(false);
            receipt["next_actions"] = serde_json::json!([
                "Treat the request presentation stream as incomplete; relay responses remain active."
            ]);
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
        TunnelEvent::BufferedSummary {
            count,
            oldest_captured_at,
        } => {
            let mut receipt = tunnel_event_base("buffered_summary", "buffered");
            receipt["resource_uri"] = serde_json::json!(tunnel_session_resource_uri(host, port));
            receipt["count"] = serde_json::json!(count);
            receipt["oldest_captured_at"] = serde_json::json!(oldest_captured_at);
            receipt
        }
        TunnelEvent::BufferedReplayed { capture_id, status } => {
            let request_resource_uri = request_resource_uri(capture_id);
            let mut receipt = tunnel_event_base("buffered_replayed", "succeeded");
            receipt["capture_id"] = serde_json::json!(capture_id);
            receipt["resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["request_resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["status_code"] = serde_json::json!(status);
            receipt
        }
        TunnelEvent::BufferedReplayFailed { capture_id, reason } => {
            let request_resource_uri = request_resource_uri(capture_id);
            let mut receipt = tunnel_event_base("buffered_replay_failed", "failed");
            receipt["capture_id"] = serde_json::json!(capture_id);
            receipt["resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["request_resource_uri"] = serde_json::json!(&request_resource_uri);
            receipt["error"] = serde_json::json!(reason);
            receipt["next_actions"] = serde_json::json!([
                "The request stays buffered. Check the local target and reconnect to retry."
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
    let forwards_command =
        format!("hooklistener endpoint list-forwards {endpoint_id} {request_id}");

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
    let endpoint_id = sanitize_terminal(endpoint_id, TerminalTextLayout::Inline);
    let organization_id = sanitize_terminal(organization_id, TerminalTextLayout::Inline);
    let request_id = sanitize_terminal(request_id, TerminalTextLayout::Inline);

    print_status_block(
        OutputStatus::Info,
        "FORWARD PREVIEW",
        &[
            output_field("DRY RUN", "true"),
            output_field("WOULD CREATE", "debug_request_forward"),
            output_field(
                "TARGET URL",
                sanitize_terminal(target_url, TerminalTextLayout::Inline).underlined(),
            ),
            output_field(
                "METHOD",
                sanitize_terminal(&method, TerminalTextLayout::Inline).bold(),
            ),
            output_field("REQUEST", request_id.dim()),
            output_field(
                "RESOURCE",
                sanitize_terminal(&request_resource_uri, TerminalTextLayout::Inline).dim(),
            ),
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
    let forward_resource = forward_resource_uri(&response.forward_id);
    let poll_command = forward_poll_command(&response.forward_id);

    print_status_block(
        OutputStatus::Ok,
        "FORWARD ACCEPTED",
        &[
            output_field(
                "FORWARD ID",
                sanitize_terminal(&response.forward_id, TerminalTextLayout::Inline).bold(),
            ),
            output_field(
                "STATUS",
                sanitize_terminal(&response.status, TerminalTextLayout::Inline).bold(),
            ),
            output_field(
                "TARGET URL",
                sanitize_terminal(&response.target_url, TerminalTextLayout::Inline).underlined(),
            ),
            output_field(
                "RESOURCE",
                sanitize_terminal(&forward_resource, TerminalTextLayout::Inline).dim(),
            ),
            output_field(
                "POLL",
                sanitize_terminal(&poll_command, TerminalTextLayout::Inline).dim(),
            ),
            output_field(
                "REQUEST",
                sanitize_terminal(request_id, TerminalTextLayout::Inline).dim(),
            ),
            output_field(
                "ENDPOINT",
                sanitize_terminal(endpoint_id, TerminalTextLayout::Inline).dim(),
            ),
            output_field(
                "ORGANIZATION",
                sanitize_terminal(organization_id, TerminalTextLayout::Inline).dim(),
            ),
        ],
    );
}

async fn run_endpoint_forward_request(
    endpoint_id: String,
    request_id: String,
    target_url: String,
    method: Option<HttpMethod>,
    dry_run: bool,
    org: Option<String>,
    json: bool,
) -> Result<()> {
    let mut config = config::Config::load()?;
    let organization_id = require_organization(org, &config)?;
    let token = ensure_valid_token(&mut config).await?;
    let method = method.map(HttpMethod::as_uppercase);
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
                method,
                &request,
            ))?;
        } else {
            print_forward_request_preview(
                &organization_id,
                &endpoint_id,
                &request_id,
                &target_url,
                method,
                &request,
            );
        }

        return Ok(());
    }

    let response = client
        .forward_endpoint_request(&endpoint_id, &request_id, &target_url, method)
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

#[derive(Debug, PartialEq, Eq)]
enum WorkerCompletion<T> {
    Stream(T),
    Shutdown,
}

async fn supervise_json_worker<T, S: Future<Output = T>, Q: Future<Output = Result<()>>>(
    worker: JoinHandle<()>,
    stream: S,
    shutdown: Q,
) -> Result<WorkerCompletion<T>> {
    tokio::pin!(stream);
    tokio::pin!(shutdown);
    let result = tokio::select! {
        value = &mut stream => WorkerCompletion::Stream(value),
        signal = &mut shutdown => { signal?; WorkerCompletion::Shutdown }
    };
    worker.abort();
    if let Err(error) = worker.await
        && !error.is_cancelled()
    {
        return Err(error.into());
    }
    Ok(result)
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

async fn run_listen_json(
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

async fn stream_tunnel_json_events(
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
async fn run_tunnel_json(
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

const SESSION_TOKEN_VALIDITY_DAYS: i64 = 60;

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    api::configure_server_url_security(cli.allow_insecure_dev_server);
    output::configure(cli.color, cli.json);
    let json = cli.json;

    if let Err(err) = run(cli).await {
        display_error(&err, json);
        std::process::exit(command_exit_code(&err));
    }
}

async fn run(cli: Cli) -> Result<()> {
    let Cli {
        command,
        json,
        color: _,
        yes,
        log_level,
        log_dir,
        log_stdout,
        allow_insecure_dev_server: _,
    } = cli;

    let Some(command) = command else {
        Cli::command().print_help()?;
        println!();
        return Ok(());
    };

    if json
        && matches!(
            &command,
            Commands::Login { .. } | Commands::Completions { .. }
        )
    {
        return Err(anyhow!(
            "This command does not support --json. Run it without --json."
        ));
    }

    // Spawn a background version check for eligible commands.
    let mut update_handle =
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
                level: log_level.as_str().to_string(),
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
            endpoint_slug,
            target,
            ws_url,
            allow_non_loopback,
            insecure_tls,
        } => {
            let ws_url = effective_listen_ws_url(ws_url.as_deref())?;
            // Initialize logging for tunnel
            let log_config = LogConfig {
                level: log_level.as_str().to_string(),
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
                    target_policy::TargetPolicy::resolve(&target, allow_non_loopback, insecure_tls)
                        .await?;
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
                    &mut update_handle,
                )
                .await;

                restore_terminal(&mut terminal)?;

                if let Err(err) = res {
                    error!(error = %err, "Application terminated with error");
                    return Err(err);
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
                        Some(_) => {
                            if config.is_token_valid() {
                                print_field("TOKEN", "(present, valid)".green());
                            } else {
                                print_field("TOKEN", "(present, expired)".red());
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
            ConfigAction::Set { key, value } => match key {
                ConfigKey::SelectedOrganizationId => {
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
                                        config
                                            .selected_organization_id
                                            .as_deref()
                                            .unwrap_or_default()
                                            .bold(),
                                    ),
                                ],
                            );
                        }
                    }
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
            OrgAction::Use { org_id } => {
                let mut config = config::Config::load()?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, None)?;
                let organizations = client.list_organizations().await?;
                let organization_name = organizations
                    .iter()
                    .find(|org| org.id == org_id)
                    .map(|org| org.name.clone())
                    .ok_or_else(|| {
                        anyhow!(
                            "Organization not found or not accessible with id: {}",
                            org_id
                        )
                    })?;

                config.selected_organization_id = Some(org_id.clone());
                config.save()?;
                if json {
                    print_json(&serde_json::json!({
                        "status": "ok",
                        "selected_organization_id": org_id,
                        "organization_name": organization_name
                    }))?;
                } else {
                    print_status_block(
                        OutputStatus::Ok,
                        "ORGANIZATION SELECTED",
                        &[
                            output_field("NAME", organization_name.bold()),
                            output_field("ORGANIZATION", org_id.dim()),
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
                if !confirm_destructive_action(
                    "DELETE ENDPOINT?",
                    &format!("endpoint {endpoint_id}"),
                    &organization_id,
                    yes,
                    json,
                )? {
                    return Ok(());
                }
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
            EndpointAction::ListRequests {
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
            EndpointAction::ShowRequest {
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
                if !confirm_destructive_action(
                    "DELETE CAPTURED REQUEST?",
                    &format!("request {request_id} from endpoint {endpoint_id}"),
                    &organization_id,
                    yes,
                    json,
                )? {
                    return Ok(());
                }
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
            EndpointAction::ListForwards {
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
            EndpointAction::ShowForward { forward_id, org } => {
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
                interval,
                interval_ms,
                org,
            } => {
                let timeout_ms = resolve_millis_flag(timeout, timeout_ms, "timeout-ms", "timeout");
                let interval_ms =
                    resolve_millis_flag(interval, interval_ms, "interval-ms", "interval");
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let params = build_case_run_params(CaseRunInput {
                    target,
                    target_url,
                    target_id,
                    target_name,
                    wait,
                    timeout: None,
                    timeout_ms,
                    interval: None,
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
            StaticTunnelAction::Delete {
                static_tunnel_id,
                org,
            } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                if !confirm_destructive_action(
                    "DELETE STATIC TUNNEL?",
                    &format!("static tunnel {static_tunnel_id}"),
                    &organization_id,
                    yes,
                    json,
                )? {
                    return Ok(());
                }
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let response = client
                    .delete_static_tunnel(&organization_id, &static_tunnel_id)
                    .await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "slug_id": static_tunnel_id,
                        "status": "deleted",
                        "message": response.message
                    }))?;
                } else {
                    print_status(OutputStatus::Ok, "STATIC TUNNEL DELETED");
                    println!();
                    print_field("SLUG/ID", static_tunnel_id);
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
                let endpoint = client.create_anon_endpoint(Some(ttl.as_secs())).await?;
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
            AnonAction::Show { endpoint_id } => {
                let client = ApiClient::unauthenticated()?;
                let status = client.get_anon_endpoint(&endpoint_id).await?;
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
            AnonAction::ListEvents {
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
            AnonAction::ShowEvent {
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
            AnonAction::Tunnel {
                port,
                host,
                name,
                ttl,
                allow_non_loopback,
            } => {
                run_anonymous_tunnel_activation(
                    host,
                    port,
                    name,
                    ttl.as_secs(),
                    allow_non_loopback,
                    json,
                    &mut update_handle,
                )
                .await?;
            }
            AnonAction::Claim {
                route_id,
                token,
                org,
            } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let access_token = ensure_valid_token(&mut config).await?;
                let client =
                    ApiClient::with_organization(access_token, Some(organization_id.clone()))?;
                let claimed = client
                    .claim_anonymous_tunnel_route(&route_id, &token)
                    .await?;

                if json {
                    print_json(&serde_json::json!({
                        "operation": "claim_anonymous_tunnel",
                        "status": "succeeded",
                        "organization_id": organization_id,
                        "route": claimed,
                    }))?;
                } else {
                    print_status(OutputStatus::Ok, "ANONYMOUS ROUTE CLAIMED");
                    println!();
                    print_field("ROUTE", claimed.id);
                    print_field(
                        "PUBLIC URL",
                        format!("https://{}.hook.events", claimed.slug),
                    );
                    print_field("ORGANIZATION", organization_id.dim());
                    print_field("CAPTURES TRANSFERRED", "0");
                    print_field(
                        "PRIVACY",
                        "Pre-claim captures were permanently discarded.".dim(),
                    );
                }
            }
        },
        Commands::Share { action } => match action {
            ShareAction::Create {
                request_id,
                expires_in,
                expires_in_hours,
                password,
                include_forwards,
                org,
            } => {
                if let Some(hours) = expires_in_hours {
                    warn_deprecated_flag("expires-in-hours", "expires-in", &format!("{hours}h"));
                }
                let expires_in_hours = expires_in_hours.or(expires_in.map(duration_hours));
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let shared = client
                    .create_shared_request(
                        &request_id,
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
            ShareAction::List { request_id, org } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let shares = client.list_shared_requests(&request_id).await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "debug_request_id": request_id,
                        "shares": shares
                    }))?;
                } else {
                    print_context("Organization:", &organization_id);
                    print_context("Request:", &request_id);
                    print_shared_requests(&shares);
                }
            }
            ShareAction::Show { share_token } => {
                let client = ApiClient::unauthenticated()?;
                let data = client.get_shared_request(&share_token).await?;
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
            ShareAction::Revoke { share_token, org } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                if !confirm_destructive_action(
                    "REVOKE SHARED LINK?",
                    &format!("share {share_token}"),
                    &organization_id,
                    yes,
                    json,
                )? {
                    return Ok(());
                }
                let access_token = ensure_valid_token(&mut config).await?;
                let client =
                    ApiClient::with_organization(access_token, Some(organization_id.clone()))?;
                client.revoke_shared_request(&share_token).await?;
                if json {
                    print_json(&serde_json::json!({
                        "status": "revoked",
                        "organization_id": organization_id,
                        "share_token": share_token
                    }))?;
                } else {
                    print_status_block(
                        OutputStatus::Ok,
                        "SHARE REVOKED",
                        &[
                            output_field("SHARE TOKEN", share_token.bold()),
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
                no_email,
                email,
                org,
            } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;

                let email_enabled = monitor_email_enabled(email, no_email);
                let mut params = serde_json::json!({
                    "name": name,
                    "url": url,
                    "method": method.as_lowercase(),
                    "expected_status_code": expected_status,
                    "check_interval": interval.minutes(),
                    "failure_threshold": failure_threshold,
                    "email_enabled": email_enabled,
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
            MonitorAction::Show { monitor_id, org } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let monitor = client.get_uptime_monitor(&monitor_id).await?;
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
                monitor_id,
                name,
                url,
                method,
                expected_status,
                interval,
                enable,
                disable,
                enabled,
                failure_threshold,
                org,
            } => {
                let enabled = monitor_enabled_update(enable, disable, enabled);
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
                    params.insert(
                        "method".into(),
                        serde_json::Value::String(v.as_lowercase().to_string()),
                    );
                }
                if let Some(v) = expected_status {
                    params.insert("expected_status_code".into(), v.into());
                }
                if let Some(v) = interval {
                    params.insert("check_interval".into(), v.minutes().into());
                }
                if let Some(v) = enabled {
                    params.insert("enabled".into(), serde_json::Value::Bool(v));
                }
                if let Some(v) = failure_threshold {
                    params.insert("failure_threshold".into(), v.into());
                }

                if params.is_empty() {
                    return Err(anyhow!(
                        "No fields to update. Use --name, --url, --method, --expected-status, --interval, --enable, --disable, or --failure-threshold."
                    ));
                }

                let monitor = client
                    .update_uptime_monitor(&monitor_id, &serde_json::Value::Object(params))
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
            MonitorAction::Delete { monitor_id, org } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                if !confirm_destructive_action(
                    "DELETE MONITOR?",
                    &format!("monitor {monitor_id}"),
                    &organization_id,
                    yes,
                    json,
                )? {
                    return Ok(());
                }
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                client.delete_uptime_monitor(&monitor_id).await?;
                if json {
                    print_json(&serde_json::json!({
                        "status": "deleted",
                        "organization_id": organization_id,
                        "monitor_id": monitor_id
                    }))?;
                } else {
                    print_status_block(
                        OutputStatus::Ok,
                        "MONITOR DELETED",
                        &[
                            output_field("MONITOR", monitor_id.bold()),
                            output_field("ORGANIZATION", organization_id.dim()),
                        ],
                    );
                }
            }
            MonitorAction::Checks {
                monitor_id,
                page,
                page_size,
                org,
            } => {
                let mut config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = ensure_valid_token(&mut config).await?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let response = client
                    .list_uptime_checks(&monitor_id, page, page_size)
                    .await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "monitor_id": monitor_id,
                        "checks": response
                    }))?;
                } else {
                    print_context("Organization:", &organization_id);
                    print_context("Monitor:", &monitor_id);
                    print_uptime_checks(&response);
                }
            }
        },
        Commands::Completions { shell } => {
            let mut stdout = io::stdout().lock();
            print_completions(shell, &mut stdout)?;
        }
        Commands::Update => {
            updater::run_self_update(json).await?;
        }
        Commands::Tunnel { action, target } => {
            // Initialize logging for tunnel
            let log_config = LogConfig {
                level: log_level.as_str().to_string(),
                output_to_stdout: false, // Disable stdout logging for TUI
                directory: log_dir
                    .clone()
                    .unwrap_or_else(|| LogConfig::default().directory),
                ..Default::default()
            };
            let _logger = Logger::new(log_config)?;
            run_tunnel_lifecycle_command(action, target, json, &mut update_handle).await?;
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

const SUPPORTED_TUNNEL_SCHEMA_MAJOR: u64 = 1;
const TUNNEL_RECEIPT_SCHEMA: &str = "hooklistener.tunnel.receipt/1";
const TUNNEL_EVENT_SCHEMA: &str = "hooklistener.tunnel.event/1";
const LISTEN_EVENT_SCHEMA: &str = "hooklistener.listen.event/1";
const COMMAND_ERROR_SCHEMA: &str = "hooklistener.cli.error/1";

struct TunnelLifecycleContext {
    config: config::Config,
    access_token: String,
    organization_id: String,
    client: ApiClient,
    contract: api::TunnelLifecycleContract,
}

struct TunnelEventOptions {
    cursor: Option<String>,
    limit: u16,
    capture_id: Option<String>,
    attempt_id: Option<String>,
    follow: bool,
    interval_ms: u64,
}

async fn tunnel_lifecycle_context(org: Option<String>) -> Result<TunnelLifecycleContext> {
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

fn validate_tunnel_schema(contract: &api::TunnelLifecycleContract) -> Result<()> {
    if contract.schema.major == SUPPORTED_TUNNEL_SCHEMA_MAJOR {
        return Ok(());
    }

    Err(errors::TunnelLifecycleError::IncompatibleSchema {
        supported: SUPPORTED_TUNNEL_SCHEMA_MAJOR,
        actual: contract.schema.major,
    }
    .into())
}

fn tunnel_lifecycle_receipt(
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

fn tunnel_lifecycle_event_envelope(event: &api::TunnelLifecycleEvent) -> serde_json::Value {
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

fn safe_tunnel_event_metadata(metadata: &serde_json::Value) -> serde_json::Value {
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

async fn run_tunnel_lifecycle_command(
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

async fn run_tunnel_activation(
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

async fn run_anonymous_tunnel_activation(
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

fn validate_tunnel_target(target: &TunnelTarget) -> Result<()> {
    if target.port == 0 {
        return Err(anyhow!("Tunnel target port must be between 1 and 65535"));
    }
    if target.host.trim().is_empty() {
        return Err(anyhow!("Tunnel target host cannot be empty"));
    }
    Ok(())
}

fn print_tunnel_lifecycle_resource<T: serde::Serialize>(
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

fn print_tunnel_sessions(sessions: &[api::TunnelSessionResource], organization_id: &str) {
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

fn format_tunnel_lifecycle_event(event: &api::TunnelLifecycleEvent) -> String {
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

async fn run_tunnel_lifecycle_events(
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

    let mut device_flow = auth::DeviceCodeFlow::new(api::default_base_url()?)?;

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

    loop {
        // Poll the API
        match device_flow.poll_for_authorization().await {
            Ok(Some(token_response)) => {
                execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
                let access_expires_at = match token_response.expires_in {
                    Some(secs) => token_expiry_from_now(secs)?,
                    None => Utc::now() + ChronoDuration::days(SESSION_TOKEN_VALIDITY_DAYS),
                };
                let refresh_expires_at = token_response
                    .refresh_expires_in
                    .map(token_expiry_from_now)
                    .transpose()?;
                save_login_tokens(
                    &config,
                    token_response.access_token,
                    access_expires_at,
                    token_response.refresh_token,
                    refresh_expires_at,
                    None,
                )?;
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

        let poll_delay = device_flow.time_until_next_poll();
        if poll_delay.is_zero() {
            continue;
        }
        let next_poll = tokio::time::sleep(poll_delay);
        tokio::pin!(next_poll);

        // Animate spinner until the server-authorized next poll.
        loop {
            if device_flow
                .time_remaining()
                .is_some_and(|remaining| remaining == ChronoDuration::zero())
            {
                execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
                return Err(anyhow!(
                    "Device code expired before authorization completed. Please run `hooklistener login` again."
                ));
            }

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
                _ = &mut next_poll => break,
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

async fn refresh_access_token_loop(config: config::Config, token_tx: watch::Sender<String>) {
    let base_url = match api::default_base_url() {
        Ok(base_url) => base_url,
        Err(error) => {
            error!(%error, "Access-token refresh disabled by invalid API URL");
            return;
        }
    };
    refresh_access_token_loop_with(config, token_tx, &base_url).await;
}

async fn refresh_access_token_loop_with(
    mut config: config::Config,
    token_tx: watch::Sender<String>,
    base_url: &str,
) {
    loop {
        if token_tx.is_closed()
            || config.refresh_token.is_none()
            || !config.is_refresh_token_valid()
        {
            return;
        }

        if sleep_or_token_receiver_closed(&token_tx, access_token_refresh_delay(&config)).await {
            return;
        }

        let refresh_result = tokio::select! {
            _ = token_tx.closed() => return,
            result = refresh_access_token_from_config_with(&mut config, base_url, None) => result,
        };
        match refresh_result {
            Ok(access_token) => {
                if token_tx.send(access_token).is_err() {
                    return;
                }
            }
            Err(err) => {
                error!(error = %err, "Failed to refresh CLI access token");
                if sleep_or_token_receiver_closed(
                    &token_tx,
                    Duration::from_secs(ACCESS_TOKEN_REFRESH_RETRY_SECONDS),
                )
                .await
                {
                    return;
                }
            }
        }
    }
}

async fn sleep_or_token_receiver_closed(
    token_tx: &watch::Sender<String>,
    duration: Duration,
) -> bool {
    tokio::select! {
        _ = token_tx.closed() => true,
        _ = sleep(duration) => false,
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
    let base_url = api::default_base_url()?;
    refresh_access_token_from_config_with(config, &base_url, None).await
}

async fn refresh_access_token_from_config_with(
    config: &mut config::Config,
    base_url: &str,
    save_path: Option<&std::path::Path>,
) -> Result<String> {
    let refresh_token = config
        .refresh_token
        .clone()
        .ok_or_else(|| anyhow!("No refresh token found"))?;

    if !config.is_refresh_token_valid() {
        return Err(anyhow!("Refresh token expired"));
    }

    let response = api::refresh_access_token(&refresh_token, base_url).await?;
    let expires_at = token_expiry_from_now(response.expires_in)?;

    // Other processes (`org use`, `config set`, `login --force`, `logout`) may
    // have saved the config since this copy was loaded. Persist only the fields
    // this refresh owns on top of what is currently on disk, so their changes
    // (including a rotated refresh token) survive.
    let mut updated = match load_saved_config(save_path) {
        Ok(Some(on_disk)) => on_disk,
        Ok(None) => in_memory_config_copy(config),
        Err(err) => {
            warn!(
                error = %err,
                "Failed to reload config before saving refreshed token; saving in-memory copy"
            );
            in_memory_config_copy(config)
        }
    };

    if updated.refresh_token.is_none() {
        // The user logged out in another process. Do not resurrect the session
        // on disk: the fresh access token is handed back for this process to
        // finish its current work, and `config` mirrors the logged-out state so
        // the background refresh loop stops on its next iteration.
        *config = updated;
        return Ok(response.access_token);
    }

    updated.access_token = Some(response.access_token.clone());
    updated.token_expires_at = Some(expires_at);
    if let Some(path) = save_path {
        updated.save_to(path)?;
    } else {
        updated.save()?;
    }
    *config = updated;

    Ok(response.access_token)
}

/// Persists freshly issued login tokens on top of the config currently on disk.
///
/// The device-authorization wait can last minutes, so `loaded` (the copy taken
/// when `login` started) may be stale: `org use`, `config set`, or an update
/// check in another process may have saved since. Only the token fields belong
/// to the login, so everything else is taken from disk and `loaded` is used
/// solely as a fallback when no config file exists any more.
fn save_login_tokens(
    loaded: &config::Config,
    access_token: String,
    access_expires_at: chrono::DateTime<Utc>,
    refresh_token: Option<String>,
    refresh_expires_at: Option<chrono::DateTime<Utc>>,
    save_path: Option<&std::path::Path>,
) -> Result<()> {
    let mut updated = match load_saved_config(save_path) {
        Ok(Some(on_disk)) => on_disk,
        Ok(None) => in_memory_config_copy(loaded),
        Err(err) => {
            warn!(
                error = %err,
                "Failed to reload config before saving login tokens; saving in-memory copy"
            );
            in_memory_config_copy(loaded)
        }
    };
    updated.set_tokens(
        access_token,
        access_expires_at,
        refresh_token,
        refresh_expires_at,
    );
    match save_path {
        Some(path) => updated.save_to(path),
        None => updated.save(),
    }
}

/// Loads the config currently on disk, or `None` when no config file exists.
fn load_saved_config(path: Option<&std::path::Path>) -> Result<Option<config::Config>> {
    let path = match path {
        Some(path) => path.to_path_buf(),
        None => config::Config::config_path()?,
    };
    match std::fs::symlink_metadata(&path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
        Ok(_) => config::Config::load_from(&path).map(Some),
    }
}

fn in_memory_config_copy(config: &config::Config) -> config::Config {
    config::Config {
        access_token: config.access_token.clone(),
        token_expires_at: config.token_expires_at,
        refresh_token: config.refresh_token.clone(),
        refresh_token_expires_at: config.refresh_token_expires_at,
        selected_organization_id: config.selected_organization_id.clone(),
        last_update_check: config.last_update_check,
        latest_known_version: config.latest_known_version.clone(),
    }
}

/// Converts a server-supplied token lifetime into an absolute expiry, failing
/// cleanly instead of panicking on absurd values.
fn token_expiry_from_now(seconds: u64) -> Result<chrono::DateTime<Utc>> {
    auth::expiry_from_now(seconds)
        .map_err(|err| anyhow!("Authorization server returned an invalid token lifetime ({err})"))
}

fn require_organization(cli_org: Option<String>, config: &config::Config) -> Result<String> {
    resolve_tunnel_org(cli_org, config).ok_or_else(|| {
        anyhow!(
            "No organization selected. Use `hooklistener org use <organization-id>` or pass --org."
        )
    })
}

fn confirmation_is_yes(input: &str) -> bool {
    input.trim().eq_ignore_ascii_case("yes")
}

fn confirm_destructive_action(
    title: &str,
    resource: &str,
    organization_id: &str,
    confirmed: bool,
    json: bool,
) -> Result<bool> {
    if confirmed {
        return Ok(true);
    }

    if json || !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Err(anyhow!(
            "Confirmation required for {resource}. Re-run with --yes after verifying the resource and organization."
        ));
    }

    eprint_status(OutputStatus::Warn, title);
    eprintln!();
    eprint_field("RESOURCE", resource);
    eprint_field("ORGANIZATION", organization_id);
    eprint_field("CONSEQUENCE", "This action cannot be undone.");
    eprint!("TYPE YES      Type `yes` to continue: ");
    io::stderr().flush()?;

    let mut response = String::new();
    io::stdin().read_line(&mut response)?;
    if confirmation_is_yes(&response) {
        return Ok(true);
    }

    eprintln!();
    eprint_status(OutputStatus::Info, "COMMAND CANCELED");
    eprint_field("ACTION", "No changes were made.");
    Ok(false)
}

const FIELD_LABEL_WIDTH: usize = 14;

#[derive(Clone, Copy)]
pub(crate) enum OutputStatus {
    Ok,
    Err,
    Warn,
    Info,
}

impl OutputStatus {
    fn token(self) -> &'static str {
        match self {
            Self::Ok => "[OK]",
            Self::Err => "[ERR]",
            Self::Warn => "[WARN]",
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

fn wrap_plain_text(value: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();

    for paragraph in value.lines() {
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            if current.is_empty() {
                current.push_str(word);
            } else if current.chars().count() + 1 + word.chars().count() <= width {
                current.push(' ');
                current.push_str(word);
            } else {
                lines.push(current);
                current = word.to_string();
            }
        }
        lines.push(current);
    }

    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

fn format_wrapped_field_lines(label: &str, value: &str, width: u16) -> String {
    let label = output_label(label);
    let prefix_width = label.chars().count() + 1;
    let value_width = usize::from(width).saturating_sub(prefix_width).max(1);
    let continuation = " ".repeat(prefix_width);

    wrap_plain_text(value, value_width)
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                format!("{label} {line}")
            } else {
                format!("{continuation}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn styled_status_line(status: OutputStatus, title: &str) -> String {
    let line = format_status_line(status, title);
    match status {
        OutputStatus::Ok => line.green().bold().to_string(),
        OutputStatus::Err => line.red().bold().to_string(),
        OutputStatus::Warn => line.yellow().bold().to_string(),
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

fn print_empty_state(title: &str, action: &str) {
    print_status(OutputStatus::Info, title);
    println!();
    println!(
        "{}",
        format_wrapped_field_lines("ACTION", action, output::terminal_width())
    );
}

fn eprint_field(label: &str, value: impl std::fmt::Display) {
    eprintln!("{} {}", output_label(label).bold(), value);
}

fn eprint_status(status: OutputStatus, title: &str) {
    eprintln!("{}", styled_status_line(status, title));
}

fn print_field(label: &str, value: impl std::fmt::Display) {
    println!("{} {}", output_label(label).bold(), value);
}

fn print_section(label: &str) {
    println!("{}", output_title(label).bold());
}

/// Print a dim context line like "ORGANIZATION abc123".
fn print_context(label: &str, value: &str) {
    println!(
        "{} {}",
        output_label(label).dim(),
        sanitize_terminal(value, TerminalTextLayout::Inline).dim()
    );
}

/// Print a pagination footer.
fn print_pagination(p: &api::Pagination) {
    println!("{}", format_pagination_line(p).dim());
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TerminalTextLayout {
    Inline,
    Block,
}

/// Escape terminal control characters while retaining readable, inert text.
///
/// Inline values escape every control character so untrusted input cannot move
/// the cursor or forge additional output. Block values additionally retain LF
/// and tab for payload readability; all other C0, DEL, and C1 controls remain
/// escaped, including ESC, BEL, CR, and the 8-bit OSC/CSI introducers.
fn sanitize_terminal(value: &str, layout: TerminalTextLayout) -> std::borrow::Cow<'_, str> {
    let should_escape = |character: char| {
        character.is_control()
            && !(layout == TerminalTextLayout::Block && matches!(character, '\n' | '\t'))
    };

    if !value.chars().any(should_escape) {
        return std::borrow::Cow::Borrowed(value);
    }

    let mut sanitized = String::with_capacity(value.len());
    for character in value.chars() {
        if should_escape(character) {
            sanitized.extend(character.escape_default());
        } else {
            sanitized.push(character);
        }
    }
    std::borrow::Cow::Owned(sanitized)
}

fn sanitize_terminal_display(value: impl std::fmt::Display) -> String {
    let value = value.to_string();
    sanitize_terminal(&value, TerminalTextLayout::Inline).into_owned()
}

fn truncate_terminal_inline(value: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    let sanitized = sanitize_terminal(value, TerminalTextLayout::Inline);
    if sanitized.chars().count() <= max_chars {
        return sanitized.into_owned();
    }

    let prefix = sanitized
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    format!("{prefix}…")
}

fn format_terminal_body(body: &str) -> String {
    sanitize_terminal(body, TerminalTextLayout::Block)
        .split('\n')
        .map(|line| format!("│ {line}"))
        .collect::<Vec<_>>()
        .join("\n")
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
            let value = value.to_string();
            println!(
                "  {}{}{}",
                sanitize_terminal(key, TerminalTextLayout::Inline).dim(),
                separator,
                sanitize_terminal(&value, TerminalTextLayout::Inline)
            );
        }
    }
}

/// Print a body section, showing "(empty)" when the body is absent or blank.
fn print_body_section(label: &str, body: Option<&str>) {
    match body {
        Some(body) if !body.is_empty() => {
            print_section(label);
            println!("{}", format_terminal_body(body));
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
        .set_width(output::terminal_width())
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(headers);
    table
}

fn print_organizations(organizations: &[api::Organization], selected_org: Option<&str>) {
    if organizations.is_empty() {
        print_empty_state(
            "NO ORGANIZATIONS FOUND",
            "Run `hooklistener login --force` to refresh account access.",
        );
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
        print_empty_state(
            "NO DEBUG ENDPOINTS FOUND",
            "Run `hooklistener endpoint create <name>` to create one.",
        );
        return;
    }

    let mut table = new_table(&["ID", "Slug", "Status", "Webhook URL", "Name"]);
    for endpoint in endpoints {
        table.add_row(vec![
            sanitize_terminal(&endpoint.id, TerminalTextLayout::Inline).into_owned(),
            sanitize_terminal(&endpoint.slug, TerminalTextLayout::Inline).into_owned(),
            sanitize_terminal(&endpoint.status, TerminalTextLayout::Inline).into_owned(),
            sanitize_terminal(&endpoint.webhook_url, TerminalTextLayout::Inline).into_owned(),
            sanitize_terminal(&endpoint.name, TerminalTextLayout::Inline).into_owned(),
        ]);
    }
    println!("{table}");
}

fn print_endpoint_detail(endpoint: &api::DebugEndpointSummary) {
    print_field(
        "ID",
        sanitize_terminal(&endpoint.id, TerminalTextLayout::Inline),
    );
    print_field(
        "SLUG",
        sanitize_terminal(&endpoint.slug, TerminalTextLayout::Inline),
    );
    print_field(
        "STATUS",
        sanitize_terminal(&endpoint.status, TerminalTextLayout::Inline),
    );
    print_field(
        "WEBHOOK URL",
        sanitize_terminal(&endpoint.webhook_url, TerminalTextLayout::Inline).underlined(),
    );
    print_field(
        "NAME",
        sanitize_terminal(&endpoint.name, TerminalTextLayout::Inline),
    );
    if let Some(created_at) = endpoint.created_at.as_deref() {
        print_field(
            "CREATED AT",
            sanitize_terminal(created_at, TerminalTextLayout::Inline).dim(),
        );
    }
}

fn print_endpoint_requests(response: &api::EndpointRequestsResponse) {
    if response.data.is_empty() {
        print_empty_state(
            "NO REQUESTS FOUND",
            "Send a webhook, then run `hooklistener endpoint list-requests <endpoint-id>` again.",
        );
        return;
    }

    let mut table = new_table(&["ID", "Method", "URL", "Remote"]);
    for request in &response.data {
        table.add_row(vec![
            sanitize_terminal(&request.id, TerminalTextLayout::Inline).into_owned(),
            sanitize_terminal(&request.method, TerminalTextLayout::Inline).into_owned(),
            sanitize_terminal(&request.url, TerminalTextLayout::Inline).into_owned(),
            sanitize_terminal(&request.remote_addr, TerminalTextLayout::Inline).into_owned(),
        ]);
    }
    println!("{table}");
    print_pagination(&response.pagination);
}

fn print_endpoint_request_detail(request: &api::DebugRequestDetail) {
    print_field(
        "REQUEST ID",
        sanitize_terminal(&request.id, TerminalTextLayout::Inline),
    );
    print_field(
        "METHOD",
        sanitize_terminal(&request.method, TerminalTextLayout::Inline).bold(),
    );
    if let Some(path) = request.path.as_deref() {
        print_field("PATH", sanitize_terminal(path, TerminalTextLayout::Inline));
    }
    print_field(
        "URL",
        sanitize_terminal(&request.url, TerminalTextLayout::Inline),
    );

    if let Some(status_remote) = request.remote_addr.as_deref() {
        print_field(
            "REMOTE",
            sanitize_terminal(status_remote, TerminalTextLayout::Inline),
        );
    }
    if let Some(content_length) = request.content_length {
        print_field("CONTENT LEN", content_length);
    }
    if let Some(created_at) = request.created_at.as_deref() {
        print_field(
            "CREATED AT",
            sanitize_terminal(created_at, TerminalTextLayout::Inline).dim(),
        );
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
        print_empty_state(
            "NO FORWARDS FOUND",
            "Run `hooklistener endpoint forward-request <endpoint-id> <request-id> <target-url>`.",
        );
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
            Some(err) => format!(
                "{}\n  [ERR] {}",
                sanitize_terminal(&forward.target_url, TerminalTextLayout::Inline),
                sanitize_terminal(err, TerminalTextLayout::Inline)
            ),
            None => sanitize_terminal(&forward.target_url, TerminalTextLayout::Inline).into_owned(),
        };
        table.add_row(vec![
            sanitize_terminal(&forward.id, TerminalTextLayout::Inline).into_owned(),
            sanitize_terminal(&forward.method, TerminalTextLayout::Inline).into_owned(),
            status,
            duration,
            target,
        ]);
    }
    println!("{table}");
    print_pagination(&response.pagination);
}

fn print_forward_detail(forward: &api::DebugRequestForwardDetail) {
    print_field(
        "FORWARD ID",
        sanitize_terminal(&forward.id, TerminalTextLayout::Inline),
    );
    print_field(
        "REQUEST ID",
        sanitize_terminal(&forward.debug_request_id, TerminalTextLayout::Inline),
    );
    print_field(
        "TARGET URL",
        sanitize_terminal(&forward.target_url, TerminalTextLayout::Inline),
    );
    print_field(
        "METHOD",
        sanitize_terminal(&forward.method, TerminalTextLayout::Inline).bold(),
    );
    if let Some(status_code) = forward.status_code {
        print_field("STATUS", style_status_code(status_code));
    } else {
        print_field("STATUS", "(pending)".yellow());
    }
    if let Some(duration_ms) = forward.duration_ms {
        print_field("DURATION", format!("{duration_ms}ms"));
    }
    if let Some(attempted_at) = forward.attempted_at.as_deref() {
        print_field(
            "ATTEMPTED AT",
            sanitize_terminal(attempted_at, TerminalTextLayout::Inline).dim(),
        );
    }
    if let Some(error_message) = forward.error_message.as_deref() {
        print_field(
            "ERROR",
            sanitize_terminal(error_message, TerminalTextLayout::Inline),
        );
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
        print_field(
            "RUN ID",
            sanitize_terminal(run_id, TerminalTextLayout::Inline),
        );
    }
    if let Some(report_url) = result.case_suite_run_url.as_deref() {
        print_field(
            "REPORT",
            sanitize_terminal(report_url, TerminalTextLayout::Inline),
        );
    }
    print_field(
        "RESULT",
        sanitize_terminal(&result.result_status, TerminalTextLayout::Inline).bold(),
    );
    print_field(
        "STATUS",
        sanitize_terminal(&result.status, TerminalTextLayout::Inline),
    );
    print_field(
        "ENDPOINT",
        sanitize_terminal(&result.endpoint_id, TerminalTextLayout::Inline),
    );
    print_field(
        "TARGET",
        sanitize_terminal(
            &case_run_target_label(&result.target),
            TerminalTextLayout::Inline,
        ),
    );
    if let Some(source) = result.source.as_deref() {
        print_field(
            "SOURCE",
            sanitize_terminal(&source.to_uppercase(), TerminalTextLayout::Inline),
        );
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
                sanitize_terminal_display(&forward.id),
                sanitize_terminal_display(&forward.debug_request_id),
                sanitize_terminal_display(value_or_dash(forward.debug_request_case_id.as_deref())),
                sanitize_terminal_display(case_run_forward_target(forward)),
                status,
                sanitize_terminal_display(assertion),
                sanitize_terminal_display(error),
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
                sanitize_terminal_display(&failure.case_id),
                sanitize_terminal_display(case_run_failure_reason(failure)),
            ]);
        }
        println!("{table}");
    }
}

fn print_static_tunnels(response: &api::StaticTunnelsResponse) {
    if response.static_tunnels.is_empty() {
        print_empty_state(
            "NO STATIC TUNNELS FOUND",
            "Run `hooklistener static-tunnel create <slug>` to reserve one.",
        );
    } else {
        let mut table = new_table(&["ID", "Slug", "Name"]);
        for tunnel in &response.static_tunnels {
            let name = tunnel.name.as_deref().unwrap_or("");
            table.add_row(vec![
                sanitize_terminal_display(&tunnel.id),
                sanitize_terminal_display(&tunnel.slug),
                sanitize_terminal_display(name),
            ]);
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
        print_empty_state(
            "NO EVENTS CAPTURED",
            "Send a webhook, then run `hooklistener anon list-events <endpoint-id> --token <token>`.",
        );
    } else {
        let mut table = new_table(&["ID", "Method", "Received At"]);
        for event in &response.data {
            table.add_row(vec![
                sanitize_terminal(&event.id, TerminalTextLayout::Inline).into_owned(),
                sanitize_terminal(&event.method, TerminalTextLayout::Inline).into_owned(),
                sanitize_terminal(
                    value_or_dash(event.inserted_at.as_deref()),
                    TerminalTextLayout::Inline,
                )
                .into_owned(),
            ]);
        }
        println!("{table}");
    }
    print_pagination(&response.pagination);
}

fn print_anon_event_detail(event: &api::AnonEvent) {
    print_field(
        "EVENT ID",
        sanitize_terminal(&event.id, TerminalTextLayout::Inline),
    );
    print_field(
        "ENDPOINT ID",
        sanitize_terminal(&event.endpoint_id, TerminalTextLayout::Inline),
    );
    print_field(
        "METHOD",
        sanitize_terminal(&event.method, TerminalTextLayout::Inline).bold(),
    );
    if let Some(status) = event.status.as_deref() {
        print_field(
            "STATUS",
            sanitize_terminal(status, TerminalTextLayout::Inline),
        );
    }
    if let Some(inserted_at) = event.inserted_at.as_deref() {
        print_field(
            "RECEIVED AT",
            sanitize_terminal(inserted_at, TerminalTextLayout::Inline).dim(),
        );
    }

    println!();
    print_key_value_map("Headers:", &event.headers, ": ");

    println!();
    print_body_section("Body:", event.body.as_deref());
}

fn print_shared_requests(shares: &[api::SharedRequestSummary]) {
    if shares.is_empty() {
        print_empty_state(
            "NO SHARES FOUND",
            "Run `hooklistener share create <request-id>` to create one.",
        );
        return;
    }

    let mut table = new_table(&["ID", "Token", "Fwds", "Views", "Protected", "Expires At"]);
    for share in shares {
        table.add_row(vec![
            sanitize_terminal(&share.id, TerminalTextLayout::Inline).into_owned(),
            sanitize_terminal(&share.share_token, TerminalTextLayout::Inline).into_owned(),
            yes_no(share.include_forwards).to_string(),
            share.view_count.to_string(),
            yes_no(share.password_protected).to_string(),
            sanitize_terminal(
                value_or_dash(share.expires_at.as_deref()),
                TerminalTextLayout::Inline,
            )
            .into_owned(),
        ]);
    }
    println!("{table}");
}

fn print_shared_request_full(data: &serde_json::Value) {
    if let Some(token) = data.get("share_token").and_then(|v| v.as_str()) {
        print_field(
            "SHARE TOKEN",
            sanitize_terminal(token, TerminalTextLayout::Inline),
        );
    }
    if let Some(expires) = data.get("expires_at").and_then(|v| v.as_str()) {
        print_field(
            "EXPIRES AT",
            sanitize_terminal(expires, TerminalTextLayout::Inline).dim(),
        );
    }
    if let Some(views) = data.get("view_count").and_then(|v| v.as_u64()) {
        print_field("VIEWS", views);
    }

    if let Some(request) = data.get("debug_request") {
        println!();
        print_section("DEBUG REQUEST");
        if let Some(id) = request.get("id").and_then(|v| v.as_str()) {
            print_field("ID", sanitize_terminal(id, TerminalTextLayout::Inline));
        }
        if let Some(method) = request.get("method").and_then(|v| v.as_str()) {
            print_field(
                "METHOD",
                sanitize_terminal(method, TerminalTextLayout::Inline).bold(),
            );
        }
        if let Some(url) = request.get("url").and_then(|v| v.as_str()) {
            print_field("URL", sanitize_terminal(url, TerminalTextLayout::Inline));
        }
        if let Some(remote) = request.get("remote_addr").and_then(|v| v.as_str()) {
            print_field(
                "REMOTE",
                sanitize_terminal(remote, TerminalTextLayout::Inline).dim(),
            );
        }
        if let Some(created) = request.get("created_at").and_then(|v| v.as_str()) {
            print_field(
                "CREATED AT",
                sanitize_terminal(created, TerminalTextLayout::Inline).dim(),
            );
        }

        if let Some(headers) = request.get("headers").and_then(|v| v.as_object())
            && !headers.is_empty()
        {
            println!();
            print_section("HEADERS");
            for (key, value) in headers {
                let value = value.to_string();
                println!(
                    "  {}: {}",
                    sanitize_terminal(key, TerminalTextLayout::Inline).dim(),
                    sanitize_terminal(&value, TerminalTextLayout::Inline)
                );
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
            print_body_section("BODY", Some(body));
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
                .and_then(|c| u16::try_from(c).ok())
                .map(style_status_code)
                .unwrap_or_else(|| "-".to_string());
            let duration = fwd
                .get("duration_ms")
                .and_then(|v| v.as_u64())
                .map(|ms| format!("{}ms", ms))
                .unwrap_or_else(|| "-".to_string());
            println!(
                "  {} {} → {} ({})",
                sanitize_terminal(method, TerminalTextLayout::Inline).bold(),
                status,
                sanitize_terminal(target, TerminalTextLayout::Inline),
                duration
            );
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
        status => sanitize_terminal(status, TerminalTextLayout::Inline)
            .yellow()
            .to_string(),
    }
}

fn print_monitors(monitors: &[api::UptimeMonitor]) {
    if monitors.is_empty() {
        print_empty_state(
            "NO UPTIME MONITORS FOUND",
            "Run `hooklistener monitor create <name> <url>` to create one.",
        );
        return;
    }

    let mut table = new_table(&["ID", "Status", "Method", "Int", "URL", "Name"]);
    for m in monitors {
        let status = monitor_status_label(m.current_status.as_deref());
        let interval = m
            .check_interval
            .map(|i| format!("{}m", i))
            .unwrap_or_default();
        let url_display = truncate_terminal_inline(&m.url, 24);
        let sanitized_name = sanitize_terminal(&m.name, TerminalTextLayout::Inline);
        let name = if m.enabled {
            sanitized_name.into_owned()
        } else {
            format!("{sanitized_name} (disabled)")
        };
        table.add_row(vec![
            sanitize_terminal_display(&m.id),
            sanitize_terminal_display(status),
            sanitize_terminal_display(m.method.to_uppercase()),
            interval,
            url_display,
            name,
        ]);
    }
    println!("{table}");
}

fn print_monitor_detail(m: &api::UptimeMonitor) {
    print_field("ID", sanitize_terminal(&m.id, TerminalTextLayout::Inline));
    print_field(
        "NAME",
        sanitize_terminal(&m.name, TerminalTextLayout::Inline),
    );
    print_field(
        "URL",
        sanitize_terminal(&m.url, TerminalTextLayout::Inline).underlined(),
    );
    print_field(
        "METHOD",
        sanitize_terminal(&m.method.to_uppercase(), TerminalTextLayout::Inline).bold(),
    );

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
        print_field(
            "BODY MATCH",
            sanitize_terminal(bc, TerminalTextLayout::Inline),
        );
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
        print_field(
            "LAST CHECK",
            sanitize_terminal(checked, TerminalTextLayout::Inline).dim(),
        );
    }
    if let Some(ref changed) = m.last_status_change_at {
        print_field(
            "STATUS CHANGE",
            sanitize_terminal(changed, TerminalTextLayout::Inline).dim(),
        );
    }
    if let Some(ref created) = m.created_at {
        print_field(
            "CREATED AT",
            sanitize_terminal(created, TerminalTextLayout::Inline).dim(),
        );
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
        print_empty_state(
            "NO CHECKS RECORDED",
            "Wait for the first interval, then run `hooklistener monitor checks <monitor-id>`.",
        );
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
                sanitize_terminal_display(&check.id),
                sanitize_terminal_display(&check.status),
                code,
                rt,
                sanitize_terminal_display(checked),
                sanitize_terminal_display(error),
            ]);
        }
        println!("{table}");
    }

    print_pagination(&response.pagination);
}

#[allow(clippy::too_many_arguments)]
fn spawn_tunnel_forwarder_manager(
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
async fn run_tunnel_forwarder_connection(
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
    if let Some(e) = err.downcast_ref::<errors::TunnelLifecycleError>() {
        return e.hint();
    }
    if let Some(e) = err.downcast_ref::<errors::UpdateError>() {
        return e.hint();
    }

    let message = err.to_string();
    if message.contains("Session expired") || message.contains("No access token") {
        return Some("Run `hooklistener login` to re-authenticate.");
    }
    if message.contains("No organization selected") {
        return Some("Run `hooklistener org use <organization-id>` or pass --org to the command.");
    }
    if message.contains("Confirmation required") {
        return Some("Verify the resource and organization, then re-run with --yes.");
    }
    None
}

fn error_code(err: &anyhow::Error) -> String {
    if let Some(error) = err.downcast_ref::<errors::TunnelLifecycleError>() {
        error.code().to_string()
    } else if err.downcast_ref::<errors::UpdateError>().is_some() {
        "update_error".to_string()
    } else {
        let message = err.to_string();
        if message.contains("Confirmation required") {
            "confirmation_required".to_string()
        } else if message.contains("does not support --json") {
            "unsupported_output_mode".to_string()
        } else if message.contains("Session expired") || message.contains("No access token") {
            "authentication_required".to_string()
        } else if message.contains("No organization selected") {
            "organization_required".to_string()
        } else {
            "command_failed".to_string()
        }
    }
}

fn command_exit_code(err: &anyhow::Error) -> i32 {
    match err.downcast_ref::<errors::TunnelLifecycleError>() {
        Some(errors::TunnelLifecycleError::IncompatibleSchema { .. }) => 3,
        Some(errors::TunnelLifecycleError::CursorExpired { .. }) => 4,
        _ => 1,
    }
}

fn json_error_receipt(err: &anyhow::Error) -> serde_json::Value {
    let causes = err
        .chain()
        .skip(1)
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    let details = match err.downcast_ref::<errors::TunnelLifecycleError>() {
        Some(errors::TunnelLifecycleError::CursorExpired {
            earliest_cursor,
            resync,
        }) => serde_json::json!({
            "earliest_cursor": earliest_cursor,
            "resync": resync,
        }),
        Some(errors::TunnelLifecycleError::IncompatibleSchema { supported, actual }) => {
            serde_json::json!({"supported_major": supported, "actual_major": actual})
        }
        _ => serde_json::Value::Null,
    };

    serde_json::json!({
        "$schema": COMMAND_ERROR_SCHEMA,
        "schema_version": 1,
        "type": "error",
        "ok": false,
        "error": {
            "code": error_code(err),
            "message": err.to_string(),
            "hint": error_hint(err),
            "causes": causes,
            "details": details,
        }
    })
}

fn display_error(err: &anyhow::Error, json: bool) {
    if json {
        match serde_json::to_string(&json_error_receipt(err)) {
            Ok(receipt) => println!("{receipt}"),
            Err(_) => println!(
                r#"{{"$schema":"hooklistener.cli.error/1","schema_version":1,"type":"error","ok":false,"error":{{"code":"serialization_error","message":"Failed to serialize the command error."}}}}"#
            ),
        }
        return;
    }

    eprint_status(OutputStatus::Err, "COMMAND FAILED");
    eprintln!();
    eprint_field("MESSAGE", sanitize_terminal_display(err));
    if let Some(hint) = error_hint(err) {
        eprint_field("HINT", sanitize_terminal_display(hint));
    }
    for cause in err.chain().skip(1) {
        eprint_field("CAUSE", sanitize_terminal_display(cause));
    }
}

fn setup_terminal() -> Result<Terminal<CrosstermBackend<io::Stdout>>> {
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
struct TerminalInitCleanup {
    raw_mode: bool,
    alternate_screen: bool,
}
impl TerminalInitCleanup {
    fn raw_mode_enabled() -> Self {
        Self {
            raw_mode: true,
            alternate_screen: false,
        }
    }
    fn disarm(&mut self) {
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

    #[test]
    fn sanitize_terminal_escapes_ansi_csi_and_c1_csi_introducers() {
        let input = "before\u{1b}[31mred\u{1b}[0m\u{9b}2Jafter";

        assert_eq!(
            sanitize_terminal(input, TerminalTextLayout::Inline),
            r"before\u{1b}[31mred\u{1b}[0m\u{9b}2Jafter"
        );
    }

    #[test]
    fn sanitize_terminal_inline_neutralizes_every_c0_del_and_c1_control() {
        let input = (0..=0x9f).filter_map(char::from_u32).collect::<String>();
        let sanitized = sanitize_terminal(&input, TerminalTextLayout::Inline);

        assert!(!sanitized.chars().any(char::is_control));
    }

    #[test]
    fn sanitize_terminal_escapes_osc_52_with_bel_terminator() {
        let input = "\u{1b}]52;c;SGVsbG8=\u{7}";

        assert_eq!(
            sanitize_terminal(input, TerminalTextLayout::Inline),
            r"\u{1b}]52;c;SGVsbG8=\u{7}"
        );
    }

    #[test]
    fn sanitize_terminal_escapes_osc_8_with_st_terminator() {
        let input = "\u{1b}]8;;https://evil.example\u{1b}\\click\u{1b}]8;;\u{1b}\\";

        assert_eq!(
            sanitize_terminal(input, TerminalTextLayout::Inline),
            r"\u{1b}]8;;https://evil.example\u{1b}\click\u{1b}]8;;\u{1b}\"
        );
    }

    #[test]
    fn sanitize_terminal_escapes_carriage_return_backspace_and_inline_layout() {
        let input = "legitimate\r[OK] forged\u{8}!\n\tnext";

        assert_eq!(
            sanitize_terminal(input, TerminalTextLayout::Inline),
            r"legitimate\r[OK] forged\u{8}!\n\tnext"
        );
    }

    #[test]
    fn sanitize_terminal_block_layout_preserves_only_newline_and_tab_controls() {
        let input = "line one\n\tline two\rrewritten\u{7}";

        assert_eq!(
            sanitize_terminal(input, TerminalTextLayout::Block),
            "line one\n\tline two\\rrewritten\\u{7}"
        );
    }

    #[test]
    fn sanitize_terminal_borrows_benign_unicode_text_unchanged() {
        let input = "Café 東京 — webhook payload";
        let sanitized = sanitize_terminal(input, TerminalTextLayout::Inline);

        assert!(matches!(sanitized, std::borrow::Cow::Borrowed(value) if value == input));
    }

    #[test]
    fn format_terminal_body_gutters_forged_status_and_blank_trailing_lines() {
        let body = "legitimate\n\n[OK] forged\n";

        assert_eq!(
            format_terminal_body(body),
            "│ legitimate\n│ \n│ [OK] forged\n│ "
        );
    }

    #[test]
    fn sanitize_terminal_display_neutralizes_server_error_controls_and_newlines() {
        let error = anyhow!("upstream \u{1b}[31mfailed\u{1b}[0m\n[OK] forged\r");

        assert_eq!(
            sanitize_terminal_display(&error),
            r"upstream \u{1b}[31mfailed\u{1b}[0m\n[OK] forged\r"
        );
    }

    #[test]
    fn tunnel_lifecycle_event_output_neutralizes_every_remote_text_field() {
        let event = api::TunnelLifecycleEvent {
            id: "event-id".to_string(),
            position: 7,
            cursor: "cursor".to_string(),
            organization_id: "organization".to_string(),
            capture_id: "capture\rforged".to_string(),
            delivery_id: Some("delivery\u{9b}2J".to_string()),
            sequence: 1,
            fence: None,
            event_type: "opened\n[OK]\u{1b}]52;c;x\u{7}".to_string(),
            metadata: serde_json::json!({}),
            created_at: "2026-07-20T00:00:00Z".to_string(),
        };

        assert_eq!(
            format_tunnel_lifecycle_event(&event),
            r"7  opened\n[OK]\u{1b}]52;c;x\u{7}  capture=capture\rforged  attempt=delivery\u{9b}2J"
        );
    }

    #[test]
    fn monitor_output_neutralizes_status_controls_and_truncates_unicode_safely() {
        let styled = style_monitor_status(Some("pending\n[OK]\u{1b}]52;c;x\u{7}"));

        assert!(!styled.contains('\n'));
        assert!(!styled.contains("\u{1b}]52"));
        assert!(styled.contains(r"pending\n[OK]\u{1b}]52;c;x\u{7}"));
        assert_eq!(truncate_terminal_inline("東京 webhook", 4), "東京 …");
        assert_eq!(
            truncate_terminal_inline("\u{1b}]52;c;x\u{7}", 64),
            r"\u{1b}]52;c;x\u{7}"
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

    fn render_empty_status(title: &str, action: &str) -> String {
        format!(
            "{}\n\n{}\n",
            format_status_line(OutputStatus::Info, title),
            format_wrapped_field_lines("ACTION", action, 80)
        )
    }

    fn make_config(selected_org: Option<&str>) -> config::Config {
        config::Config {
            selected_organization_id: selected_org.map(String::from),
            ..config::Config::default()
        }
    }

    fn refreshable_config() -> config::Config {
        config::Config {
            access_token: Some("old-token".into()),
            token_expires_at: Some(Utc::now() + ChronoDuration::minutes(5)),
            refresh_token: Some("refresh-token".into()),
            refresh_token_expires_at: Some(Utc::now() + ChronoDuration::hours(1)),
            ..config::Config::default()
        }
    }

    #[test]
    fn access_token_refresh_delay_applies_skew_and_clamps_expired_tokens() {
        let mut config = refreshable_config();
        config.token_expires_at = Some(Utc::now() + ChronoDuration::seconds(90));
        let delay = access_token_refresh_delay(&config);
        assert!((29..=30).contains(&delay.as_secs()));

        config.token_expires_at = Some(Utc::now() - ChronoDuration::seconds(1));
        assert_eq!(access_token_refresh_delay(&config), Duration::ZERO);
    }

    #[tokio::test]
    async fn refresh_loop_terminates_when_receiver_is_closed() {
        let config = refreshable_config();
        let (tx, rx) = watch::channel("old-token".to_string());
        let refresh_loop = tokio::spawn(refresh_access_token_loop(config, tx));
        tokio::task::yield_now().await;
        drop(rx);
        tokio::time::timeout(Duration::from_secs(1), refresh_loop)
            .await
            .expect("loop should interrupt its refresh sleep")
            .unwrap();
    }

    #[tokio::test]
    async fn refresh_loop_cancels_hanging_request_when_receiver_is_closed() {
        let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .unwrap();
        let base_url = format!("http://{}", listener.local_addr().unwrap());
        let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
        let server = tokio::spawn(async move {
            let (connection, _) = listener.accept().await.unwrap();
            accepted_tx.send(()).unwrap();
            let _connection = connection;
            std::future::pending::<()>().await;
        });
        let mut config = refreshable_config();
        config.token_expires_at = Some(Utc::now() - ChronoDuration::seconds(1));
        let (tx, rx) = watch::channel("old-token".to_string());
        let refresh_loop = tokio::spawn(async move {
            refresh_access_token_loop_with(config, tx, &base_url).await;
        });

        tokio::time::timeout(Duration::from_secs(5), accepted_rx)
            .await
            .expect("refresh request should reach server")
            .unwrap();
        drop(rx);
        tokio::time::timeout(Duration::from_secs(5), refresh_loop)
            .await
            .expect("loop should cancel its in-flight refresh request")
            .unwrap();
        server.abort();
        server.await.unwrap_err();
    }

    #[tokio::test]
    async fn json_worker_is_cancelled_and_awaited_on_injected_shutdown() {
        let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
        struct NotifyDrop(Option<tokio::sync::oneshot::Sender<()>>);
        impl Drop for NotifyDrop {
            fn drop(&mut self) {
                let _ = self.0.take().expect("sender").send(());
            }
        }
        let worker = tokio::spawn(async move {
            let _guard = NotifyDrop(Some(dropped_tx));
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
        let result = supervise_json_worker(
            worker,
            std::future::pending::<()>(),
            std::future::ready(Ok(())),
        )
        .await
        .unwrap();
        assert_eq!(result, WorkerCompletion::Shutdown);
        dropped_rx.await.expect("worker drop notification");
    }

    #[test]
    fn terminal_initialization_cleanup_state_can_be_disarmed_without_terminal_io() {
        let mut cleanup = TerminalInitCleanup::raw_mode_enabled();
        assert!(cleanup.raw_mode);
        cleanup.alternate_screen = true;
        cleanup.disarm();
        assert_eq!(cleanup, TerminalInitCleanup::default());
    }

    #[tokio::test]
    async fn refresh_persists_before_returning_new_token() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/auth/refresh")
            .with_status(200)
            .with_body(r#"{"access_token":"new-token","expires_in":3600}"#)
            .create_async()
            .await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut config = refreshable_config();
        let token = refresh_access_token_from_config_with(&mut config, &server.url(), Some(&path))
            .await
            .unwrap();
        mock.assert_async().await;
        let saved = config::Config::load_from(&path).unwrap();
        assert_eq!(
            (token.as_str(), saved.access_token.as_deref()),
            ("new-token", Some("new-token"))
        );
    }

    #[tokio::test]
    async fn expired_refresh_token_does_not_call_server() {
        let mut config = refreshable_config();
        config.refresh_token_expires_at = Some(Utc::now() - ChronoDuration::seconds(1));
        let err = refresh_access_token_from_config_with(&mut config, "http://unused", None)
            .await
            .unwrap_err();
        assert!(err.to_string().contains("Refresh token expired"));
    }

    #[tokio::test]
    async fn save_failure_does_not_publish_or_mutate_new_token() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/api/v1/auth/refresh")
            .with_status(200)
            .with_body(r#"{"access_token":"must-not-publish","expires_in":3600}"#)
            .create_async()
            .await;
        let dir = tempfile::tempdir().unwrap();
        let mut config = refreshable_config();
        let err =
            refresh_access_token_from_config_with(&mut config, &server.url(), Some(dir.path()))
                .await
                .unwrap_err();
        assert!(!err.to_string().is_empty());
        assert_eq!(config.access_token.as_deref(), Some("old-token"));
    }

    async fn mock_refresh_success(server: &mut mockito::Server) -> mockito::Mock {
        server
            .mock("POST", "/api/v1/auth/refresh")
            .with_status(200)
            .with_body(r#"{"access_token":"new-token","expires_in":3600}"#)
            .create_async()
            .await
    }

    #[tokio::test]
    async fn refresh_preserves_organization_selected_by_another_process() {
        let mut server = mockito::Server::new_async().await;
        mock_refresh_success(&mut server).await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut config = refreshable_config();
        config.selected_organization_id = Some("org-a".into());
        config.save_to(&path).unwrap();

        let mut other_process = config::Config::load_from(&path).unwrap();
        other_process.selected_organization_id = Some("org-b".into());
        other_process.save_to(&path).unwrap();

        refresh_access_token_from_config_with(&mut config, &server.url(), Some(&path))
            .await
            .unwrap();

        let saved = config::Config::load_from(&path).unwrap();
        assert_eq!(saved.selected_organization_id.as_deref(), Some("org-b"));
        assert_eq!(saved.access_token.as_deref(), Some("new-token"));
        assert_eq!(config.selected_organization_id.as_deref(), Some("org-b"));
        assert_eq!(config.access_token.as_deref(), Some("new-token"));
    }

    #[tokio::test]
    async fn refresh_preserves_refresh_token_rotated_by_another_process() {
        let mut server = mockito::Server::new_async().await;
        mock_refresh_success(&mut server).await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut config = refreshable_config();
        config.save_to(&path).unwrap();

        let rotated_expiry = Utc::now() + ChronoDuration::days(30);
        let mut other_process = config::Config::load_from(&path).unwrap();
        other_process.set_tokens(
            "relogin-token".into(),
            Utc::now() + ChronoDuration::hours(1),
            Some("rotated-refresh".into()),
            Some(rotated_expiry),
        );
        other_process.save_to(&path).unwrap();

        refresh_access_token_from_config_with(&mut config, &server.url(), Some(&path))
            .await
            .unwrap();

        let saved = config::Config::load_from(&path).unwrap();
        assert_eq!(saved.refresh_token.as_deref(), Some("rotated-refresh"));
        assert_eq!(saved.refresh_token_expires_at, Some(rotated_expiry));
        assert_eq!(saved.access_token.as_deref(), Some("new-token"));
        assert_eq!(config.refresh_token.as_deref(), Some("rotated-refresh"));
    }

    #[tokio::test]
    async fn refresh_does_not_resurrect_session_after_logout_elsewhere() {
        let mut server = mockito::Server::new_async().await;
        mock_refresh_success(&mut server).await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut config = refreshable_config();
        config.selected_organization_id = Some("org-a".into());
        config.save_to(&path).unwrap();

        let mut other_process = config::Config::load_from(&path).unwrap();
        other_process.clear_token();
        other_process.save_to(&path).unwrap();

        let token = refresh_access_token_from_config_with(&mut config, &server.url(), Some(&path))
            .await
            .unwrap();

        assert_eq!(token, "new-token");
        let saved = config::Config::load_from(&path).unwrap();
        assert_eq!(saved.access_token, None);
        assert_eq!(saved.refresh_token, None);
        assert_eq!(saved.selected_organization_id.as_deref(), Some("org-a"));
        assert_eq!(config.refresh_token, None);
    }

    #[tokio::test]
    async fn refresh_writes_in_memory_config_when_file_is_missing() {
        let mut server = mockito::Server::new_async().await;
        mock_refresh_success(&mut server).await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut config = refreshable_config();
        config.selected_organization_id = Some("org-a".into());

        refresh_access_token_from_config_with(&mut config, &server.url(), Some(&path))
            .await
            .unwrap();

        let saved = config::Config::load_from(&path).unwrap();
        assert_eq!(saved.access_token.as_deref(), Some("new-token"));
        assert_eq!(saved.refresh_token.as_deref(), Some("refresh-token"));
        assert_eq!(saved.selected_organization_id.as_deref(), Some("org-a"));
    }

    #[tokio::test]
    async fn refresh_falls_back_to_in_memory_config_when_file_is_corrupt() {
        let mut server = mockito::Server::new_async().await;
        mock_refresh_success(&mut server).await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        std::fs::write(&path, "{ not json").unwrap();
        let mut config = refreshable_config();
        config.selected_organization_id = Some("org-a".into());

        let token = refresh_access_token_from_config_with(&mut config, &server.url(), Some(&path))
            .await
            .unwrap();

        assert_eq!(token, "new-token");
        let saved = config::Config::load_from(&path).unwrap();
        assert_eq!(saved.access_token.as_deref(), Some("new-token"));
        assert_eq!(saved.refresh_token.as_deref(), Some("refresh-token"));
        assert_eq!(saved.selected_organization_id.as_deref(), Some("org-a"));
        assert_eq!(config.access_token.as_deref(), Some("new-token"));
    }

    #[tokio::test]
    async fn refresh_rejects_absurd_expires_in_with_clear_error() {
        let mut server = mockito::Server::new_async().await;
        server
            .mock("POST", "/api/v1/auth/refresh")
            .with_status(200)
            .with_body(r#"{"access_token":"new-token","expires_in":18446744073709551615}"#)
            .create_async()
            .await;
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.json");
        let mut config = refreshable_config();

        let err = refresh_access_token_from_config_with(&mut config, &server.url(), Some(&path))
            .await
            .unwrap_err();

        assert!(
            err.to_string()
                .contains("Authorization server returned an invalid token lifetime"),
            "{err}"
        );
        assert_eq!(config.access_token.as_deref(), Some("old-token"));
        assert!(!path.exists());
    }

    fn parsed_tunnel_target(args: &[&str]) -> TunnelTarget {
        let cli = Cli::try_parse_from(args).expect("tunnel command");
        match cli.command.expect("parsed command") {
            Commands::Tunnel {
                action: None,
                target,
            } => target.resolve(),
            Commands::Tunnel {
                action:
                    Some(TunnelAction::Start(action_target) | TunnelAction::Prepare(action_target)),
                target,
            } => target.merge(action_target).resolve(),
            _ => panic!("expected a tunnel target command"),
        }
    }

    #[test]
    fn json_error_receipt_has_stable_machine_readable_shape() {
        let receipt = json_error_receipt(&anyhow!("Something failed"));

        assert_eq!(receipt["$schema"], COMMAND_ERROR_SCHEMA);
        assert_eq!(receipt["type"], "error");
        assert_eq!(receipt["ok"], false);
        assert_eq!(receipt["error"]["code"], "command_failed");
        assert_eq!(receipt["error"]["message"], "Something failed");
        assert!(receipt["error"]["causes"].is_array());
    }

    #[test]
    fn command_events_declare_document_specific_schemas() {
        let listen = listen_event_base(
            "connected",
            "connected",
            "endpoint-123",
            "http://localhost:3000",
        );
        let tunnel = tunnel_event_base("connected", "connected");

        assert_eq!(listen["$schema"], LISTEN_EVENT_SCHEMA);
        assert_eq!(listen["type"], "event");
        assert_eq!(tunnel["$schema"], TUNNEL_EVENT_SCHEMA);
        assert_eq!(tunnel["type"], "event");
    }

    fn lifecycle_contract(major: u64) -> api::TunnelLifecycleContract {
        api::TunnelLifecycleContract {
            id: "hooklistener.tunnel.lifecycle".to_string(),
            version: format!("{major}.0.0"),
            schema: api::TunnelSchemaVersion { major, minor: 0 },
        }
    }

    #[test]
    fn tunnel_lifecycle_subcommands_preserve_default_start_syntax() {
        let default = Cli::try_parse_from(["hooklistener", "tunnel", "--port", "4000"])
            .expect("legacy tunnel syntax");
        assert!(matches!(
            default.command,
            Some(Commands::Tunnel {
                action: None,
                target: TunnelTargetArgs {
                    port: Some(4000),
                    ..
                }
            })
        ));

        let before_start = parsed_tunnel_target(&[
            "hooklistener",
            "tunnel",
            "--port",
            "4000",
            "--host",
            "127.0.0.1",
            "--org",
            "org_123",
            "--slug",
            "billing",
            "--allow-non-loopback",
            "--no-replay-buffered",
            "start",
        ]);
        let after_start = parsed_tunnel_target(&[
            "hooklistener",
            "tunnel",
            "start",
            "--port",
            "4000",
            "--host",
            "127.0.0.1",
            "--org",
            "org_123",
            "--slug",
            "billing",
            "--allow-non-loopback",
            "--no-replay-buffered",
        ]);
        assert_eq!(before_start, after_start);
        assert_eq!(before_start.port, 4000);
        assert_eq!(before_start.host, "127.0.0.1");
        assert_eq!(before_start.org.as_deref(), Some("org_123"));
        assert_eq!(before_start.slug.as_deref(), Some("billing"));
        assert!(before_start.allow_non_loopback);
        assert!(before_start.no_replay_buffered);

        for action in ["prepare", "start", "activate"] {
            let before =
                parsed_tunnel_target(&["hooklistener", "tunnel", "--port", "4001", action]);
            let after = parsed_tunnel_target(&["hooklistener", "tunnel", action, "--port", "4001"]);
            assert_eq!(before, after);
        }

        let events = Cli::try_parse_from([
            "hooklistener",
            "tunnel",
            "events",
            "--cursor",
            "opaque",
            "--follow",
            "--json",
        ])
        .expect("events command");
        assert!(events.json);
        assert!(matches!(
            events.command,
            Some(Commands::Tunnel {
                action: Some(TunnelAction::Events {
                    cursor: Some(cursor),
                    follow: true,
                    ..
                }),
                ..
            }) if cursor == "opaque"
        ));
    }

    #[test]
    fn anonymous_tunnel_claim_and_detach_commands_parse_explicit_credentials() {
        let anonymous = Cli::try_parse_from([
            "hooklistener",
            "anon",
            "tunnel",
            "--port",
            "4000",
            "--name",
            "stable-demo",
            "--ttl",
            "1200",
        ])
        .expect("anonymous tunnel command");
        assert!(matches!(
            anonymous.command,
            Some(Commands::Anon {
                action: AnonAction::Tunnel {
                    port: 4000,
                    name: Some(name),
                    ttl,
                    ..
                }
            }) if name == "stable-demo" && ttl == Duration::from_secs(1200)
        ));

        let claim = Cli::try_parse_from([
            "hooklistener",
            "anon",
            "claim",
            "route-123",
            "--token",
            "hkac_secret",
            "--org",
            "org-123",
        ])
        .expect("anonymous claim command");
        assert!(matches!(
            claim.command,
            Some(Commands::Anon {
                action: AnonAction::Claim {
                    route_id,
                    token,
                    org: Some(org),
                }
            }) if route_id == "route-123" && token == "hkac_secret" && org == "org-123"
        ));

        let detach = Cli::try_parse_from([
            "hooklistener",
            "tunnel",
            "detach",
            "session-123",
            "--reason",
            "switching-machines",
        ])
        .expect("tunnel detach command");
        assert!(matches!(
            detach.command,
            Some(Commands::Tunnel {
                action: Some(TunnelAction::Detach {
                    session_id,
                    reason: Some(reason),
                    ..
                }),
                ..
            }) if session_id == "session-123" && reason == "switching-machines"
        ));

        assert!(Cli::try_parse_from(["hooklistener", "anon", "tunnel", "--ttl", "59"]).is_err());
    }

    #[test]
    fn tunnel_schema_mismatch_is_typed_before_activation() {
        let error = validate_tunnel_schema(&lifecycle_contract(2)).unwrap_err();

        assert_eq!(error_code(&error), "incompatible_schema");
        assert_eq!(command_exit_code(&error), 3);
        assert_eq!(
            json_error_receipt(&error)["error"]["details"]["actual_major"],
            2
        );
    }

    #[test]
    fn expired_tunnel_cursor_has_stable_exit_code_and_resync_details() {
        let error = anyhow::Error::new(errors::TunnelLifecycleError::CursorExpired {
            earliest_cursor: Some("earliest".to_string()),
            resync: serde_json::json!({"sessions": "/api/v1/tunnel/sessions"}),
        });
        let receipt = json_error_receipt(&error);

        assert_eq!(error_code(&error), "cursor_expired");
        assert_eq!(command_exit_code(&error), 4);
        assert_eq!(receipt["error"]["details"]["earliest_cursor"], "earliest");
        assert_eq!(
            receipt["error"]["details"]["resync"]["sessions"],
            "/api/v1/tunnel/sessions"
        );

        let without_cursor = anyhow::Error::new(errors::TunnelLifecycleError::CursorExpired {
            earliest_cursor: None,
            resync: serde_json::json!({}),
        });
        assert_eq!(
            error_hint(&without_cursor),
            Some(
                "Resync sessions, captures, attempts, and outcomes, then request a fresh event cursor."
            )
        );
    }

    #[test]
    fn tunnel_receipts_are_versioned_and_do_not_expose_credentials() {
        let receipt = tunnel_lifecycle_receipt(
            "prepare",
            "prepared",
            "org_123",
            None,
            serde_json::json!({
                "local_target_url": "http://localhost:3000",
                "requested_slug": "billing"
            }),
        );
        let serialized = serde_json::to_string(&receipt).unwrap();

        assert_eq!(receipt["$schema"], TUNNEL_RECEIPT_SCHEMA);
        assert_eq!(receipt["schema_version"], 1);
        assert!(receipt["event_id"].is_string());
        assert_eq!(receipt["sequence"], 0);
        assert!(!serialized.contains("access_token"));
        assert!(!serialized.contains("resume_token"));
    }

    #[test]
    fn tunnel_event_envelope_carries_cursor_identity_sequence_and_resources() {
        let event = api::TunnelLifecycleEvent {
            id: "event_123".to_string(),
            position: 42,
            cursor: "opaque-cursor".to_string(),
            organization_id: "org_123".to_string(),
            capture_id: "capture_123".to_string(),
            delivery_id: Some("attempt_123".to_string()),
            sequence: 3,
            fence: Some(2),
            event_type: "forward_started".to_string(),
            metadata: serde_json::json!({
                "source": "tunnel",
                "status_code": 202,
                "headers": {"authorization": "Bearer secret-header"},
                "body": "secret-body",
                "access_token": "secret-access-token",
                "credentials": {"password": "secret-password"},
                "error_code": {"token": "secret-nested-token"}
            }),
            created_at: "2026-07-14T20:00:00Z".to_string(),
        };

        let envelope = tunnel_lifecycle_event_envelope(&event);

        assert_eq!(envelope["$schema"], TUNNEL_EVENT_SCHEMA);
        assert_eq!(envelope["event_id"], "event_123");
        assert_eq!(envelope["sequence"], 3);
        assert_eq!(envelope["cursor"], "opaque-cursor");
        assert_eq!(envelope["metadata"]["source"], "tunnel");
        assert_eq!(envelope["metadata"]["status_code"], 202);
        assert!(envelope["metadata"].get("headers").is_none());
        assert!(envelope["metadata"].get("body").is_none());
        assert!(envelope["metadata"].get("access_token").is_none());
        assert!(envelope["metadata"].get("credentials").is_none());
        assert!(envelope["metadata"].get("error_code").is_none());
        let serialized = serde_json::to_string(&envelope).unwrap();
        assert!(!serialized.contains("secret"));
        assert_eq!(
            envelope["resources"]["attempt"],
            "hooklistener://tunnel/attempts/attempt_123"
        );
    }

    #[test]
    fn confirmation_requires_the_full_yes_token() {
        assert!(confirmation_is_yes("yes\n"));
        assert!(!confirmation_is_yes("y"));
    }

    #[test]
    fn destructive_commands_accept_global_yes_flag() {
        let cli =
            Cli::try_parse_from(["hooklistener", "endpoint", "delete", "ep_123", "--yes"]).unwrap();

        assert!(cli.yes);
    }

    #[test]
    fn listen_accepts_explicit_insecure_dev_server_opt_in_as_global_flag() {
        let cli = Cli::try_parse_from([
            "hooklistener",
            "listen",
            "example",
            "--ws-url",
            "ws://dev.example.com",
            "--allow-insecure-dev-server",
        ])
        .unwrap();

        assert!(cli.allow_insecure_dev_server);
    }

    #[test]
    fn cli_definition_is_consistent() {
        Cli::command().debug_assert();
    }

    fn render_help_snapshot(path: &[&str]) -> String {
        let mut cli = Cli::command()
            .term_width(100)
            .color(clap::ColorChoice::Never);
        cli.build();
        let mut command = &mut cli;
        for name in path {
            command = command
                .find_subcommand_mut(name)
                .unwrap_or_else(|| panic!("subcommand `{name}` exists"));
        }
        let help = command.render_help().to_string();
        assert_no_emoji(&help);
        help
    }

    #[test]
    fn help_snapshot_top_level() {
        insta::assert_snapshot!("help_top_level", render_help_snapshot(&[]));
    }

    #[test]
    fn help_snapshot_endpoint() {
        insta::assert_snapshot!("help_endpoint", render_help_snapshot(&["endpoint"]));
    }

    #[test]
    fn help_snapshot_endpoint_list() {
        insta::assert_snapshot!(
            "help_endpoint_list",
            render_help_snapshot(&["endpoint", "list"])
        );
    }

    #[test]
    fn help_snapshot_tunnel() {
        insta::assert_snapshot!("help_tunnel", render_help_snapshot(&["tunnel"]));
    }

    #[test]
    fn help_snapshot_tunnel_events() {
        insta::assert_snapshot!(
            "help_tunnel_events",
            render_help_snapshot(&["tunnel", "events"])
        );
    }

    #[test]
    fn help_snapshot_anon() {
        insta::assert_snapshot!("help_anon", render_help_snapshot(&["anon"]));
    }

    #[test]
    fn help_snapshot_monitor_create() {
        insta::assert_snapshot!(
            "help_monitor_create",
            render_help_snapshot(&["monitor", "create"])
        );
    }

    #[test]
    fn help_snapshot_cases_run() {
        insta::assert_snapshot!("help_cases_run", render_help_snapshot(&["cases", "run"]));
    }

    #[test]
    fn help_snapshot_share_create() {
        insta::assert_snapshot!(
            "help_share_create",
            render_help_snapshot(&["share", "create"])
        );
    }

    #[test]
    fn org_flag_accepts_short_o_on_every_command() {
        let cli = Cli::try_parse_from(["hooklistener", "endpoint", "list", "-o", "org_1"])
            .expect("endpoint list -o parses");
        assert!(matches!(
            cli.command,
            Some(Commands::Endpoint {
                action: EndpointAction::List { ref org },
            }) if org.as_deref() == Some("org_1")
        ));

        let cli = Cli::try_parse_from(["hooklistener", "monitor", "list", "-o", "org_1"])
            .expect("monitor list -o parses");
        assert!(matches!(
            cli.command,
            Some(Commands::Monitor {
                action: MonitorAction::List { ref org },
            }) if org.as_deref() == Some("org_1")
        ));

        let cli = Cli::try_parse_from(["hooklistener", "share", "list", "req_1", "-o", "org_1"])
            .expect("share list -o parses");
        assert!(matches!(
            cli.command,
            Some(Commands::Share {
                action: ShareAction::List { ref request_id, ref org },
            }) if request_id == "req_1" && org.as_deref() == Some("org_1")
        ));

        let cli = Cli::try_parse_from([
            "hooklistener",
            "static-tunnel",
            "delete",
            "st_1",
            "-o",
            "org_1",
        ])
        .expect("static-tunnel delete -o parses");
        assert!(matches!(
            cli.command,
            Some(Commands::StaticTunnel {
                action: StaticTunnelAction::Delete { ref static_tunnel_id, ref org },
            }) if static_tunnel_id == "st_1" && org.as_deref() == Some("org_1")
        ));

        let cli = Cli::try_parse_from(["hooklistener", "cases", "run", "ep_1", "-o", "org_1"])
            .expect("cases run -o parses");
        assert!(matches!(
            cli.command,
            Some(Commands::Cases {
                action: CasesAction::Run { ref org, .. },
            }) if org.as_deref() == Some("org_1")
        ));
    }

    #[test]
    fn completions_power_shell_is_alias_of_powershell() {
        for spelling in ["powershell", "power-shell", "PowerShell"] {
            let cli = Cli::try_parse_from(["hooklistener", "completions", spelling])
                .unwrap_or_else(|err| panic!("completions {spelling} parses: {err}"));
            assert!(
                matches!(
                    cli.command,
                    Some(Commands::Completions {
                        shell: CompletionShell::PowerShell,
                    })
                ),
                "completions {spelling} must yield PowerShell"
            );
        }
    }

    #[test]
    fn completions_generate_for_every_shell() {
        for shell in CompletionShell::value_variants() {
            let mut buf: Vec<u8> = Vec::new();
            write_completions(*shell, &mut buf)
                .unwrap_or_else(|err| panic!("completions {shell:?} writes: {err}"));
            assert!(!buf.is_empty(), "completions {shell:?} must not be empty");
            let script = String::from_utf8(buf)
                .unwrap_or_else(|err| panic!("completions {shell:?} is UTF-8: {err}"));
            assert!(
                script.contains("hooklistener"),
                "completions {shell:?} must mention the binary name"
            );
        }
    }

    struct BrokenPipeWriter;

    impl io::Write for BrokenPipeWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::from(io::ErrorKind::BrokenPipe))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    struct FailingWriter;

    impl io::Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::other("disk full"))
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn completions_broken_pipe_is_not_an_error() {
        for shell in CompletionShell::value_variants() {
            let mut out = BrokenPipeWriter;
            assert!(
                print_completions(*shell, &mut out).is_ok(),
                "completions {shell:?} must treat a closed pipe as success"
            );
        }
    }

    #[test]
    fn completions_other_write_errors_propagate() {
        let mut out = FailingWriter;
        let err = print_completions(CompletionShell::Bash, &mut out)
            .expect_err("a non-pipe write error must propagate");
        assert_eq!(err.kind(), io::ErrorKind::Other);
        assert_eq!(err.to_string(), "disk full");
    }

    #[test]
    fn renamed_positionals_keep_their_positions() {
        let cli = Cli::try_parse_from(["hooklistener", "listen", "my-endpoint"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Listen { ref endpoint_slug, .. }) if endpoint_slug == "my-endpoint"
        ));

        let cli = Cli::try_parse_from(["hooklistener", "org", "use", "org_1"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Org {
                action: OrgAction::Use { ref org_id },
            }) if org_id == "org_1"
        ));

        let cli = Cli::try_parse_from(["hooklistener", "anon", "show", "ep_1"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Anon {
                action: AnonAction::Show { ref endpoint_id },
            }) if endpoint_id == "ep_1"
        ));

        let cli = Cli::try_parse_from(["hooklistener", "share", "show", "tok_1"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Share {
                action: ShareAction::Show { ref share_token },
            }) if share_token == "tok_1"
        ));

        let cli = Cli::try_parse_from(["hooklistener", "monitor", "show", "mon_1"]).unwrap();
        assert!(matches!(
            cli.command,
            Some(Commands::Monitor {
                action: MonitorAction::Show { ref monitor_id, .. },
            }) if monitor_id == "mon_1"
        ));
    }

    #[test]
    fn endpoint_requests_is_alias_of_list_requests() {
        for spelling in ["requests", "list-requests"] {
            let cli = Cli::try_parse_from(["hooklistener", "endpoint", spelling, "ep_1"])
                .unwrap_or_else(|err| panic!("endpoint {spelling} parses: {err}"));
            assert!(
                matches!(
                    cli.command,
                    Some(Commands::Endpoint {
                        action: EndpointAction::ListRequests {
                            ref endpoint_id,
                            page: 1,
                            page_size: 50,
                            org: None,
                        },
                    }) if endpoint_id == "ep_1"
                ),
                "endpoint {spelling} must yield ListRequests"
            );
        }
    }

    #[test]
    fn endpoint_request_is_alias_of_show_request() {
        for spelling in ["request", "show-request"] {
            let cli = Cli::try_parse_from(["hooklistener", "endpoint", spelling, "ep_1", "req_1"])
                .unwrap_or_else(|err| panic!("endpoint {spelling} parses: {err}"));
            assert!(
                matches!(
                    cli.command,
                    Some(Commands::Endpoint {
                        action: EndpointAction::ShowRequest {
                            ref endpoint_id,
                            ref request_id,
                            org: None,
                        },
                    }) if endpoint_id == "ep_1" && request_id == "req_1"
                ),
                "endpoint {spelling} must yield ShowRequest"
            );
        }
    }

    #[test]
    fn endpoint_forwards_is_alias_of_list_forwards() {
        for spelling in ["forwards", "list-forwards"] {
            let cli = Cli::try_parse_from([
                "hooklistener",
                "endpoint",
                spelling,
                "ep_1",
                "req_1",
                "--page",
                "2",
            ])
            .unwrap_or_else(|err| panic!("endpoint {spelling} parses: {err}"));
            assert!(
                matches!(
                    cli.command,
                    Some(Commands::Endpoint {
                        action: EndpointAction::ListForwards {
                            ref endpoint_id,
                            ref request_id,
                            page: 2,
                            page_size: 50,
                            org: None,
                        },
                    }) if endpoint_id == "ep_1" && request_id == "req_1"
                ),
                "endpoint {spelling} must yield ListForwards"
            );
        }
    }

    #[test]
    fn endpoint_forward_is_alias_of_show_forward() {
        for spelling in ["forward", "show-forward"] {
            let cli = Cli::try_parse_from(["hooklistener", "endpoint", spelling, "fwd_1"])
                .unwrap_or_else(|err| panic!("endpoint {spelling} parses: {err}"));
            assert!(
                matches!(
                    cli.command,
                    Some(Commands::Endpoint {
                        action: EndpointAction::ShowForward {
                            ref forward_id,
                            org: None,
                        },
                    }) if forward_id == "fwd_1"
                ),
                "endpoint {spelling} must yield ShowForward"
            );
        }
    }

    #[test]
    fn endpoint_forward_aliases_do_not_capture_forward_request_or_delete_request() {
        let cli = Cli::try_parse_from([
            "hooklistener",
            "endpoint",
            "forward-request",
            "ep_1",
            "req_1",
            "http://localhost:3000/hook",
        ])
        .expect("endpoint forward-request parses");
        assert!(matches!(
            cli.command,
            Some(Commands::Endpoint {
                action: EndpointAction::ForwardRequest { .. },
            })
        ));

        let cli = Cli::try_parse_from([
            "hooklistener",
            "endpoint",
            "delete-request",
            "ep_1",
            "req_1",
        ])
        .expect("endpoint delete-request parses");
        assert!(matches!(
            cli.command,
            Some(Commands::Endpoint {
                action: EndpointAction::DeleteRequest { .. },
            })
        ));
    }

    #[test]
    fn anon_events_is_alias_of_list_events() {
        for spelling in ["events", "list-events"] {
            let cli = Cli::try_parse_from([
                "hooklistener",
                "anon",
                spelling,
                "ep_1",
                "--token",
                "viewer_token",
            ])
            .unwrap_or_else(|err| panic!("anon {spelling} parses: {err}"));
            assert!(
                matches!(
                    cli.command,
                    Some(Commands::Anon {
                        action: AnonAction::ListEvents {
                            ref endpoint_id,
                            ref token,
                            page: 1,
                            page_size: 50,
                        },
                    }) if endpoint_id == "ep_1" && token == "viewer_token"
                ),
                "anon {spelling} must yield ListEvents"
            );
        }
    }

    #[test]
    fn anon_event_is_alias_of_show_event() {
        for spelling in ["event", "show-event"] {
            let cli = Cli::try_parse_from([
                "hooklistener",
                "anon",
                spelling,
                "ep_1",
                "evt_1",
                "--token",
                "viewer_token",
            ])
            .unwrap_or_else(|err| panic!("anon {spelling} parses: {err}"));
            assert!(
                matches!(
                    cli.command,
                    Some(Commands::Anon {
                        action: AnonAction::ShowEvent {
                            ref endpoint_id,
                            ref event_id,
                            ref token,
                        },
                    }) if endpoint_id == "ep_1" && event_id == "evt_1" && token == "viewer_token"
                ),
                "anon {spelling} must yield ShowEvent"
            );
        }
    }

    #[test]
    fn tunnel_activate_is_alias_of_start() {
        let cli = Cli::try_parse_from(["hooklistener", "tunnel", "activate", "--port", "5000"])
            .expect("tunnel activate parses");
        assert!(matches!(
            cli.command,
            Some(Commands::Tunnel {
                action: Some(TunnelAction::Start(TunnelTargetArgs {
                    port: Some(5000),
                    ..
                })),
                ..
            })
        ));

        let alias = parsed_tunnel_target(&["hooklistener", "tunnel", "activate", "--port", "5000"]);
        let start = parsed_tunnel_target(&["hooklistener", "tunnel", "start", "--port", "5000"]);
        assert_eq!(alias, start);
    }

    #[test]
    fn tunnel_list_accepts_status_limit_and_short_org() {
        let cli = Cli::try_parse_from([
            "hooklistener",
            "tunnel",
            "list",
            "--status",
            "active",
            "--limit",
            "10",
            "-o",
            "org_1",
        ])
        .expect("tunnel list parses");
        assert!(matches!(
            cli.command,
            Some(Commands::Tunnel {
                action: Some(TunnelAction::List {
                    limit: 10,
                    status: Some(status),
                    org: Some(org),
                }),
                ..
            }) if status == "active" && org == "org_1"
        ));
    }

    #[test]
    fn tunnel_lifecycle_subcommands_accept_short_org() {
        for args in [
            vec!["tunnel", "status", "session-1", "-o", "org_1"],
            vec!["tunnel", "events", "-o", "org_1"],
            vec!["tunnel", "capture", "capture-1", "-o", "org_1"],
            vec!["tunnel", "attempt", "attempt-1", "-o", "org_1"],
            vec!["tunnel", "stop", "session-1", "-o", "org_1"],
            vec!["tunnel", "detach", "session-1", "-o", "org_1"],
            vec!["anon", "claim", "route-1", "--token", "t", "-o", "org_1"],
        ] {
            let mut full = vec!["hooklistener"];
            full.extend(args.iter().copied());
            let cli = Cli::try_parse_from(&full).unwrap_or_else(|err| panic!("{args:?}: {err}"));
            let org = match cli.command.expect("parsed command") {
                Commands::Tunnel {
                    action:
                        Some(
                            TunnelAction::Status { org, .. }
                            | TunnelAction::Events { org, .. }
                            | TunnelAction::Capture { org, .. }
                            | TunnelAction::Attempt { org, .. }
                            | TunnelAction::Stop { org, .. }
                            | TunnelAction::Detach { org, .. },
                        ),
                    ..
                } => org,
                Commands::Anon {
                    action: AnonAction::Claim { org, .. },
                } => org,
                _ => panic!("{args:?}: unexpected command"),
            };
            assert_eq!(org.as_deref(), Some("org_1"), "{args:?}");
        }
    }

    #[test]
    fn top_level_commands_are_listed_in_grouped_order() {
        let command = Cli::command();
        let names: Vec<&str> = command
            .get_subcommands()
            .map(|subcommand| subcommand.get_name())
            .collect();

        assert_eq!(
            names,
            [
                "listen",
                "tunnel",
                "endpoint",
                "static-tunnel",
                "anon",
                "cases",
                "share",
                "monitor",
                "login",
                "logout",
                "org",
                "config",
                "diagnostics",
                "clean-logs",
                "completions",
                "update",
            ]
        );
        assert_eq!(
            command.get_about().map(ToString::to_string).as_deref(),
            Some("Inspect webhooks, replay failures, and expose localhost from your terminal")
        );
    }

    #[test]
    fn log_level_is_global_after_tunnel_subcommand() {
        let cli =
            Cli::try_parse_from(["hooklistener", "tunnel", "--log-level", "debug", "prepare"])
                .unwrap();

        assert_eq!(cli.log_level, LogLevel::Debug);
    }

    #[test]
    fn log_flags_are_global_after_endpoint_list() {
        let cli = Cli::try_parse_from([
            "hooklistener",
            "endpoint",
            "list",
            "--log-stdout",
            "--log-dir",
            "/tmp/x",
        ])
        .unwrap();

        assert!(cli.log_stdout);
        assert_eq!(cli.log_dir, Some(PathBuf::from("/tmp/x")));
    }

    #[test]
    fn log_level_ignores_case() {
        let cli = Cli::try_parse_from(["hooklistener", "--log-level", "WARN", "endpoint", "list"])
            .unwrap();

        assert_eq!(cli.log_level, LogLevel::Warn);
    }

    #[test]
    fn log_level_defaults_to_info() {
        let cli = Cli::try_parse_from(["hooklistener", "endpoint", "list"]).unwrap();

        assert_eq!(cli.log_level, LogLevel::Info);
        assert_eq!(cli.log_level.as_str(), "info");
    }

    #[test]
    fn log_level_rejects_unknown_value() {
        let result =
            Cli::try_parse_from(["hooklistener", "--log-level", "verbose", "endpoint", "list"]);

        match result {
            Ok(_) => panic!("expected --log-level verbose to be rejected"),
            Err(error) => assert_eq!(error.kind(), clap::error::ErrorKind::InvalidValue),
        }
    }

    #[test]
    fn insecure_dev_server_flag_is_hidden_from_help() {
        let mut command = Cli::command().term_width(100);
        let help = command.render_help().to_string();

        assert!(!help.contains("insecure"), "{help}");
        assert_eq!(help.matches("Global options:").count(), 1, "{help}");
        for flag in [
            "--json",
            "--color",
            "--yes",
            "--log-level",
            "--log-dir",
            "--log-stdout",
        ] {
            assert!(help.contains(flag), "missing {flag} in {help}");
        }

        let mut root = Cli::command().term_width(100);
        root.build();
        let help = root
            .find_subcommand_mut("endpoint")
            .and_then(|endpoint| endpoint.find_subcommand_mut("list"))
            .expect("endpoint list command")
            .render_help()
            .to_string();

        assert!(!help.contains("insecure"), "{help}");
        assert_eq!(help.matches("Global options:").count(), 1, "{help}");
    }

    fn parse_error<I, T>(args: I) -> clap::Error
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        match Cli::try_parse_from(args) {
            Ok(_) => panic!("expected argument parsing to fail"),
            Err(err) => err,
        }
    }

    fn parse_error_kind<I, T>(args: I) -> clap::error::ErrorKind
    where
        I: IntoIterator<Item = T>,
        T: Into<std::ffi::OsString> + Clone,
    {
        parse_error(args).kind()
    }

    #[test]
    fn forward_request_method_is_a_value_enum_rendered_uppercase() {
        let cli = Cli::try_parse_from([
            "hooklistener",
            "endpoint",
            "forward-request",
            "ep_1",
            "req_1",
            "http://localhost:3000/hook",
            "--method",
            "post",
        ])
        .unwrap();

        match cli.command {
            Some(Commands::Endpoint {
                action: EndpointAction::ForwardRequest { method, .. },
            }) => {
                assert_eq!(method, Some(HttpMethod::Post));
                assert_eq!(method.unwrap().as_uppercase(), "POST");
            }
            _ => panic!("expected endpoint forward-request command"),
        }
    }

    #[test]
    fn forward_request_method_rejects_unknown_values() {
        let kind = parse_error_kind([
            "hooklistener",
            "endpoint",
            "forward-request",
            "ep_1",
            "req_1",
            "http://localhost:3000/hook",
            "--method",
            "trace",
        ]);
        assert_eq!(kind, clap::error::ErrorKind::InvalidValue);
    }

    #[test]
    fn monitor_create_method_defaults_to_get_and_accepts_any_case() {
        let cli = Cli::try_parse_from([
            "hooklistener",
            "monitor",
            "create",
            "API",
            "https://example.com/health",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Monitor {
                action:
                    MonitorAction::Create {
                        method,
                        expected_status,
                        failure_threshold,
                        ..
                    },
            }) => {
                assert_eq!(method, MonitorMethod::Get);
                assert_eq!(method.as_lowercase(), "get");
                assert_eq!(method.to_string(), "GET");
                assert_eq!(expected_status, 200);
                assert_eq!(failure_threshold, 2);
            }
            _ => panic!("expected monitor create command"),
        }

        let cli = Cli::try_parse_from([
            "hooklistener",
            "monitor",
            "create",
            "API",
            "https://example.com/health",
            "--method",
            "Post",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Monitor {
                action: MonitorAction::Create { method, .. },
            }) => {
                assert_eq!(method, MonitorMethod::Post);
                assert_eq!(method.as_lowercase(), "post");
            }
            _ => panic!("expected monitor create command"),
        }
    }

    #[test]
    fn monitor_method_rejects_options() {
        for args in [
            vec![
                "hooklistener",
                "monitor",
                "create",
                "API",
                "https://example.com/health",
                "--method",
                "options",
            ],
            vec![
                "hooklistener",
                "monitor",
                "update",
                "mon_1",
                "--method",
                "OPTIONS",
            ],
        ] {
            assert_eq!(parse_error_kind(args), clap::error::ErrorKind::InvalidValue);
        }
    }

    #[test]
    fn monitor_update_method_parses_case_insensitively() {
        let cli = Cli::try_parse_from([
            "hooklistener",
            "monitor",
            "update",
            "mon_1",
            "--method",
            "head",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Monitor {
                action: MonitorAction::Update { method, .. },
            }) => assert_eq!(method, Some(MonitorMethod::Head)),
            _ => panic!("expected monitor update command"),
        }
    }

    #[test]
    fn monitor_expected_status_must_be_an_http_status_code() {
        let kind = parse_error_kind([
            "hooklistener",
            "monitor",
            "create",
            "API",
            "https://example.com/health",
            "--expected-status",
            "42",
        ]);
        assert_eq!(kind, clap::error::ErrorKind::ValueValidation);

        let kind = parse_error_kind([
            "hooklistener",
            "monitor",
            "update",
            "mon_1",
            "--expected-status",
            "600",
        ]);
        assert_eq!(kind, clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn monitor_failure_threshold_rejects_zero() {
        let kind = parse_error_kind([
            "hooklistener",
            "monitor",
            "create",
            "API",
            "https://example.com/health",
            "--failure-threshold",
            "0",
        ]);
        assert_eq!(kind, clap::error::ErrorKind::ValueValidation);
    }

    #[test]
    fn paginated_lists_reject_page_zero() {
        for args in [
            vec![
                "hooklistener",
                "endpoint",
                "list-requests",
                "ep_1",
                "--page",
                "0",
            ],
            vec![
                "hooklistener",
                "endpoint",
                "list-forwards",
                "ep_1",
                "req_1",
                "--page-size",
                "0",
            ],
            vec![
                "hooklistener",
                "anon",
                "list-events",
                "ep_1",
                "--token",
                "t",
                "--page",
                "0",
            ],
            vec!["hooklistener", "monitor", "checks", "mon_1", "--page", "0"],
        ] {
            assert_eq!(
                parse_error_kind(args.clone()),
                clap::error::ErrorKind::ValueValidation,
                "{args:?} must fail range validation"
            );
        }

        let cli = Cli::try_parse_from([
            "hooklistener",
            "monitor",
            "checks",
            "mon_1",
            "--page",
            "2",
            "--page-size",
            "10",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Monitor {
                action:
                    MonitorAction::Checks {
                        page, page_size, ..
                    },
            }) => {
                assert_eq!(page, 2);
                assert_eq!(page_size, 10);
            }
            _ => panic!("expected monitor checks command"),
        }
    }

    #[test]
    fn config_set_key_is_a_value_enum() {
        let cli = Cli::try_parse_from([
            "hooklistener",
            "config",
            "set",
            "selected_organization_id",
            "org_1",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Config {
                action: ConfigAction::Set { key, value },
            }) => {
                assert_eq!(key, ConfigKey::SelectedOrganizationId);
                assert_eq!(value, "org_1");
            }
            _ => panic!("expected config set command"),
        }

        assert_eq!(
            parse_error_kind(["hooklistener", "config", "set", "other", "x"]),
            clap::error::ErrorKind::InvalidValue
        );
    }

    #[test]
    fn monitor_email_accepts_explicit_false() {
        let cli = Cli::try_parse_from([
            "hooklistener",
            "monitor",
            "create",
            "API",
            "https://example.com/health",
            "--email=false",
        ])
        .unwrap();

        match cli.command {
            Some(Commands::Monitor {
                action: MonitorAction::Create { email, .. },
            }) => assert!(!email),
            _ => panic!("expected monitor create command"),
        }
    }

    #[test]
    fn monitor_no_email_disables_notifications() {
        let cli = Cli::try_parse_from([
            "hooklistener",
            "monitor",
            "create",
            "API",
            "https://example.com/health",
            "--no-email",
        ])
        .unwrap();

        match cli.command {
            Some(Commands::Monitor {
                action:
                    MonitorAction::Create {
                        email, no_email, ..
                    },
            }) => {
                assert!(email);
                assert!(no_email);
                assert!(!monitor_email_enabled(email, no_email));
            }
            _ => panic!("expected monitor create command"),
        }

        assert!(monitor_email_enabled(true, false));
        assert!(!monitor_email_enabled(false, false));
    }

    #[test]
    fn monitor_email_and_no_email_conflict() {
        assert_eq!(
            parse_error_kind([
                "hooklistener",
                "monitor",
                "create",
                "API",
                "https://example.com/health",
                "--email",
                "--no-email",
            ]),
            clap::error::ErrorKind::ArgumentConflict
        );
    }

    #[test]
    fn monitor_update_enable_disable_conflict() {
        assert_eq!(
            parse_error_kind([
                "hooklistener",
                "monitor",
                "update",
                "mon_1",
                "--enable",
                "--disable",
            ]),
            clap::error::ErrorKind::ArgumentConflict
        );
        assert_eq!(
            parse_error_kind([
                "hooklistener",
                "monitor",
                "update",
                "mon_1",
                "--enable",
                "--enabled",
                "true",
            ]),
            clap::error::ErrorKind::ArgumentConflict
        );
    }

    #[test]
    fn monitor_update_enable_and_disable_set_enabled() {
        for (flag, expected) in [("--enable", Some(true)), ("--disable", Some(false))] {
            let cli =
                Cli::try_parse_from(["hooklistener", "monitor", "update", "mon_1", flag]).unwrap();
            match cli.command {
                Some(Commands::Monitor {
                    action:
                        MonitorAction::Update {
                            enable,
                            disable,
                            enabled,
                            ..
                        },
                }) => {
                    assert_eq!(enable, expected == Some(true));
                    assert_eq!(disable, expected == Some(false));
                    assert_eq!(enabled, None);
                    assert_eq!(monitor_enabled_update(enable, disable, enabled), expected);
                }
                _ => panic!("expected monitor update command"),
            }
        }

        assert_eq!(monitor_enabled_update(false, false, None), None);
    }

    #[test]
    fn monitor_update_enabled_hidden_flag_still_parses() {
        let cli = Cli::try_parse_from([
            "hooklistener",
            "monitor",
            "update",
            "mon_1",
            "--enabled",
            "false",
        ])
        .unwrap();

        match cli.command {
            Some(Commands::Monitor {
                action:
                    MonitorAction::Update {
                        enable,
                        disable,
                        enabled,
                        ..
                    },
            }) => {
                assert!(!enable);
                assert!(!disable);
                assert_eq!(enabled, Some(false));
                assert_eq!(
                    monitor_enabled_update(enable, disable, enabled),
                    Some(false)
                );
            }
            _ => panic!("expected monitor update command"),
        }
    }

    #[test]
    fn cases_run_hidden_target_flags_still_parse() {
        let cli = Cli::try_parse_from([
            "hooklistener",
            "cases",
            "run",
            "ep_123",
            "--target-url",
            "http://localhost:3000",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Cases {
                action:
                    CasesAction::Run {
                        target,
                        target_url,
                        target_id,
                        ..
                    },
            }) => {
                assert_eq!(target, None);
                assert_eq!(target_url.as_deref(), Some("http://localhost:3000"));
                assert_eq!(target_id, None);
            }
            _ => panic!("expected cases run command"),
        }

        let cli = Cli::try_parse_from([
            "hooklistener",
            "cases",
            "run",
            "ep_123",
            "--target-id",
            "t_1",
        ])
        .unwrap();
        match cli.command {
            Some(Commands::Cases {
                action:
                    CasesAction::Run {
                        target,
                        target_url,
                        target_id,
                        ..
                    },
            }) => {
                assert_eq!(target, None);
                assert_eq!(target_url, None);
                assert_eq!(target_id.as_deref(), Some("t_1"));
            }
            _ => panic!("expected cases run command"),
        }
    }

    #[test]
    fn cases_run_help_shows_only_the_target_flag() {
        let mut cmd = Cli::command();
        let run = cmd
            .find_subcommand_mut("cases")
            .unwrap()
            .find_subcommand_mut("run")
            .unwrap();
        let help = run.render_help().to_string();
        assert!(help.contains("--target <TARGET>"));
        assert!(!help.contains("--target-url"));
        assert!(!help.contains("--target-id"));
        assert!(help.contains("--target-name <NAME>"));
    }

    #[test]
    fn cases_run_parser_rejects_conflicting_target_options() {
        let result = Cli::try_parse_from([
            "hooklistener",
            "cases",
            "run",
            "ep_123",
            "--target",
            "cli",
            "--target-url",
            "http://localhost:3000",
        ]);

        assert!(result.is_err());
    }

    #[test]
    fn command_tables_fit_the_default_terminal_width() {
        let output = render_table(
            &["ID", "Method", "Status", "Webhook URL", "Name"],
            [[
                "endpoint_identifier_123",
                "POST",
                "active",
                "https://example.hooklistener.dev/a/very/long/webhook/path",
                "Production webhook receiver",
            ]],
        );

        assert!(output.lines().all(|line| line.chars().count() <= 80));
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
            timeout: Some(Duration::from_secs(60)),
            timeout_ms: None,
            interval: None,
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
            interval: None,
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
            interval: None,
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
            interval: None,
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
            interval: None,
            interval_ms: None,
        })
        .unwrap_err();
        assert!(err.to_string().contains("Target is required"));
    }

    #[test]
    fn parse_duration_accepts_bare_seconds_and_units() {
        assert_eq!(parse_duration("60").unwrap(), Duration::from_secs(60));
        assert_eq!(parse_duration("60s").unwrap(), Duration::from_secs(60));
        assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
        assert_eq!(
            parse_duration("1500ms").unwrap(),
            Duration::from_millis(1_500)
        );
        assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3_600));
        assert_eq!(parse_duration("7d").unwrap(), Duration::from_secs(604_800));
        assert_eq!(
            parse_duration(" 2 Hours ").unwrap(),
            Duration::from_secs(7_200)
        );
    }

    #[test]
    fn parse_duration_reports_errors_in_cli_voice() {
        assert_eq!(parse_duration("").unwrap_err(), "Duration cannot be empty.");
        assert_eq!(
            parse_duration("abc").unwrap_err(),
            "Duration must start with a number."
        );
        assert_eq!(
            parse_duration("5w").unwrap_err(),
            "Invalid duration unit 'w'. Use ms, s, m, h, or d."
        );
        assert_eq!(
            parse_duration("99999999999999999999").unwrap_err(),
            "Duration is too large."
        );
        assert_eq!(
            parse_duration("9999999999999999d").unwrap_err(),
            "Duration is too large."
        );
    }

    #[test]
    fn format_duration_uses_the_largest_exact_unit() {
        assert_eq!(format_duration(Duration::from_millis(1_500)), "1500ms");
        assert_eq!(format_duration(Duration::from_secs(1)), "1s");
        assert_eq!(format_duration(Duration::from_secs(90)), "90s");
        assert_eq!(format_duration(Duration::from_secs(1_800)), "30m");
        assert_eq!(format_duration(Duration::from_secs(86_400)), "1d");
        assert_eq!(format_duration(Duration::from_secs(90_000)), "25h");
    }

    #[test]
    fn parse_whole_hours_rejects_partial_hours_and_zero() {
        assert_eq!(
            parse_whole_hours("24h").unwrap(),
            Duration::from_secs(86_400)
        );
        assert_eq!(
            parse_whole_hours("86400").unwrap(),
            Duration::from_secs(86_400)
        );
        assert_eq!(
            parse_whole_hours("7d").unwrap(),
            Duration::from_secs(604_800)
        );
        for raw in ["90m", "0", "3601s", "500ms"] {
            assert!(
                parse_whole_hours(raw)
                    .unwrap_err()
                    .contains("whole number of hours"),
                "{raw}"
            );
        }
    }

    fn cases_run_args(cli: Cli) -> (Option<Duration>, Option<u64>, Option<Duration>, Option<u64>) {
        match cli.command {
            Some(Commands::Cases {
                action:
                    CasesAction::Run {
                        timeout,
                        timeout_ms,
                        interval,
                        interval_ms,
                        ..
                    },
            }) => (timeout, timeout_ms, interval, interval_ms),
            _ => panic!("expected cases run command"),
        }
    }

    #[test]
    fn cases_run_duration_flags_replace_millisecond_flags() {
        let cli = Cli::try_parse_from([
            "hooklistener",
            "cases",
            "run",
            "ep_1",
            "--target",
            "cli",
            "--timeout",
            "2m",
            "--interval",
            "500ms",
        ])
        .unwrap();
        assert_eq!(
            cases_run_args(cli),
            (
                Some(Duration::from_secs(120)),
                None,
                Some(Duration::from_millis(500)),
                None
            )
        );

        let cli = Cli::try_parse_from([
            "hooklistener",
            "cases",
            "run",
            "ep_1",
            "--target",
            "cli",
            "--timeout",
            "60",
        ])
        .unwrap();
        assert_eq!(cases_run_args(cli).0, Some(Duration::from_secs(60)));

        let cli = Cli::try_parse_from([
            "hooklistener",
            "cases",
            "run",
            "ep_1",
            "--target",
            "cli",
            "--timeout-ms",
            "500",
            "--interval-ms",
            "250",
        ])
        .unwrap();
        assert_eq!(cases_run_args(cli), (None, Some(500), None, Some(250)));

        assert_eq!(
            parse_error_kind([
                "hooklistener",
                "cases",
                "run",
                "ep_1",
                "--target",
                "cli",
                "--timeout",
                "1s",
                "--timeout-ms",
                "5",
            ]),
            clap::error::ErrorKind::ArgumentConflict
        );
        assert_eq!(
            parse_error_kind([
                "hooklistener",
                "cases",
                "run",
                "ep_1",
                "--target",
                "cli",
                "--interval",
                "1s",
                "--interval-ms",
                "5",
            ]),
            clap::error::ErrorKind::ArgumentConflict
        );
    }

    #[test]
    fn cases_run_params_convert_durations_to_milliseconds() {
        let params = build_case_run_params(CaseRunInput {
            target: Some("cli".to_string()),
            target_url: None,
            target_id: None,
            target_name: None,
            wait: true,
            timeout: Some(Duration::from_secs(120)),
            timeout_ms: None,
            interval: Some(Duration::from_millis(500)),
            interval_ms: None,
        })
        .unwrap();
        assert_eq!(params.timeout_ms, Some(120_000));
        assert_eq!(params.interval_ms, Some(500));
    }

    #[test]
    fn hidden_millisecond_flags_win_over_defaulted_duration_flags() {
        assert_eq!(
            resolve_millis_flag(
                Some(Duration::from_secs(1)),
                Some(250),
                "interval-ms",
                "interval"
            ),
            Some(250)
        );
        assert_eq!(
            resolve_millis_flag(
                Some(Duration::from_secs(2)),
                None,
                "interval-ms",
                "interval"
            ),
            Some(2_000)
        );
        assert_eq!(
            resolve_millis_flag(None, None, "timeout-ms", "timeout"),
            None
        );
    }

    #[test]
    fn tunnel_events_interval_accepts_durations_and_hidden_milliseconds() {
        fn interval_args(cli: Cli) -> (Duration, Option<u64>) {
            match cli.command {
                Some(Commands::Tunnel {
                    action:
                        Some(TunnelAction::Events {
                            interval,
                            interval_ms,
                            ..
                        }),
                    ..
                }) => (interval, interval_ms),
                _ => panic!("expected tunnel events command"),
            }
        }

        let cli = Cli::try_parse_from(["hooklistener", "tunnel", "events"]).unwrap();
        assert_eq!(interval_args(cli), (Duration::from_secs(1), None));

        let cli = Cli::try_parse_from([
            "hooklistener",
            "tunnel",
            "events",
            "--follow",
            "--interval",
            "2s",
        ])
        .unwrap();
        assert_eq!(interval_args(cli), (Duration::from_secs(2), None));

        let cli = Cli::try_parse_from(["hooklistener", "tunnel", "events", "--interval-ms", "250"])
            .unwrap();
        let (interval, interval_ms) = interval_args(cli);
        assert_eq!(interval_ms, Some(250));
        assert_eq!(
            resolve_millis_flag(Some(interval), interval_ms, "interval-ms", "interval"),
            Some(250)
        );

        assert_eq!(
            parse_error_kind([
                "hooklistener",
                "tunnel",
                "events",
                "--interval",
                "2s",
                "--interval-ms",
                "1",
            ]),
            clap::error::ErrorKind::ArgumentConflict
        );
    }

    #[test]
    fn anon_ttl_flags_accept_seconds_and_units() {
        fn create_ttl(args: &[&str]) -> Duration {
            match Cli::try_parse_from(args).unwrap().command {
                Some(Commands::Anon {
                    action: AnonAction::Create { ttl },
                }) => ttl,
                _ => panic!("expected anon create command"),
            }
        }
        fn tunnel_ttl(args: &[&str]) -> Duration {
            match Cli::try_parse_from(args).unwrap().command {
                Some(Commands::Anon {
                    action: AnonAction::Tunnel { ttl, .. },
                }) => ttl,
                _ => panic!("expected anon tunnel command"),
            }
        }

        assert_eq!(
            create_ttl(&["hooklistener", "anon", "create"]),
            Duration::from_secs(86_400)
        );
        assert_eq!(
            create_ttl(&["hooklistener", "anon", "create", "--ttl", "3600"]),
            Duration::from_secs(3_600)
        );
        assert_eq!(
            create_ttl(&["hooklistener", "anon", "create", "--ttl", "7d"]),
            Duration::from_secs(604_800)
        );

        assert_eq!(
            tunnel_ttl(&["hooklistener", "anon", "tunnel"]),
            Duration::from_secs(900)
        );
        assert_eq!(
            tunnel_ttl(&["hooklistener", "anon", "tunnel", "--ttl", "900"]),
            Duration::from_secs(900)
        );
        assert_eq!(
            tunnel_ttl(&["hooklistener", "anon", "tunnel", "--ttl", "10m"]),
            Duration::from_secs(600)
        );

        let error = parse_error(["hooklistener", "anon", "tunnel", "--ttl", "31m"]);
        assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
        assert!(error.to_string().contains("between 1m and 30m"), "{error}");
    }

    #[test]
    fn share_create_expires_in_accepts_whole_hours_and_hidden_hours_flag() {
        fn expiry(args: &[&str]) -> (Option<Duration>, Option<u64>) {
            match Cli::try_parse_from(args).unwrap().command {
                Some(Commands::Share {
                    action:
                        ShareAction::Create {
                            expires_in,
                            expires_in_hours,
                            ..
                        },
                }) => (expires_in, expires_in_hours),
                _ => panic!("expected share create command"),
            }
        }

        assert_eq!(
            expiry(&["hooklistener", "share", "create", "req_1"]),
            (None, None)
        );
        assert_eq!(
            expiry(&[
                "hooklistener",
                "share",
                "create",
                "req_1",
                "--expires-in",
                "24h"
            ]),
            (Some(Duration::from_secs(86_400)), None)
        );
        assert_eq!(
            expiry(&[
                "hooklistener",
                "share",
                "create",
                "req_1",
                "--expires-in-hours",
                "24"
            ]),
            (None, Some(24))
        );
        assert_eq!(duration_hours(Duration::from_secs(604_800)), 168);

        let error = parse_error([
            "hooklistener",
            "share",
            "create",
            "req_1",
            "--expires-in",
            "90m",
        ]);
        assert!(
            error.to_string().contains("whole number of hours"),
            "{error}"
        );

        assert_eq!(
            parse_error_kind([
                "hooklistener",
                "share",
                "create",
                "req_1",
                "--expires-in",
                "24h",
                "--expires-in-hours",
                "24",
            ]),
            clap::error::ErrorKind::ArgumentConflict
        );
    }

    #[test]
    fn monitor_interval_is_a_closed_set_of_minutes() {
        fn create_interval(raw: &str) -> MonitorInterval {
            match Cli::try_parse_from([
                "hooklistener",
                "monitor",
                "create",
                "API",
                "https://example.com/health",
                "--interval",
                raw,
            ])
            .unwrap()
            .command
            {
                Some(Commands::Monitor {
                    action: MonitorAction::Create { interval, .. },
                }) => interval,
                _ => panic!("expected monitor create command"),
            }
        }

        assert_eq!(create_interval("5"), MonitorInterval::M5);
        assert_eq!(create_interval("1h"), MonitorInterval::M60);
        assert_eq!(create_interval("60M"), MonitorInterval::M60);
        assert_eq!(create_interval("10m"), MonitorInterval::M10);
        assert_eq!(MonitorInterval::M30.minutes(), 30);

        match Cli::try_parse_from([
            "hooklistener",
            "monitor",
            "create",
            "API",
            "https://example.com/health",
        ])
        .unwrap()
        .command
        {
            Some(Commands::Monitor {
                action: MonitorAction::Create { interval, .. },
            }) => assert_eq!(interval, MonitorInterval::M5),
            _ => panic!("expected monitor create command"),
        }

        match Cli::try_parse_from([
            "hooklistener",
            "monitor",
            "update",
            "mon_1",
            "--interval",
            "10m",
        ])
        .unwrap()
        .command
        {
            Some(Commands::Monitor {
                action: MonitorAction::Update { interval, .. },
            }) => assert_eq!(interval, Some(MonitorInterval::M10)),
            _ => panic!("expected monitor update command"),
        }

        for raw in ["7", "2h", "90s"] {
            assert_eq!(
                parse_error_kind([
                    "hooklistener",
                    "monitor",
                    "create",
                    "API",
                    "https://example.com/health",
                    "--interval",
                    raw,
                ]),
                clap::error::ErrorKind::InvalidValue,
                "{raw}"
            );
        }
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
                assert_eq!(timeout, Some(Duration::from_secs(60)));
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
                render_empty_status(
                    "NO DEBUG ENDPOINTS FOUND",
                    "Run `hooklistener endpoint create <name>` to create one.",
                ),
            ),
            (
                "request list",
                render_empty_status(
                    "NO REQUESTS FOUND",
                    "Send a webhook, then run `hooklistener endpoint list-requests <endpoint-id>` again.",
                ),
            ),
            (
                "forward list",
                render_empty_status(
                    "NO FORWARDS FOUND",
                    "Run `hooklistener endpoint forward-request <endpoint-id> <request-id> <target-url>`.",
                ),
            ),
            (
                "share list",
                render_empty_status(
                    "NO SHARES FOUND",
                    "Run `hooklistener share create <request-id>` to create one.",
                ),
            ),
            (
                "monitor list",
                render_empty_status(
                    "NO UPTIME MONITORS FOUND",
                    "Run `hooklistener monitor create <name> <url>` to create one.",
                ),
            ),
            (
                "monitor checks",
                render_empty_status(
                    "NO CHECKS RECORDED",
                    "Wait for the first interval, then run `hooklistener monitor checks <monitor-id>`.",
                ),
            ),
            (
                "static tunnel list",
                render_empty_status(
                    "NO STATIC TUNNELS FOUND",
                    "Run `hooklistener static-tunnel create <slug>` to reserve one.",
                ),
            ),
            (
                "anon events",
                render_empty_status(
                    "NO EVENTS CAPTURED",
                    "Send a webhook, then run `hooklistener anon list-events <endpoint-id> --token <token>`.",
                ),
            ),
        ]);

        assert!(output.lines().all(|line| line.chars().count() <= 80));
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
            "hooklistener endpoint show-forward fwd_123"
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
            "wss://api.example.dev/socket/websocket",
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
        headers.insert("authorization".to_string(), "Bearer secret".to_string());
        let event = TunnelEvent::RequestReceived {
            request_id: "req_123".to_string(),
            method: "POST".to_string(),
            path: "webhooks/github".to_string(),
            headers,
            body: Some("{\"ok\":true}".to_string()),
            query_string: "delivery=abc".to_string(),
            replay: false,
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
        assert_eq!(receipt["headers"]["authorization"], "[REDACTED]");
        assert!(!receipt.to_string().contains("Bearer secret"));
    }

    #[test]
    fn tunnel_request_event_redacts_sensitive_headers() {
        let headers = std::collections::HashMap::from([
            (
                "authorization".to_string(),
                "Bearer request-secret".to_string(),
            ),
            ("cookie".to_string(), "session=request-secret".to_string()),
            ("x-api-key".to_string(), "request-api-key".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ]);
        let event = TunnelEvent::RequestReceived {
            request_id: "req_secure".to_string(),
            method: "POST".to_string(),
            path: "/webhook".to_string(),
            headers,
            body: None,
            query_string: String::new(),
            replay: false,
        };

        let receipt = tunnel_event_receipt(&event, "127.0.0.1", 8080, None, None);

        assert_eq!(receipt["headers"]["authorization"], "[REDACTED]");
        assert_eq!(receipt["headers"]["cookie"], "[REDACTED]");
        assert_eq!(receipt["headers"]["x-api-key"], "[REDACTED]");
        assert_eq!(receipt["headers"]["content-type"], "application/json");
        let output = receipt.to_string();
        assert!(!output.contains("request-secret"));
        assert!(!output.contains("request-api-key"));
    }

    #[test]
    fn tunnel_response_event_redacts_sensitive_headers() {
        let event = TunnelEvent::RequestForwarded {
            request_id: "req_secure".to_string(),
            status: 200,
            duration_ms: 10,
            response_headers: std::collections::HashMap::from([
                (
                    "set-cookie".to_string(),
                    "session=response-secret".to_string(),
                ),
                ("x-auth-token".to_string(), "response-token".to_string()),
                ("content-type".to_string(), "application/json".to_string()),
            ]),
            response_body: None,
        };

        let receipt = tunnel_event_receipt(&event, "127.0.0.1", 8080, None, None);

        assert_eq!(receipt["response_headers"]["set-cookie"], "[REDACTED]");
        assert_eq!(receipt["response_headers"]["x-auth-token"], "[REDACTED]");
        assert_eq!(
            receipt["response_headers"]["content-type"],
            "application/json"
        );
        let output = receipt.to_string();
        assert!(!output.contains("response-secret"));
        assert!(!output.contains("response-token"));
    }

    #[test]
    fn buffered_summary_event_receipt_includes_count_and_oldest() {
        let event = TunnelEvent::BufferedSummary {
            count: 3,
            oldest_captured_at: Some("2026-07-14T09:00:00Z".to_string()),
        };

        let receipt = tunnel_event_receipt(&event, "localhost", 3000, None, None);

        assert_eq!(receipt["event"], "buffered_summary");
        assert_eq!(receipt["count"], 3);
        assert_eq!(receipt["oldest_captured_at"], "2026-07-14T09:00:00Z");
    }

    #[test]
    fn buffered_replayed_event_receipt_includes_status() {
        let event = TunnelEvent::BufferedReplayed {
            capture_id: "cap_123".to_string(),
            status: 204,
        };

        let receipt = tunnel_event_receipt(&event, "localhost", 3000, None, None);

        assert_eq!(receipt["event"], "buffered_replayed");
        assert_eq!(receipt["capture_id"], "cap_123");
        assert_eq!(receipt["status_code"], 204);
        assert_eq!(receipt["resource_uri"], "hooklistener://requests/cap_123");
    }

    #[test]
    fn buffered_replay_failed_event_receipt_includes_reason() {
        let event = TunnelEvent::BufferedReplayFailed {
            capture_id: "cap_123".to_string(),
            reason: "local_unreachable".to_string(),
        };

        let receipt = tunnel_event_receipt(&event, "localhost", 3000, None, None);

        assert_eq!(receipt["event"], "buffered_replay_failed");
        assert_eq!(receipt["capture_id"], "cap_123");
        assert_eq!(receipt["error"], "local_unreachable");
    }

    #[test]
    fn tunnel_request_event_marks_replay() {
        let event = TunnelEvent::RequestReceived {
            request_id: "cap_456".to_string(),
            method: "POST".to_string(),
            path: "/webhook".to_string(),
            headers: std::collections::HashMap::new(),
            body: None,
            query_string: String::new(),
            replay: true,
        };

        let receipt = tunnel_event_receipt(&event, "localhost", 3000, None, None);

        assert_eq!(receipt["event"], "request_received");
        assert_eq!(receipt["replay"], true);
    }

    #[test]
    fn tunnel_stream_gap_is_recoverable_and_does_not_affect_delivery() {
        let event = TunnelEvent::StreamGap { dropped_events: 7 };
        let receipt = tunnel_event_receipt(&event, "127.0.0.1", 8080, None, None);

        assert_eq!(receipt["event"], "stream_gap");
        assert_eq!(receipt["status"], "recoverable");
        assert_eq!(receipt["dropped_events"], 7);
        assert_eq!(receipt["delivery_affected"], false);
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
}

#[cfg(test)]
mod audit_findings {
    //! Regression tests for the 2026-09 security audit findings.
    //! Run with: cargo test audit_findings
    use super::*;
    use chrono::Duration as ChronoDuration;

    fn mock_refresh(server: &mut mockito::ServerGuard, body: &str) -> mockito::Mock {
        server
            .mock("POST", "/api/v1/auth/refresh")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create()
    }

    fn config_with_refresh_token() -> config::Config {
        config::Config {
            access_token: Some("old-access".to_string()),
            token_expires_at: Some(Utc::now() - ChronoDuration::minutes(1)),
            refresh_token: Some("refresh-1".to_string()),
            refresh_token_expires_at: Some(Utc::now() + ChronoDuration::days(1)),
            selected_organization_id: Some("org-a".to_string()),
            ..config::Config::default()
        }
    }

    /// Regression test for finding 3: the background refresh loop (and
    /// `ensure_valid_token`) used to rebuild the whole config from the copy
    /// loaded at process start and write it back, silently reverting anything
    /// another process saved in the meantime (`org use`, `login --force`,
    /// `config set`). The refresh must now only touch the fields it owns.
    #[tokio::test]
    async fn token_refresh_preserves_config_changes_made_by_other_processes() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.json");
        let mut server = mockito::Server::new_async().await;
        let _mock = mock_refresh(
            &mut server,
            r#"{"access_token":"new-access","expires_in":3600}"#,
        );

        // Long-running process loads its config at startup.
        let mut in_memory = config_with_refresh_token();
        in_memory.save_to(&path).unwrap();

        // Meanwhile another process changes the selected organization and
        // rotates the credentials via `hooklistener login --force`.
        let mut other_process = config::Config::load_from(&path).unwrap();
        other_process.selected_organization_id = Some("org-b".to_string());
        other_process.set_tokens(
            "relogin-access".to_string(),
            Utc::now() + ChronoDuration::hours(1),
            Some("refresh-2".to_string()),
            Some(Utc::now() + ChronoDuration::days(30)),
        );
        other_process.save_to(&path).unwrap();

        refresh_access_token_from_config_with(&mut in_memory, &server.url(), Some(&path))
            .await
            .unwrap();

        let on_disk = config::Config::load_from(&path).unwrap();
        assert_eq!(on_disk.selected_organization_id.as_deref(), Some("org-b"));
        assert_eq!(on_disk.refresh_token.as_deref(), Some("refresh-2"));
    }

    /// Regression test for the login-path instance of finding 3:
    /// `run_login_flow` used to save the config copy it loaded before the
    /// minutes-long device-authorization wait, reverting anything another
    /// process (`org use`, `config set`, an update check) saved meanwhile.
    /// The login must now write only the token fields on top of what is on
    /// disk.
    #[test]
    fn login_preserves_config_changes_made_by_other_processes() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.json");

        // `hooklistener login` loads its config before waiting on the browser.
        let loaded = config::Config {
            selected_organization_id: Some("org-a".to_string()),
            ..config::Config::default()
        };
        loaded.save_to(&path).unwrap();

        // Meanwhile another process switches organizations and records an
        // update check.
        let mut other_process = config::Config::load_from(&path).unwrap();
        other_process.selected_organization_id = Some("org-b".to_string());
        other_process.latest_known_version = Some("9.9.9".to_string());
        other_process.save_to(&path).unwrap();

        save_login_tokens(
            &loaded,
            "new-access".to_string(),
            Utc::now() + ChronoDuration::hours(1),
            Some("new-refresh".to_string()),
            Some(Utc::now() + ChronoDuration::days(30)),
            Some(&path),
        )
        .unwrap();

        let on_disk = config::Config::load_from(&path).unwrap();
        assert_eq!(on_disk.selected_organization_id.as_deref(), Some("org-b"));
        assert_eq!(on_disk.latest_known_version.as_deref(), Some("9.9.9"));
        assert_eq!(on_disk.access_token.as_deref(), Some("new-access"));
        assert_eq!(on_disk.refresh_token.as_deref(), Some("new-refresh"));
    }

    #[test]
    fn login_saves_tokens_when_config_file_was_removed() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.json");
        let loaded = config::Config {
            selected_organization_id: Some("org-a".to_string()),
            ..config::Config::default()
        };

        save_login_tokens(
            &loaded,
            "new-access".to_string(),
            Utc::now() + ChronoDuration::hours(1),
            None,
            None,
            Some(&path),
        )
        .unwrap();

        let on_disk = config::Config::load_from(&path).unwrap();
        assert_eq!(on_disk.selected_organization_id.as_deref(), Some("org-a"));
        assert_eq!(on_disk.access_token.as_deref(), Some("new-access"));
    }

    /// Regression test for finding 7 (second site):
    /// `refresh_access_token_from_config_with` used to cast the
    /// server-supplied `expires_in` with `as i64` and add it to now, which
    /// panicked for large values. It must now fail with a clean error.
    #[tokio::test]
    async fn token_refresh_survives_absurd_expires_in() {
        let dir = tempfile::TempDir::new().unwrap();
        let path = dir.path().join("config.json");
        let mut server = mockito::Server::new_async().await;
        let _mock = mock_refresh(
            &mut server,
            r#"{"access_token":"new-access","expires_in":9000000000000000000}"#,
        );
        let mut in_memory = config_with_refresh_token();
        let base_url = server.url();

        let result = tokio::task::spawn(async move {
            refresh_access_token_from_config_with(&mut in_memory, &base_url, Some(&path)).await
        })
        .await
        .expect("token refresh must not panic");

        let _ = result;
    }
}
