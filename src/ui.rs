use crate::app::{
    App, AppState, DetailSearchTarget, FeedbackKind, TRUNCATED_BODY_MARKER, TunnelRequest,
    detail_label_width, detail_match_indices, detail_value_width, sorted_headers,
};
use crate::syntax::JsonHighlighter;
use crate::theme as colors;
use ratatui::{
    prelude::*,
    widgets::{
        Block, Borders, Cell, Paragraph, Row, Scrollbar, ScrollbarOrientation, ScrollbarState,
        Table, TableState, Tabs, Wrap,
    },
};

pub fn draw(frame: &mut Frame, app: &App) {
    let footer_height = if app.available_update.is_some() { 2 } else { 1 };
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .margin(1)
        .constraints([
            Constraint::Min(0),                // Main content
            Constraint::Length(footer_height), // Update notice and status bar
        ])
        .split(frame.area());

    // Draw main content
    match &app.state {
        AppState::ShowRequestDetail => draw_request_detail(frame, app, chunks[0]),
        AppState::InputForwardUrl => draw_forward_url_input(frame, app, chunks[0]),
        AppState::ForwardingRequest => draw_forwarding(frame, app, chunks[0]),
        AppState::ForwardResult => draw_forward_result(frame, app, chunks[0]),
        AppState::Listening => draw_listening(frame, app, chunks[0]),
        AppState::Tunneling => {
            draw_tunneling(frame, app, chunks[0]);
            if app.tunnel_actions_open {
                draw_tunnel_actions(frame, app, chunks[0]);
            }
        }
        AppState::ExportMenu => draw_export_menu(frame, app, chunks[0]),
        AppState::Error { message, hint } => {
            draw_error(frame, app, message, hint.as_deref(), chunks[0])
        }
    }

    if app.available_update.is_some() && chunks[1].height >= 2 {
        let footer = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Length(1), Constraint::Length(1)])
            .split(chunks[1]);
        draw_update_warning(frame, app, footer[0]);
        draw_status_bar(frame, app, footer[1]);
    } else {
        draw_status_bar(frame, app, chunks[1]);
    }

    if app.monochrome {
        for cell in &mut frame.buffer_mut().content {
            cell.fg = Color::Reset;
            cell.bg = Color::Reset;
            cell.underline_color = Color::Reset;
        }
    }
}

fn feedback_color(kind: FeedbackKind) -> Color {
    match kind {
        FeedbackKind::Success => colors::SUCCESS,
        FeedbackKind::Info => colors::INFO,
        FeedbackKind::Warning => colors::WARNING,
        FeedbackKind::Error => colors::ERROR,
    }
}

fn feedback_label(kind: FeedbackKind) -> &'static str {
    match kind {
        FeedbackKind::Success => "OK",
        FeedbackKind::Info => "INFO",
        FeedbackKind::Warning => "WARN",
        FeedbackKind::Error => "ERR",
    }
}

fn draw_update_warning(frame: &mut Frame, app: &App, area: Rect) {
    let Some(new_version) = app.available_update.as_deref() else {
        return;
    };
    let text = format!(
        "[{}] UPDATE AVAILABLE   {} → {}   Run `hooklistener update`",
        feedback_label(FeedbackKind::Warning),
        env!("CARGO_PKG_VERSION"),
        new_version
    );
    let warning = Paragraph::new(Line::from(Span::styled(
        text,
        Style::default()
            .fg(feedback_color(FeedbackKind::Warning))
            .add_modifier(Modifier::BOLD),
    )));

    frame.render_widget(warning, area);
}

fn listening_history_title(app: &App) -> String {
    let retained = app.listening_requests.len();
    let total = app.listening_stats.total_requests;
    if total > retained as u64 {
        format!(" Live Requests ({retained} retained of {total}) ")
    } else {
        " Live Requests ".to_string()
    }
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
                    "ENDPOINT  ",
                    Style::default()
                        .fg(colors::PRIMARY)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(&app.listening_endpoint),
            ]),
            Line::from(vec![
                Span::styled(
                    "TARGET    ",
                    Style::default()
                        .fg(colors::PRIMARY)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(&app.listening_target),
            ]),
            Line::from(vec![
                Span::styled(
                    "STATUS    ",
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
                "STATUS    ",
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
                    "ENDPOINT  ",
                    Style::default()
                        .fg(colors::PRIMARY)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(&app.listening_endpoint),
            ]),
            Line::from(vec![
                Span::styled(
                    "TARGET    ",
                    Style::default()
                        .fg(colors::PRIMARY)
                        .add_modifier(Modifier::BOLD),
                ),
                Span::raw(&app.listening_target),
            ]),
            Line::from(vec![
                Span::styled(
                    "STATUS    ",
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
            Span::styled("TOTAL    ", Style::default().fg(colors::TEXT)),
            Span::styled(
                app.listening_stats.total_requests.to_string(),
                Style::default()
                    .fg(colors::INFO)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("SUCCESS  ", Style::default().fg(colors::TEXT)),
            Span::styled(
                app.listening_stats.successful_forwards.to_string(),
                Style::default()
                    .fg(colors::SUCCESS)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled("FAILED   ", Style::default().fg(colors::TEXT)),
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
        draw_search_bar(frame, app, chunks[1]);
    }

    let requests_area = chunks[2];

    let filtered_indices =
        crate::app::App::filter_requests(&app.listening_requests, &app.search_query);

    // Requests List
    if filtered_indices.is_empty() {
        let message = if app.listening_requests.is_empty() {
            "Waiting for webhooks..."
        } else {
            "No matching requests"
        };
        let no_requests_block = Block::default()
            .borders(Borders::ALL)
            .title(listening_history_title(app))
            .border_style(Style::default().fg(colors::MUTED));

        let mut lines = Vec::new();
        if app.listening_requests.is_empty() {
            lines.extend(logo_frame_lines(
                app,
                no_requests_block.inner(requests_area),
                1,
            ));
        }
        lines.push(Line::from(Span::styled(
            message,
            Style::default().fg(colors::MUTED),
        )));

        let no_requests = Paragraph::new(lines)
            .alignment(Alignment::Center)
            .block(no_requests_block);

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

                let method_style = match request.method.as_str() {
                    "GET" => style.fg(colors::INFO),
                    "POST" => style.fg(colors::SUCCESS),
                    "PUT" => style.fg(colors::WARNING),
                    "DELETE" => style.fg(colors::ERROR),
                    "PATCH" => style.fg(colors::ACCENT),
                    _ => style.fg(colors::TEXT),
                };

                Row::new(vec![
                    Cell::from(time_display).style(style.fg(colors::MUTED)),
                    Cell::from(request.method.clone()).style(method_style),
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
                .title(listening_history_title(app))
                .borders(Borders::ALL)
                .border_style(Style::default().fg(colors::PRIMARY)),
        )
        .row_highlight_style(Style::default().add_modifier(Modifier::BOLD))
        .highlight_symbol("> ");

        let mut table_state = TableState::default();
        table_state.select(Some(app.selected_request_index));

        frame.render_stateful_widget(requests_table, requests_area, &mut table_state);
    }
}

fn draw_search_bar(frame: &mut Frame, app: &App, area: Rect) {
    let search_border_color = if app.search_active {
        colors::PRIMARY
    } else {
        colors::MUTED
    };
    let cursor = if app.search_active { "▎" } else { "" };
    let title = if matches!(app.state, AppState::Tunneling) {
        if area.width >= 74 {
            " Filter: method: status: path: header: pinned: "
        } else {
            " Filter: field:value "
        }
    } else {
        " Filter "
    };
    let search = Paragraph::new(format!("/{}{}", app.search_query, cursor))
        .style(Style::default().fg(colors::TEXT))
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(search_border_color)),
        );
    frame.render_widget(search, area);
}

struct TunnelMetric {
    label: &'static str,
    value: String,
    value_color: Color,
    min_width: u16,
}

impl TunnelMetric {
    fn new(
        label: &'static str,
        value: impl Into<String>,
        value_color: Color,
        min_width: u16,
    ) -> Self {
        Self {
            label,
            value: value.into(),
            value_color,
            min_width,
        }
    }

    fn width(&self) -> u16 {
        let content_width = self.label.chars().count() + 1 + self.value.chars().count() + 2;
        let content_width = u16::try_from(content_width).unwrap_or(u16::MAX);

        self.min_width.max(content_width)
    }
}

fn tunnel_metric_cells(
    app: &App,
    avg_duration: String,
    last_display: String,
    last_color: Color,
    pinned_count: usize,
) -> Vec<TunnelMetric> {
    let (follow_display, follow_color) = if app.is_tunnel_follow_pinned() {
        ("Paused", colors::WARNING)
    } else {
        ("Live", colors::SUCCESS)
    };
    let (view_display, view_color) = if app.tunnel_pinned_only {
        ("Pinned", colors::ACCENT)
    } else {
        ("All", colors::MUTED)
    };

    let mut metrics = vec![
        TunnelMetric::new(
            "Requests",
            app.tunnel_stats.total.to_string(),
            colors::INFO,
            11,
        ),
        TunnelMetric::new(
            "2xx",
            app.tunnel_stats.status_2xx.to_string(),
            colors::SUCCESS,
            7,
        ),
        TunnelMetric::new(
            "4xx",
            app.tunnel_stats.status_4xx.to_string(),
            colors::WARNING,
            7,
        ),
        TunnelMetric::new(
            "5xx",
            app.tunnel_stats.status_5xx.to_string(),
            colors::ERROR,
            7,
        ),
        TunnelMetric::new("Err", app.tunnel_stats.failed.to_string(), colors::ERROR, 7),
    ];

    if pinned_count > 0 {
        metrics.push(TunnelMetric::new(
            "Pins",
            pinned_count.to_string(),
            colors::ACCENT,
            7,
        ));
    }

    metrics.push(TunnelMetric::new(
        "Follow",
        follow_display,
        follow_color,
        13,
    ));
    metrics.push(TunnelMetric::new("View", view_display, view_color, 10));
    metrics.push(TunnelMetric::new("Avg", avg_duration, colors::SECONDARY, 8));
    metrics.push(TunnelMetric::new("Last", last_display, last_color, 9));

    metrics
}

fn render_tunnel_metric_row(frame: &mut Frame, area: Rect, metrics: &[TunnelMetric]) {
    if area.is_empty() {
        return;
    }

    let right = area.x.saturating_add(area.width);
    let mut x = area.x;

    for metric in metrics {
        let width = metric.width();
        if x.saturating_add(width) > right {
            break;
        }

        let metric_area = Rect::new(x, area.y, width, area.height);
        let metric_text = Line::from(vec![
            Span::styled(metric.label, Style::default().fg(colors::MUTED)),
            Span::raw(" "),
            Span::styled(
                metric.value.as_str(),
                Style::default()
                    .fg(metric.value_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]);

        frame.render_widget(Paragraph::new(metric_text), metric_area);
        x = x.saturating_add(width);
    }
}

fn tunnel_table_constraints(width: u16) -> [Constraint; 8] {
    if width < 90 {
        [
            Constraint::Length(8),  // ID
            Constraint::Length(5),  // Age
            Constraint::Length(7),  // Method
            Constraint::Min(14),    // Path
            Constraint::Length(7),  // Status
            Constraint::Length(9),  // Duration
            Constraint::Length(0),  // Size moves to details at narrow widths
            Constraint::Length(15), // From
        ]
    } else {
        [
            Constraint::Length(8),  // ID
            Constraint::Length(6),  // Age
            Constraint::Length(8),  // Method
            Constraint::Min(20),    // Path
            Constraint::Length(8),  // Status
            Constraint::Length(10), // Duration
            Constraint::Length(8),  // Size
            Constraint::Length(15), // From
        ]
    }
}

fn format_tunnel_request_age(received_at: std::time::Instant) -> String {
    let elapsed = received_at.elapsed();
    let total_seconds = elapsed.as_secs();

    if total_seconds < 1 {
        "now".to_string()
    } else if total_seconds < 60 {
        format!("{}s", total_seconds)
    } else if total_seconds < 3600 {
        format!("{}m", total_seconds / 60)
    } else {
        format!("{}h", total_seconds / 3600)
    }
}

fn format_request_duration(
    received_at: std::time::Instant,
    completed_at: Option<std::time::Instant>,
) -> String {
    completed_at
        .map(|completed_at| format!("{}ms", completed_at.duration_since(received_at).as_millis()))
        .unwrap_or_else(|| "--".to_string())
}

fn format_body_size(body: Option<&str>) -> String {
    let Some(body) = body else {
        return "--".to_string();
    };

    let bytes = body.len();
    if bytes < 1024 {
        format!("{}b", bytes)
    } else if bytes < 1024 * 1024 {
        format!("{:.1}kb", bytes as f64 / 1024.0)
    } else {
        format!("{:.1}mb", bytes as f64 / (1024.0 * 1024.0))
    }
}

fn method_color(method: &str) -> Color {
    match method {
        "GET" | "HEAD" => colors::INFO,
        "POST" => colors::SUCCESS,
        "PUT" | "PATCH" => colors::WARNING,
        "DELETE" => colors::ERROR,
        "OPTIONS" => colors::PRIMARY,
        _ => colors::TEXT,
    }
}

fn header_value<'a>(
    headers: &'a std::collections::HashMap<String, String>,
    name: &str,
) -> Option<&'a str> {
    headers
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.trim())
        .filter(|value| !value.is_empty())
}

fn parse_forwarded_source(value: &str) -> Option<String> {
    value.split(';').find_map(|part| {
        let (key, value) = part.split_once('=')?;
        if key.trim().eq_ignore_ascii_case("for") {
            Some(value.trim().trim_matches('"').to_string())
        } else {
            None
        }
    })
}

fn tunnel_request_source(headers: &std::collections::HashMap<String, String>) -> String {
    if let Some(value) =
        header_value(headers, "cf-connecting-ip").or_else(|| header_value(headers, "x-real-ip"))
    {
        return value.to_string();
    }

    if let Some(value) = header_value(headers, "x-forwarded-for")
        && let Some(first_ip) = value.split(',').map(str::trim).find(|ip| !ip.is_empty())
    {
        return first_ip.to_string();
    }

    if let Some(value) = header_value(headers, "forwarded")
        && let Some(source) = parse_forwarded_source(value)
    {
        return source;
    }

    "--".to_string()
}

fn tunnel_request_path(request: &TunnelRequest) -> String {
    if request.query_string.is_empty() {
        request.path.clone()
    } else {
        format!("{}?{}", request.path, request.query_string)
    }
}

fn tunnel_status_display(request: &TunnelRequest) -> (String, Color) {
    if let Some(status) = request.status {
        (status.to_string(), colors::for_http_status(status))
    } else if request.error.is_some() {
        ("ERR".to_string(), colors::ERROR)
    } else {
        ("...".to_string(), colors::WARNING)
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum TunnelRequestTone {
    Success,
    ClientError,
    ServerError,
    Failed,
    Pending,
    Other,
}

fn tunnel_request_tone(request: &TunnelRequest) -> TunnelRequestTone {
    if request.error.is_some() {
        return TunnelRequestTone::Failed;
    }

    match request.status {
        Some(200..=299) => TunnelRequestTone::Success,
        Some(400..=499) => TunnelRequestTone::ClientError,
        Some(500..=599) => TunnelRequestTone::ServerError,
        Some(_) => TunnelRequestTone::Other,
        None => TunnelRequestTone::Pending,
    }
}

fn tunnel_row_style(request: &TunnelRequest) -> Style {
    let tone = tunnel_request_tone(request);
    let mut style = Style::default();
    if matches!(
        tone,
        TunnelRequestTone::ServerError | TunnelRequestTone::Failed
    ) {
        style = style.add_modifier(Modifier::BOLD);
    }

    style
}

fn request_duration_ms(request: &TunnelRequest) -> Option<u128> {
    request
        .completed_at
        .map(|completed_at| completed_at.duration_since(request.received_at).as_millis())
}

fn request_duration_color(request: &TunnelRequest) -> Color {
    match request_duration_ms(request) {
        Some(0..=99) => colors::MUTED,
        Some(100..=999) => colors::SECONDARY,
        Some(_) => colors::WARNING,
        None => colors::WARNING,
    }
}

fn compact_tunnel_request_id(request_id: &str) -> String {
    let compact: String = request_id.chars().take(6).collect();
    if compact.is_empty() {
        "--".to_string()
    } else {
        compact
    }
}

fn tunnel_request_id_display(request: &TunnelRequest) -> String {
    let request_id = compact_tunnel_request_id(&request.request_id);
    if request.pinned {
        format!("*{}", request_id)
    } else {
        request_id
    }
}

fn truncate_table_value(value: &str, max_chars: usize) -> String {
    let mut chars = value.chars();
    let truncated: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        format!("{}...", truncated)
    } else {
        truncated
    }
}

fn tunnel_request_table_row(request: &TunnelRequest) -> Row<'static> {
    let age_display = format_tunnel_request_age(request.received_at);
    let method_fg = method_color(request.method.as_str());
    let path_display = tunnel_request_path(request);
    let (status_display, status_color) = tunnel_status_display(request);
    let duration_display = format_request_duration(request.received_at, request.completed_at);
    let size_display = format_body_size(request.body.as_deref());
    let source_display = tunnel_request_source(&request.headers);

    Row::new(vec![
        Cell::from(tunnel_request_id_display(request)).style(Style::default().fg(colors::ACCENT)),
        Cell::from(age_display).style(Style::default().fg(colors::MUTED)),
        Cell::from(request.method.clone()).style(Style::default().fg(method_fg)),
        Cell::from(path_display).style(Style::default().fg(colors::TEXT)),
        Cell::from(status_display).style(Style::default().fg(status_color)),
        Cell::from(duration_display).style(Style::default().fg(request_duration_color(request))),
        Cell::from(size_display).style(Style::default().fg(colors::MUTED)),
        Cell::from(source_display).style(Style::default().fg(colors::MUTED)),
    ])
    .style(tunnel_row_style(request))
}

fn compact_content_type(headers: &std::collections::HashMap<String, String>) -> Option<String> {
    header_value(headers, "content-type")
        .and_then(|value| value.split(';').next())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(|value| truncate_table_value(value, 34))
}

fn format_header_count(count: usize) -> String {
    if count == 1 {
        "1 header".to_string()
    } else {
        format!("{} headers", count)
    }
}

fn tunnel_metadata_summary(
    headers: Option<&std::collections::HashMap<String, String>>,
    body: Option<&str>,
) -> String {
    let header_count = headers.map_or(0, std::collections::HashMap::len);
    let content_type = headers
        .and_then(compact_content_type)
        .unwrap_or_else(|| "--".to_string());

    format!(
        "{} | type {} | body {}",
        format_header_count(header_count),
        content_type,
        format_body_size(body)
    )
}

fn tunnel_response_metadata_summary(request: &TunnelRequest) -> String {
    if let Some(error) = &request.error {
        return format!("error {}", truncate_table_value(error, 120));
    }

    if request.status.is_none()
        && request.response_headers.is_none()
        && request.response_body.as_deref().is_none_or(str::is_empty)
    {
        return "pending".to_string();
    }

    tunnel_metadata_summary(
        request.response_headers.as_ref(),
        request.response_body.as_deref(),
    )
}

fn owned_line(line: Line<'_>) -> Line<'static> {
    Line {
        spans: line
            .spans
            .into_iter()
            .map(|span| Span::styled(span.content.into_owned(), span.style))
            .collect(),
        style: line.style,
        alignment: line.alignment,
    }
}

fn try_pretty_json_preview(trimmed_body: &str) -> Option<String> {
    if !trimmed_body.starts_with('{') && !trimmed_body.starts_with('[') {
        return None;
    }

    serde_json::from_str::<serde_json::Value>(trimmed_body)
        .ok()
        .and_then(|value| serde_json::to_string_pretty(&value).ok())
}

fn format_preview_body(body: &str) -> String {
    let (body, was_truncated) = body
        .strip_suffix(TRUNCATED_BODY_MARKER)
        .map_or((body, false), |body| (body, true));
    let trimmed = body.trim();

    let mut preview = try_pretty_json_preview(trimmed).unwrap_or_else(|| body.to_string());

    if was_truncated {
        if !preview.ends_with('\n') {
            preview.push('\n');
        }
        preview.push_str("...(truncated)");
    }

    preview
}

fn preview_body_lines(body: Option<&str>, max_lines: usize) -> Vec<Line<'static>> {
    let Some(body) = body.filter(|body| !body.is_empty()) else {
        return Vec::new();
    };

    let preview = format_preview_body(body);
    let highlighted_lines = JsonHighlighter::highlight_json(&preview);
    let total_lines = highlighted_lines.len();

    let mut lines: Vec<Line<'static>> = highlighted_lines
        .into_iter()
        .take(max_lines)
        .map(owned_line)
        .collect();

    if total_lines > max_lines {
        lines.push(Line::from(Span::styled(
            format!("... {} more lines", total_lines - max_lines),
            Style::default().fg(colors::MUTED),
        )));
    }

    lines
}

fn tunnel_expanded_section_cell(
    title: &'static str,
    summary: String,
    title_color: Color,
) -> Cell<'static> {
    Cell::from(Line::from(vec![
        Span::styled(
            format!("{:<9}", title),
            Style::default()
                .fg(title_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::styled(summary, Style::default().fg(colors::MUTED)),
    ]))
}

fn tunnel_expanded_request_section_row(request: &TunnelRequest) -> Row<'static> {
    Row::new(vec![
        Cell::from(""),
        Cell::from(""),
        Cell::from(""),
        tunnel_expanded_section_cell(
            "Request",
            tunnel_metadata_summary(Some(&request.headers), request.body.as_deref()),
            colors::PRIMARY,
        ),
        Cell::from(""),
        Cell::from(""),
        Cell::from(format_body_size(request.body.as_deref()))
            .style(Style::default().fg(colors::TEXT)),
        Cell::from(tunnel_request_source(&request.headers))
            .style(Style::default().fg(colors::TEXT)),
    ])
    .style(tunnel_row_style(request))
}

fn tunnel_expanded_response_section_row(request: &TunnelRequest) -> Row<'static> {
    let (status_display, status_color) = tunnel_status_display(request);

    Row::new(vec![
        Cell::from(""),
        Cell::from(""),
        Cell::from(""),
        tunnel_expanded_section_cell(
            "Response",
            tunnel_response_metadata_summary(request),
            colors::SECONDARY,
        ),
        Cell::from(status_display).style(Style::default().fg(status_color)),
        Cell::from(format_request_duration(
            request.received_at,
            request.completed_at,
        ))
        .style(Style::default().fg(request_duration_color(request))),
        Cell::from(format_body_size(request.response_body.as_deref()))
            .style(Style::default().fg(colors::TEXT)),
        Cell::from(""),
    ])
    .style(tunnel_row_style(request))
}

fn tunnel_expanded_preview_row(
    request: &TunnelRequest,
    label: &'static str,
    preview_line: Line<'static>,
) -> Row<'static> {
    let mut spans = vec![Span::styled(
        format!("{:<9}", label),
        Style::default().fg(colors::MUTED),
    )];
    spans.extend(preview_line.spans);

    Row::new(vec![
        Cell::from(""),
        Cell::from(""),
        Cell::from(""),
        Cell::from(Line {
            spans,
            style: preview_line.style,
            alignment: preview_line.alignment,
        }),
        Cell::from(""),
        Cell::from(""),
        Cell::from(""),
        Cell::from(""),
    ])
    .style(tunnel_row_style(request))
}

fn tunnel_expanded_body_rows(
    request: &TunnelRequest,
    label: &'static str,
    body: Option<&str>,
) -> Vec<Row<'static>> {
    preview_body_lines(body, 3)
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            tunnel_expanded_preview_row(request, if index == 0 { label } else { "" }, line)
        })
        .collect()
}

fn tunnel_request_expanded_rows(request: &TunnelRequest) -> Vec<Row<'static>> {
    let mut rows = vec![tunnel_expanded_request_section_row(request)];
    rows.extend(tunnel_expanded_body_rows(
        request,
        "req body",
        request.body.as_deref(),
    ));

    rows.push(tunnel_expanded_response_section_row(request));

    if request.error.is_none() {
        rows.extend(tunnel_expanded_body_rows(
            request,
            "res body",
            request.response_body.as_deref(),
        ));
    }

    rows
}

fn tunnel_test_curl(subdomain: Option<&str>) -> Option<String> {
    subdomain
        .filter(|subdomain| !subdomain.is_empty())
        .map(|subdomain| format!("curl -i https://{}/test", subdomain.trim_end_matches('/')))
}

fn logo_frame_lines(app: &App, content_area: Rect, following_lines: usize) -> Vec<Line<'static>> {
    let Some(frame) = app.logo_frame.as_deref() else {
        return Vec::new();
    };

    let frame_lines = frame.lines().collect::<Vec<_>>();
    let max_width = frame_lines
        .iter()
        .map(|line| line.chars().count())
        .max()
        .unwrap_or(0);
    let content_width = content_area.width as usize;
    let content_height = content_area.height as usize;
    let required_height = frame_lines.len() + following_lines + 1;

    if frame_lines.is_empty() || max_width > content_width || required_height > content_height {
        return Vec::new();
    }

    let top_padding = content_height.saturating_sub(required_height) / 2;
    let mut lines = std::iter::repeat_with(|| Line::from(""))
        .take(top_padding)
        .chain(frame_lines.into_iter().map(|line| {
            Line::from(Span::styled(
                line.to_string(),
                Style::default().fg(colors::ACCENT),
            ))
        }))
        .collect::<Vec<_>>();
    lines.push(Line::from(""));
    lines
}

fn tunnel_empty_block(title: &'static str) -> Block<'static> {
    Block::default()
        .title(title)
        .borders(Borders::TOP)
        .border_style(Style::default().fg(colors::MUTED))
}

fn render_tunnel_empty_lines(
    frame: &mut Frame,
    area: Rect,
    title: &'static str,
    lines: Vec<Line<'static>>,
) {
    let empty = Paragraph::new(lines)
        .alignment(Alignment::Center)
        .block(tunnel_empty_block(title));

    frame.render_widget(empty, area);
}

fn draw_tunnel_empty_state(frame: &mut Frame, app: &App, area: Rect) {
    let command = tunnel_test_curl(app.tunnel_subdomain.as_deref());
    let following_lines = if command.is_some() { 5 } else { 4 };
    let content_area = tunnel_empty_block(" Live Requests ").inner(area);
    let mut lines = logo_frame_lines(app, content_area, following_lines);

    if lines.is_empty() {
        lines.push(Line::from(""));
    }

    lines.push(Line::from(Span::styled(
        "Waiting for requests",
        Style::default()
            .fg(colors::TEXT)
            .add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    if let Some(command) = command {
        lines.push(Line::from(Span::styled(
            "Try",
            Style::default().fg(colors::MUTED),
        )));
        lines.push(Line::from(Span::styled(
            command,
            Style::default()
                .fg(colors::PRIMARY)
                .add_modifier(Modifier::BOLD),
        )));
    } else {
        lines.push(Line::from(Span::styled(
            "Tunnel URL will appear when the connection is ready",
            Style::default().fg(colors::MUTED),
        )));
    }

    render_tunnel_empty_lines(frame, area, " Live Requests ", lines);
}

fn draw_tunnel_no_matches(frame: &mut Frame, app: &App, area: Rect) {
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "No matching requests",
            Style::default()
                .fg(colors::TEXT)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("Filter ", Style::default().fg(colors::MUTED)),
            Span::styled(
                format!("/{}", app.search_query),
                Style::default().fg(colors::PRIMARY),
            ),
        ]),
        Line::from(Span::styled(
            "Esc clears the filter",
            Style::default().fg(colors::MUTED),
        )),
    ];

    render_tunnel_empty_lines(frame, area, " Live Requests ", lines);
}

fn draw_tunnel_no_pinned(frame: &mut Frame, area: Rect) {
    let lines = vec![
        Line::from(""),
        Line::from(Span::styled(
            "No pinned requests",
            Style::default()
                .fg(colors::TEXT)
                .add_modifier(Modifier::BOLD),
        )),
        Line::from(""),
        Line::from(vec![
            Span::styled("Press ", Style::default().fg(colors::MUTED)),
            Span::styled(
                "Tab",
                Style::default()
                    .fg(colors::SECONDARY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(" for all requests", Style::default().fg(colors::MUTED)),
        ]),
        Line::from(vec![
            Span::styled("Use ", Style::default().fg(colors::MUTED)),
            Span::styled(
                "P",
                Style::default()
                    .fg(colors::SECONDARY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                " to pin a selected request",
                Style::default().fg(colors::MUTED),
            ),
        ]),
    ];

    render_tunnel_empty_lines(frame, area, " Pinned Requests ", lines);
}

fn draw_tunneling(frame: &mut Frame, app: &App, area: Rect) {
    let header_height = if app.tunnel_status_message.is_some() {
        5
    } else {
        4
    };
    let show_search = app.search_active || !app.search_query.is_empty();
    let pinned_count = app.tunnel_pinned_count();

    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints(if show_search {
            vec![
                Constraint::Length(header_height), // Compact tunnel header
                Constraint::Length(3),             // Statistics
                Constraint::Length(3),             // Filter
                Constraint::Min(0),                // Requests table
            ]
        } else {
            vec![
                Constraint::Length(header_height), // Compact tunnel header
                Constraint::Length(3),             // Statistics
                Constraint::Length(0),             // No filter
                Constraint::Min(0),                // Requests table
            ]
        })
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
        "connected"
    } else if let Some(err) = &app.tunnel_error {
        err.as_str()
    } else {
        "connecting"
    };

    let uptime_text = if let Some(connected_at) = app.tunnel_connected_at {
        let elapsed = connected_at.elapsed();
        let total_seconds = elapsed.as_secs();
        let hours = total_seconds / 3600;
        let minutes = (total_seconds % 3600) / 60;
        let seconds = total_seconds % 60;

        if hours > 0 {
            format!("{}h {}m", hours, minutes)
        } else if minutes > 0 {
            format!("{}m {}s", minutes, seconds)
        } else {
            format!("{}s", seconds)
        }
    } else {
        String::new()
    };

    let mode_text = if app.tunnel_is_static {
        "static"
    } else {
        "ephemeral"
    };

    let mut header_text = vec![
        Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "hooklistener",
                Style::default()
                    .fg(colors::TEXT)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                format!("{} {}", status_symbol, status_text),
                Style::default()
                    .fg(status_color)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::raw("  "),
            Span::styled(
                mode_text,
                Style::default()
                    .fg(colors::ACCENT)
                    .add_modifier(Modifier::BOLD),
            ),
            if !uptime_text.is_empty() {
                Span::styled(
                    format!("  uptime {}", uptime_text),
                    Style::default().fg(colors::MUTED),
                )
            } else {
                Span::raw("")
            },
        ]),
        Line::from(vec![
            Span::raw(" "),
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
    ];

    // Show tunnel status message (e.g. "URL copied to clipboard!")
    if let Some(feedback) = &app.tunnel_status_message {
        header_text.push(Line::from(Span::styled(
            format!(" [{}] {}", feedback_label(feedback.kind), feedback.message),
            Style::default()
                .fg(feedback_color(feedback.kind))
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
    let avg_duration = app
        .tunnel_stats
        .total_duration_ms
        .checked_div(app.tunnel_stats.success)
        .map(|duration| format!("{}ms", duration))
        .unwrap_or_else(|| "--".to_string());

    let (last_display, last_color) = app
        .tunnel_requests
        .back()
        .map(|request| {
            if let Some(status) = request.status {
                let duration = request
                    .completed_at
                    .map(|completed_at| completed_at.duration_since(request.received_at));
                let display = duration
                    .map(|duration| format!("{} {}ms", status, duration.as_millis()))
                    .unwrap_or_else(|| status.to_string());
                (display, colors::for_http_status(status))
            } else if request.error.is_some() {
                ("error".to_string(), colors::ERROR)
            } else {
                ("pending".to_string(), colors::WARNING)
            }
        })
        .unwrap_or_else(|| ("--".to_string(), colors::MUTED));

    let metrics = tunnel_metric_cells(app, avg_duration, last_display, last_color, pinned_count);

    let stats_block = Block::default()
        .title(" Statistics ")
        .borders(Borders::TOP)
        .border_style(Style::default().fg(colors::MUTED));
    let stats_inner = stats_block.inner(chunks[1]);
    let metrics_area = Rect {
        x: stats_inner.x.saturating_add(1),
        width: stats_inner.width.saturating_sub(1),
        ..stats_inner
    };

    frame.render_widget(stats_block, chunks[1]);
    render_tunnel_metric_row(frame, metrics_area, &metrics);

    if show_search {
        draw_search_bar(frame, app, chunks[2]);
    }

    let requests_area = chunks[3];

    // Live Requests table
    if app.tunnel_requests.is_empty() {
        draw_tunnel_empty_state(frame, app, requests_area);
    } else {
        let filtered_indices = app.visible_tunnel_request_indices();

        if filtered_indices.is_empty() {
            if app.tunnel_pinned_only && pinned_count == 0 {
                draw_tunnel_no_pinned(frame, requests_area);
            } else {
                draw_tunnel_no_matches(frame, app, requests_area);
            }
            return;
        }

        let table_area = requests_area;
        let available_rows = table_area.height.saturating_sub(3) as usize; // top border + header + header margin

        if available_rows == 0 {
            let compact = Paragraph::new("Expand terminal height to view requests")
                .style(Style::default().fg(colors::MUTED))
                .alignment(Alignment::Center)
                .block(
                    Block::default()
                        .title(format!(
                            " {} Requests ({}) ",
                            if app.tunnel_pinned_only {
                                "Pinned"
                            } else {
                                "Live"
                            },
                            filtered_indices.len()
                        ))
                        .borders(Borders::TOP)
                        .border_style(Style::default().fg(colors::MUTED)),
                );
            frame.render_widget(compact, table_area);
            return;
        }

        // Keep the selected row inside the visible window.
        let selected_expanded_rows = filtered_indices
            .get(app.tunnel_selected_index)
            .and_then(|request_index| app.tunnel_requests.get(*request_index))
            .filter(|request| {
                app.tunnel_expanded_request_id.as_deref() == Some(request.request_id.as_str())
            })
            .map(|request| tunnel_request_expanded_rows(request).len())
            .unwrap_or(0);
        let available_request_rows = available_rows.saturating_sub(selected_expanded_rows);
        let start_idx = app
            .tunnel_selected_index
            .saturating_sub(available_request_rows.saturating_sub(1));
        let end_idx = (start_idx + available_rows).min(filtered_indices.len());
        let visible_indices = &filtered_indices[start_idx..end_idx];

        let mut rows: Vec<Row> = Vec::new();
        let mut selected_row_index = None;
        for (visible_offset, request_index) in visible_indices.iter().enumerate() {
            let Some(request) = app.tunnel_requests.get(*request_index) else {
                continue;
            };
            let request_position = start_idx + visible_offset;
            if request_position == app.tunnel_selected_index {
                selected_row_index = Some(rows.len());
            }
            rows.push(tunnel_request_table_row(request));

            if app.tunnel_expanded_request_id.as_deref() == Some(request.request_id.as_str()) {
                rows.extend(tunnel_request_expanded_rows(request));
            }
        }

        let headers = Row::new(vec![
            "ID", "Age", "Method", "Path", "Status", "Duration", "Size", "From",
        ])
        .style(
            Style::default()
                .fg(colors::PRIMARY)
                .add_modifier(Modifier::BOLD),
        )
        .bottom_margin(1);

        let filtered_from = if app.tunnel_pinned_only {
            pinned_count
        } else {
            app.tunnel_requests.len()
        };
        let filter_suffix = if app.search_query.is_empty() {
            String::new()
        } else {
            format!(" filtered from {}", filtered_from)
        };
        let pinned_suffix = if pinned_count > 0 {
            format!(" | {} pinned", pinned_count)
        } else {
            String::new()
        };
        let view_title = if app.tunnel_pinned_only {
            "Pinned Requests"
        } else {
            "Live Requests"
        };
        let title = if filtered_indices.len() > available_rows {
            format!(
                " {} ({}-{}/{}){}{} | Enter expand | D details ",
                view_title,
                start_idx + 1,
                end_idx,
                filtered_indices.len(),
                filter_suffix,
                pinned_suffix
            )
        } else {
            format!(
                " {} ({}){}{} | Enter expand | D details ",
                view_title,
                filtered_indices.len(),
                filter_suffix,
                pinned_suffix
            )
        };

        let requests_table = Table::new(rows, tunnel_table_constraints(table_area.width))
            .header(headers)
            .block(
                Block::default()
                    .title(title)
                    .borders(Borders::TOP)
                    .border_style(Style::default().fg(colors::MUTED)),
            )
            .row_highlight_style(Style::default().add_modifier(Modifier::BOLD))
            .highlight_symbol("▸ ");

        let mut table_state = TableState::default();
        table_state.select(selected_row_index);

        frame.render_stateful_widget(requests_table, table_area, &mut table_state);
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
        // Include the Response tab only when tunnel response data is available.
        let mut titles = vec!["Info", "Headers", "Body"];
        if app.selected_tunnel_response.is_some() {
            titles.push("Response");
        }
        let tab_titles: Vec<Line> = titles.into_iter().map(Line::from).collect();

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
            3 => draw_response_tab(frame, app, chunks[1]),
            _ => {}
        }
    }
}

fn line_text(line: &Line<'_>) -> String {
    line.spans
        .iter()
        .map(|span| span.content.as_ref())
        .collect()
}

fn highlight_detail_search_match(
    app: &App,
    target: DetailSearchTarget,
    line: Line<'static>,
) -> Line<'static> {
    if app.detail_search_matches().binary_search(&target).is_err() {
        return line;
    }

    let indices = detail_match_indices(
        &line_text(&line),
        app.detail_search_text(),
        app.detail_search_is_fuzzy(),
    );
    if indices.is_empty() {
        return line;
    }

    let is_active = app.active_detail_search_match() == Some(target);
    let line_style = line.style;
    let alignment = line.alignment;
    let mut highlighted_spans = Vec::new();
    let mut char_index = 0;

    for span in line.spans {
        for ch in span.content.chars() {
            let mut style = span.style;
            if indices.binary_search(&char_index).is_ok() {
                style = style.add_modifier(Modifier::REVERSED);
                if is_active {
                    style = style
                        .add_modifier(Modifier::BOLD)
                        .add_modifier(Modifier::UNDERLINED);
                }
            }
            push_styled_char(&mut highlighted_spans, ch, style);
            char_index += 1;
        }
    }

    let mut highlighted = Line::from(highlighted_spans);
    highlighted.style = line_style;
    highlighted.alignment = alignment;
    highlighted
}

fn draw_info_tab(
    frame: &mut Frame,
    app: &App,
    request: &crate::models::WebhookRequest,
    area: Rect,
) {
    let content_width = detail_content_width(area);
    let label_width = detail_label_width(content_width, 18);
    let value_width = detail_value_width(content_width, label_width);

    let mut rows = vec![
        info_table_row(
            app,
            0,
            "Method",
            Line::from(Span::styled(
                request.method.clone(),
                Style::default()
                    .fg(colors::SUCCESS)
                    .add_modifier(Modifier::BOLD),
            )),
            value_width,
        ),
        info_table_row(
            app,
            1,
            "URL",
            Line::from(Span::styled(
                request.url.clone(),
                Style::default().fg(colors::TEXT),
            )),
            value_width,
        ),
        info_table_row(
            app,
            2,
            "Remote IP",
            Line::from(Span::styled(
                request.remote_addr.clone(),
                Style::default().fg(colors::TEXT),
            )),
            value_width,
        ),
        info_table_row(
            app,
            3,
            "Timestamp",
            Line::from(Span::styled(
                request.created_at.clone(),
                Style::default().fg(colors::TEXT),
            )),
            value_width,
        ),
        info_table_row(
            app,
            4,
            "Content Length",
            Line::from(Span::styled(
                request.content_length.to_string(),
                Style::default().fg(colors::TEXT),
            )),
            value_width,
        ),
        info_table_row(
            app,
            5,
            "Request ID",
            Line::from(Span::styled(
                request.id.clone(),
                Style::default().fg(colors::MUTED),
            )),
            value_width,
        ),
    ];

    if !app.forward_url_input.is_empty() {
        rows.push(info_table_row(
            app,
            6,
            "Last Forward URL",
            Line::from(vec![
                Span::styled(
                    app.forward_url_input.clone(),
                    Style::default().fg(colors::PRIMARY),
                ),
                Span::styled(" (press r to replay)", Style::default().fg(colors::MUTED)),
            ]),
            value_width,
        ));
    }

    if let Some(feedback) = &app.status_message {
        rows.push(info_table_row(
            app,
            rows.len(),
            "Status",
            Line::from(Span::styled(
                format!("[{}] {}", feedback_label(feedback.kind), feedback.message),
                Style::default()
                    .fg(feedback_color(feedback.kind))
                    .add_modifier(Modifier::BOLD),
            )),
            value_width,
        ));
    }

    let info = Table::new(
        rows,
        [Constraint::Length(label_width as u16), Constraint::Min(1)],
    )
    .column_spacing(2)
    .block(
        Block::default()
            .title(" Request Information ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(colors::PRIMARY)),
    );

    frame.render_widget(info, area);
}

fn info_table_row(
    app: &App,
    index: usize,
    label: &'static str,
    value: Line<'static>,
    value_width: usize,
) -> Row<'static> {
    let target = DetailSearchTarget::Info(index);
    let label = highlight_detail_search_match(
        app,
        target,
        Line::from(Span::styled(
            label,
            Style::default()
                .fg(colors::PRIMARY)
                .add_modifier(Modifier::BOLD),
        )),
    );
    let value = highlight_detail_search_match(app, target, value);
    let value_lines = wrap_styled_line(value, value_width, 2);
    let row_height = lines_to_height(value_lines.len());

    Row::new(vec![Cell::from(label), Cell::from(Text::from(value_lines))]).height(row_height)
}

fn lines_to_height(line_count: usize) -> u16 {
    line_count.min(u16::MAX as usize).max(1) as u16
}

fn draw_headers_tab(
    frame: &mut Frame,
    app: &App,
    request: &crate::models::WebhookRequest,
    area: Rect,
) {
    let headers = sorted_headers(&request.headers);
    let lines = if app.detail_search_text().is_empty() {
        format_header_lines(&headers, detail_content_width(area))
    } else {
        format_searchable_header_lines(
            &headers,
            detail_content_width(area),
            app,
            HeaderSearchScope::Request,
        )
    };
    let title = format!("Headers ({})", headers.len());

    render_scrollable_lines(
        frame,
        lines,
        app.headers_scroll_offset,
        &title,
        colors::SECONDARY,
        area,
    );
}

/// Render an empty/missing body placeholder.
fn render_empty_body(frame: &mut Frame, title: &str, message: &str, area: Rect) {
    let widget = Paragraph::new(message)
        .style(Style::default().fg(colors::MUTED))
        .alignment(Alignment::Center)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(colors::MUTED)),
        );
    frame.render_widget(widget, area);
}

fn detail_content_width(area: Rect) -> usize {
    area.width.saturating_sub(2).max(1) as usize
}

fn format_header_lines(headers: &[(&String, &String)], width: usize) -> Vec<Line<'static>> {
    format_header_lines_with_search(headers, width, None)
}

#[derive(Clone, Copy)]
enum HeaderSearchScope {
    Request,
    Response,
}

fn format_searchable_header_lines(
    headers: &[(&String, &String)],
    width: usize,
    app: &App,
    scope: HeaderSearchScope,
) -> Vec<Line<'static>> {
    format_header_lines_with_search(headers, width, Some((app, scope)))
}

fn format_header_lines_with_search(
    headers: &[(&String, &String)],
    width: usize,
    search: Option<(&App, HeaderSearchScope)>,
) -> Vec<Line<'static>> {
    if headers.is_empty() {
        return vec![Line::from(Span::styled(
            "(no headers)",
            Style::default().fg(colors::MUTED),
        ))];
    }

    let key_width = detail_label_width(width, 28);
    let value_width = detail_value_width(width, key_width);
    let mut lines = Vec::new();

    for (index, (key, value)) in headers.iter().enumerate() {
        let mut key_line = Line::from(Span::styled(
            (*key).clone(),
            Style::default()
                .fg(colors::PRIMARY)
                .add_modifier(Modifier::BOLD),
        ));
        let mut value_line = Line::from(Span::styled(
            (*value).clone(),
            Style::default().fg(colors::TEXT),
        ));
        if let Some((app, scope)) = search {
            let target = match scope {
                HeaderSearchScope::Request => DetailSearchTarget::RequestHeader(index),
                HeaderSearchScope::Response => DetailSearchTarget::ResponseHeader(index),
            };
            key_line = highlight_detail_search_match(app, target, key_line);
            value_line = highlight_detail_search_match(app, target, value_line);
        }

        let key_lines = wrap_styled_line(key_line, key_width, 0);
        let value_lines = wrap_styled_line(value_line, value_width, 0);
        let row_height = key_lines.len().max(value_lines.len());

        for line_index in 0..row_height {
            let key_line = key_lines
                .get(line_index)
                .cloned()
                .unwrap_or_else(|| Line::from(""));
            let value_line = value_lines
                .get(line_index)
                .cloned()
                .unwrap_or_else(|| Line::from(""));
            lines.push(two_column_line(
                key_line,
                key_width,
                value_line,
                value_width,
            ));
        }
    }

    lines
}

fn two_column_line(
    left: Line<'static>,
    left_width: usize,
    right: Line<'static>,
    right_width: usize,
) -> Line<'static> {
    let mut line = pad_line_to_width(left, left_width);
    line.spans.push(Span::raw("  "));
    line.spans
        .extend(pad_line_to_width(right, right_width).spans);
    line
}

fn pad_line_to_width(mut line: Line<'static>, width: usize) -> Line<'static> {
    let padding = width.saturating_sub(line.width());
    if padding > 0 {
        line.spans.push(Span::raw(" ".repeat(padding)));
    }
    line
}

fn render_scrollable_lines(
    frame: &mut Frame,
    lines: Vec<Line<'static>>,
    scroll_offset: usize,
    title: &str,
    border_color: Color,
    area: Rect,
) {
    let total_lines = lines.len();
    let visible_capacity = area.height.saturating_sub(2) as usize;
    let viewport_lines = visible_capacity.max(1);
    let max_scroll = total_lines.saturating_sub(viewport_lines);
    let start_line = scroll_offset.min(max_scroll);
    let end_line = (start_line + visible_capacity).min(total_lines);
    let visible_lines = if visible_capacity == 0 {
        Vec::new()
    } else {
        lines[start_line..end_line].to_vec()
    };

    let block_title = if total_lines > viewport_lines {
        format!(
            " {} (lines {}-{}/{}) ",
            title,
            start_line + 1,
            end_line,
            total_lines
        )
    } else {
        format!(" {} ", title)
    };

    let body = Paragraph::new(visible_lines).block(
        Block::default()
            .title(block_title)
            .borders(Borders::ALL)
            .border_style(Style::default().fg(border_color)),
    );
    frame.render_widget(body, area);

    if total_lines > viewport_lines && area.height > 2 {
        let scrollbar = Scrollbar::new(ScrollbarOrientation::VerticalRight)
            .begin_symbol(None)
            .end_symbol(None)
            .track_style(Style::default().fg(colors::MUTED))
            .thumb_style(Style::default().fg(border_color));
        let mut scrollbar_state = ScrollbarState::new(total_lines).position(start_line);
        frame.render_stateful_widget(scrollbar, area, &mut scrollbar_state);
    }
}

fn wrap_styled_lines(
    lines: Vec<Line<'static>>,
    width: usize,
    continuation_indent: usize,
) -> Vec<Line<'static>> {
    lines
        .into_iter()
        .flat_map(|line| wrap_styled_line(line, width, continuation_indent))
        .collect()
}

fn wrap_styled_line(
    line: Line<'static>,
    width: usize,
    continuation_indent: usize,
) -> Vec<Line<'static>> {
    let width = width.max(1);
    let indent = continuation_indent.min(width.saturating_sub(1));
    let line_style = line.style;
    let alignment = line.alignment;
    let mut wrapped = Vec::new();
    let mut current = Vec::new();
    let mut current_width = 0;

    for span in line.spans {
        let span_style = span.style;
        for ch in span.content.chars() {
            let ch_width = char_display_width(ch);
            if current_width > 0 && current_width + ch_width > width {
                push_wrapped_line(&mut wrapped, &mut current, line_style, alignment);
                current_width = 0;

                if indent > 0 {
                    current.push(Span::raw(" ".repeat(indent)));
                    current_width = indent;
                }
            }

            push_styled_char(&mut current, ch, span_style);
            current_width += ch_width;
        }
    }

    if !current.is_empty() || wrapped.is_empty() {
        push_wrapped_line(&mut wrapped, &mut current, line_style, alignment);
    }

    wrapped
}

fn push_wrapped_line(
    wrapped: &mut Vec<Line<'static>>,
    current: &mut Vec<Span<'static>>,
    style: Style,
    alignment: Option<Alignment>,
) {
    let mut line = Line::from(std::mem::take(current));
    line.style = style;
    line.alignment = alignment;
    wrapped.push(line);
}

fn push_styled_char(spans: &mut Vec<Span<'static>>, ch: char, style: Style) {
    if let Some(last) = spans.last_mut()
        && last.style == style
    {
        last.content.to_mut().push(ch);
        return;
    }

    spans.push(Span::styled(ch.to_string(), style));
}

fn char_display_width(ch: char) -> usize {
    let mut buffer = [0; 4];
    let encoded: &str = ch.encode_utf8(&mut buffer);
    Span::raw(encoded).width().max(1)
}

#[derive(Clone, Copy)]
enum BodySearchScope {
    Request,
    Response,
}

struct HighlightedBodyOptions<'a> {
    scroll_offset: usize,
    section_title: &'a str,
    border_color: Color,
    app: &'a App,
    scope: BodySearchScope,
}

/// Render a scrollable, syntax-highlighted body section.
fn render_highlighted_body(
    frame: &mut Frame,
    content: &str,
    area: Rect,
    options: HighlightedBodyOptions<'_>,
) {
    let highlighted_lines = JsonHighlighter::highlight_json(content)
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            let target = match options.scope {
                BodySearchScope::Request => DetailSearchTarget::RequestBody(index),
                BodySearchScope::Response => DetailSearchTarget::ResponseBody(index),
            };
            highlight_detail_search_match(options.app, target, line)
        })
        .collect();
    let wrapped_lines = wrap_styled_lines(highlighted_lines, detail_content_width(area), 2);

    render_scrollable_lines(
        frame,
        wrapped_lines,
        options.scroll_offset,
        options.section_title,
        options.border_color,
        area,
    );
}

fn draw_body_tab(
    frame: &mut Frame,
    app: &App,
    request: &crate::models::WebhookRequest,
    area: Rect,
) {
    let body_text = request.body.as_ref().or(request.body_preview.as_ref());

    let Some(body_content) = body_text else {
        render_empty_body(frame, " Body ", "(no body)", area);
        return;
    };

    if body_content.is_empty() {
        render_empty_body(frame, " Body ", "(empty body)", area);
        return;
    }

    let source_suffix = if request.body.is_some() {
        " (Full)"
    } else {
        " (Preview)"
    };

    let content_type =
        if body_content.trim().starts_with('{') || body_content.trim().starts_with('[') {
            " JSON"
        } else {
            ""
        };

    let title_extra = format!("{}{}", content_type, source_suffix);

    render_highlighted_body(
        frame,
        body_content,
        area,
        HighlightedBodyOptions {
            scroll_offset: app.body_scroll_offset,
            section_title: &format!("Body{}", title_extra),
            border_color: colors::SUCCESS,
            app,
            scope: BodySearchScope::Request,
        },
    );
}

fn draw_response_tab(frame: &mut Frame, app: &App, area: Rect) {
    let Some(resp) = &app.selected_tunnel_response else {
        let empty = Paragraph::new("(no response data)")
            .style(Style::default().fg(colors::MUTED))
            .alignment(Alignment::Center)
            .block(
                Block::default()
                    .title(" Response ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(colors::MUTED)),
            );
        frame.render_widget(empty, area);
        return;
    };

    // Three-section layout: status info, response headers, response body
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5), // Status info
            Constraint::Length(8), // Response headers
            Constraint::Min(0),    // Response body
        ])
        .split(area);

    // Section 1: Status info
    let status_color = resp
        .status
        .map(colors::for_http_status)
        .unwrap_or(colors::MUTED);

    let mut status_lines = vec![
        Line::from(vec![
            Span::styled(
                "STATUS    ",
                Style::default()
                    .fg(colors::PRIMARY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                resp.status
                    .map(|s| s.to_string())
                    .unwrap_or_else(|| "Pending".to_string()),
                Style::default()
                    .fg(status_color)
                    .add_modifier(Modifier::BOLD),
            ),
        ]),
        Line::from(vec![
            Span::styled(
                "Duration: ",
                Style::default()
                    .fg(colors::PRIMARY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                resp.duration_ms
                    .map(|d| format!("{}ms", d))
                    .unwrap_or_else(|| "-".to_string()),
                Style::default().fg(colors::TEXT),
            ),
        ]),
    ];

    if let Some(error) = &resp.error {
        status_lines.push(Line::from(vec![
            Span::styled(
                "ERROR   ",
                Style::default()
                    .fg(colors::ERROR)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(error.clone(), Style::default().fg(colors::ERROR)),
        ]));
    }
    let status_lines: Vec<_> = status_lines
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            highlight_detail_search_match(app, DetailSearchTarget::ResponseStatus(index), line)
        })
        .collect();

    let status_info = Paragraph::new(status_lines).block(
        Block::default()
            .title(" Response Status ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(status_color)),
    );
    frame.render_widget(status_info, chunks[0]);

    // Section 2: Response headers
    let headers = sorted_headers(&resp.headers);
    let header_lines = if app.detail_search_text().is_empty() {
        format_header_lines(&headers, detail_content_width(chunks[1]))
    } else {
        format_searchable_header_lines(
            &headers,
            detail_content_width(chunks[1]),
            app,
            HeaderSearchScope::Response,
        )
    };
    let headers_title = format!("Response Headers ({})", headers.len());
    render_scrollable_lines(
        frame,
        header_lines,
        app.response_headers_scroll_offset,
        &headers_title,
        colors::SECONDARY,
        chunks[1],
    );

    // Section 3: Response body
    match resp.body.as_deref() {
        Some(body_content) if !body_content.is_empty() => {
            render_highlighted_body(
                frame,
                body_content,
                chunks[2],
                HighlightedBodyOptions {
                    scroll_offset: app.response_scroll_offset,
                    section_title: "Response Body",
                    border_color: colors::SUCCESS,
                    app,
                    scope: BodySearchScope::Response,
                },
            );
        }
        Some(_) => {
            render_empty_body(frame, " Response Body ", "(empty response body)", chunks[2]);
        }
        None => {
            render_empty_body(frame, " Response Body ", "(no response body)", chunks[2]);
        }
    }
}

fn draw_error(frame: &mut Frame, app: &App, error_msg: &str, hint: Option<&str>, area: Rect) {
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
            format!("HINT    {}", hint_text),
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

    let can_retry = app.selected_request.is_some() && app.is_valid_url(&app.forward_url_input);
    let mut help_spans = Vec::new();
    if can_retry {
        help_spans.extend([
            Span::styled(
                "r",
                Style::default()
                    .fg(colors::SECONDARY)
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled(": Retry | ", Style::default().fg(colors::TEXT)),
        ]);
    }
    help_spans.extend([
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
    ]);
    let help_text = vec![Line::from(help_spans)];

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

    let input = Paragraph::new(format!("{}▎", app.forward_url_input))
        .style(Style::default().fg(colors::TEXT))
        .block(input_block);

    frame.render_widget(input, chunks[1]);

    // Help text
    let validation = if app.forward_url_input.is_empty() {
        (
            "INFO",
            "Enter an http:// or https:// target URL.",
            colors::INFO,
        )
    } else if app.is_valid_url(&app.forward_url_input) {
        ("OK", "Target URL is ready.", colors::SUCCESS)
    } else {
        (
            "ERR",
            "Target URL needs a host and an http:// or https:// scheme.",
            colors::ERROR,
        )
    };

    let help_text = vec![
        Line::from(Span::styled(
            format!("[{}] {}", validation.0, validation.1),
            Style::default()
                .fg(validation.2)
                .add_modifier(Modifier::BOLD),
        )),
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
                    "STATUS    ",
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
                    "TARGET    ",
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

fn tunnel_status_shortcuts(app: &App) -> (String, String) {
    let filter_shortcut = if app.search_query.is_empty() {
        "/ Filter"
    } else {
        "Esc Clear | / Filter"
    };
    let follow_shortcut = if app.is_tunnel_follow_pinned() {
        " | Home Top"
    } else {
        ""
    };
    let view_shortcut = if app.tunnel_pinned_only {
        "Tab All"
    } else {
        "Tab Pins"
    };
    let compact_filter_shortcut = if app.search_query.is_empty() {
        "/ Filter"
    } else {
        "Esc Clear"
    };

    let shortcuts = format!(
        "↑↓ Select | Enter Expand | {} | P Pin | A Actions{} | {} | Q Quit",
        view_shortcut, follow_shortcut, filter_shortcut
    );
    let compact_shortcuts = format!(
        "↑↓ | Enter Expand | A Menu | {} | Q Quit",
        compact_filter_shortcut
    );

    (shortcuts, compact_shortcuts)
}

fn selected_visible_tunnel_request(app: &App) -> Option<&TunnelRequest> {
    app.visible_tunnel_request_indices()
        .get(app.tunnel_selected_index)
        .and_then(|request_index| app.tunnel_requests.get(*request_index))
}

fn base_url_unavailable_reason(app: &App) -> Option<&'static str> {
    app.tunnel_subdomain
        .as_deref()
        .map(str::trim)
        .filter(|subdomain| !subdomain.is_empty())
        .is_none()
        .then_some("URL not ready")
}

fn draw_tunnel_actions(frame: &mut Frame, app: &App, area: Rect) {
    let popup_area = centered_rect(40, 14, area);
    frame.render_widget(ratatui::widgets::Clear, popup_area);

    let selected_request = selected_visible_tunnel_request(app);
    let selected = selected_request
        .map(|request| {
            format!(
                "{}  {} {}",
                tunnel_request_id_display(request),
                request.method,
                truncate_table_value(&tunnel_request_path(request), 44)
            )
        })
        .unwrap_or_else(|| "No request selected".to_string());
    let pin_label = selected_request
        .map(|request| {
            if request.pinned {
                "Unpin request"
            } else {
                "Pin request"
            }
        })
        .unwrap_or("Pin request");
    let view_label = if app.tunnel_pinned_only {
        "View all requests"
    } else {
        "View pinned only"
    };
    let no_request_reason = selected_request.is_none().then_some("no request");
    let base_url_reason = base_url_unavailable_reason(app);
    let request_url_reason = no_request_reason.or(base_url_reason);
    let replay_reason = no_request_reason.or_else(|| {
        selected_request
            .and_then(|request| request.body.as_deref())
            .is_some_and(|body| body.ends_with(TRUNCATED_BODY_MARKER))
            .then_some("body truncated")
    });

    let menu_text = vec![
        Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(selected, Style::default().fg(colors::TEXT)),
        ]),
        Line::from(""),
        tunnel_action_line("Tab", view_label, None),
        tunnel_action_line("D", "Details", no_request_reason),
        tunnel_action_line("P", pin_label, no_request_reason),
        tunnel_action_line("R", "Replay", replay_reason),
        tunnel_action_line("U", "Copy request URL", request_url_reason),
        tunnel_action_line("I", "Copy request ID", no_request_reason),
        tunnel_action_line("C", "Copy base URL", base_url_reason),
        tunnel_action_line("Ctrl+R", "Reconnect tunnel", None),
        Line::from(""),
        Line::from(Span::styled(
            "  Esc  Close",
            Style::default().fg(colors::MUTED),
        )),
    ];

    let popup = Paragraph::new(menu_text).block(
        Block::default()
            .title(" Actions ")
            .borders(Borders::ALL)
            .border_style(Style::default().fg(colors::PRIMARY)),
    );

    frame.render_widget(popup, popup_area);
}

fn tunnel_action_line(
    shortcut: &'static str,
    label: &'static str,
    unavailable_reason: Option<&'static str>,
) -> Line<'static> {
    let shortcut_style = if unavailable_reason.is_some() {
        Style::default().fg(colors::MUTED)
    } else {
        Style::default()
            .fg(colors::SECONDARY)
            .add_modifier(Modifier::BOLD)
    };
    let label_style = if unavailable_reason.is_some() {
        Style::default().fg(colors::MUTED)
    } else {
        Style::default().fg(colors::TEXT)
    };
    let reason = unavailable_reason
        .map(|reason| format!("  {}", reason))
        .unwrap_or_default();

    Line::from(vec![
        Span::styled(format!("  {:<7}", shortcut), shortcut_style),
        Span::styled(label, label_style),
        Span::styled(reason, Style::default().fg(colors::MUTED)),
    ])
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
            Constraint::Length(16), // API auth status
        ])
        .split(area);

    let (status_text, shortcuts, compact_shortcuts) = match &app.state {
        AppState::ShowRequestDetail => {
            if app.detail_search_active {
                let match_status = match app.detail_search_progress() {
                    Some((current, total)) if total > 0 => {
                        let kind = if app.detail_search_is_fuzzy() {
                            "fuzzy"
                        } else {
                            "exact"
                        };
                        format!("Match {current} of {total} ({kind})")
                    }
                    Some(_) => "No matches".to_string(),
                    None => "Type to find".to_string(),
                };
                (
                    format!("Find /{}▎", app.detail_search_text()),
                    format!("{match_status} | Enter Done | Esc Cancel"),
                    Some(format!("{match_status} | Enter Done | Esc Cancel")),
                )
            } else {
                let replay =
                    if app.selected_request.is_some() && app.is_valid_url(&app.forward_url_input) {
                        " | R Replay"
                    } else {
                        ""
                    };
                if let Some((current, total)) = app.detail_search_progress() {
                    if total == 0 {
                        (
                            "No matches".to_string(),
                            format!(
                                "/ Edit | Esc Clear | Tab Tabs | ↑↓ Scroll | F Forward{replay} | E Export | B Back | Q Quit"
                            ),
                            Some(
                                "/ Edit | Esc Clear | Tab | ↑↓ | F Forward | B Back | Q Quit"
                                    .to_string(),
                            ),
                        )
                    } else {
                        let kind = if app.detail_search_is_fuzzy() {
                            "fuzzy"
                        } else {
                            "exact"
                        };
                        (
                            format!("Match {current} of {total}"),
                            format!(
                                "{kind} | n/N Next/Prev | / Edit | Esc Clear | Tab Tabs | ↑↓ Scroll | F Forward{replay} | E Export | B Back | Q Quit"
                            ),
                            Some(format!(
                                "{kind} | n/N | / Edit | Esc Clear | Tab | ↑↓ | F Forward | B Back | Q Quit"
                            )),
                        )
                    }
                } else {
                    (
                        "Request".to_string(),
                        format!(
                            "Tab Tabs | ↑↓ Scroll | / Find | F Forward{replay} | E Export | B Back | Q Quit"
                        ),
                        Some("Tab | ↑↓ | / Find | F Forward | B Back | Q Quit".to_string()),
                    )
                }
            }
        }
        AppState::InputForwardUrl => (
            "Forward".to_string(),
            "Enter Send | Esc Cancel".to_string(),
            None,
        ),
        AppState::ForwardingRequest => {
            let spinner_chars = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
            let spinner = spinner_chars[app.loading_frame % spinner_chars.len()];
            (
                format!("{} Forwarding", spinner),
                "Please wait".to_string(),
                None,
            )
        }
        AppState::ForwardResult => (
            "Forward result".to_string(),
            "B Back | Q Quit".to_string(),
            None,
        ),
        AppState::ExportMenu => (
            "Export".to_string(),
            "1 cURL | 2 JSON | Esc Cancel".to_string(),
            None,
        ),
        AppState::Listening => {
            let retained = app.listening_requests.len();
            let total = app.listening_stats.total_requests;
            let count = if total > retained as u64 {
                format!("{retained}/{total}")
            } else {
                retained.to_string()
            };
            (
                format!("Listen ({count})"),
                "↑↓ Select | Enter Details | / Search | Q Quit".to_string(),
                Some("↑↓ | Enter Details | / Search | Q Quit".to_string()),
            )
        }
        AppState::Tunneling => {
            let total_requests = app.tunnel_requests.len();
            let (shortcuts, compact_shortcuts) = tunnel_status_shortcuts(app);

            (
                format!("Tunnel ({})", total_requests),
                shortcuts,
                Some(compact_shortcuts),
            )
        }
        AppState::Error { .. } => {
            let shortcuts =
                if app.selected_request.is_some() && app.is_valid_url(&app.forward_url_input) {
                    "R Retry | B/Esc Back | Q Quit"
                } else {
                    "B/Esc Back | Q Quit"
                };
            ("Error".to_string(), shortcuts.to_string(), None)
        }
    };

    let search_has_focus =
        matches!(app.state, AppState::ShowRequestDetail) && app.detail_search_active;
    let (status_text, status_color) = if let Some(feedback) = &app.status_message
        && !search_has_focus
    {
        (
            format!("{} {}", feedback_label(feedback.kind), feedback.message),
            feedback_color(feedback.kind),
        )
    } else {
        (status_text, colors::SECONDARY)
    };

    let full_status_width = status_text.chars().count() + 2 + shortcuts.chars().count();
    let shortcuts = if full_status_width <= usize::from(chunks[0].width) {
        shortcuts
    } else {
        compact_shortcuts.unwrap_or(shortcuts)
    };

    let status_spans = vec![
        Span::styled(
            status_text,
            Style::default()
                .fg(status_color)
                .add_modifier(Modifier::BOLD),
        ),
        Span::raw("  "),
        Span::styled(shortcuts, Style::default().fg(colors::MUTED)),
    ];

    let status_paragraph = Paragraph::new(Line::from(status_spans)).alignment(Alignment::Left);

    frame.render_widget(status_paragraph, chunks[0]);

    let connection_status = if app.config.access_token.is_some() && app.config.is_token_valid() {
        Span::styled(
            "API connected",
            Style::default()
                .fg(colors::SUCCESS)
                .add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            "not signed in",
            Style::default()
                .fg(colors::ERROR)
                .add_modifier(Modifier::BOLD),
        )
    };

    let connection_paragraph =
        Paragraph::new(Line::from(vec![connection_status])).alignment(Alignment::Center);

    frame.render_widget(connection_paragraph, chunks[1]);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use chrono::{Duration as ChronoDuration, Utc};
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::{Terminal, backend::TestBackend, buffer::Buffer};
    use std::collections::HashMap;
    use std::time::{Duration, Instant};

    fn render_app_to_buffer(app: &App, width: u16, height: u16) -> Buffer {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("test terminal should be created");

        terminal
            .draw(|frame| draw(frame, app))
            .expect("app should render into test terminal");

        terminal.backend().buffer().clone()
    }

    fn render_app_to_text(app: &App, width: u16, height: u16) -> String {
        let buffer = render_app_to_buffer(app, width, height);
        let area = *buffer.area();
        let mut lines = Vec::with_capacity(area.height as usize);

        for y in area.y..area.y + area.height {
            let mut line = String::new();
            for x in area.x..area.x + area.width {
                line.push_str(buffer.cell((x, y)).expect("cell is inside buffer").symbol());
            }
            lines.push(line.trim_end().to_string());
        }

        lines.join("\n")
    }

    fn apply_detail_search(app: &mut App, query: &str, keep_editor_open: bool) {
        app.handle_key_event(KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE))
            .expect("find should open");
        for ch in query.chars() {
            app.handle_key_event(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE))
                .expect("find query should accept text");
        }
        if !keep_editor_open {
            app.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
                .expect("find query should apply");
        }
    }

    fn render_tunnel_80x24_to_buffer() -> Buffer {
        render_app_to_buffer(&app_with_tunnel_rows(), 80, 24)
    }

    fn render_tunnel_80x24_to_text() -> String {
        render_app_to_text(&app_with_tunnel_rows(), 80, 24)
    }

    fn valid_test_config() -> Config {
        Config {
            access_token: Some("test-token".to_string()),
            token_expires_at: Some(Utc::now() + ChronoDuration::hours(1)),
            ..Config::default()
        }
    }

    fn make_tunnel_request(
        status: Option<u16>,
        error: Option<&str>,
        duration_ms: Option<u64>,
    ) -> TunnelRequest {
        let received_at = Instant::now();

        TunnelRequest {
            request_id: "abcdef123456".to_string(),
            method: "GET".to_string(),
            path: "/test".to_string(),
            received_at,
            status,
            completed_at: duration_ms.map(|duration_ms| {
                received_at
                    .checked_add(Duration::from_millis(duration_ms))
                    .unwrap_or(received_at)
            }),
            error: error.map(str::to_string),
            headers: HashMap::new(),
            body: None,
            query_string: String::new(),
            response_headers: None,
            response_body: None,
            pinned: false,
        }
    }

    fn make_detail_request() -> crate::models::WebhookRequest {
        crate::models::WebhookRequest {
            id: "req-visual-0001abcdef".to_string(),
            timestamp: 0,
            remote_addr: "Tunnel".to_string(),
            headers: HashMap::from([
                (
                    "content-type".to_string(),
                    "application/json; charset=utf-8".to_string(),
                ),
                ("user-agent".to_string(), "GitHub-Hookshot/8d49094".to_string()),
                (
                    "x-forwarded-for".to_string(),
                    "172.71.190.83, 10.0.0.42".to_string(),
                ),
                (
                    "x-hub-signature-256".to_string(),
                    format!("sha256={}", "a1b2c3d4e5f6".repeat(8)),
                ),
            ]),
            content_length: 10_354,
            method: "POST".to_string(),
            url: "/webhooks/github/push?installation_id=1001947732&delivery=1c07ce58-51df-11f1-902c-ae6ef7f4b5d5".to_string(),
            path: Some("/webhooks/github/push".to_string()),
            query_params: HashMap::new(),
            created_at: "2026-05-28T13:40:26.091627790Z".to_string(),
            body_preview: None,
            body: Some(
                r#"{"ref":"refs/heads/main","repository":{"id":1001947732,"name":"hooklistener"}}"#
                    .to_string(),
            ),
        }
    }

    fn listening_request_fixture(
        id: &str,
        method: &str,
        path: &str,
        remote_addr: &str,
        content_length: i64,
        headers: &[(&str, &str)],
    ) -> crate::models::WebhookRequest {
        crate::models::WebhookRequest {
            id: id.to_string(),
            timestamp: 0,
            remote_addr: remote_addr.to_string(),
            headers: headers
                .iter()
                .map(|(key, value)| (key.to_string(), value.to_string()))
                .collect(),
            content_length,
            method: method.to_string(),
            url: path.to_string(),
            path: Some(path.to_string()),
            query_params: HashMap::new(),
            created_at: "2026-05-28T13:40:26Z".to_string(),
            body_preview: None,
            body: None,
        }
    }

    fn base_tunnel_app() -> App {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::Tunneling;
        app.tunnel_subdomain = Some("plant-07.hook.events".to_string());
        app.tunnel_connected = true;
        app.tunnel_connected_at = Some(Instant::now() - Duration::from_secs(125));
        app.tunnel_local_host = "localhost".to_string();
        app.tunnel_local_port = 8080;
        app.tunnel_is_static = true;
        app
    }

    fn tunnel_request_fixture(
        request_id: &str,
        method: &str,
        path: &str,
        status: Option<u16>,
        duration_ms: Option<u64>,
        source_ip: &str,
    ) -> TunnelRequest {
        let mut request = make_tunnel_request(status, None, duration_ms);
        request.request_id = request_id.to_string();
        request.method = method.to_string();
        request.path = path.to_string();
        request.headers = HashMap::from([
            ("content-type".to_string(), "application/json".to_string()),
            ("x-forwarded-for".to_string(), source_ip.to_string()),
        ]);
        request.body = Some(
            r#"{"event":"deployment.finished","environment":"production","ok":true}"#.to_string(),
        );
        if let Some(status) = status {
            request.response_headers = Some(HashMap::from([(
                "content-type".to_string(),
                "application/json".to_string(),
            )]));
            request.response_body = Some(format!(r#"{{"status":{},"accepted":true}}"#, status));
        }
        request
    }

    fn app_with_tunnel_rows() -> App {
        let mut app = base_tunnel_app();
        let fixtures = [
            (
                "req-200-orders",
                "POST",
                "/webhooks/orders",
                Some(200),
                Some(42),
                "198.51.100.10",
            ),
            (
                "req-202-build",
                "PUT",
                "/deployments/prod",
                Some(202),
                Some(318),
                "198.51.100.11",
            ),
            (
                "req-404-user",
                "GET",
                "/webhooks/users/missing",
                Some(404),
                Some(88),
                "203.0.113.24",
            ),
            (
                "req-502-billing",
                "POST",
                "/webhooks/billing/invoice",
                Some(502),
                Some(1_420),
                "203.0.113.91",
            ),
            (
                "req-pending-replay",
                "PATCH",
                "/replay/pending",
                Some(200),
                None,
                "192.0.2.77",
            ),
        ];

        for (request_id, method, path, status, duration_ms, source_ip) in fixtures {
            let request =
                tunnel_request_fixture(request_id, method, path, status, duration_ms, source_ip);
            if let (Some(status), Some(duration_ms)) = (status, duration_ms) {
                app.tunnel_stats.record_response(status, duration_ms);
            }
            app.push_tunnel_request(request);
            app.tunnel_stats.total += 1;
        }

        app.tunnel_selected_index = 3;
        app
    }

    #[test]
    fn tunnel_actions_menu_snapshot() {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::Tunneling;
        app.tunnel_subdomain = Some("snap.hook.events".to_string());
        app.tunnel_connected = true;
        app.tunnel_actions_open = true;
        app.tunnel_stats.total = 1;
        app.tunnel_stats.record_response(200, 42);

        let mut request = make_tunnel_request(Some(200), None, Some(42));
        request.request_id = "req-snapshot-001".to_string();
        request.method = "POST".to_string();
        request.path = "/webhooks/orders".to_string();
        request.query_string = "source=insta".to_string();
        request
            .headers
            .insert("content-type".to_string(), "application/json".to_string());
        request.body = Some(r#"{"order_id":"ord_123","paid":true}"#.to_string());
        request.response_body = Some(r#"{"ok":true}"#.to_string());
        app.push_tunnel_request(request);

        let snapshot = render_app_to_text(&app, 120, 28);

        insta::assert_snapshot!(snapshot);
    }

    #[test]
    fn tunneling_live_requests_80x24_snapshot() {
        let snapshot = render_tunnel_80x24_to_text();

        insta::assert_snapshot!(snapshot);
    }

    #[test]
    fn tunneling_update_warning_80x24_snapshot() {
        let mut app = app_with_tunnel_rows();
        app.available_update = Some("1.9.0".to_string());

        let snapshot = render_app_to_text(&app, 80, 24);

        insta::assert_snapshot!(snapshot);
    }

    #[test]
    fn tunneling_inherits_terminal_background() {
        let buffer = render_tunnel_80x24_to_buffer();

        assert!(buffer.content().iter().all(|cell| cell.bg == Color::Reset));
    }

    #[test]
    fn monochrome_mode_strips_all_rendered_colors() {
        let mut app = app_with_tunnel_rows();
        app.monochrome = true;
        let buffer = render_app_to_buffer(&app, 80, 24);

        assert!(buffer.content().iter().all(|cell| {
            cell.fg == Color::Reset
                && cell.bg == Color::Reset
                && cell.underline_color == Color::Reset
        }));
    }

    #[test]
    fn tunneling_selection_uses_weight_instead_of_background() {
        let buffer = render_tunnel_80x24_to_buffer();
        let selection = buffer
            .content()
            .iter()
            .find(|cell| cell.symbol() == "▸")
            .expect("selected tunnel row should render a caret");

        assert!(selection.modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn tunneling_metrics_remain_complete_at_80_columns() {
        let rendered = render_tunnel_80x24_to_text();

        assert!(rendered.contains("Follow Paused"));
    }

    #[test]
    fn tunneling_source_remains_complete_at_80_columns() {
        let rendered = render_tunnel_80x24_to_text();

        assert!(rendered.contains("198.51.100.11"));
    }

    #[test]
    fn tunneling_shortcuts_remain_complete_at_80_columns() {
        let rendered = render_tunnel_80x24_to_text();

        assert!(rendered.contains("A Menu | / Filter | Q Quit"));
    }

    #[test]
    fn tunnel_actions_remain_identifiable_at_80_columns() {
        let mut app = app_with_tunnel_rows();
        app.tunnel_actions_open = true;

        let rendered = render_app_to_text(&app, 80, 24);

        assert!(rendered.contains("Copy request URL"));
        assert!(rendered.contains("Replay"));
        assert!(rendered.contains("Esc  Close"));
    }

    #[test]
    fn tunneling_search_active_snapshot() {
        let mut app = app_with_tunnel_rows();
        app.search_active = true;
        app.search_query = "billing".to_string();

        let snapshot = render_app_to_text(&app, 100, 24);

        insta::assert_snapshot!(snapshot);
    }

    #[test]
    fn request_detail_info_tab_snapshot() {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::ShowRequestDetail;
        app.current_tab = 0;
        app.selected_request = Some(make_detail_request());

        let snapshot = render_app_to_text(&app, 110, 24);

        insta::assert_snapshot!(snapshot);
    }

    #[test]
    fn request_detail_keeps_recovery_controls_at_80_columns() {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::ShowRequestDetail;
        app.selected_request = Some(make_detail_request());

        let rendered = render_app_to_text(&app, 80, 24);

        assert!(rendered.contains("F Forward"));
        assert!(rendered.contains("B Back"));
        assert!(rendered.contains("Q Quit"));
    }

    #[test]
    fn request_detail_headers_tab_snapshot() {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::ShowRequestDetail;
        app.current_tab = 1;
        app.selected_request = Some(make_detail_request());

        let snapshot = render_app_to_text(&app, 110, 24);

        insta::assert_snapshot!(snapshot);
    }

    #[test]
    fn request_detail_fuzzy_find_preserves_header_context() {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::ShowRequestDetail;
        app.current_tab = 1;
        app.selected_request = Some(make_detail_request());
        apply_detail_search(&mut app, "cntyp", false);

        let rendered = render_app_to_text(&app, 110, 24);

        assert!(rendered.contains("Headers (4)"));
        assert!(rendered.contains("content-type"));
        assert!(rendered.contains("user-agent"));
        assert!(rendered.contains("Match 1 of 1"));
        assert!(rendered.contains("fuzzy"));
    }

    #[test]
    fn request_detail_find_visually_distinguishes_matches_and_active_match() {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::ShowRequestDetail;
        app.current_tab = 1;
        app.selected_request = Some(make_detail_request());
        apply_detail_search(&mut app, "a", false);

        let buffer = render_app_to_buffer(&app, 110, 24);
        let highlighted_cells: Vec<_> = buffer
            .content()
            .iter()
            .filter(|cell| cell.modifier.contains(Modifier::REVERSED))
            .collect();

        assert!(!highlighted_cells.is_empty());
        assert!(
            highlighted_cells
                .iter()
                .any(|cell| cell.modifier.contains(Modifier::UNDERLINED))
        );
        assert!(
            highlighted_cells
                .iter()
                .any(|cell| !cell.modifier.contains(Modifier::UNDERLINED))
        );
    }

    #[test]
    fn request_detail_find_scrolls_to_a_wrapped_header_at_80_columns() {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::ShowRequestDetail;
        app.current_tab = 1;
        let mut request = make_detail_request();
        request.headers = (0..10)
            .map(|index| (format!("a-header-{index:02}"), "x".repeat(200)))
            .chain([("z-needle".to_string(), "target".to_string())])
            .collect();
        app.selected_request = Some(request);
        app.set_detail_content_width(76);
        apply_detail_search(&mut app, "needle", false);

        let rendered = render_app_to_text(&app, 80, 24);

        assert!(rendered.contains("z-needle"));
    }

    #[test]
    fn request_detail_find_preserves_formatted_body_structure() {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::ShowRequestDetail;
        app.current_tab = 2;
        app.selected_request = Some(make_detail_request());
        apply_detail_search(&mut app, "rpstry", false);

        let rendered = render_app_to_text(&app, 110, 24);

        assert!(rendered.contains("\"repository\": {"));
        assert!(rendered.contains("\"ref\": \"refs/heads/main\""));
        assert!(rendered.contains("Match 1 of 1"));
    }

    #[test]
    fn request_detail_active_find_uses_status_bar_without_reducing_content() {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::ShowRequestDetail;
        app.current_tab = 2;
        app.selected_request = Some(make_detail_request());
        apply_detail_search(&mut app, "repo", true);

        let rendered = render_app_to_text(&app, 110, 24);

        assert!(rendered.contains("Find /repo▎"));
        assert!(rendered.contains("Match 1 of 1 (exact) | Enter Done | Esc Cancel"));
        assert!(rendered.contains("\"ref\": \"refs/heads/main\""));
    }

    #[test]
    fn request_detail_find_keeps_navigation_and_clear_controls_at_80_columns() {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::ShowRequestDetail;
        app.current_tab = 2;
        app.selected_request = Some(make_detail_request());
        apply_detail_search(&mut app, "repo", false);

        let rendered = render_app_to_text(&app, 80, 24);

        assert!(rendered.contains("Match 1 of 1"));
        assert!(rendered.contains("n/N"));
        assert!(rendered.contains("/ Edit"));
        assert!(rendered.contains("Esc Clear"));
    }

    #[test]
    fn request_detail_response_tab_snapshot() {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::ShowRequestDetail;
        app.current_tab = 3;
        app.selected_request = Some(make_detail_request());
        app.selected_tunnel_response = Some(crate::app::TunnelResponseData {
            status: Some(202),
            headers: HashMap::from([
                ("content-type".to_string(), "application/json".to_string()),
                (
                    "x-request-id".to_string(),
                    "fwd_01hy8gm9vk4r8mwrq27z".to_string(),
                ),
                ("server".to_string(), "local-test-rig".to_string()),
            ]),
            body: Some(
                r#"{"accepted":true,"queue":"webhook-forwarder","attempt":1,"duration_ms":64}"#
                    .to_string(),
            ),
            duration_ms: Some(64),
            error: None,
        });

        let snapshot = render_app_to_text(&app, 110, 28);

        insta::assert_snapshot!(snapshot);
    }

    #[test]
    fn request_detail_find_searches_response_as_one_sequence() {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::ShowRequestDetail;
        app.current_tab = 3;
        app.selected_request = Some(make_detail_request());
        app.selected_tunnel_response = Some(crate::app::TunnelResponseData {
            status: Some(202),
            headers: HashMap::from([("content-type".to_string(), "application/json".to_string())]),
            body: Some(r#"{"accepted":true,"queue":"webhook-forwarder"}"#.to_string()),
            duration_ms: Some(64),
            error: None,
        });
        apply_detail_search(&mut app, "que", false);

        let rendered = render_app_to_text(&app, 110, 28);

        assert!(rendered.contains("Response Headers (1)"));
        assert!(rendered.contains("\"queue\": \"webhook-forwarder\""));
        assert!(rendered.contains("\"accepted\": true"));
        assert!(rendered.contains("Match 1 of 1"));
    }

    #[test]
    fn listening_empty_state_logo_snapshot() {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::Listening;
        app.listening_connected = true;
        app.listening_endpoint = "github-webhooks-v7zd".to_string();
        app.listening_target = "http://localhost:3000".to_string();
        app.logo_frame = Some(
            [
                "          **%%%%%%%%%%%%*",
                "       *%%%%%%%%%%%%%%%%%%%%*",
                "    *%%%%%%%%%%%%**      *%%%%",
                "%%%%%%%%%%%%%%%         ***   ",
                "%%%%%%%%%%%%*        %@@@@@%  ",
                "  **************     %@@@@@%  ",
            ]
            .join("\n"),
        );

        let snapshot = render_app_to_text(&app, 120, 18);

        insta::assert_snapshot!(snapshot);
    }

    #[test]
    fn listening_live_requests_80x24_snapshot() {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::Listening;
        app.listening_connected = true;
        app.listening_endpoint = "orders-prod-v7zd".to_string();
        app.listening_target = "http://localhost:8080/webhook".to_string();
        app.listening_stats.total_requests = 3;
        app.listening_stats.successful_forwards = 2;
        app.listening_stats.failed_forwards = 1;
        app.selected_request_index = 1;
        app.listening_requests = vec![
            listening_request_fixture(
                "req-listen-001",
                "POST",
                "/webhooks/orders",
                "198.51.100.20",
                128,
                &[("content-type", "application/json")],
            ),
            listening_request_fixture(
                "req-listen-002",
                "PATCH",
                "/webhooks/billing/invoice",
                "203.0.113.42",
                512,
                &[
                    ("content-type", "application/json"),
                    ("x-provider", "stripe"),
                ],
            ),
            listening_request_fixture(
                "req-listen-003",
                "GET",
                "/health",
                "192.0.2.9",
                96,
                &[("user-agent", "GitHub-Hookshot")],
            ),
        ]
        .into();

        let snapshot = render_app_to_text(&app, 80, 24);

        insta::assert_snapshot!(snapshot);
    }

    #[test]
    fn listening_update_warning_80x24_snapshot() {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::Listening;
        app.listening_connected = true;
        app.listening_endpoint = "orders-prod-v7zd".to_string();
        app.listening_target = "http://localhost:8080/webhook".to_string();
        app.available_update = Some("1.9.0".to_string());

        let snapshot = render_app_to_text(&app, 80, 24);

        insta::assert_snapshot!(snapshot);
    }

    #[test]
    fn monochrome_update_warning_keeps_warning_text() {
        let mut app = base_tunnel_app();
        app.available_update = Some("1.9.0".to_string());
        app.monochrome = true;

        let rendered = render_app_to_text(&app, 80, 24);

        assert!(rendered.contains(&format!(
            "[WARN] UPDATE AVAILABLE   {} → 1.9.0   Run `hooklistener update`",
            env!("CARGO_PKG_VERSION")
        )));
    }

    #[test]
    fn update_warning_preserves_status_bar_when_only_one_footer_row_fits() {
        let mut app = base_tunnel_app();
        app.available_update = Some("1.9.0".to_string());

        let rendered = render_app_to_text(&app, 80, 3);

        assert!(rendered.contains("Tunnel (0)"));
    }

    #[test]
    fn listening_view_reports_retained_and_lifetime_counts_after_eviction() {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::Listening;
        app.listening_connected = true;
        app.listening_stats.total_requests = 2;
        app.listening_requests.push_back(listening_request_fixture(
            "req-listen-002",
            "POST",
            "/webhooks/orders",
            "198.51.100.20",
            128,
            &[],
        ));

        let rendered = render_app_to_text(&app, 80, 24);

        assert!(rendered.contains("Live Requests (1 retained of 2)"));
        assert!(rendered.contains("Listen (1/2)"));
    }

    #[test]
    fn forward_url_input_snapshot() {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::InputForwardUrl;
        app.selected_request = Some(make_detail_request());
        app.forward_url_input = "http://localhost:8080/github".to_string();

        let snapshot = render_app_to_text(&app, 90, 20);

        insta::assert_snapshot!(snapshot);
    }

    #[test]
    fn invalid_forward_url_has_text_error_at_80_columns() {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::InputForwardUrl;
        app.selected_request = Some(make_detail_request());
        app.forward_url_input = "ftp://example.com/webhook".to_string();

        let rendered = render_app_to_text(&app, 80, 20);

        assert!(
            rendered.contains("[ERR] Target URL needs a host and an http:// or https:// scheme.")
        );
    }

    #[test]
    fn forwarding_progress_snapshot() {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::ForwardingRequest;
        app.loading_frame = 3;

        let snapshot = render_app_to_text(&app, 90, 16);

        insta::assert_snapshot!(snapshot);
    }

    #[test]
    fn error_state_snapshot() {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::Error {
            message: "Tunnel connection failed: upstream returned 503".to_string(),
            hint: Some("Check the local service, then press r to retry".to_string()),
        };
        app.selected_request = Some(make_detail_request());
        app.forward_url_input = "http://localhost:3000/webhook".to_string();

        let snapshot = render_app_to_text(&app, 90, 18);

        insta::assert_snapshot!(snapshot);
    }

    #[test]
    fn error_recovery_controls_fit_at_80_columns() {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::Error {
            message: "Forwarding failed".to_string(),
            hint: Some("Check the local service, then retry".to_string()),
        };
        app.selected_request = Some(make_detail_request());
        app.forward_url_input = "http://localhost:3000/webhook".to_string();

        let rendered = render_app_to_text(&app, 80, 18);

        assert!(rendered.contains("R Retry | B/Esc Back | Q Quit"));
    }

    #[test]
    fn format_body_size_handles_empty_and_large_bodies() {
        assert_eq!(format_body_size(None), "--");
        assert_eq!(format_body_size(Some("")), "0b");
        assert_eq!(format_body_size(Some("abc")), "3b");
        assert_eq!(format_body_size(Some(&"a".repeat(1536))), "1.5kb");
    }

    #[test]
    fn compact_content_type_trims_parameters() {
        let mut headers = HashMap::new();
        headers.insert(
            "Content-Type".to_string(),
            "application/json; charset=utf-8".to_string(),
        );

        assert_eq!(
            compact_content_type(&headers).as_deref(),
            Some("application/json")
        );
    }

    #[test]
    fn tunnel_metadata_summary_includes_header_type_and_body_size() {
        let mut headers = HashMap::new();
        headers.insert("content-type".to_string(), "text/plain".to_string());

        assert_eq!(
            tunnel_metadata_summary(Some(&headers), Some("hello")),
            "1 header | type text/plain | body 5b"
        );
    }

    #[test]
    fn format_preview_body_pretty_prints_json() {
        let preview = format_preview_body(r#"{"name":"Ada","ok":true}"#);

        assert!(preview.contains("\n  \"name\""));
        assert!(preview.contains("\n  \"ok\""));
    }

    #[test]
    fn header_detail_lines_wrap_long_values_to_width() {
        let key = "x-hub-signature-256".to_string();
        let value = format!("sha256={}", "a".repeat(96));
        let headers = vec![(&key, &value)];

        let lines = format_header_lines(&headers, 32);

        assert!(lines.len() > 1);
        assert!(lines.iter().all(|line| line.width() <= 32));
    }

    #[test]
    fn header_detail_lines_use_table_style_columns() {
        let key = "content-type".to_string();
        let value = "application/json".to_string();
        let headers = vec![(&key, &value)];

        let rendered = format_header_lines(&headers, 60)
            .into_iter()
            .map(|line| {
                line.spans
                    .into_iter()
                    .map(|span| span.content.into_owned())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();

        assert!(rendered[0].starts_with("content-type"));
        assert!(rendered[0].contains("application/json"));
        assert!(!rendered[0].contains("content-type:"));
    }

    #[test]
    fn request_detail_info_renders_as_table() {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::ShowRequestDetail;
        app.current_tab = 0;
        app.selected_request = Some(crate::models::WebhookRequest {
            id: "req-info-table".to_string(),
            timestamp: 0,
            remote_addr: "Tunnel".to_string(),
            headers: HashMap::new(),
            content_length: 42,
            method: "POST".to_string(),
            url: "/webhook".to_string(),
            path: Some("/webhook".to_string()),
            query_params: HashMap::new(),
            created_at: "2026-05-28T13:40:26Z".to_string(),
            body_preview: None,
            body: None,
        });

        let rendered = render_app_to_text(&app, 90, 20);

        assert!(rendered.contains("Method"));
        assert!(rendered.contains("POST"));
        assert!(!rendered.contains("Method:"));
    }

    #[test]
    fn request_detail_body_renders_pretty_json() {
        let mut app = App::with_config(valid_test_config());
        app.state = AppState::ShowRequestDetail;
        app.current_tab = 2;
        app.selected_request = Some(crate::models::WebhookRequest {
            id: "req-pretty-json".to_string(),
            timestamp: 0,
            remote_addr: "Tunnel".to_string(),
            headers: HashMap::new(),
            content_length: 96,
            method: "POST".to_string(),
            url: "/webhook".to_string(),
            path: Some("/webhook".to_string()),
            query_params: HashMap::new(),
            created_at: "2026-05-28T13:40:26Z".to_string(),
            body_preview: None,
            body: Some(
                r#"{"ref":"refs/heads/main","repository":{"id":1001947732,"name":"hooklistener"}}"#
                    .to_string(),
            ),
        });

        let rendered = render_app_to_text(&app, 90, 20);

        assert!(rendered.contains("\"repository\": {"));
        assert!(!rendered.contains(r#"{"ref""#));
    }

    #[test]
    fn preview_body_lines_reports_hidden_lines() {
        let lines: Vec<String> = preview_body_lines(Some(r#"{"name":"Ada","ok":true}"#), 1)
            .into_iter()
            .map(String::from)
            .collect();

        assert_eq!(lines.last().map(String::as_str), Some("... 3 more lines"));
    }

    #[test]
    fn tunnel_response_metadata_summary_marks_pending_response() {
        assert_eq!(
            tunnel_response_metadata_summary(&make_tunnel_request(None, None, None)),
            "pending"
        );
    }

    #[test]
    fn compact_tunnel_request_id_uses_stable_prefix() {
        assert_eq!(compact_tunnel_request_id("abcdef123456"), "abcdef");
        assert_eq!(compact_tunnel_request_id("abc"), "abc");
        assert_eq!(compact_tunnel_request_id(""), "--");
    }

    #[test]
    fn tunnel_request_id_display_marks_pinned_requests() {
        let mut request = make_tunnel_request(Some(200), None, Some(10));

        assert_eq!(tunnel_request_id_display(&request), "abcdef");

        request.pinned = true;
        assert_eq!(tunnel_request_id_display(&request), "*abcdef");
    }

    #[test]
    fn tunnel_request_tone_classifies_status_and_errors() {
        assert_eq!(
            tunnel_request_tone(&make_tunnel_request(Some(204), None, Some(10))),
            TunnelRequestTone::Success
        );
        assert_eq!(
            tunnel_request_tone(&make_tunnel_request(Some(404), None, Some(10))),
            TunnelRequestTone::ClientError
        );
        assert_eq!(
            tunnel_request_tone(&make_tunnel_request(Some(502), None, Some(10))),
            TunnelRequestTone::ServerError
        );
        assert_eq!(
            tunnel_request_tone(&make_tunnel_request(None, Some("connection refused"), None)),
            TunnelRequestTone::Failed
        );
        assert_eq!(
            tunnel_request_tone(&make_tunnel_request(None, None, None)),
            TunnelRequestTone::Pending
        );
    }

    #[test]
    fn request_duration_color_marks_slow_requests() {
        assert_eq!(
            request_duration_color(&make_tunnel_request(Some(200), None, Some(40))),
            colors::MUTED
        );
        assert_eq!(
            request_duration_color(&make_tunnel_request(Some(200), None, Some(400))),
            colors::SECONDARY
        );
        assert_eq!(
            request_duration_color(&make_tunnel_request(Some(200), None, Some(1_400))),
            colors::WARNING
        );
        assert_eq!(
            request_duration_color(&make_tunnel_request(None, None, None)),
            colors::WARNING
        );
    }

    #[test]
    fn tunnel_request_source_prefers_direct_client_headers() {
        let mut headers = HashMap::new();
        headers.insert(
            "x-forwarded-for".to_string(),
            "198.51.100.2, 10.0.0.1".to_string(),
        );
        headers.insert("cf-connecting-ip".to_string(), "203.0.113.10".to_string());

        assert_eq!(tunnel_request_source(&headers), "203.0.113.10");
    }

    #[test]
    fn tunnel_request_source_reads_forwarded_for() {
        let mut headers = HashMap::new();
        headers.insert(
            "forwarded".to_string(),
            "for=192.0.2.60;proto=https;by=203.0.113.43".to_string(),
        );

        assert_eq!(tunnel_request_source(&headers), "192.0.2.60");
    }

    #[test]
    fn tunnel_request_path_includes_query_string() {
        let request = TunnelRequest {
            request_id: "req-1".to_string(),
            method: "GET".to_string(),
            path: "/search".to_string(),
            received_at: std::time::Instant::now(),
            status: None,
            completed_at: None,
            error: None,
            headers: HashMap::new(),
            body: None,
            query_string: "q=test".to_string(),
            response_headers: None,
            response_body: None,
            pinned: false,
        };

        assert_eq!(tunnel_request_path(&request), "/search?q=test");
    }

    #[test]
    fn tunnel_test_curl_uses_public_tunnel_url() {
        assert_eq!(
            tunnel_test_curl(Some("abc123.hook.events")).as_deref(),
            Some("curl -i https://abc123.hook.events/test")
        );
    }

    #[test]
    fn tunnel_test_curl_is_missing_without_subdomain() {
        assert_eq!(tunnel_test_curl(None), None);
        assert_eq!(tunnel_test_curl(Some("")), None);
    }

    #[test]
    fn logo_frame_lines_render_when_panel_has_room() {
        let mut app = App::with_config(valid_test_config());
        app.logo_frame = Some("AB\nCD".to_string());

        let lines = logo_frame_lines(&app, Rect::new(0, 0, 10, 5), 1);

        assert_eq!(lines.len(), 3);
    }

    #[test]
    fn logo_frame_lines_hide_when_panel_is_too_small() {
        let mut app = App::with_config(valid_test_config());
        app.logo_frame = Some("AB\nCD".to_string());

        assert!(logo_frame_lines(&app, Rect::new(0, 0, 1, 5), 1).is_empty());
        assert!(logo_frame_lines(&app, Rect::new(0, 0, 10, 2), 1).is_empty());
    }
}
