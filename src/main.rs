mod api;
mod app;
mod auth;
mod config;
mod logger;
mod models;
mod syntax;
mod tunnel;
mod ui;

use anyhow::{Result, anyhow};
use chrono::{Duration as ChronoDuration, Utc};
use clap::{CommandFactory, Parser, Subcommand};
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
use tracing::{debug, error, info};

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
    /// Launch the interactive TUI to browse and replay requests
    #[command(alias = "ui")]
    Tui,
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

#[tokio::main]
async fn main() -> Result<()> {
    let Cli {
        command,
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
        Commands::Tui => {
            let log_config = LogConfig {
                level: log_level.clone(),
                output_to_stdout: log_stdout,
                directory: log_dir
                    .clone()
                    .unwrap_or_else(|| LogConfig::default().directory),
                ..Default::default()
            };

            let _logger = Logger::new(log_config)?;

            info!("HookListener CLI starting");

            let mut terminal = setup_terminal()?;
            let mut app = App::new()?;

            let res = run_app(&mut terminal, &mut app, None).await;

            restore_terminal(&mut terminal)?;

            if let Err(err) = res {
                error!(error = %err, "Application terminated with error");
                eprintln!("Error: {}", err);
            } else {
                info!("HookListener CLI terminated successfully");
            }
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
                if let Err(e) = tunnel_client.connect_and_listen().await {
                    error!("Tunnel client error: {}", e);
                }
            });

            let res = run_app(&mut terminal, &mut app, Some(event_rx)).await;

            restore_terminal(&mut terminal)?;

            if let Err(err) = res {
                error!(error = %err, "Application terminated with error");
                eprintln!("Error: {}", err);
            }
        }
        Commands::Diagnostics { output } => {
            // Initialize minimal logging for diagnostics
            let log_config = LogConfig {
                level: "info".to_string(),
                output_to_stdout: true,
                ..Default::default()
            };
            let logger = Logger::new(log_config)?;
            logger.create_diagnostic_bundle(&output)?;
            println!("Diagnostic bundle created in: {}", output.display());
        }
        Commands::CleanLogs { keep } => {
            println!("Cleaning up old log files, keeping {} most recent", keep);
            // This is handled automatically by the logger initialization
        }
    }

    Ok(())
}

async fn run_login_flow(force_reauth: bool) -> Result<()> {
    let mut config = config::Config::load()?;

    if config.is_token_valid() && !force_reauth {
        println!("✅ You're already authenticated.");
        println!(
            "Run `hooklistener listen <endpoint>` to start forwarding webhooks or `hooklistener` to open the TUI."
        );
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
                let expires_at = Utc::now() + ChronoDuration::hours(24);
                config.set_access_token(access_token, expires_at);
                config.save()?;
                println!("✅ Authentication successful!");
                println!(
                    "Run `hooklistener listen <endpoint>` to forward webhooks or `hooklistener` to browse them."
                );
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

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
    mut tunnel_rx: Option<mpsc::Receiver<TunnelEvent>>,
) -> Result<()> {
    // Ensure proper terminal cleanup on any exit
    let _cleanup = TerminalCleanup;

    // Handle initial states
    match app.state {
        AppState::InitiatingDeviceFlow => {
            app.initiate_device_flow().await?;
        }
        AppState::Loading => {
            // Only load organizations if we are NOT in listening mode
            if !matches!(app.state, AppState::Listening) {
                app.load_organizations().await?;
            }
        }
        _ => {}
    }

    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;

        // Update animations
        app.tick();

        if app.should_quit {
            break;
        }

        // Handle tunnel events if receiver is present
        if let Some(rx) = &mut tunnel_rx {
            while let Ok(event) = rx.try_recv() {
                match event {
                    TunnelEvent::Connected => {
                        app.listening_connected = true;
                        app.listening_error = None;
                    }
                    TunnelEvent::ConnectionError(err) => {
                        app.listening_connected = false;
                        app.listening_error = Some(err);
                    }
                    TunnelEvent::WebhookReceived(request) => {
                        app.listening_requests.push(*request);
                        app.listening_stats.total_requests += 1;
                        // Auto-select new request if user was at the bottom or list was empty?
                        // Simple behavior: Update selection index if we want to follow.
                        // But currently we don't auto-scroll unless we implement it.
                        // For now, just adding to list is enough.
                    }
                    TunnelEvent::ForwardSuccess => {
                        app.listening_stats.successful_forwards += 1;
                    }
                    TunnelEvent::ForwardError => {
                        app.listening_stats.failed_forwards += 1;
                    }
                }
            }
        }

        // Handle non-blocking authentication polling
        if matches!(app.state, AppState::DisplayingDeviceCode) {
            app.poll_device_authentication().await?;
        }

        // Handle async states that don't require user input
        match app.state {
            AppState::ForwardingRequest => {
                app.forward_request().await?;
                continue;
            }
            AppState::Loading if app.just_authenticated => {
                // Automatically load organizations after successful authentication
                app.just_authenticated = false;
                app.load_organizations().await?;
                continue;
            }
            AppState::DisplayingDeviceCode => {
                // This state will transition to Loading automatically after successful auth
            }
            _ => {}
        }

        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            let prev_state = format!("{:?}", app.state);
            app.handle_key_event(key)?;

            crate::log_state_transition!(prev_state, app.state, "user_key_event");

            match app.state {
                AppState::InitiatingDeviceFlow => {
                    app.initiate_device_flow().await?;
                }
                AppState::Loading => {
                    debug!(
                        prev_state = %prev_state,
                        current_state = ?app.state,
                        "Handling Loading state"
                    );
                    match prev_state.as_str() {
                        "ShowOrganizations" => {
                            debug!("Calling select_organization");
                            app.select_organization().await?;
                        }
                        "ShowEndpoints" => {
                            if let Some(endpoint_id) = app.get_selected_endpoint_id() {
                                debug!(endpoint_id = %endpoint_id, "Loading endpoint detail");
                                app.load_endpoint_detail(&endpoint_id).await?;
                            }
                        }
                        "ShowEndpointDetail" => {
                            if let Some(endpoint_id) =
                                app.selected_endpoint.as_ref().map(|e| e.id.clone())
                            {
                                debug!(endpoint_id = %endpoint_id, "Loading requests");
                                app.load_requests(&endpoint_id).await?;
                            }
                        }
                        "ShowRequests" => {
                            if let Some(endpoint_id) =
                                app.selected_endpoint.as_ref().map(|e| e.id.clone())
                            {
                                if let Some(request_id) = app
                                    .requests
                                    .get(app.selected_request_index)
                                    .map(|r| r.id.clone())
                                {
                                    debug!(
                                        endpoint_id = %endpoint_id,
                                        request_id = %request_id,
                                        "Loading request details"
                                    );
                                    app.load_request_details(&endpoint_id, &request_id).await?;
                                } else {
                                    debug!(endpoint_id = %endpoint_id, "Reloading requests");
                                    app.load_requests(&endpoint_id).await?;
                                }
                            }
                        }
                        _ => {
                            debug!("Loading state default case, calling load_organizations");
                            app.load_organizations().await?;
                        }
                    }
                }
                AppState::ForwardingRequest => {
                    app.forward_request().await?;
                }
                _ => {}
            }
        }
    }

    Ok(())
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
