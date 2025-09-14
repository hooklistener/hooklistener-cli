mod api;
mod app;
mod auth;
mod config;
mod models;
mod syntax;
mod ui;

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

use app::{App, AppState};

#[tokio::main]
async fn main() -> Result<()> {
    // Initialize logger to write to a file instead of stdout to avoid interfering with TUI
    env_logger::Builder::from_default_env()
        .target(env_logger::Target::Pipe(Box::new(
            std::fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open("hooklistener.log")?,
        )))
        .init();
    log::info!("Starting HookListener CLI");

    let mut terminal = setup_terminal()?;
    let mut app = App::new()?;

    let res = run_app(&mut terminal, &mut app).await;

    restore_terminal(&mut terminal)?;

    if let Err(err) = res {
        eprintln!("Error: {}", err);
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
            log::debug!("State transition: {} -> {:?}", prev_state, app.state);

            match app.state {
                AppState::InitiatingDeviceFlow => {
                    app.initiate_device_flow().await?;
                }
                AppState::Loading => {
                    log::info!("Handling Loading state, prev_state: {}", prev_state);
                    match prev_state.as_str() {
                        "ShowOrganizations" => {
                            log::info!("Calling select_organization");
                            app.select_organization().await?;
                        }
                        "ShowEndpoints" => {
                            if let Some(endpoint_id) = app.get_selected_endpoint_id() {
                                app.load_endpoint_detail(&endpoint_id).await?;
                            }
                        }
                        "ShowEndpointDetail" => {
                            if let Some(endpoint_id) =
                                app.selected_endpoint.as_ref().map(|e| e.id.clone())
                            {
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
                                    app.load_request_details(&endpoint_id, &request_id).await?;
                                } else {
                                    app.load_requests(&endpoint_id).await?;
                                }
                            }
                        }
                        _ => {
                            log::info!("Loading state default case, calling load_organizations");
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
