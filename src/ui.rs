use crate::app::{App, AppState};
use crate::syntax::JsonHighlighter;
use ratatui::{
    prelude::*,
    widgets::{
        Block, Borders, Cell, List, ListItem, Paragraph, Row, Table, TableState, Tabs, Wrap,
    },
};

// Pastel color palette — soft, pleasant, modern
mod colors {
    use ratatui::style::Color;

    pub const PRIMARY: Color = Color::Rgb(137, 180, 250); // Soft blue — borders, main accent
    pub const SECONDARY: Color = Color::Rgb(250, 179, 135); // Warm peach — highlights, selected
    pub const SUCCESS: Color = Color::Rgb(166, 227, 161); // Soft mint — success, POST
    pub const ERROR: Color = Color::Rgb(243, 139, 168); // Soft rose — errors, DELETE
    pub const WARNING: Color = Color::Rgb(249, 226, 175); // Pastel amber — warnings, PUT
    pub const INFO: Color = Color::Rgb(116, 199, 236); // Sky blue — info, GET
    pub const MUTED: Color = Color::Rgb(127, 132, 156); // Muted lavender — secondary text
    pub const TEXT: Color = Color::Rgb(205, 214, 244); // Soft white — primary text
    pub const ACCENT: Color = Color::Rgb(203, 166, 247); // Soft mauve — PATCH, specials
    pub const BACKGROUND: Color = Color::Rgb(49, 50, 68); // Dark surface — status bar
    pub const SURFACE: Color = Color::Rgb(69, 71, 90); // Raised surface — row highlights
}

pub fn draw(frame: &mut Frame, app: &App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Min(0),    // Main content
            Constraint::Length(1), // Status bar
        ])
        .split(frame.area());

    // Draw main content
    match &app.state {
        AppState::ShowRequestDetail => draw_request_detail(frame, app, chunks[0]),
        AppState::InputForwardUrl => draw_forward_url_input(frame, app, chunks[0]),
        AppState::ForwardingRequest => draw_forwarding(frame, app, chunks[0]),
        AppState::ForwardResult => draw_forward_result(frame, app, chunks[0]),
        AppState::Listening => draw_listening(frame, app, chunks[0]),
        AppState::Tunneling => draw_tunneling(frame, app, chunks[0]),
        AppState::ExportMenu => draw_export_menu(frame, app, chunks[0]),
        AppState::Error { message, hint } => draw_error(frame, message, hint.as_deref(), chunks[0]),
    }

    // Draw status bar
    draw_status_bar(frame, app, chunks[1]);
}

fn draw_listening(frame: &mut Frame, app: &App, area: Rect) {
    let show_search = app.search_active || !app.search_query.is_empty();
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if show_search {
            vec![
                Constraint::Length(5), // Header & Stats
                Constraint::Length(3), // Search bar
                Constraint::Min(0),    // Requests List
            ]
        } else {
            vec![
                Constraint::Length(5), // Header & Stats
                Constraint::Length(0), // No search bar
                Constraint::Min(0),    // Requests List
            ]
        })
        .split(area);

    // Header & Stats
    let header_chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(60), // Connection Info
            Constraint::Percentage(40), // Stats
        ])
        .split(chunks[0]);

    // Connection Info Block
    let connection_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(colors::PRIMARY))
        .title(" Tunnel Connection ");

    let connection_status_text = if app.listening_connected {
        vec![
            Line::from(vec![
                Span::styled(
                    "Endpoint: ",
                    Style::default()
                        .fg(colors::PRIMARY)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(&app.listening_endpoint),
            ]),
            Line::from(vec![
                Span::styled(
                    "Target:   ",
                    Style::default()
                        .fg(colors::PRIMARY)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(&app.listening_target),
            ]),
            Line::from(vec![
                Span::styled(
                    "Status:   ",
                    Style::default()
                        .fg(colors::PRIMARY)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    "● Connected",
                    Style::default()
                        .fg(colors::SUCCESS)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
        ]
    } else if let Some(err) = &app.listening_error {
        let (symbol, color) = if err.starts_with("Reconnecting") {
            ("⟳", colors::WARNING)
        } else {
            ("✗", colors::ERROR)
        };
        vec![Line::from(vec![
            Span::styled(
                "Status: ",
                Style::default()
                    .fg(colors::PRIMARY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                format!("{} {}", symbol, err),
                Style::default().fg(color).add_modifier(Modifier::BOLD),
            ),
        ])]
    } else {
        let spinner_chars = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
        let spinner = spinner_chars[app.loading_frame % spinner_chars.len()];
        vec![
            Line::from(vec![
                Span::styled(
                    "Endpoint: ",
                    Style::default()
                        .fg(colors::PRIMARY)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(&app.listening_endpoint),
            ]),
            Line::from(vec![
                Span::styled(
                    "Target:   ",
                    Style::default()
                        .fg(colors::PRIMARY)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(&app.listening_target),
            ]),
            Line::from(vec![
                Span::styled(
                    "Status:   ",
                    Style::default()
                        .fg(colors::PRIMARY)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    format!("{} Connecting...", spinner),
                    Style::default()
                        .fg(colors::WARNING)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
        ]
    };

    let connection_info = Paragraph::new(connection_status_text).block(connection_block);
    frame.render_widget(connection_info, header_chunks[0]);

    // Stats Block
    let stats_block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(colors::SECONDARY))
        .title(" Statistics ");

    let stats_text = vec![
        Line::from(vec![
            Span::styled("Total:   ", Style::default().fg(colors::TEXT)),
            Span::styled(
                app.listening_stats.total_requests.to_string(),
                Style::default()
                    .fg(colors::INFO)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Success: ", Style::default().fg(colors::TEXT)),
            Span::styled(
                app.listening_stats.successful_forwards.to_string(),
                Style::default()
                    .fg(colors::SUCCESS)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Failed:  ", Style::default().fg(colors::TEXT)),
            Span::styled(
                app.listening_stats.failed_forwards.to_string(),
                Style::default()
                    .fg(colors::ERROR)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ];

    let stats_info = Paragraph::new(stats_text).block(stats_block);
    frame.render_widget(stats_info, header_chunks[1]);

    // Search bar
    if show_search {
        let search_border_color = if app.search_active {
            colors::PRIMARY
        } else {
            colors::MUTED
        };
        let search_text = format!("/{}", app.search_query);
        let cursor = if app.search_active { "▎" } else { "" };
        let search = Paragraph::new(format!("{}{}", search_text, cursor))
            .style(Style::default().fg(colors::TEXT))
            .block(
                Block::default()
                    .title(" Search ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(search_border_color)),
            );
        frame.render_widget(search, chunks[1]);
    }

    let requests_area = chunks[2];

    let filtered_indices =
        crate::app::App::filter_requests(&app.listening_requests, &app.search_query);

    // Requests List
    if filtered_indices.is_empty() {
        let msg = if app.listening_requests.is_empty() {
            "Waiting for webhooks..."
        } else {
            "No matching requests"
        };
        let no_requests = Paragraph::new(msg)
            .style(Style::default().fg(colors::MUTED))
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Live Requests ")
                    .border_style(Style::default().fg(colors::MUTED)),
            );

        frame.render_widget(no_requests, requests_area);
    } else {
        let rows: Vec<Row> = filtered_indices
            .iter()
            .enumerate()
            .map(|(display_idx, &real_idx)| {
                let request = &app.listening_requests[real_idx];
                let is_selected = display_idx == app.selected_request_index;
                let style = if is_selected {
                    Style::default()
                        .fg(colors::SECONDARY)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(colors::TEXT)
                };

                let time_display = "Just now";

                let (method_symbol, method_style) = match request.method.as_str() {
                    "GET" => ("🔽", style.fg(colors::INFO)),
                    "POST" => ("📝", style.fg(colors::SUCCESS)),
                    "PUT" => ("📤", style.fg(colors::WARNING)),
                    "DELETE" => ("🗑️", style.fg(colors::ERROR)),
                    "PATCH" => ("✏️", style.fg(colors::ACCENT)),
                    _ => ("❓", style.fg(colors::TEXT)),
                };

                Row::new(vec![
                    Cell::from(time_display).style(style.fg(colors::MUTED)),
                    Cell::from(format!("{} {}", method_symbol, request.method)).style(method_style),
                    Cell::from(request.path.clone().unwrap_or(request.url.clone())).style(style),
                    Cell::from(format!("{} headers", request.headers.len()))
                        .style(style.fg(colors::MUTED)),
                ])
            })
            .collect();

        let headers = Row::new(vec!["Time", "Method", "Path", "Details"])
            .style(
                Style::default()
                    .fg(colors::PRIMARY)
                    .add_modifier(Modifier::BOLD),
            )
            .bottom_margin(1);

        let requests_table = Table::new(
            rows,
            [
                Constraint::Percentage(15), // Time
                Constraint::Percentage(15), // Method
                Constraint::Percentage(50), // Path
                Constraint::Percentage(20), // Details
            ],
        )
        .header(headers)
        .block(
            Block::default()
                .title(" Live Requests ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(colors::PRIMARY)),
        )
        .row_highlight_style(Style::default().bg(colors::SURFACE))
        .highlight_symbol("> ");

        let mut table_state = TableState::default();
        table_state.select(Some(app.selected_request_index));

        frame.render_stateful_widget(requests_table, requests_area, &mut table_state);
    }
}

fn draw_tunneling(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(7), // Header with URL and status
            Constraint::Length(5), // Statistics
            Constraint::Min(0),    // Requests table
        ])
        .split(area);

    // Header with tunnel URL and status
    let tunnel_url = if let Some(subdomain) = &app.tunnel_subdomain {
        format!("https://{}", subdomain)
    } else {
        "Connecting...".to_string()
    };

    let target_url = format!("{}:{}", app.tunnel_local_host, app.tunnel_local_port);

    let is_reconnecting = app
        .tunnel_error
        .as_ref()
        .is_some_and(|e| e.starts_with("Reconnecting"));

    let status_symbol = if app.tunnel_connected {
        "●"
    } else if is_reconnecting {
        "⟳"
    } else if app.tunnel_error.is_some() {
        "✗"
    } else {
        let spinner_chars = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
        spinner_chars[app.loading_frame % spinner_chars.len()]
    };
    let status_color = if app.tunnel_connected {
        colors::SUCCESS
    } else if is_reconnecting {
        colors::WARNING
    } else if app.tunnel_error.is_some() {
        colors::ERROR
    } else {
        colors::WARNING
    };

    let status_text = if app.tunnel_connected {
        if app.tunnel_is_static {
            "Connected (Static)"
        } else {
            "Connected (Ephemeral)"
        }
    } else if let Some(err) = &app.tunnel_error {
        err.as_str()
    } else {
        "Connecting..."
    };

    let uptime_text = if let Some(connected_at) = app.tunnel_connected_at {
        let elapsed = connected_at.elapsed();
        let minutes = elapsed.as_secs() / 60;
        let seconds = elapsed.as_secs() % 60;
        if minutes > 0 {
            format!("Uptime: {}m {}s", minutes, seconds)
        } else {
            format!("Uptime: {}s", seconds)
        }
    } else {
        String::new()
    };

    let mut header_text = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled("   ", Style::default()),
            Span::styled("🌐  ", Style::default().fg(colors::INFO)),
            Span::styled(
                tunnel_url,
                Style::default()
                    .fg(colors::PRIMARY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("  →  ", Style::default().fg(colors::MUTED)),
            Span::styled(
                target_url,
                Style::default()
                    .fg(colors::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("   Status: ", Style::default().fg(colors::TEXT)),
            Span::styled(
                format!("{} {}", status_symbol, status_text),
                Style::default()
                    .fg(status_color)
                    .add_modifier(Modifier::BOLD),
            ),
            if !uptime_text.is_empty() {
                Span::styled(
                    format!("     {}", uptime_text),
                    Style::default().fg(colors::MUTED),
                )
            } else {
                Span::raw("")
            },
        ]),
    ];

    // Show tunnel status message (e.g. "URL copied to clipboard!")
    if let Some((msg, _)) = &app.tunnel_status_message {
        header_text.push(Line::from(Span::styled(
            format!("   {}", msg),
            Style::default()
                .fg(colors::SUCCESS)
                .add_modifier(Modifier::BOLD),
        )));
    }

    let header = Paragraph::new(header_text).block(
        Block::default()
            .title(" HTTP Tunnel ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(colors::PRIMARY)),
    );

    frame.render_widget(header, chunks[0]);

    // Statistics
    let avg_duration = if app.tunnel_stats.success > 0 {
        app.tunnel_stats.total_duration_ms / app.tunnel_stats.success
    } else {
        0
    };

    let success_rate = if app.tunnel_stats.total > 0 {
        (app.tunnel_stats.success * 100) / app.tunnel_stats.total
    } else {
        0
    };

    let failed_rate = if app.tunnel_stats.total > 0 {
        (app.tunnel_stats.failed * 100) / app.tunnel_stats.total
    } else {
        0
    };

    let stats_text = vec![
        Line::from(vec![
            Span::styled("Total Requests: ", Style::default().fg(colors::TEXT)),
            Span::styled(
                app.tunnel_stats.total.to_string(),
                Style::default()
                    .fg(colors::INFO)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("     Success: ", Style::default().fg(colors::TEXT)),
            Span::styled(
                format!("{} ({}%)", app.tunnel_stats.success, success_rate),
                Style::default()
                    .fg(colors::SUCCESS)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("     Failed: ", Style::default().fg(colors::TEXT)),
            Span::styled(
                format!("{} ({}%)", app.tunnel_stats.failed, failed_rate),
                Style::default()
                    .fg(colors::ERROR)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("Avg Response: ", Style::default().fg(colors::TEXT)),
            Span::styled(
                format!("{}ms", avg_duration),
                Style::default()
                    .fg(colors::WARNING)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
    ];

    let stats = Paragraph::new(stats_text).block(
        Block::default()
            .title(" Statistics ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(colors::SECONDARY)),
    );

    frame.render_widget(stats, chunks[1]);

    // Live Requests table
    if app.tunnel_requests.is_empty() {
        let no_requests = Paragraph::new("Waiting for requests...")
            .style(Style::default().fg(colors::MUTED))
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .title(" Live Requests ")
                    .border_style(Style::default().fg(colors::MUTED)),
            );

        frame.render_widget(no_requests, chunks[2]);
    } else {
        // Calculate visible window
        let available_rows = chunks[2].height.saturating_sub(3) as usize; // Subtract header and borders
        let start_idx = app.tunnel_scroll_offset;
        let end_idx = (start_idx + available_rows).min(app.tunnel_requests.len());

        // Reverse to show newest first
        let mut visible_requests: Vec<_> = app.tunnel_requests.iter().collect();
        visible_requests.reverse();
        let visible_requests = &visible_requests[start_idx..end_idx];

        let rows: Vec<Row> = visible_requests
            .iter()
            .map(|request| {
                // Calculate time since received
                let elapsed = request.received_at.elapsed();
                let time_display = if elapsed.as_secs() < 1 {
                    "now".to_string()
                } else if elapsed.as_secs() < 60 {
                    format!("{}s", elapsed.as_secs())
                } else {
                    format!("{}m", elapsed.as_secs() / 60)
                };

                // Get method symbol and color
                let (method_symbol, method_color) = match request.method.as_str() {
                    "GET" => ("🔽", colors::INFO),
                    "POST" => ("📝", colors::SUCCESS),
                    "PUT" => ("📤", colors::WARNING),
                    "DELETE" => ("🗑️", colors::ERROR),
                    "PATCH" => ("✏️", colors::ACCENT),
                    "HEAD" => ("👤", colors::MUTED),
                    "OPTIONS" => ("⚙️", colors::PRIMARY),
                    _ => ("❓", colors::TEXT),
                };

                // Status display
                let (status_display, status_color) = if let Some(status) = request.status {
                    let color = if (200..300).contains(&status) {
                        colors::SUCCESS
                    } else if (400..500).contains(&status) {
                        colors::WARNING
                    } else if status >= 500 {
                        colors::ERROR
                    } else {
                        colors::INFO
                    };
                    (status.to_string(), color)
                } else if request.error.is_some() {
                    ("Error".to_string(), colors::ERROR)
                } else {
                    let spinner_chars = ["⏳", "⏳", "⏳", "⏳"];
                    (spinner_chars[0].to_string(), colors::WARNING)
                };

                // Duration display
                let duration_display = if let Some(completed_at) = request.completed_at {
                    let duration = completed_at.duration_since(request.received_at);
                    format!("{}ms", duration.as_millis())
                } else {
                    "-".to_string()
                };

                Row::new(vec![
                    Cell::from(time_display).style(Style::default().fg(colors::MUTED)),
                    Cell::from(format!("{} {}", method_symbol, request.method))
                        .style(Style::default().fg(method_color)),
                    Cell::from(request.path.clone()).style(Style::default().fg(colors::TEXT)),
                    Cell::from(status_display).style(Style::default().fg(status_color)),
                    Cell::from(duration_display).style(Style::default().fg(colors::MUTED)),
                ])
            })
            .collect();

        let headers = Row::new(vec!["Time", "Method", "Path", "Status", "Duration"])
            .style(
                Style::default()
                    .fg(colors::PRIMARY)
                    .add_modifier(Modifier::BOLD),
            )
            .bottom_margin(1);

        let title = if app.tunnel_requests.len() > available_rows {
            format!(
                " Live Requests ({}-{}/{}) ",
                start_idx + 1,
                end_idx,
                app.tunnel_requests.len()
            )
        } else {
            format!(" Live Requests ({}) ", app.tunnel_requests.len())
        };

        let requests_table = Table::new(
            rows,
            [
                Constraint::Percentage(10), // Time
                Constraint::Percentage(15), // Method
                Constraint::Percentage(50), // Path
                Constraint::Percentage(12), // Status
                Constraint::Percentage(13), // Duration
            ],
        )
        .header(headers)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(colors::PRIMARY)),
        );

        frame.render_widget(requests_table, chunks[2]);
    }
}

fn draw_request_detail(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3), // Tab bar
            Constraint::Min(0),    // Tab content
        ])
        .split(area);

    if let Some(request) = &app.selected_request {
        // Tab titles
        let tab_titles: Vec<Line> = ["Info", "Headers", "Body"]
            .iter()
            .cloned()
            .map(Line::from)
            .collect();

        let tabs = Tabs::new(tab_titles)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(colors::MUTED)),
            )
            .style(Style::default().fg(colors::TEXT))
            .highlight_style(
                Style::default()
                    .fg(colors::SECONDARY)
                    .add_modifier(Modifier::BOLD),
            )
            .select(app.current_tab);

        frame.render_widget(tabs, chunks[0]);

        // Tab content
        match app.current_tab {
            0 => draw_info_tab(frame, app, request, chunks[1]),
            1 => draw_headers_tab(frame, app, request, chunks[1]),
            2 => draw_body_tab(frame, app, request, chunks[1]),
            _ => {}
        }
    }
}

fn draw_info_tab(
    frame: &mut Frame,
    app: &App,
    request: &crate::models::WebhookRequest,
    area: Rect,
) {
    let mut info_text = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "Method: ",
                Style::default()
                    .fg(colors::PRIMARY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                &request.method,
                Style::default()
                    .fg(colors::SUCCESS)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "URL: ",
                Style::default()
                    .fg(colors::PRIMARY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(&request.url, Style::default().fg(colors::TEXT)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "Remote IP: ",
                Style::default()
                    .fg(colors::PRIMARY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(&request.remote_addr, Style::default().fg(colors::TEXT)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "Timestamp: ",
                Style::default()
                    .fg(colors::PRIMARY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(&request.created_at, Style::default().fg(colors::TEXT)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "Content Length: ",
                Style::default()
                    .fg(colors::PRIMARY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                request.content_length.to_string(),
                Style::default().fg(colors::TEXT),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "Request ID: ",
                Style::default()
                    .fg(colors::PRIMARY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(&request.id, Style::default().fg(colors::MUTED)),
        ]),
    ];

    // Show last forward URL with replay hint
    if !app.forward_url_input.is_empty() {
        info_text.push(Line::from(""));
        info_text.push(Line::from(vec![
            Span::styled(
                "Last Forward URL: ",
                Style::default()
                    .fg(colors::SECONDARY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(&app.forward_url_input, Style::default().fg(colors::PRIMARY)),
            Span::styled(" (press r to replay)", Style::default().fg(colors::MUTED)),
        ]));
    }

    // Show status message if present
    if let Some((msg, _)) = &app.status_message {
        info_text.push(Line::from(""));
        info_text.push(Line::from(Span::styled(
            msg.as_str(),
            Style::default()
                .fg(colors::SUCCESS)
                .add_modifier(Modifier::BOLD),
        )));
    }

    let info = Paragraph::new(info_text)
        .block(
            Block::default()
                .title(" Request Information ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(colors::PRIMARY)),
        )
        .wrap(Wrap { trim: true });

    frame.render_widget(info, area);
}

fn draw_headers_tab(
    frame: &mut Frame,
    app: &App,
    request: &crate::models::WebhookRequest,
    area: Rect,
) {
    let headers: Vec<(&String, &String)> = request.headers.iter().collect();
    let available_lines = area.height.saturating_sub(2) as usize;

    let start_line = app.headers_scroll_offset;
    let end_line = (start_line + available_lines).min(headers.len());
    let visible_headers = &headers[start_line..end_line];

    let header_items: Vec<ListItem> = visible_headers
        .iter()
        .map(|(key, value)| {
            ListItem::new(Line::from(vec![
                Span::styled(
                    format!("{}: ", key),
                    Style::default()
                        .fg(colors::PRIMARY)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(value.as_str(), Style::default().fg(colors::TEXT)),
            ]))
        })
        .collect();

    let title = if headers.len() > available_lines {
        format!(
            " Headers ({}-{}/{}) ",
            start_line + 1,
            end_line,
            headers.len()
        )
    } else {
        format!(" Headers ({}) ", headers.len())
    };

    let headers_list = List::new(header_items).block(
        Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(colors::SECONDARY)),
    );

    frame.render_widget(headers_list, area);
}

fn draw_body_tab(
    frame: &mut Frame,
    app: &App,
    request: &crate::models::WebhookRequest,
    area: Rect,
) {
    // Use full body if available, otherwise fall back to preview
    let body_text = request.body.as_ref().or(request.body_preview.as_ref());

    if let Some(body_content) = body_text {
        if !body_content.is_empty() {
            // Apply syntax highlighting to get formatted Lines
            let highlighted_lines = JsonHighlighter::highlight_json(body_content);

            // Account for borders (2 lines) and potential padding
            let available_lines = area.height.saturating_sub(2) as usize;

            let start_line = app.body_scroll_offset;
            let end_line = (start_line + available_lines).min(highlighted_lines.len());

            // Ensure we don't go past the available content
            let actual_start = if end_line <= highlighted_lines.len() {
                start_line
            } else {
                highlighted_lines.len().saturating_sub(available_lines)
            };
            let actual_end = (actual_start + available_lines).min(highlighted_lines.len());

            let visible_lines = highlighted_lines[actual_start..actual_end].to_vec();

            let title_suffix = if request.body.is_some() {
                " (Full)"
            } else {
                " (Preview)"
            };

            // Detect if content is JSON for title indication
            let content_type =
                if body_content.trim().starts_with('{') || body_content.trim().starts_with('[') {
                    " JSON"
                } else {
                    ""
                };

            let title = if highlighted_lines.len() > available_lines {
                format!(
                    " Body{}{} (lines {}-{}/{}) ",
                    content_type,
                    title_suffix,
                    actual_start + 1,
                    actual_end,
                    highlighted_lines.len()
                )
            } else {
                format!(" Body{}{} ", content_type, title_suffix)
            };

            let body = Paragraph::new(visible_lines).block(
                Block::default()
                    .title(title)
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(colors::SUCCESS)),
            );

            frame.render_widget(body, area);
        } else {
            let body = Paragraph::new("(empty body)")
                .style(Style::default().fg(colors::MUTED))
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .title(" Body ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(colors::MUTED)),
                );

            frame.render_widget(body, area);
        }
    } else {
        let body = Paragraph::new("(no body)")
            .style(Style::default().fg(colors::MUTED))
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .title(" Body ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(colors::MUTED)),
            );

        frame.render_widget(body, area);
    }
}

fn draw_error(frame: &mut Frame, error_msg: &str, hint: Option<&str>, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(area);

    let mut lines = vec![Line::from(Span::styled(
        error_msg,
        Style::default()
            .fg(colors::ERROR)
            .add_modifier(Modifier::BOLD),
    ))];

    if let Some(hint_text) = hint {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("💡 {}", hint_text),
            Style::default().fg(colors::WARNING),
        )));
    }

    let error = Paragraph::new(lines)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .title(" Error ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(colors::ERROR)),
        );

    frame.render_widget(error, chunks[0]);

    let help_text = vec![Line::from(vec![
        Span::styled(
            "q/Esc",
            Style::default()
                .fg(colors::ERROR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(": Quit", Style::default().fg(colors::TEXT)),
    ])];

    let help = Paragraph::new(help_text)
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::TOP));

    frame.render_widget(help, chunks[1]);
}

fn draw_forward_url_input(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .split(area);

    // Show request summary
    if let Some(request) = &app.selected_request {
        let request_info = vec![Line::from(vec![
            Span::styled(
                "Forwarding Request: ",
                Style::default()
                    .fg(colors::PRIMARY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                &request.method,
                Style::default()
                    .fg(colors::SUCCESS)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" from ", Style::default().fg(colors::TEXT)),
            Span::styled(&request.remote_addr, Style::default().fg(colors::SECONDARY)),
        ])];

        let info = Paragraph::new(request_info).block(
            Block::default()
                .title(" Request to Forward ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(colors::PRIMARY)),
        );

        frame.render_widget(info, chunks[0]);
    }

    // URL input
    let input_block = Block::default()
        .title(" Enter Target URL ")
        .borders(Borders::ALL)
        .border_style(
            if app.is_valid_url(&app.forward_url_input) || app.forward_url_input.is_empty() {
                Style::default().fg(colors::SECONDARY)
            } else {
                Style::default().fg(colors::ERROR)
            },
        );

    let input = Paragraph::new(app.forward_url_input.as_str())
        .style(Style::default().fg(colors::TEXT))
        .block(input_block);

    frame.render_widget(input, chunks[1]);

    // Help text
    let help_text = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "Enter",
                Style::default()
                    .fg(colors::SUCCESS)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(": Forward | ", Style::default().fg(colors::TEXT)),
            Span::styled(
                "Esc",
                Style::default()
                    .fg(colors::ERROR)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(": Cancel", Style::default().fg(colors::TEXT)),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled("Example: ", Style::default().fg(colors::MUTED)),
            Span::styled(
                "https://your-server.com/webhook",
                Style::default().fg(colors::PRIMARY),
            ),
        ]),
    ];

    let help = Paragraph::new(help_text)
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true });

    frame.render_widget(help, chunks[2]);
}

fn draw_forwarding(frame: &mut Frame, app: &App, area: Rect) {
    let spinner_chars = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
    let spinner = spinner_chars[app.loading_frame % spinner_chars.len()];

    let forwarding_text = format!("{} Forwarding request...", spinner);

    let forwarding = Paragraph::new(forwarding_text)
        .style(
            Style::default()
                .fg(colors::WARNING)
                .add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(colors::WARNING)),
        );

    frame.render_widget(forwarding, area);
}

fn draw_forward_result(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Percentage(30),
            Constraint::Percentage(30),
            Constraint::Percentage(30),
            Constraint::Length(3),
        ])
        .split(area);

    if let Some(result) = &app.forward_result {
        // Status and timing info
        let status_color = if result.success {
            colors::SUCCESS
        } else {
            colors::ERROR
        };
        let status_text = if result.success {
            format!(
                "✓ SUCCESS - {} ({}ms)",
                result
                    .status_code
                    .map(|s| s.to_string())
                    .unwrap_or("N/A".to_string()),
                result.duration_ms
            )
        } else {
            format!("✗ FAILED ({}ms)", result.duration_ms)
        };

        let status_info = vec![
            Line::from(vec![
                Span::styled(
                    "Status: ",
                    Style::default()
                        .fg(colors::PRIMARY)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    status_text,
                    Style::default()
                        .fg(status_color)
                        .add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    "Target: ",
                    Style::default()
                        .fg(colors::PRIMARY)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(&result.target_url, Style::default().fg(colors::SECONDARY)),
            ]),
        ];

        let status = Paragraph::new(status_info).block(
            Block::default()
                .title(" Forward Result ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(status_color)),
        );

        frame.render_widget(status, chunks[0]);

        // Response headers (if success)
        if result.success && !result.headers.is_empty() {
            let header_rows: Vec<Row> = result
                .headers
                .iter()
                .take(5) // Limit to first 5 headers
                .map(|(key, value)| {
                    let value_display = if value.len() > 50 {
                        format!("{}...", &value[..50])
                    } else {
                        value.clone()
                    };
                    Row::new(vec![
                        Cell::from(key.clone()).style(
                            Style::default()
                                .fg(colors::PRIMARY)
                                .add_modifier(Modifier::BOLD),
                        ),
                        Cell::from(value_display).style(Style::default().fg(colors::TEXT)),
                    ])
                })
                .collect();

            let headers_table = Table::new(
                header_rows,
                [Constraint::Percentage(30), Constraint::Percentage(70)],
            )
            .block(
                Block::default()
                    .title(" Response Headers ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(colors::SUCCESS)),
            );

            frame.render_widget(headers_table, chunks[1]);
        } else if !result.success {
            // Show error message
            let error_text = result.error_message.as_deref().unwrap_or("Unknown error");
            let error = Paragraph::new(error_text)
                .style(Style::default().fg(colors::ERROR))
                .wrap(Wrap { trim: true })
                .block(
                    Block::default()
                        .title(" Error Details ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(colors::ERROR)),
                );

            frame.render_widget(error, chunks[1]);
        }

        // Response body
        let body_text = if result.success {
            if result.body.is_empty() {
                "(empty response)"
            } else if result.body.len() > 500 {
                &format!(
                    "{}...\n\n[Truncated - showing first 500 characters]",
                    &result.body[..500]
                )
            } else {
                &result.body
            }
        } else {
            "(no response body)"
        };

        let body = Paragraph::new(body_text)
            .style(Style::default().fg(colors::TEXT))
            .block(
                Block::default()
                    .title(" Response Body ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(colors::SUCCESS)),
            )
            .wrap(Wrap { trim: true });

        frame.render_widget(body, chunks[2]);
    }

    // Help
    let help = Paragraph::new(vec![Line::from(vec![
        Span::styled(
            "b/Esc",
            Style::default()
                .fg(colors::WARNING)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(": Back | ", Style::default().fg(colors::TEXT)),
        Span::styled(
            "q",
            Style::default()
                .fg(colors::ERROR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(": Quit", Style::default().fg(colors::TEXT)),
    ])])
    .alignment(Alignment::Center)
    .block(Block::default().borders(Borders::TOP));

    frame.render_widget(help, chunks[3]);
}

fn centered_rect(percent_x: u16, height: u16, area: Rect) -> Rect {
    let popup_width = (area.width * percent_x / 100).max(1);
    let x = area.x + (area.width.saturating_sub(popup_width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, popup_width, height.min(area.height))
}

fn draw_export_menu(frame: &mut Frame, app: &App, area: Rect) {
    // Draw the request detail as background
    draw_request_detail(frame, app, area);

    // Overlay the export popup
    let popup_area = centered_rect(30, 7, area);

    // Clear the popup area
    frame.render_widget(ratatui::widgets::Clear, popup_area);

    let menu_text = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "  1/c  ",
                Style::default()
                    .fg(colors::SECONDARY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("cURL command", Style::default().fg(colors::TEXT)),
        ]),
        Line::from(vec![
            Span::styled(
                "  2/j  ",
                Style::default()
                    .fg(colors::SECONDARY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("JSON export", Style::default().fg(colors::TEXT)),
        ]),
        Line::from(""),
        Line::from(Span::styled(
            "  Esc  Cancel",
            Style::default().fg(colors::MUTED),
        )),
    ];

    let popup = Paragraph::new(menu_text).block(
        Block::default()
            .title(" Export Request ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(colors::PRIMARY)),
    );

    frame.render_widget(popup, popup_area);
}

fn draw_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Min(0),     // Status and shortcuts
            Constraint::Length(20), // Connection status
        ])
        .split(area);

    // Build status text with shortcuts based on current state
    let (status_text, shortcuts) = match &app.state {
        AppState::ShowRequestDetail => (
            "📄 Request Details".to_string(),
            "Tab/←→: Tabs | ↑/↓: Scroll | F: Forward | R: Replay | E: Export | B/Esc: Back | Q: Quit",
        ),
        AppState::InputForwardUrl => (
            "🚀 Forward Request".to_string(),
            "Enter: Forward | Esc: Cancel",
        ),
        AppState::ForwardingRequest => {
            let spinner_chars = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
            let spinner = spinner_chars[app.loading_frame % spinner_chars.len()];
            (format!("{} Forwarding...", spinner), "Please wait")
        }
        AppState::ForwardResult => ("✅ Forward Result".to_string(), "B/Esc: Back | Q: Quit"),
        AppState::ExportMenu => (
            "📤 Export Request".to_string(),
            "1/c: cURL | 2/j: JSON | Esc: Cancel",
        ),
        AppState::Listening => {
            let total_requests = app.listening_requests.len();
            (
                format!("🎧 Listening ({})", total_requests),
                "↑/↓: Navigate | Enter: Details | /: Search | Q: Quit",
            )
        }
        AppState::Tunneling => {
            let total_requests = app.tunnel_requests.len();
            (
                format!("🌐 Tunnel ({})", total_requests),
                "↑/↓/j/k: Scroll | PgUp/PgDn: Page | C: Copy URL | Q: Quit",
            )
        }
        AppState::Error { .. } => ("❌ Error".to_string(), "Q/Esc: Quit"),
    };

    // Left side: Status and shortcuts
    let status_spans = vec![
        Span::styled(
            status_text,
            Style::default()
                .fg(colors::SECONDARY)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(" | "),
        Span::styled(shortcuts, Style::default().fg(colors::MUTED)),
    ];

    let status_paragraph = Paragraph::new(Line::from(status_spans))
        .style(Style::default().bg(colors::BACKGROUND))
        .alignment(Alignment::Left);

    frame.render_widget(status_paragraph, chunks[0]);

    // Right side: API connection status
    let connection_status = if app.config.access_token.is_some() && app.config.is_token_valid() {
        Span::styled(
            "🟢 Connected",
            Style::default()
                .fg(colors::SUCCESS)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            "🔴 No API Key",
            Style::default()
                .fg(colors::ERROR)
                .add_modifier(Modifier::BOLD),
        )
    };

    let connection_paragraph = Paragraph::new(Line::from(vec![connection_status]))
        .style(Style::default().bg(colors::BACKGROUND))
        .alignment(Alignment::Center);

    frame.render_widget(connection_paragraph, chunks[1]);
}
