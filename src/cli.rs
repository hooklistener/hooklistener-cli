//! Top-level clap definitions, shared value enums, and duration parsing.

use clap::{Parser, Subcommand, ValueEnum};
use std::path::PathBuf;
use std::time::Duration;

use crate::commands::anon::AnonAction;
use crate::commands::cases::CasesAction;
use crate::commands::completions::CompletionShell;
use crate::commands::config::ConfigAction;
use crate::commands::endpoint::EndpointAction;
use crate::commands::monitor::MonitorAction;
use crate::commands::org::OrgAction;
use crate::commands::share::ShareAction;
use crate::commands::static_tunnel::StaticTunnelAction;
use crate::commands::tunnel::{TunnelAction, TunnelTargetArgs};
use crate::output::ColorMode;

#[derive(Parser)]
#[command(name = "hooklistener")]
#[command(about = "Inspect webhooks, replay failures, and expose localhost from your terminal")]
#[command(version)]
#[command(
    after_help = "COMMAND GROUPS:\n  Capture and delivery: listen, tunnel, endpoint, static-tunnel, anon\n  Review and automation: cases, share, monitor\n  Account and settings: login, logout, org, config\n  Maintenance: diagnostics, clean-logs, completions, update\n\nCOMMON WORKFLOWS:\n  Sign in:\n    hooklistener login\n\n  Forward an existing debug endpoint:\n    hooklistener listen <endpoint-slug> --target http://localhost:3000\n\n  Expose a local HTTP server:\n    hooklistener tunnel --port 3000\n\n  Create and inspect hosted captures:\n    hooklistener endpoint create <name>\n    hooklistener endpoint list-requests <endpoint-id>"
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: Option<Commands>,

    /// Output supported command responses or event streams as JSON
    #[arg(long, global = true, help_heading = "Global options")]
    pub json: bool,

    /// Styling policy for human output
    #[arg(
        long,
        global = true,
        value_enum,
        default_value_t,
        help_heading = "Global options"
    )]
    pub color: ColorMode,

    /// Confirm destructive commands without an interactive prompt
    #[arg(long, global = true, help_heading = "Global options")]
    pub yes: bool,

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
    pub log_level: LogLevel,

    /// Directory for log files
    #[arg(
        long,
        global = true,
        value_name = "DIR",
        help_heading = "Global options"
    )]
    pub log_dir: Option<PathBuf>,

    /// Also write logs to stdout
    #[arg(long, global = true, help_heading = "Global options")]
    pub log_stdout: bool,

    /// Allow a non-loopback cleartext Hooklistener server (development only)
    #[arg(long, global = true, hide = true)]
    pub allow_insecure_dev_server: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, ValueEnum)]
pub enum LogLevel {
    Trace,
    Debug,
    #[default]
    Info,
    Warn,
    Error,
}

impl LogLevel {
    pub fn as_str(self) -> &'static str {
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
pub enum Commands {
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
    /// Save captured requests as cases, then replay or run them against a target
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

/// HTTP method for `endpoint forward-request` (sent uppercase on the wire).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum HttpMethod {
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
    pub fn as_uppercase(self) -> &'static str {
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

pub const MILLIS_PER_SECOND: u64 = 1_000;

pub const MILLIS_PER_MINUTE: u64 = 60 * MILLIS_PER_SECOND;

pub const MILLIS_PER_HOUR: u64 = 60 * MILLIS_PER_MINUTE;

pub const MILLIS_PER_DAY: u64 = 24 * MILLIS_PER_HOUR;

/// Parses a duration such as `60`, `1500ms`, `2m`, `1h`, or `7d`.
///
/// A bare integer is seconds. Unit names are case-insensitive and may be
/// separated from the number by whitespace.
pub fn parse_duration(raw: &str) -> std::result::Result<Duration, String> {
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
pub fn parse_duration_within(
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
pub fn parse_whole_hours(raw: &str) -> std::result::Result<Duration, String> {
    let duration = parse_duration(raw)?;
    let millis = duration_millis(duration);
    if millis == 0 || !millis.is_multiple_of(MILLIS_PER_HOUR) {
        return Err("must be a whole number of hours, such as 24h, 86400, or 7d".to_string());
    }
    Ok(duration)
}

pub fn duration_millis(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

pub fn duration_hours(duration: Duration) -> u64 {
    duration.as_secs() / (MILLIS_PER_HOUR / MILLIS_PER_SECOND)
}

/// Renders a duration in the largest unit that expresses it exactly.
pub fn format_duration(duration: Duration) -> String {
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
pub fn warn_deprecated_flag(old: &str, new: &str, example: &str) {
    eprintln!("warning: --{old} is deprecated; use --{new} {example}");
}

/// Resolves a canonical duration flag against its hidden `--<flag>-ms` twin.
///
/// The hidden flag wins when given (clap already rejects supplying both
/// explicitly; a defaulted canonical flag must not mask it) and prints a
/// deprecation notice.
pub fn resolve_millis_flag(
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
