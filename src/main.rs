mod api;
mod app;
mod auth;
mod config;
mod errors;
mod logger;
mod models;
mod syntax;
mod tunnel;
mod ui;

use anyhow::{Result, anyhow};
use chrono::{Duration as ChronoDuration, Utc};
use clap::{CommandFactory, Parser, Subcommand, ValueEnum};
use crossterm::{
    cursor::Show,
    event::{self, Event, KeyEventKind},
    execute,
    terminal::{EnterAlternateScreen, LeaveAlternateScreen, disable_raw_mode, enable_raw_mode},
};
use ratatui::{Terminal, backend::CrosstermBackend};
use std::io;
use std::path::PathBuf;
use std::time::Duration;
use tokio::{sync::mpsc, time::sleep};
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

    /// Output non-interactive command responses as JSON
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
    /// Static tunnel slug management
    StaticTunnel {
        #[command(subcommand)]
        action: StaticTunnelAction,
    },
    /// Generate shell completion scripts
    Completions {
        /// Target shell
        #[arg(value_enum)]
        shell: CompletionShell,
    },
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

fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
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

            // Load config for auth token
            let config = config::Config::load()?;

            // Check if authenticated
            if !config.is_token_valid() {
                eprintln!(
                    "❌ Not authenticated. Please run 'hooklistener login' to authenticate first."
                );
                std::process::exit(1);
            }

            let access_token = config
                .access_token
                .ok_or_else(|| anyhow::anyhow!("No access token found"))?;

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
                access_token,
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

            let res = run_app(&mut terminal, &mut app, event_rx, None).await;

            restore_terminal(&mut terminal)?;

            if let Err(err) = res {
                error!(error = %err, "Application terminated with error");
                display_error(&err);
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
                println!("Diagnostic bundle created in: {}", output.display());
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
                println!(
                    "Removed {} old log file(s) from {} (keeping {} most recent)",
                    removed,
                    directory.display(),
                    keep
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
                    println!("Config file: {}", config_path.display());
                    println!();
                    match &config.access_token {
                        Some(token) => {
                            let truncated = if token.len() > 8 {
                                format!("{}...", &token[..8])
                            } else {
                                token.clone()
                            };
                            let status = if config.is_token_valid() {
                                "valid"
                            } else {
                                "expired"
                            };
                            println!("Token: {} ({})", truncated, status);
                        }
                        None => println!("Token: (none)"),
                    }
                    match &config.selected_organization_id {
                        Some(org_id) => println!("Organization: {}", org_id),
                        None => println!("Organization: (none)"),
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
                            println!("Cleared selected_organization_id");
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
                            println!(
                                "Set selected_organization_id to {}",
                                config.selected_organization_id.as_deref().unwrap()
                            );
                        }
                    }
                }
                _ => {
                    eprintln!(
                        "Unknown config key: {}. Available keys: selected_organization_id",
                        key
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
                    println!("Already logged out.");
                }
            } else {
                config.clear_token();
                config.save()?;
                if json {
                    print_json(&serde_json::json!({
                        "status": "logged_out"
                    }))?;
                } else {
                    println!("Logged out successfully.");
                }
            }
        }
        Commands::Org { action } => match action {
            OrgAction::List => {
                let config = config::Config::load()?;
                let token = access_token_from_config(&config)?;
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
                let token = access_token_from_config(&config)?;
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
                    println!("Selected organization: {} ({})", organization_name, id);
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
                    println!("Cleared selected organization.");
                }
            }
        },
        Commands::Endpoint { action } => match action {
            EndpointAction::Create { name, slug, org } => {
                let config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = access_token_from_config(&config)?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let endpoint = client.create_endpoint(&name, slug.as_deref()).await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "endpoint": endpoint
                    }))?;
                } else {
                    println!("Organization: {}", organization_id);
                    println!("Created endpoint:");
                    print_endpoints(std::slice::from_ref(&endpoint));
                }
            }
            EndpointAction::List { org } => {
                let config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = access_token_from_config(&config)?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let endpoints = client.list_endpoints().await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "endpoints": endpoints
                    }))?;
                } else {
                    println!("Organization: {}", organization_id);
                    print_endpoints(&endpoints);
                }
            }
            EndpointAction::Show { endpoint_id, org } => {
                let config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = access_token_from_config(&config)?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let endpoint = client.get_endpoint(&endpoint_id).await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "endpoint": endpoint
                    }))?;
                } else {
                    println!("Organization: {}", organization_id);
                    print_endpoints(std::slice::from_ref(&endpoint));
                }
            }
            EndpointAction::Delete { endpoint_id, org } => {
                let config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = access_token_from_config(&config)?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                client.delete_endpoint(&endpoint_id).await?;
                if json {
                    print_json(&serde_json::json!({
                        "status": "deleted",
                        "organization_id": organization_id,
                        "endpoint_id": endpoint_id
                    }))?;
                } else {
                    println!(
                        "Deleted endpoint: {} (organization {})",
                        endpoint_id, organization_id
                    );
                }
            }
            EndpointAction::Requests {
                endpoint_id,
                page,
                page_size,
                org,
            } => {
                let config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = access_token_from_config(&config)?;
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
                    println!("Organization: {}", organization_id);
                    println!("Endpoint: {}", endpoint_id);
                    print_endpoint_requests(&requests);
                }
            }
            EndpointAction::Request {
                endpoint_id,
                request_id,
                org,
            } => {
                let config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = access_token_from_config(&config)?;
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
                    println!("Organization: {}", organization_id);
                    println!("Endpoint: {}", endpoint_id);
                    print_endpoint_request_detail(&request);
                }
            }
            EndpointAction::DeleteRequest {
                endpoint_id,
                request_id,
                org,
            } => {
                let config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = access_token_from_config(&config)?;
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
                    println!(
                        "Deleted request: {} (endpoint {}, organization {})",
                        request_id, endpoint_id, organization_id
                    );
                }
            }
            EndpointAction::ForwardRequest {
                endpoint_id,
                request_id,
                target_url,
                method,
                org,
            } => {
                let config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = access_token_from_config(&config)?;
                let normalized_method = normalize_http_method(method)?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let response = client
                    .forward_endpoint_request(
                        &endpoint_id,
                        &request_id,
                        &target_url,
                        normalized_method.as_deref(),
                    )
                    .await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "endpoint_id": endpoint_id,
                        "request_id": request_id,
                        "forward": response
                    }))?;
                } else {
                    println!("Organization: {}", organization_id);
                    println!("Endpoint: {}", endpoint_id);
                    println!("Request: {}", request_id);
                    println!(
                        "Forward accepted: {} (status: {}, target: {})",
                        response.forward_id, response.status, response.target_url
                    );
                }
            }
            EndpointAction::Forwards {
                endpoint_id,
                request_id,
                page,
                page_size,
                org,
            } => {
                let config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = access_token_from_config(&config)?;
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
                    println!("Organization: {}", organization_id);
                    println!("Endpoint: {}", endpoint_id);
                    println!("Request: {}", request_id);
                    print_endpoint_request_forwards(&forwards);
                }
            }
            EndpointAction::Forward { forward_id, org } => {
                let config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = access_token_from_config(&config)?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let forward = client.get_forward(&forward_id).await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "forward": forward
                    }))?;
                } else {
                    println!("Organization: {}", organization_id);
                    print_forward_detail(&forward);
                }
            }
        },
        Commands::StaticTunnel { action } => match action {
            StaticTunnelAction::List { org } => {
                let config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = access_token_from_config(&config)?;
                let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
                let tunnels = client.list_static_tunnels(&organization_id).await?;
                if json {
                    print_json(&serde_json::json!({
                        "organization_id": organization_id,
                        "tunnels": tunnels
                    }))?;
                } else {
                    println!("Organization: {}", organization_id);
                    print_static_tunnels(&tunnels);
                }
            }
            StaticTunnelAction::Create { slug, name, org } => {
                let config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = access_token_from_config(&config)?;
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
                    println!(
                        "Created static tunnel: {} ({})",
                        created.static_tunnel.slug, created.static_tunnel.id
                    );
                    if let Some(message) = created.message {
                        println!("{}", message);
                    }
                }
            }
            StaticTunnelAction::Delete { slug_id, org } => {
                let config = config::Config::load()?;
                let organization_id = require_organization(org, &config)?;
                let token = access_token_from_config(&config)?;
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
                    println!(
                        "{}",
                        response
                            .message
                            .unwrap_or_else(|| "Static tunnel deleted.".to_string())
                    );
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

            // Load config for auth token
            let config = config::Config::load()?;

            // Check if authenticated
            if !config.is_token_valid() {
                eprintln!(
                    "❌ Not authenticated. Please run 'hooklistener login' to authenticate first."
                );
                std::process::exit(1);
            }

            let selected_org = resolve_tunnel_org(org, &config);

            let access_token = config
                .access_token
                .ok_or_else(|| anyhow::anyhow!("No access token found"))?;

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
                access_token,
                host,
                port,
                selected_org,
                slug,
                event_tx,
            );

            let res = run_app(&mut terminal, &mut app, event_rx, Some(reconnect_tx)).await;

            restore_terminal(&mut terminal)?;

            if let Err(err) = res {
                error!(error = %err, "Application terminated with error");
                display_error(&err);
            }
        }
    }

    Ok(())
}

async fn run_login_flow(force_reauth: bool) -> Result<()> {
    let mut config = config::Config::load()?;

    if config.is_token_valid() && !force_reauth {
        println!("✅ You're already authenticated.");
        println!("Run `hooklistener listen <endpoint>` to start forwarding webhooks.");
        println!("Use `hooklistener login --force` if you need to re-authenticate.");
        return Ok(());
    }

    if force_reauth {
        config.clear_token();
        config.save()?;
    }

    let base_url = std::env::var("HOOKLISTENER_API_URL")
        .unwrap_or_else(|_| "https://app.hooklistener.com".to_string());
    let mut device_flow = auth::DeviceCodeFlow::new(base_url);

    let user_code = device_flow.initiate_device_flow().await?;
    let display_code = device_flow
        .format_user_code()
        .unwrap_or_else(|| user_code.clone());
    let portal_url = device_portal_url();

    println!("🔐 Hooklistener Login");
    println!("Visit {} and enter the code {}", portal_url, display_code);
    println!("Waiting for you to approve the device...");

    loop {
        match device_flow.poll_for_authorization().await {
            Ok(Some(access_token)) => {
                let expires_at = Utc::now() + ChronoDuration::days(SESSION_TOKEN_VALIDITY_DAYS);
                config.set_access_token(access_token, expires_at);
                config.save()?;
                println!("✅ Authentication successful!");
                println!("Run `hooklistener listen <endpoint>` to forward webhooks.");
                break;
            }
            Ok(None) => {
                if let Some(remaining) = device_flow.time_remaining() {
                    let minutes = remaining.num_minutes();
                    let seconds = remaining.num_seconds() % 60;
                    if minutes > 0 {
                        println!(
                            "Still waiting for confirmation... code expires in {}m {}s",
                            minutes, seconds
                        );
                    } else {
                        println!(
                            "Still waiting for confirmation... code expires in {}s",
                            seconds
                        );
                    }

                    if remaining == ChronoDuration::zero() {
                        return Err(anyhow!(
                            "Device code expired before authorization completed. Please run `hooklistener login` again."
                        ));
                    }
                } else {
                    println!("Still waiting for confirmation...");
                }

                sleep(Duration::from_secs(5)).await;
            }
            Err(err) => {
                return Err(anyhow!("Authentication failed: {}", err));
            }
        }
    }

    Ok(())
}

fn device_portal_url() -> String {
    std::env::var("HOOKLISTENER_DEVICE_PORTAL_URL")
        .unwrap_or_else(|_| "https://app.hooklistener.com/device-codes".to_string())
}

fn resolve_tunnel_org(cli_org: Option<String>, config: &config::Config) -> Option<String> {
    cli_org.or_else(|| config.selected_organization_id.clone())
}

fn access_token_from_config(config: &config::Config) -> Result<String> {
    if !config.is_token_valid() {
        return Err(anyhow!(
            "Not authenticated. Please run `hooklistener login` first."
        ));
    }

    config
        .access_token
        .clone()
        .ok_or_else(|| anyhow!("No access token found. Please run `hooklistener login`."))
}

fn require_organization(cli_org: Option<String>, config: &config::Config) -> Result<String> {
    resolve_tunnel_org(cli_org, config).ok_or_else(|| {
        anyhow!(
            "No organization selected. Use `hooklistener org use <organization-id>` or pass --org."
        )
    })
}

fn print_organizations(organizations: &[api::Organization], selected_org: Option<&str>) {
    if organizations.is_empty() {
        println!("No organizations found.");
        return;
    }

    println!("Organizations:");
    for org in organizations {
        let marker = if selected_org.is_some_and(|id| id == org.id) {
            "*"
        } else {
            " "
        };
        println!("{} {}  {}", marker, org.id, org.name);
    }
}

fn print_endpoints(endpoints: &[api::DebugEndpointSummary]) {
    if endpoints.is_empty() {
        println!("No debug endpoints found.");
        return;
    }

    println!(
        "{:<36}  {:<20}  {:<10}  {:<40}  Name",
        "ID", "Slug", "Status", "Webhook URL"
    );
    for endpoint in endpoints {
        println!(
            "{:<36}  {:<20}  {:<10}  {:<40}  {}",
            endpoint.id, endpoint.slug, endpoint.status, endpoint.webhook_url, endpoint.name
        );
    }
}

fn print_endpoint_requests(response: &api::EndpointRequestsResponse) {
    if response.data.is_empty() {
        println!("No requests found.");
    } else {
        println!("{:<36}  {:<7}  {:<40}  Remote", "ID", "Method", "URL");
        for request in &response.data {
            println!(
                "{:<36}  {:<7}  {:<40}  {}",
                request.id, request.method, request.url, request.remote_addr
            );
        }
    }

    println!(
        "Page {}/{} (page_size={}, total={})",
        response.pagination.page,
        response.pagination.total_pages,
        response.pagination.page_size,
        response.pagination.total_count
    );
}

fn print_endpoint_request_detail(request: &api::DebugRequestDetail) {
    println!("Request ID: {}", request.id);
    println!("Method: {}", request.method);
    if let Some(path) = request.path.as_deref() {
        println!("Path: {}", path);
    }
    println!("URL: {}", request.url);

    if let Some(status_remote) = request.remote_addr.as_deref() {
        println!("Remote Address: {}", status_remote);
    }
    if let Some(content_length) = request.content_length {
        println!("Content Length: {}", content_length);
    }
    if let Some(created_at) = request.created_at.as_deref() {
        println!("Created At: {}", created_at);
    }

    if request.headers.is_empty() {
        println!("Headers: (none)");
    } else {
        println!("Headers:");
        for (key, value) in &request.headers {
            println!("  {}: {}", key, value);
        }
    }

    if request.query_params.is_empty() {
        println!("Query Params: (none)");
    } else {
        println!("Query Params:");
        for (key, value) in &request.query_params {
            println!("  {}={}", key, value);
        }
    }

    match request.body.as_deref().or(request.body_preview.as_deref()) {
        Some(body) if !body.is_empty() => {
            println!("Body:");
            println!("{}", body);
        }
        _ => println!("Body: (empty)"),
    }
}

fn print_endpoint_request_forwards(response: &api::EndpointRequestForwardsResponse) {
    if response.data.is_empty() {
        println!("No forwards found.");
    } else {
        println!(
            "{:<36}  {:<7}  {:<6}  {:<8}  Target",
            "ID", "Method", "Status", "Duration"
        );
        for forward in &response.data {
            let status = forward
                .status_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "-".to_string());
            let duration = forward
                .duration_ms
                .map(|ms| format!("{}ms", ms))
                .unwrap_or_else(|| "-".to_string());
            println!(
                "{:<36}  {:<7}  {:<6}  {:<8}  {}",
                forward.id, forward.method, status, duration, forward.target_url
            );
            if let Some(error) = forward.error_message.as_deref() {
                println!("  error: {}", error);
            }
        }
    }

    println!(
        "Page {}/{} (page_size={}, total={})",
        response.pagination.page,
        response.pagination.total_pages,
        response.pagination.page_size,
        response.pagination.total_count
    );
}

fn print_forward_detail(forward: &api::DebugRequestForwardDetail) {
    println!("Forward ID: {}", forward.id);
    println!("Request ID: {}", forward.debug_request_id);
    println!("Target URL: {}", forward.target_url);
    println!("Method: {}", forward.method);
    if let Some(status_code) = forward.status_code {
        println!("Status: {}", status_code);
    } else {
        println!("Status: (pending)");
    }
    if let Some(duration_ms) = forward.duration_ms {
        println!("Duration: {}ms", duration_ms);
    }
    if let Some(attempted_at) = forward.attempted_at.as_deref() {
        println!("Attempted At: {}", attempted_at);
    }
    if let Some(error_message) = forward.error_message.as_deref() {
        println!("Error: {}", error_message);
    }

    if forward.request_headers.is_empty() {
        println!("Request Headers: (none)");
    } else {
        println!("Request Headers:");
        for (key, value) in &forward.request_headers {
            println!("  {}: {}", key, value);
        }
    }

    if let Some(request_body) = forward.request_body.as_deref()
        && !request_body.is_empty()
    {
        println!("Request Body:");
        println!("{}", request_body);
    }

    if forward.response_headers.is_empty() {
        println!("Response Headers: (none)");
    } else {
        println!("Response Headers:");
        for (key, value) in &forward.response_headers {
            println!("  {}: {}", key, value);
        }
    }

    if let Some(response_body) = forward.response_body.as_deref()
        && !response_body.is_empty()
    {
        println!("Response Body:");
        println!("{}", response_body);
    }
}

fn print_static_tunnels(response: &api::StaticTunnelsResponse) {
    if response.static_tunnels.is_empty() {
        println!("No static tunnels found.");
    } else {
        println!("{:<36}  {:<24}  Name", "ID", "Slug");
        for tunnel in &response.static_tunnels {
            let name = tunnel.name.as_deref().unwrap_or("");
            println!("{:<36}  {:<24}  {}", tunnel.id, tunnel.slug, name);
        }
    }

    println!("Used {}/{} static tunnels", response.used, response.limit);
}

fn spawn_tunnel_forwarder_manager(
    access_token: String,
    host: String,
    port: u16,
    org: Option<String>,
    slug: Option<String>,
    event_tx: mpsc::Sender<TunnelEvent>,
) -> mpsc::UnboundedSender<()> {
    let (reconnect_tx, mut reconnect_rx) = mpsc::unbounded_channel::<()>();

    tokio::spawn(async move {
        let mut worker = tokio::spawn(run_tunnel_forwarder_connection(
            access_token.clone(),
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
                access_token.clone(),
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
    access_token: String,
    host: String,
    port: u16,
    org: Option<String>,
    slug: Option<String>,
    event_tx: mpsc::Sender<TunnelEvent>,
) {
    let tunnel_forwarder =
        tunnel::TunnelForwarder::new(access_token, host, port, org, slug, event_tx);

    if let Err(e) = tunnel_forwarder
        .connect_with_reconnect(tunnel::ReconnectConfig::default())
        .await
    {
        error!("Tunnel forwarder error: {}", e);
    }
}

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    mut tunnel_rx: mpsc::Receiver<TunnelEvent>,
    tunnel_reconnect_tx: Option<mpsc::UnboundedSender<()>>,
) -> Result<()> {
    // Ensure proper terminal cleanup on any exit
    let _cleanup = TerminalCleanup;

    loop {
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
                    };
                    app.tunnel_requests.push_back(tunnel_request);
                    if app.tunnel_requests.len() > app::MAX_TUNNEL_REQUESTS {
                        app.tunnel_requests.pop_front();
                        // Clamp selected index if it now exceeds the new length
                        if !app.tunnel_requests.is_empty() {
                            app.tunnel_selected_index =
                                app.tunnel_selected_index.min(app.tunnel_requests.len() - 1);
                        }
                    }
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
                    app.tunnel_stats.success += 1;
                    app.tunnel_stats.total_duration_ms += duration_ms;
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
                TunnelEvent::ForwardSuccess => {
                    app.listening_stats.successful_forwards += 1;
                }
                TunnelEvent::ForwardError => {
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

            if matches!(app.state, AppState::ForwardingRequest) {
                app.forward_request().await?;
            }
        }
    }

    Ok(())
}

fn display_error(err: &anyhow::Error) {
    eprintln!("Error: {}", err);
    if let Some(api_err) = err.downcast_ref::<errors::ApiError>()
        && let Some(hint) = api_err.hint()
    {
        eprintln!("Hint: {}", hint);
    } else if let Some(tunnel_err) = err.downcast_ref::<errors::TunnelError>()
        && let Some(hint) = tunnel_err.hint()
    {
        eprintln!("Hint: {}", hint);
    } else if let Some(config_err) = err.downcast_ref::<errors::ConfigError>()
        && let Some(hint) = config_err.hint()
    {
        eprintln!("Hint: {}", hint);
    }
    // Print the error chain
    for cause in err.chain().skip(1) {
        eprintln!("Caused by: {}", cause);
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

    fn make_config(selected_org: Option<&str>) -> config::Config {
        config::Config {
            access_token: None,
            token_expires_at: None,
            selected_organization_id: selected_org.map(String::from),
        }
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

    #[test]
    fn access_token_from_config_returns_error_when_invalid() {
        let config = make_config(Some("org-config"));
        let err = access_token_from_config(&config).unwrap_err();
        assert!(
            err.to_string().contains("Not authenticated"),
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
