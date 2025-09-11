mod api;
mod app;
mod config;
mod models;
mod syntax;
mod ui;

use anyhow::Result;
use crossterm::{
    event::{self, Event, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{backend::CrosstermBackend, Terminal};
use std::io;
use std::time::Duration;

use app::{App, AppState};

#[tokio::main]
async fn main() -> Result<()> {
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
    if matches!(app.state, AppState::Loading) {
        app.load_endpoints().await?;
    }

    loop {
        terminal.draw(|frame| ui::draw(frame, app))?;
        
        // Update animations
        app.tick();
        
        if app.should_quit {
            break;
        }

        // Handle forwarding request state immediately
        if matches!(app.state, AppState::ForwardingRequest) {
            app.forward_request().await?;
            continue;
        }
        
        if event::poll(Duration::from_millis(100))?
            && let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            let prev_state = format!("{:?}", app.state);
            app.handle_key_event(key)?;
            
            match app.state {
                AppState::Loading => {
                    match prev_state.as_str() {
                        "ShowEndpoints" => {
                            if let Some(endpoint_id) = app.get_selected_endpoint_id() {
                                app.load_endpoint_detail(&endpoint_id).await?;
                            }
                        }
                        "ShowEndpointDetail" => {
                            if let Some(endpoint_id) = app.selected_endpoint.as_ref().map(|e| e.id.clone()) {
                                app.load_requests(&endpoint_id).await?;
                            }
                        }
                        "ShowRequests" => {
                            if let Some(endpoint_id) = app.selected_endpoint.as_ref().map(|e| e.id.clone()) {
                                if let Some(request_id) = app.requests.get(app.selected_request_index).map(|r| r.id.clone()) {
                                    app.load_request_details(&endpoint_id, &request_id).await?;
                                } else {
                                    app.load_requests(&endpoint_id).await?;
                                }
                            }
                        }
                        _ => {
                            app.load_endpoints().await?;
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

fn restore_terminal(terminal: &mut Terminal<CrosstermBackend<io::Stdout>>) -> Result<()> {
    disable_raw_mode()?;
    execute!(terminal.backend_mut(), LeaveAlternateScreen)?;
    terminal.show_cursor()?;
    Ok(())
}