use crate::app::{App, AppState};
use crate::syntax::JsonHighlighter;
use ratatui::{
    prelude::*,
    widgets::{
        Block, Borders, Cell, List, ListItem, Paragraph, Row, Table, TableState, Tabs, Wrap,
    },
};

// Color scheme constants for consistency
mod colors {
    use ratatui::style::Color;

    pub const PRIMARY: Color = Color::Cyan; // Main UI elements, borders
    pub const SECONDARY: Color = Color::Yellow; // Highlights, selected items
    pub const SUCCESS: Color = Color::Green; // Success states, POST
    pub const ERROR: Color = Color::Red; // Error states, DELETE
    pub const WARNING: Color = Color::Yellow; // Warning states, PUT
    pub const INFO: Color = Color::Blue; // Info states, GET
    pub const MUTED: Color = Color::Gray; // Secondary text, timestamps
    pub const TEXT: Color = Color::White; // Primary text
    pub const ACCENT: Color = Color::Magenta; // Special highlights, PATCH
    pub const BACKGROUND: Color = Color::DarkGray; // Status bar background
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
        AppState::InitiatingDeviceFlow => draw_loading(frame, app, chunks[0]),
        AppState::DisplayingDeviceCode => draw_device_code(frame, app, chunks[0]),
        AppState::WaitingForAuth => draw_waiting_for_auth(frame, app, chunks[0]),
        AppState::Loading => draw_loading(frame, app, chunks[0]),
        AppState::ShowOrganizations => draw_organizations_list(frame, app, chunks[0]),
        AppState::ShowEndpoints => draw_endpoints_list(frame, app, chunks[0]),
        AppState::ShowEndpointDetail => draw_endpoint_detail(frame, app, chunks[0]),
        AppState::ShowRequests => draw_requests_list(frame, app, chunks[0]),
        AppState::ShowRequestDetail => draw_request_detail(frame, app, chunks[0]),
        AppState::InputForwardUrl => draw_forward_url_input(frame, app, chunks[0]),
        AppState::ForwardingRequest => draw_forwarding(frame, app, chunks[0]),
        AppState::ForwardResult => draw_forward_result(frame, app, chunks[0]),
        AppState::Error(msg) => draw_error(frame, msg, chunks[0]),
    }

    // Draw status bar
    draw_status_bar(frame, app, chunks[1]);
}

fn draw_device_code(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(5),
            Constraint::Length(3),
            Constraint::Min(0),
        ])
        .split(area);

    let title = Paragraph::new("Device Authentication")
        .style(
            Style::default()
                .fg(colors::PRIMARY)
                .add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::NONE));

    frame.render_widget(title, chunks[0]);

    if let Some((user_code, time_remaining)) = app.get_device_code_info() {
        let code_block = Block::default()
            .title(" Enter this code in your browser ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(colors::SUCCESS));

        let code_display = Paragraph::new(user_code)
            .style(
                Style::default()
                    .fg(colors::SUCCESS)
                    .add_modifier(Modifier::BOLD),
            )
            .alignment(Alignment::Center)
            .block(code_block);

        frame.render_widget(code_display, chunks[1]);

        let time_text = if let Some(remaining) = time_remaining {
            let minutes = remaining.num_minutes();
            let seconds = remaining.num_seconds() % 60;
            if minutes > 0 {
                format!("Code expires in: {}m {}s", minutes, seconds)
            } else {
                format!("Code expires in: {}s", seconds)
            }
        } else {
            "Code expired".to_string()
        };

        let timer = Paragraph::new(time_text)
            .style(Style::default().fg(colors::WARNING))
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::NONE));

        frame.render_widget(timer, chunks[2]);
    }

    let help_text = vec![
        Line::from(""),
        Line::from("Visit https://app.hooklistener.com/device-codes and enter the code above"),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "r",
                Style::default()
                    .fg(colors::SUCCESS)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(": Refresh | "),
            Span::styled(
                "Esc/q",
                Style::default()
                    .fg(colors::ERROR)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(": Quit"),
        ]),
    ];

    let help = Paragraph::new(help_text)
        .style(Style::default().fg(colors::MUTED))
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::NONE));

    frame.render_widget(help, chunks[3]);
}

fn draw_waiting_for_auth(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(3),
            Constraint::Length(7),
            Constraint::Min(0),
        ])
        .split(area);

    let title = Paragraph::new("Waiting for Authorization...")
        .style(
            Style::default()
                .fg(colors::PRIMARY)
                .add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::NONE));

    frame.render_widget(title, chunks[0]);

    let spinner_chars = ['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧'];
    let spinner_char = spinner_chars[app.loading_frame % spinner_chars.len()];

    let status_text = vec![
        Line::from(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(
                format!("{} ", spinner_char),
                Style::default().fg(colors::SUCCESS),
            ),
            Span::raw("Checking for authorization..."),
        ]),
        Line::from(""),
        Line::from("Please complete the authorization in your web browser."),
        Line::from("This may take a few moments."),
    ];

    let status = Paragraph::new(status_text)
        .style(Style::default().fg(colors::TEXT))
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(colors::PRIMARY)),
        );

    frame.render_widget(status, chunks[1]);

    let help_text = vec![Line::from(vec![
        Span::styled(
            "r",
            Style::default()
                .fg(colors::SUCCESS)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(": Restart authentication | "),
        Span::styled(
            "Esc/q",
            Style::default()
                .fg(colors::ERROR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(": Quit"),
    ])];

    let help = Paragraph::new(help_text)
        .style(Style::default().fg(colors::MUTED))
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true })
        .block(Block::default().borders(Borders::NONE));

    frame.render_widget(help, chunks[2]);
}

fn draw_loading(frame: &mut Frame, app: &App, area: Rect) {
    let spinner_chars = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
    let spinner = spinner_chars[app.loading_frame % spinner_chars.len()];

    let loading_text = format!("{} Loading...", spinner);

    let loading = Paragraph::new(loading_text)
        .style(
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
        );

    frame.render_widget(loading, area);
}

fn draw_endpoints_list(frame: &mut Frame, app: &App, area: Rect) {
    // No need for chunks since we removed help text - status bar handles it
    let table_area = area;

    let rows: Vec<Row> = app
        .endpoints
        .iter()
        .enumerate()
        .map(|(i, endpoint)| {
            let style = if i == app.selected_index {
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(Color::White)
            };

            // Format dates to shorter format (just date, not time)
            let created_date = endpoint
                .created_at
                .split('T')
                .next()
                .unwrap_or(&endpoint.created_at);
            let updated_date = endpoint
                .updated_at
                .split('T')
                .next()
                .unwrap_or(&endpoint.updated_at);

            let (status_symbol, status_style) = if endpoint.status == "active" {
                ("🟢", style.fg(colors::SUCCESS))
            } else {
                ("🔴", style.fg(colors::ERROR))
            };

            let status_display = format!("{} {}", status_symbol, endpoint.status);

            Row::new(vec![
                Cell::from(endpoint.name.clone()).style(style),
                Cell::from(endpoint.slug.clone()).style(style.fg(colors::PRIMARY)),
                Cell::from(status_display).style(status_style),
                Cell::from(created_date).style(style.fg(colors::MUTED)),
                Cell::from(updated_date).style(style.fg(colors::MUTED)),
            ])
        })
        .collect();

    let headers = Row::new(vec!["Name", "Slug", "Status", "Created", "Updated"])
        .style(
            Style::default()
                .fg(colors::PRIMARY)
                .add_modifier(Modifier::BOLD),
        )
        .bottom_margin(1);

    let endpoints_table = Table::new(
        rows,
        [
            Constraint::Percentage(25), // Name
            Constraint::Percentage(18), // Slug
            Constraint::Percentage(17), // Status (wider for symbols)
            Constraint::Percentage(20), // Created
            Constraint::Percentage(20), // Updated
        ],
    )
    .header(headers)
    .block(
        Block::default()
            .title(" Debug Endpoints ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(Color::Cyan)),
    )
    .row_highlight_style(Style::default().bg(Color::DarkGray))
    .highlight_symbol("> ");

    let mut table_state = TableState::default();
    table_state.select(Some(app.selected_index));
    frame.render_stateful_widget(endpoints_table, table_area, &mut table_state);
}

fn draw_endpoint_detail(frame: &mut Frame, app: &App, area: Rect) {
    let detail_area = area;

    if let Some(endpoint) = &app.selected_endpoint {
        let detail_text = vec![
            Line::from(vec![
                Span::styled(
                    "Name: ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(&endpoint.name),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    "ID: ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(&endpoint.id),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    "Slug: ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(&endpoint.slug),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    "Status: ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(
                    &endpoint.status,
                    if endpoint.status == "active" {
                        Style::default().fg(Color::Green)
                    } else {
                        Style::default().fg(Color::Red)
                    },
                ),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    "Webhook URL: ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(&endpoint.webhook_url, Style::default().fg(Color::Yellow)),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    "Created: ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(&endpoint.created_at),
            ]),
            Line::from(""),
            Line::from(vec![
                Span::styled(
                    "Updated: ",
                    Style::default()
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(&endpoint.updated_at),
            ]),
        ];

        let detail = Paragraph::new(detail_text)
            .block(
                Block::default()
                    .title(format!(" Endpoint: {} ", endpoint.name))
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Cyan)),
            )
            .wrap(Wrap { trim: true });

        frame.render_widget(detail, detail_area);
    }
}

fn draw_requests_list(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Length(3), Constraint::Min(0)])
        .split(area);

    if let Some(endpoint) = &app.selected_endpoint {
        let header = Paragraph::new(format!("Requests for: {}", endpoint.name))
            .style(
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            )
            .alignment(Alignment::Center)
            .block(Block::default().borders(Borders::ALL));

        frame.render_widget(header, chunks[0]);
    }

    if app.requests.is_empty() {
        let no_requests = Paragraph::new("No requests found")
            .style(Style::default().fg(Color::Gray))
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Gray)),
            );

        frame.render_widget(no_requests, chunks[1]);
    } else {
        let rows: Vec<Row> = app
            .requests
            .iter()
            .enumerate()
            .map(|(i, request)| {
                let style = if i == app.selected_request_index {
                    Style::default()
                        .fg(Color::Yellow)
                        .add_modifier(Modifier::BOLD)
                } else {
                    Style::default().fg(Color::White)
                };

                // Format time to shorter format (just time, not date)
                let time_part = request
                    .created_at
                    .split('T')
                    .nth(1)
                    .unwrap_or(&request.created_at);
                let time_short = time_part.split('.').next().unwrap_or(time_part);

                // Format content length to human readable
                let size_str = if request.content_length > 1024 {
                    format!("{:.1}KB", request.content_length as f64 / 1024.0)
                } else {
                    format!("{}B", request.content_length)
                };

                // Get method symbol and color
                let (method_symbol, method_style) = match request.method.as_str() {
                    "GET" => ("🔽", style.fg(colors::INFO)),
                    "POST" => ("📝", style.fg(colors::SUCCESS)),
                    "PUT" => ("📤", style.fg(colors::WARNING)),
                    "DELETE" => ("🗑️", style.fg(colors::ERROR)),
                    "PATCH" => ("✏️", style.fg(colors::ACCENT)),
                    "HEAD" => ("👤", style.fg(colors::MUTED)),
                    "OPTIONS" => ("⚙️", style.fg(colors::PRIMARY)),
                    _ => ("❓", style.fg(colors::TEXT)),
                };

                let method_display = format!("{} {}", method_symbol, request.method);

                let body_preview = request.body_preview.as_deref().unwrap_or("(empty)");
                let body_preview = if body_preview.len() > 80 {
                    format!("{}...", &body_preview[..80])
                } else {
                    body_preview.to_string()
                };

                Row::new(vec![
                    Cell::from(method_display).style(method_style),
                    Cell::from(time_short).style(style.fg(colors::MUTED)),
                    Cell::from(request.remote_addr.clone()).style(style.fg(colors::PRIMARY)),
                    Cell::from(size_str).style(style.fg(colors::MUTED)),
                    Cell::from(body_preview).style(style.fg(colors::MUTED)),
                ])
            })
            .collect();

        let headers = Row::new(vec!["Method", "Time", "From", "Size", "Preview"])
            .style(
                Style::default()
                    .fg(colors::PRIMARY)
                    .add_modifier(Modifier::BOLD),
            )
            .bottom_margin(1);

        let requests_table = Table::new(
            rows,
            [
                Constraint::Percentage(12), // Method (wider for symbols)
                Constraint::Percentage(16), // Time
                Constraint::Percentage(15), // From
                Constraint::Percentage(8),  // Size
                Constraint::Percentage(49), // Preview (adjusted)
            ],
        )
        .header(headers)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        )
        .row_highlight_style(Style::default().bg(Color::DarkGray))
        .highlight_symbol("> ");

        let mut table_state = TableState::default();
        table_state.select(Some(app.selected_request_index));
        frame.render_stateful_widget(requests_table, chunks[1], &mut table_state);
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
            .block(Block::default().borders(Borders::ALL))
            .style(Style::default().fg(Color::White))
            .highlight_style(
                Style::default()
                    .fg(Color::Yellow)
                    .add_modifier(Modifier::BOLD),
            )
            .select(app.current_tab);

        frame.render_widget(tabs, chunks[0]);

        // Tab content
        match app.current_tab {
            0 => draw_info_tab(frame, request, chunks[1]),
            1 => draw_headers_tab(frame, app, request, chunks[1]),
            2 => draw_body_tab(frame, app, request, chunks[1]),
            _ => {}
        }
    }
}

fn draw_info_tab(frame: &mut Frame, request: &crate::models::WebhookRequest, area: Rect) {
    let info_text = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "Method: ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                &request.method,
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "URL: ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(&request.url),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "Remote IP: ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(&request.remote_addr),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "Timestamp: ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(&request.created_at),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "Content Length: ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(request.content_length.to_string()),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "Request ID: ",
                Style::default()
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(&request.id),
        ]),
    ];

    let info = Paragraph::new(info_text)
        .block(
            Block::default()
                .title(" Request Information ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
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
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(value.as_str()),
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
            .border_style(Style::default().fg(Color::Yellow)),
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
                    .border_style(Style::default().fg(Color::Green)),
            );

            frame.render_widget(body, area);
        } else {
            let body = Paragraph::new("(empty body)")
                .style(Style::default().fg(Color::Gray))
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .title(" Body ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Gray)),
                );

            frame.render_widget(body, area);
        }
    } else {
        let body = Paragraph::new("(no body)")
            .style(Style::default().fg(Color::Gray))
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .title(" Body ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Gray)),
            );

        frame.render_widget(body, area);
    }
}

fn draw_error(frame: &mut Frame, error_msg: &str, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(area);

    let error = Paragraph::new(error_msg)
        .style(Style::default().fg(Color::Red))
        .alignment(Alignment::Center)
        .wrap(Wrap { trim: true })
        .block(
            Block::default()
                .title(" Error ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Red)),
        );

    frame.render_widget(error, chunks[0]);

    let help_text = vec![Line::from(vec![
        Span::styled(
            "r",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(": Retry | "),
        Span::styled(
            "c",
            Style::default()
                .fg(Color::Cyan)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(": Change API Key | "),
        Span::styled(
            "q",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw(": Quit"),
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
                    .fg(Color::Cyan)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(&request.method, Style::default().fg(Color::Green)),
            Span::raw(" from "),
            Span::styled(&request.remote_addr, Style::default().fg(Color::Yellow)),
        ])];

        let info = Paragraph::new(request_info).block(
            Block::default()
                .title(" Request to Forward ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Cyan)),
        );

        frame.render_widget(info, chunks[0]);
    }

    // URL input
    let input_block = Block::default()
        .title(" Enter Target URL ")
        .borders(Borders::ALL)
        .border_style(
            if app.is_valid_url(&app.forward_url_input) || app.forward_url_input.is_empty() {
                Style::default().fg(Color::Yellow)
            } else {
                Style::default().fg(Color::Red)
            },
        );

    let input = Paragraph::new(app.forward_url_input.as_str())
        .style(Style::default().fg(Color::White))
        .block(input_block);

    frame.render_widget(input, chunks[1]);

    // Help text
    let help_text = vec![
        Line::from(""),
        Line::from(vec![
            Span::styled(
                "Enter",
                Style::default()
                    .fg(Color::Green)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw(": Forward | "),
            Span::styled(
                "Esc",
                Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
            ),
            Span::raw(": Cancel"),
        ]),
        Line::from(""),
        Line::from(vec![
            Span::raw("Example: "),
            Span::styled(
                "https://your-server.com/webhook",
                Style::default().fg(Color::Cyan),
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
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        )
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Yellow)),
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
            Color::Green
        } else {
            Color::Red
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
                        .fg(Color::Cyan)
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
                        .fg(Color::Cyan)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::styled(&result.target_url, Style::default().fg(Color::Yellow)),
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
                        Cell::from(key.clone()).style(Style::default().fg(Color::Cyan)),
                        Cell::from(value_display),
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
                    .border_style(Style::default().fg(Color::Green)),
            );

            frame.render_widget(headers_table, chunks[1]);
        } else if !result.success {
            // Show error message
            let error_text = result.error_message.as_deref().unwrap_or("Unknown error");
            let error = Paragraph::new(error_text)
                .style(Style::default().fg(Color::Red))
                .wrap(Wrap { trim: true })
                .block(
                    Block::default()
                        .title(" Error Details ")
                        .borders(Borders::ALL)
                        .border_style(Style::default().fg(Color::Red)),
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
            .block(
                Block::default()
                    .title(" Response Body ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Green)),
            )
            .wrap(Wrap { trim: true });

        frame.render_widget(body, chunks[2]);
    }

    // Help
    let help = Paragraph::new(vec![Line::from(vec![
        Span::styled(
            "b/Esc",
            Style::default()
                .fg(Color::Yellow)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(": Back | "),
        Span::styled(
            "q",
            Style::default().fg(Color::Red).add_modifier(Modifier::BOLD),
        ),
        Span::raw(": Quit"),
    ])])
    .alignment(Alignment::Center)
    .block(Block::default().borders(Borders::TOP));

    frame.render_widget(help, chunks[3]);
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
        AppState::InitiatingDeviceFlow => {
            ("🔄 Starting authentication...".to_string(), "Please wait")
        }
        AppState::DisplayingDeviceCode => (
            "🔑 Device Code Ready".to_string(),
            "r: Refresh | Esc/q: Quit",
        ),
        AppState::WaitingForAuth => (
            "⏳ Waiting for authorization...".to_string(),
            "r: Restart | Esc/q: Quit",
        ),
        AppState::Loading => {
            let spinner_chars = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
            let spinner = spinner_chars[app.loading_frame % spinner_chars.len()];
            (format!("{} Loading...", spinner), "Please wait")
        }
        AppState::ShowOrganizations => (
            format!(
                "🏢 Organizations ({}/{})",
                app.selected_organization_index + 1,
                app.organizations.len()
            ),
            "↑/↓: Navigate | Enter: Select | R: Refresh | Q: Quit",
        ),
        AppState::ShowEndpoints => (
            format!(
                "📋 Endpoints ({}/{})",
                app.selected_index + 1,
                app.endpoints.len()
            ),
            "↑/↓: Navigate | Enter: Details | O: Switch Org | L: Logout | R: Refresh | Q: Quit",
        ),
        AppState::ShowEndpointDetail => (
            "🔍 Endpoint Details".to_string(),
            "R: View Requests | B/Esc: Back | Q: Quit",
        ),
        AppState::ShowRequests => {
            let total_requests = app.requests.len();
            let current_req = if total_requests > 0 {
                app.selected_request_index + 1
            } else {
                0
            };
            (
                format!("📨 Requests ({}/{})", current_req, total_requests),
                "↑/↓: Navigate | Enter: Details | ←/→: Pages | B/Esc: Back | Q: Quit",
            )
        }
        AppState::ShowRequestDetail => (
            "📄 Request Details".to_string(),
            "Tab/←→: Switch Tabs | ↑/↓: Scroll | F: Forward | B/Esc: Back | Q: Quit",
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
        AppState::Error(_) => (
            "❌ Error".to_string(),
            "R: Retry | C: Change API Key | Q: Quit",
        ),
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

fn draw_organizations_list(frame: &mut Frame, app: &App, area: Rect) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(3)])
        .split(area);

    // Main organizations list
    let org_items: Vec<ListItem> = app
        .organizations
        .iter()
        .enumerate()
        .map(|(i, org)| {
            let style = if i == app.selected_organization_index {
                Style::default()
                    .bg(colors::PRIMARY)
                    .fg(Color::Black)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(colors::TEXT)
            };

            let content = vec![Line::from(vec![
                Span::styled(&org.name, style),
                if org.signing_secret_prefix.is_some() {
                    Span::styled(" 🔐", Style::default().fg(colors::SUCCESS))
                } else {
                    Span::raw("")
                },
            ])];

            ListItem::new(content).style(style)
        })
        .collect();

    let orgs_list = List::new(org_items)
        .block(
            Block::default()
                .title(" Select Organization ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(colors::PRIMARY)),
        )
        .highlight_style(
            Style::default()
                .bg(colors::PRIMARY)
                .add_modifier(Modifier::BOLD),
        );

    frame.render_widget(orgs_list, chunks[0]);

    // Help text
    let help_text = vec![Line::from(vec![
        Span::styled(
            "↑/↓",
            Style::default()
                .fg(colors::SECONDARY)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(": Navigate | "),
        Span::styled(
            "Enter",
            Style::default()
                .fg(colors::SUCCESS)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(": Select | "),
        Span::styled(
            "r",
            Style::default()
                .fg(colors::WARNING)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(": Refresh | "),
        Span::styled(
            "q",
            Style::default()
                .fg(colors::ERROR)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw(": Quit"),
    ])];

    let help = Paragraph::new(help_text)
        .alignment(Alignment::Center)
        .block(Block::default().borders(Borders::TOP));

    frame.render_widget(help, chunks[1]);
}
