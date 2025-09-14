mod api;
mod app;
mod auth;
mod config;
mod logger;
mod models;
mod syntax;
mod ui;

use anyhow::Result;
use clap::{Parser, Subcommand};
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
use tracing::{debug, error, info};

use app::{App, AppState};
use logger::{LogConfig, Logger};

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
    let cli = Cli::parse();

    // Handle CLI subcommands first
    if let Some(command) = cli.command {
        match command {
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
                return Ok(());
            }
            Commands::CleanLogs { keep } => {
                println!("Cleaning up old log files, keeping {} most recent", keep);
                // This is handled automatically by the logger initialization
                return Ok(());
            }
        }
    }

    // Configure logging based on CLI arguments
    let log_config = LogConfig {
        level: cli.log_level,
        output_to_stdout: cli.log_stdout,
        directory: cli
            .log_dir
            .unwrap_or_else(|| LogConfig::default().directory),
        ..Default::default()
    };

    // Initialize the professional logging system
    let _logger = Logger::new(log_config)?;

    info!("HookListener CLI starting");

    let mut terminal = setup_terminal()?;
    let mut app = App::new()?;

    let res = run_app(&mut terminal, &mut app).await;

    restore_terminal(&mut terminal)?;

    if let Err(err) = res {
        error!(error = %err, "Application terminated with error");
        eprintln!("Error: {}", err);
    } else {
        info!("HookListener CLI terminated successfully");
    }

    Ok(())
}

async fn run_app<B: ratatui::backend::Backend>(
    terminal: &mut Terminal<B>,
    app: &mut App,
) -> Result<()> {
    // Ensure proper terminal cleanup on any exit
    let _cleanup = TerminalCleanup;

    // Handle initial states
    match app.state {
        AppState::InitiatingDeviceFlow => {
            app.initiate_device_flow().await?;
        }
        AppState::Loading => {
            app.load_organizations().await?;
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
