use crate::api::ApiClient;
use crate::config::Config;
use crate::errors::ApiError;
use crate::models::{ForwardResponse, WebhookRequest};
use crate::syntax::JsonHighlighter;
use anyhow::{Result, anyhow};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use std::collections::{HashMap, VecDeque};

pub const MAX_LISTENING_REQUESTS: usize = 500;
pub const MAX_TUNNEL_REQUESTS: usize = 500;
pub const MAX_BODY_SIZE: usize = 256 * 1024;
pub const TRUNCATED_BODY_MARKER: &str = "\n...(truncated)";

fn is_truncated_body(body: Option<&str>) -> bool {
    body.is_some_and(|body| body.ends_with(TRUNCATED_BODY_MARKER))
}

fn format_path_with_query(path: &str, query_string: &str) -> String {
    let path = if path.starts_with('/') {
        path.to_string()
    } else {
        format!("/{}", path)
    };

    if query_string.is_empty() {
        path
    } else {
        format!("{}?{}", path, query_string)
    }
}

#[derive(Debug)]
pub enum AppState {
    ShowRequestDetail,
    InputForwardUrl,
    ForwardingRequest,
    ForwardResult,
    Listening,  // State for the listen command (debug endpoints)
    Tunneling,  // State for HTTP tunnel command
    ExportMenu, // Export request menu (cURL/JSON)
    Error {
        message: String,
        hint: Option<String>,
    },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FeedbackKind {
    Success,
    Info,
    Warning,
    Error,
}

#[derive(Debug)]
pub struct FeedbackMessage {
    pub kind: FeedbackKind,
    pub message: String,
    pub created_at: std::time::Instant,
}

impl FeedbackMessage {
    fn new(kind: FeedbackKind, message: impl Into<String>) -> Self {
        Self {
            kind,
            message: message.into(),
            created_at: std::time::Instant::now(),
        }
    }
}

#[derive(Default)]
pub struct ListeningStats {
    pub total_requests: u64,
    pub successful_forwards: u64,
    pub failed_forwards: u64,
}

#[derive(Default, Debug)]
pub struct TunnelStats {
    pub total: u64,
    pub success: u64,
    pub failed: u64,
    pub status_2xx: u64,
    pub status_4xx: u64,
    pub status_5xx: u64,
    pub total_duration_ms: u64,
    #[allow(dead_code)]
    pub bytes_in: u64,
    #[allow(dead_code)]
    pub bytes_out: u64,
}

impl TunnelStats {
    pub fn record_response(&mut self, status: u16, duration_ms: u64) {
        self.success += 1;
        self.total_duration_ms += duration_ms;

        match status {
            200..=299 => self.status_2xx += 1,
            400..=499 => self.status_4xx += 1,
            500..=599 => self.status_5xx += 1,
            _ => {}
        }
    }
}

#[derive(Debug, Clone)]
pub struct TunnelRequest {
    pub request_id: String,
    pub method: String,
    pub path: String,
    pub received_at: std::time::Instant,
    pub status: Option<u16>,
    pub completed_at: Option<std::time::Instant>,
    pub error: Option<String>,
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
    pub query_string: String,
    pub response_headers: Option<HashMap<String, String>>,
    pub response_body: Option<String>,
    pub pinned: bool,
}

#[derive(Debug, Clone)]
pub struct TunnelResponseData {
    pub status: Option<u16>,
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
    pub duration_ms: Option<u64>,
    pub error: Option<String>,
}

#[derive(Debug, Clone)]
pub struct TunnelReplayRequest {
    pub request_id: String,
    pub method: String,
    pub path: String,
    pub query_string: String,
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
    pub local_host: String,
    pub local_port: u16,
}

#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub struct TunnelReplayOutcome {
    pub status: u16,
    pub duration_ms: u64,
}

impl TunnelReplayRequest {
    pub fn target_url(&self) -> String {
        format!(
            "http://{}:{}{}",
            self.local_host,
            self.local_port,
            format_path_with_query(&self.path, &self.query_string)
        )
    }

    pub async fn send(&self) -> Result<TunnelReplayOutcome> {
        if is_truncated_body(self.body.as_deref()) {
            return Err(anyhow!("request body was truncated and cannot be replayed"));
        }

        let method = reqwest::Method::from_bytes(self.method.as_bytes())
            .map_err(|_| anyhow!("unsupported method: {}", self.method))?;
        let target = self.target_url();
        let client = reqwest::Client::builder()
            .timeout(std::time::Duration::from_secs(30))
            .build()?;
        let mut request = client.request(method, target);

        for (key, value) in &self.headers {
            let key_lower = key.to_lowercase();
            if matches!(key_lower.as_str(), "host" | "content-length") {
                continue;
            }
            if let Ok(header_name) = reqwest::header::HeaderName::from_bytes(key.as_bytes())
                && let Ok(header_value) = reqwest::header::HeaderValue::from_str(value)
            {
                request = request.header(header_name, header_value);
            }
        }

        if let Some(body) = &self.body
            && !body.is_empty()
        {
            request = request.body(body.clone());
        }

        let started_at = std::time::Instant::now();
        let response = request.send().await?;
        Ok(TunnelReplayOutcome {
            status: response.status().as_u16(),
            duration_ms: started_at.elapsed().as_millis() as u64,
        })
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum DetailReturnTarget {
    Listening,
    Tunneling,
}

pub fn truncate_body(body: Option<String>) -> Option<String> {
    body.map(|b| {
        if b.len() > MAX_BODY_SIZE {
            // Clamp to a valid UTF-8 boundary so truncation never panics.
            let mut cutoff = MAX_BODY_SIZE.min(b.len());
            while !b.is_char_boundary(cutoff) {
                cutoff = cutoff.saturating_sub(1);
            }

            let mut truncated = b[..cutoff].to_string();
            truncated.push_str(TRUNCATED_BODY_MARKER);
            truncated
        } else {
            b
        }
    })
}

const VIEWPORT_LINES: usize = 20;
const DETAIL_SCROLL_WRAP_WIDTH: usize = 100;

fn parse_query_string(query_string: &str) -> HashMap<String, String> {
    if query_string.is_empty() {
        return HashMap::new();
    }

    let parse_target = format!("http://localhost/?{}", query_string);
    if let Ok(url) = reqwest::Url::parse(&parse_target) {
        url.query_pairs()
            .map(|(k, v)| (k.into_owned(), v.into_owned()))
            .collect()
    } else {
        // Fallback to raw parsing if the query string is malformed.
        query_string
            .split('&')
            .filter_map(|pair| {
                let mut parts = pair.splitn(2, '=');
                let key = parts.next()?.to_string();
                let value = parts.next().unwrap_or_default().to_string();
                Some((key, value))
            })
            .collect()
    }
}

/// Compute the maximum scroll offset for a text body given a fixed viewport.
fn max_body_scroll(text: &str) -> usize {
    let display_text = JsonHighlighter::format_json_for_display(text);
    let line_count = visual_line_count(display_text.as_ref(), DETAIL_SCROLL_WRAP_WIDTH);
    line_count.saturating_sub(VIEWPORT_LINES)
}

fn max_headers_scroll(headers: &HashMap<String, String>) -> usize {
    let entry_bound = headers.len().saturating_sub(1);
    let visual_bound = headers
        .iter()
        .map(|(key, value)| {
            visual_line_count(&format!("{}: {}", key, value), DETAIL_SCROLL_WRAP_WIDTH)
        })
        .sum::<usize>()
        .saturating_sub(VIEWPORT_LINES);

    entry_bound.max(visual_bound)
}

fn visual_line_count(text: &str, width: usize) -> usize {
    let width = width.max(1);
    let mut count = 0;

    for line in text.lines() {
        let line_width = line.chars().count().max(1);
        count += line_width.div_ceil(width);
    }

    count.max(1)
}

pub struct App {
    pub state: AppState,
    pub config: Config,
    pub selected_request_index: usize,
    pub selected_request: Option<WebhookRequest>,
    pub forward_url_input: String,
    pub forward_result: Option<ForwardResponse>,
    pub current_tab: usize,
    pub headers_scroll_offset: usize,
    pub body_scroll_offset: usize,
    pub should_quit: bool,
    pub loading_frame: usize,
    pub logo_frame: Option<String>,

    // Listening mode state (debug endpoints)
    pub listening_requests: VecDeque<WebhookRequest>,
    pub listening_stats: ListeningStats,
    pub listening_connected: bool,
    pub listening_error: Option<String>,
    pub listening_endpoint: String,
    pub listening_target: String,

    // Tunneling mode state (HTTP tunnel)
    pub tunnel_subdomain: Option<String>,
    pub tunnel_id: Option<String>,
    pub tunnel_connected: bool,
    pub tunnel_connected_at: Option<std::time::Instant>,
    pub tunnel_requests: VecDeque<TunnelRequest>,
    pub tunnel_stats: TunnelStats,
    pub tunnel_selected_index: usize,
    pub tunnel_expanded_request_id: Option<String>,
    pub detail_return_state: Option<DetailReturnTarget>,
    pub selected_tunnel_response: Option<TunnelResponseData>,
    pub response_headers_scroll_offset: usize,
    pub response_scroll_offset: usize,
    pub tunnel_local_host: String,
    pub tunnel_local_port: u16,
    pub tunnel_org_id: Option<String>,
    pub tunnel_error: Option<String>,
    pub tunnel_requested_slug: Option<String>,
    pub tunnel_is_static: bool,
    pub tunnel_reconnect_requested: bool,
    pub tunnel_replay_requested: Option<TunnelReplayRequest>,
    pub tunnel_actions_open: bool,
    pub tunnel_pinned_only: bool,

    // Status messages (auto-expire)
    pub tunnel_status_message: Option<FeedbackMessage>,
    pub status_message: Option<FeedbackMessage>,

    // Accessible display mode strips all foreground and background colors after rendering.
    pub monochrome: bool,

    // Search/filter
    pub search_active: bool,
    pub search_query: String,
}

impl App {
    pub fn new() -> Result<Self> {
        let config = Config::load()?;
        Ok(Self::with_config(config))
    }

    pub fn with_config(config: Config) -> Self {
        Self {
            state: AppState::Listening,
            config,
            selected_request_index: 0,
            selected_request: None,
            forward_url_input: String::new(),
            forward_result: None,
            current_tab: 0,
            headers_scroll_offset: 0,
            body_scroll_offset: 0,
            should_quit: false,
            loading_frame: 0,
            logo_frame: None,
            listening_requests: VecDeque::new(),
            listening_stats: ListeningStats::default(),
            listening_connected: false,
            listening_error: None,
            listening_endpoint: String::new(),
            listening_target: String::new(),
            tunnel_subdomain: None,
            tunnel_id: None,
            tunnel_connected: false,
            tunnel_connected_at: None,
            tunnel_requests: VecDeque::new(),
            tunnel_stats: TunnelStats::default(),
            tunnel_selected_index: 0,
            tunnel_expanded_request_id: None,
            detail_return_state: None,
            selected_tunnel_response: None,
            response_headers_scroll_offset: 0,
            response_scroll_offset: 0,
            tunnel_local_host: String::from("localhost"),
            tunnel_local_port: 3000,
            tunnel_org_id: None,
            tunnel_error: None,
            tunnel_requested_slug: None,
            tunnel_is_static: false,
            tunnel_reconnect_requested: false,
            tunnel_replay_requested: None,
            tunnel_actions_open: false,
            tunnel_pinned_only: false,
            tunnel_status_message: None,
            status_message: None,
            monochrome: false,
            search_active: false,
            search_query: String::new(),
        }
    }

    /// Number of tabs available in the request detail view.
    /// Returns 4 when tunnel response data is present (Info, Headers, Body, Response),
    /// otherwise 3 (Info, Headers, Body).
    fn num_detail_tabs(&self) -> usize {
        if self.selected_tunnel_response.is_some() {
            4
        } else {
            3
        }
    }

    /// Return the body text of the currently selected request (full body or preview).
    fn selected_body_text(&self) -> Option<&str> {
        self.selected_request
            .as_ref()
            .and_then(|r| r.body.as_deref().or(r.body_preview.as_deref()))
    }

    /// Return the response body text from the selected tunnel response.
    fn response_body_text(&self) -> Option<&str> {
        self.selected_tunnel_response
            .as_ref()
            .and_then(|r| r.body.as_deref())
    }

    /// Maximum scroll offset for response headers in the Response tab.
    /// Use a conservative bound (`len - 1`) so tiny terminals can still
    /// navigate to the final headers even when the rendered viewport shrinks.
    fn max_response_headers_scroll(&self) -> usize {
        self.selected_tunnel_response
            .as_ref()
            .map(|r| max_headers_scroll(&r.headers))
            .unwrap_or(0)
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) -> Result<()> {
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }

        match &self.state {
            AppState::ShowRequestDetail => {
                match key.code {
                    KeyCode::Char('q') => {
                        self.should_quit = true;
                    }
                    KeyCode::Char('b') | KeyCode::Esc => {
                        self.current_tab = 0;
                        self.headers_scroll_offset = 0;
                        self.body_scroll_offset = 0;
                        self.response_headers_scroll_offset = 0;
                        self.response_scroll_offset = 0;
                        self.selected_tunnel_response = None;
                        self.state = match self.detail_return_state {
                            Some(DetailReturnTarget::Tunneling) => AppState::Tunneling,
                            _ => AppState::Listening,
                        };
                        self.detail_return_state = None;
                    }
                    KeyCode::Char('f') => {
                        self.forward_url_input.clear();
                        self.status_message = None;
                        self.state = AppState::InputForwardUrl;
                    }
                    KeyCode::Char('r') => {
                        if !self.forward_url_input.is_empty()
                            && self.is_valid_url(&self.forward_url_input)
                            && self.selected_request.is_some()
                        {
                            self.state = AppState::ForwardingRequest;
                        } else {
                            self.set_status_feedback(
                                FeedbackKind::Warning,
                                "Replay unavailable: forward this request to set a valid target URL.",
                            );
                        }
                    }
                    KeyCode::Char('e') if self.selected_request.is_some() => {
                        self.state = AppState::ExportMenu;
                    }
                    KeyCode::Tab | KeyCode::Right => {
                        self.current_tab = (self.current_tab + 1) % self.num_detail_tabs();
                    }
                    KeyCode::BackTab | KeyCode::Left => {
                        let num_tabs = self.num_detail_tabs();
                        self.current_tab = if self.current_tab == 0 {
                            num_tabs - 1
                        } else {
                            self.current_tab - 1
                        };
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        match self.current_tab {
                            1 if self.headers_scroll_offset > 0 => {
                                self.headers_scroll_offset -= 1;
                            }
                            2 if self.body_scroll_offset > 0 => {
                                self.body_scroll_offset -= 1;
                            }
                            3 => {
                                // Response tab: body first, then headers
                                if self.response_scroll_offset > 0 {
                                    self.response_scroll_offset -= 1;
                                } else if self.response_headers_scroll_offset > 0 {
                                    self.response_headers_scroll_offset -= 1;
                                }
                            }
                            _ => {} // Info tab - no scrolling
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => match self.current_tab {
                        1 => {
                            if let Some(request) = &self.selected_request {
                                let max = max_headers_scroll(&request.headers);
                                if self.headers_scroll_offset < max {
                                    self.headers_scroll_offset += 1;
                                }
                            }
                        }
                        2 => {
                            if let Some(body) = self.selected_body_text() {
                                let max = max_body_scroll(body);
                                if self.body_scroll_offset < max {
                                    self.body_scroll_offset += 1;
                                }
                            }
                        }
                        3 => {
                            let headers_max = self.max_response_headers_scroll();
                            if self.response_headers_scroll_offset < headers_max {
                                self.response_headers_scroll_offset += 1;
                            } else if let Some(body) = self.response_body_text() {
                                let body_max = max_body_scroll(body);
                                if self.response_scroll_offset < body_max {
                                    self.response_scroll_offset += 1;
                                }
                            }
                        }
                        _ => {}
                    },
                    KeyCode::PageUp => match self.current_tab {
                        1 => {
                            self.headers_scroll_offset =
                                self.headers_scroll_offset.saturating_sub(10);
                        }
                        2 => {
                            self.body_scroll_offset = self.body_scroll_offset.saturating_sub(10);
                        }
                        3 => {
                            let take_from_body = self.response_scroll_offset.min(10);
                            self.response_scroll_offset -= take_from_body;
                            let remaining = 10 - take_from_body;
                            self.response_headers_scroll_offset = self
                                .response_headers_scroll_offset
                                .saturating_sub(remaining);
                        }
                        _ => {}
                    },
                    KeyCode::PageDown => match self.current_tab {
                        1 => {
                            if let Some(request) = &self.selected_request {
                                let max = max_headers_scroll(&request.headers);
                                self.headers_scroll_offset =
                                    (self.headers_scroll_offset + 10).min(max);
                            }
                        }
                        2 => {
                            if let Some(body) = self.selected_body_text() {
                                let max = max_body_scroll(body);
                                self.body_scroll_offset = (self.body_scroll_offset + 10).min(max);
                            }
                        }
                        3 => {
                            let headers_max = self.max_response_headers_scroll();
                            let header_room =
                                headers_max.saturating_sub(self.response_headers_scroll_offset);
                            let to_headers = header_room.min(10);
                            self.response_headers_scroll_offset += to_headers;

                            let remaining = 10 - to_headers;
                            if remaining > 0
                                && let Some(body) = self.response_body_text()
                            {
                                let body_max = max_body_scroll(body);
                                self.response_scroll_offset =
                                    (self.response_scroll_offset + remaining).min(body_max);
                            }
                        }
                        _ => {}
                    },
                    KeyCode::Home => match self.current_tab {
                        1 => self.headers_scroll_offset = 0,
                        2 => self.body_scroll_offset = 0,
                        3 => {
                            self.response_headers_scroll_offset = 0;
                            self.response_scroll_offset = 0;
                        }
                        _ => {}
                    },
                    KeyCode::End => match self.current_tab {
                        1 => {
                            if let Some(request) = &self.selected_request {
                                self.headers_scroll_offset = max_headers_scroll(&request.headers);
                            }
                        }
                        2 => {
                            if let Some(body) = self.selected_body_text() {
                                self.body_scroll_offset = max_body_scroll(body);
                            }
                        }
                        3 => {
                            self.response_headers_scroll_offset =
                                self.max_response_headers_scroll();
                            if let Some(body) = self.response_body_text() {
                                self.response_scroll_offset = max_body_scroll(body);
                            } else {
                                self.response_scroll_offset = 0;
                            }
                        }
                        _ => {}
                    },
                    _ => {}
                }
            }
            AppState::Listening => {
                if self.search_active {
                    match key.code {
                        KeyCode::Esc => {
                            self.search_active = false;
                            self.search_query.clear();
                            self.selected_request_index = 0;
                        }
                        KeyCode::Enter => {
                            self.search_active = false;
                        }
                        KeyCode::Backspace => {
                            self.search_query.pop();
                            self.selected_request_index = 0;
                        }
                        KeyCode::Char(c) => {
                            self.search_query.push(c);
                            self.selected_request_index = 0;
                        }
                        _ => {}
                    }
                } else {
                    match key.code {
                        KeyCode::Char('q') | KeyCode::Esc => {
                            self.should_quit = true;
                        }
                        KeyCode::Char('/') => {
                            self.search_active = true;
                            self.search_query.clear();
                            self.selected_request_index = 0;
                        }
                        KeyCode::Up => {
                            let filtered =
                                Self::filter_requests(&self.listening_requests, &self.search_query);
                            if self.selected_request_index > 0 {
                                self.selected_request_index -= 1;
                            }
                            if !filtered.is_empty() {
                                self.selected_request_index =
                                    self.selected_request_index.min(filtered.len() - 1);
                            }
                        }
                        KeyCode::Down => {
                            let filtered =
                                Self::filter_requests(&self.listening_requests, &self.search_query);
                            if !filtered.is_empty()
                                && self.selected_request_index < filtered.len() - 1
                            {
                                self.selected_request_index += 1;
                            }
                        }
                        KeyCode::Enter => {
                            let filtered =
                                Self::filter_requests(&self.listening_requests, &self.search_query);
                            if let Some(&real_index) = filtered.get(self.selected_request_index)
                                && let Some(request) = self.listening_requests.get(real_index)
                            {
                                self.selected_request = Some(request.clone());
                                self.current_tab = 0;
                                self.headers_scroll_offset = 0;
                                self.body_scroll_offset = 0;
                                self.response_headers_scroll_offset = 0;
                                self.detail_return_state = Some(DetailReturnTarget::Listening);
                                self.state = AppState::ShowRequestDetail;
                            }
                        }
                        _ => {}
                    }
                }
            }
            AppState::Tunneling => match key.code {
                _ if self.tunnel_actions_open => {
                    match key.code {
                        KeyCode::Esc | KeyCode::Char('a') | KeyCode::Char('A') => {}
                        KeyCode::Char('q') => {
                            self.should_quit = true;
                        }
                        KeyCode::Char('d') | KeyCode::Char('D') => {
                            self.open_selected_tunnel_request_detail();
                        }
                        KeyCode::Char('r') | KeyCode::Char('R')
                            if key.modifiers.contains(KeyModifiers::CONTROL) =>
                        {
                            self.request_tunnel_reconnect();
                        }
                        KeyCode::Char('r') | KeyCode::Char('R') => {
                            self.request_selected_tunnel_replay();
                        }
                        KeyCode::Char('c') | KeyCode::Char('C') => {
                            self.copy_tunnel_base_url();
                        }
                        KeyCode::Char('u') | KeyCode::Char('U') => {
                            self.copy_selected_tunnel_request_url();
                        }
                        KeyCode::Char('i') | KeyCode::Char('I') => {
                            self.copy_selected_tunnel_request_id();
                        }
                        KeyCode::Char('p') | KeyCode::Char('P') => {
                            self.toggle_selected_tunnel_request_pin();
                        }
                        KeyCode::Tab => {
                            self.toggle_tunnel_pinned_view();
                        }
                        _ => return Ok(()),
                    }
                    self.tunnel_actions_open = false;
                }
                _ if self.search_active => match key.code {
                    KeyCode::Esc => {
                        self.search_active = false;
                        self.search_query.clear();
                        self.tunnel_selected_index = 0;
                        self.tunnel_expanded_request_id = None;
                    }
                    KeyCode::Enter => {
                        self.search_active = false;
                    }
                    KeyCode::Backspace => {
                        self.search_query.pop();
                        self.tunnel_selected_index = 0;
                        self.tunnel_expanded_request_id = None;
                    }
                    KeyCode::Char(c) => {
                        self.search_query.push(c);
                        self.tunnel_selected_index = 0;
                        self.tunnel_expanded_request_id = None;
                    }
                    _ => {}
                },
                KeyCode::Esc if !self.search_query.is_empty() => {
                    self.search_query.clear();
                    self.tunnel_selected_index = 0;
                    self.tunnel_expanded_request_id = None;
                }
                KeyCode::Char('q') | KeyCode::Esc => {
                    self.should_quit = true;
                }
                KeyCode::Char('/') => {
                    self.search_active = true;
                    self.search_query.clear();
                    self.tunnel_selected_index = 0;
                    self.tunnel_expanded_request_id = None;
                    self.tunnel_actions_open = false;
                }
                KeyCode::Up | KeyCode::Char('k') if self.tunnel_selected_index > 0 => {
                    self.tunnel_selected_index -= 1;
                    self.tunnel_expanded_request_id = None;
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let filtered = self.visible_tunnel_request_indices();
                    if !filtered.is_empty() && self.tunnel_selected_index < filtered.len() - 1 {
                        self.tunnel_selected_index += 1;
                        self.tunnel_expanded_request_id = None;
                    }
                }
                KeyCode::PageUp => {
                    self.tunnel_selected_index = self.tunnel_selected_index.saturating_sub(10);
                    self.tunnel_expanded_request_id = None;
                }
                KeyCode::PageDown => {
                    let filtered = self.visible_tunnel_request_indices();
                    if !filtered.is_empty() {
                        self.tunnel_selected_index =
                            (self.tunnel_selected_index + 10).min(filtered.len() - 1);
                        self.tunnel_expanded_request_id = None;
                    }
                }
                KeyCode::Home => {
                    self.tunnel_selected_index = 0;
                    self.tunnel_expanded_request_id = None;
                }
                KeyCode::End => {
                    let filtered = self.visible_tunnel_request_indices();
                    if !filtered.is_empty() {
                        self.tunnel_selected_index = filtered.len() - 1;
                        self.tunnel_expanded_request_id = None;
                    }
                }
                KeyCode::Tab => {
                    self.toggle_tunnel_pinned_view();
                }
                KeyCode::Enter => {
                    self.toggle_selected_tunnel_request_expansion();
                }
                KeyCode::Char('d') | KeyCode::Char('D') => {
                    self.open_selected_tunnel_request_detail();
                }
                KeyCode::Char('a') | KeyCode::Char('A') => {
                    self.tunnel_actions_open = true;
                }
                KeyCode::Char('r') | KeyCode::Char('R')
                    if key.modifiers.contains(KeyModifiers::CONTROL) =>
                {
                    self.request_tunnel_reconnect();
                }
                KeyCode::Char('r') | KeyCode::Char('R') => {
                    self.request_selected_tunnel_replay();
                }
                KeyCode::Char('c') | KeyCode::Char('C') => {
                    self.copy_tunnel_base_url();
                }
                KeyCode::Char('u') | KeyCode::Char('U') => {
                    self.copy_selected_tunnel_request_url();
                }
                KeyCode::Char('i') | KeyCode::Char('I') => {
                    self.copy_selected_tunnel_request_id();
                }
                KeyCode::Char('p') | KeyCode::Char('P') => {
                    self.toggle_selected_tunnel_request_pin();
                }
                _ => {}
            },
            AppState::ExportMenu => match key.code {
                KeyCode::Char('1') | KeyCode::Char('c') => {
                    if let Some(request) = &self.selected_request {
                        let curl = Self::generate_curl(request);
                        let request_id = request.id.clone();
                        self.copy_or_save_export(
                            &curl,
                            &request_id,
                            "sh",
                            "cURL command copied to clipboard.",
                        );
                    }
                    self.state = AppState::ShowRequestDetail;
                }
                KeyCode::Char('2') | KeyCode::Char('j') => {
                    if let Some(request) = &self.selected_request {
                        let request_id = request.id.clone();
                        match Self::generate_json_export(request) {
                            Ok(json) => {
                                self.copy_or_save_export(
                                    &json,
                                    &request_id,
                                    "json",
                                    "JSON copied to clipboard.",
                                );
                            }
                            Err(e) => {
                                self.set_status_feedback(
                                    FeedbackKind::Error,
                                    format!("Failed to serialize request: {e}"),
                                );
                            }
                        }
                    }
                    self.state = AppState::ShowRequestDetail;
                }
                KeyCode::Esc => {
                    self.state = AppState::ShowRequestDetail;
                }
                _ => {}
            },
            AppState::InputForwardUrl => match key.code {
                KeyCode::Enter => {
                    if self.forward_url_input.is_empty() {
                        self.set_status_feedback(
                            FeedbackKind::Error,
                            "Enter a target URL before forwarding.",
                        );
                    } else if !self.is_valid_url(&self.forward_url_input) {
                        self.set_status_feedback(
                            FeedbackKind::Error,
                            "Target URL needs a host and an http:// or https:// scheme.",
                        );
                    } else {
                        self.status_message = None;
                        self.state = AppState::ForwardingRequest;
                    }
                }
                KeyCode::Char(c) => {
                    self.forward_url_input.push(c);
                    self.status_message = None;
                }
                KeyCode::Backspace => {
                    self.forward_url_input.pop();
                    self.status_message = None;
                }
                KeyCode::Esc => {
                    self.state = AppState::ShowRequestDetail;
                }
                _ => {}
            },
            AppState::ForwardResult => match key.code {
                KeyCode::Char('q') => {
                    self.should_quit = true;
                }
                KeyCode::Char('b') | KeyCode::Esc => {
                    self.state = AppState::ShowRequestDetail;
                }
                _ => {}
            },
            AppState::Error { .. } => match key.code {
                KeyCode::Char('q') => {
                    self.should_quit = true;
                }
                KeyCode::Char('b') | KeyCode::Esc => {
                    self.state = AppState::ShowRequestDetail;
                }
                KeyCode::Char('r') => {
                    if self.selected_request.is_some() && self.is_valid_url(&self.forward_url_input)
                    {
                        self.state = AppState::ForwardingRequest;
                    } else {
                        self.set_status_feedback(
                            FeedbackKind::Warning,
                            "Retry unavailable: return to the request and set a valid target URL.",
                        );
                    }
                }
                _ => {}
            },
            _ => {}
        }

        Ok(())
    }

    pub fn is_valid_url(&self, url: &str) -> bool {
        reqwest::Url::parse(url).is_ok_and(|parsed| {
            matches!(parsed.scheme(), "http" | "https") && parsed.host_str().is_some()
        })
    }

    pub async fn forward_request(&mut self) -> Result<()> {
        if let Some(request) = &self.selected_request {
            let client = ApiClient::for_forwarding();

            match client
                .forward_request(request, &self.forward_url_input)
                .await
            {
                Ok(response) => {
                    self.forward_result = Some(response);
                    self.state = AppState::ForwardResult;
                }
                Err(e) => {
                    let hint = e
                        .downcast_ref::<ApiError>()
                        .and_then(|ae| ae.hint().map(String::from));
                    self.state = AppState::Error {
                        message: format!("Failed to forward request: {}", e),
                        hint,
                    };
                }
            }
        }

        Ok(())
    }

    pub fn generate_curl(request: &WebhookRequest) -> String {
        let mut parts = vec![format!("curl -X {} '{}'", request.method, request.url)];

        let skip_headers = ["cf-", "x-forwarded", "host", "content-length", "x-real-ip"];

        for (key, value) in &request.headers {
            let key_lower = key.to_lowercase();
            if skip_headers
                .iter()
                .any(|prefix| key_lower.starts_with(prefix))
            {
                continue;
            }
            let escaped_value = value.replace('\'', "'\\''");
            parts.push(format!("  -H '{}: {}'", key, escaped_value));
        }

        let has_body_method = matches!(request.method.as_str(), "POST" | "PUT" | "PATCH");
        if has_body_method {
            let body = request
                .body
                .as_ref()
                .or(request.body_preview.as_ref())
                .cloned()
                .unwrap_or_default();
            if !body.is_empty() {
                let escaped_body = body.replace('\'', "'\\''");
                parts.push(format!("  -d '{}'", escaped_body));
            }
        }

        parts.join(" \\\n")
    }

    pub fn generate_json_export(request: &WebhookRequest) -> Result<String> {
        Ok(serde_json::to_string_pretty(request)?)
    }

    fn export_filename(request_id: &str, extension: &str) -> String {
        let safe_id = request_id
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                    character
                } else {
                    '_'
                }
            })
            .collect::<String>();
        format!("hooklistener-{safe_id}.{extension}")
    }

    fn save_export_fallback(
        content: &str,
        request_id: &str,
        extension: &str,
    ) -> Result<std::path::PathBuf> {
        let path = std::env::current_dir()?.join(Self::export_filename(request_id, extension));
        std::fs::write(&path, content)?;
        Ok(path)
    }

    fn copy_or_save_export(
        &mut self,
        content: &str,
        request_id: &str,
        extension: &str,
        success_message: &'static str,
    ) {
        match arboard::Clipboard::new().and_then(|mut clipboard| clipboard.set_text(content)) {
            Ok(()) => self.set_status_feedback(FeedbackKind::Success, success_message),
            Err(clipboard_error) => {
                match Self::save_export_fallback(content, request_id, extension) {
                    Ok(path) => self.set_status_feedback(
                        FeedbackKind::Warning,
                        format!(
                            "Clipboard unavailable; saved export to {}.",
                            path.display()
                        ),
                    ),
                    Err(file_error) => self.set_status_feedback(
                        FeedbackKind::Error,
                        format!(
                            "Export failed: clipboard unavailable ({clipboard_error}); file write failed ({file_error})."
                        ),
                    ),
                }
            }
        }
    }

    pub fn filter_requests(requests: &VecDeque<WebhookRequest>, query: &str) -> Vec<usize> {
        if query.is_empty() {
            return (0..requests.len()).collect();
        }
        let q = query.to_lowercase();
        requests
            .iter()
            .enumerate()
            .filter(|(_, r)| {
                r.method.to_lowercase().contains(&q)
                    || r.url.to_lowercase().contains(&q)
                    || r.path
                        .as_deref()
                        .is_some_and(|p| p.to_lowercase().contains(&q))
                    || r.body_preview
                        .as_deref()
                        .is_some_and(|b| b.to_lowercase().contains(&q))
                    || r.remote_addr.to_lowercase().contains(&q)
            })
            .map(|(i, _)| i)
            .collect()
    }

    pub fn push_listening_request(&mut self, mut request: WebhookRequest) {
        let selected_request_id =
            Self::filter_requests(&self.listening_requests, &self.search_query)
                .get(self.selected_request_index)
                .and_then(|index| self.listening_requests.get(*index))
                .map(|request| request.id.clone());

        request.body = truncate_body(request.body);
        request.body_preview = truncate_body(request.body_preview);
        self.listening_requests.push_back(request);
        self.listening_stats.total_requests += 1;

        if self.listening_requests.len() > MAX_LISTENING_REQUESTS {
            self.listening_requests.pop_front();
        }

        let filtered = Self::filter_requests(&self.listening_requests, &self.search_query);
        if let Some(selected_request_id) = selected_request_id
            && let Some(position) = filtered.iter().position(|index| {
                self.listening_requests
                    .get(*index)
                    .is_some_and(|request| request.id == selected_request_id)
            })
        {
            self.selected_request_index = position;
        } else {
            self.selected_request_index = self
                .selected_request_index
                .min(filtered.len().saturating_sub(1));
        }
    }

    fn contains_query(value: &str, query: &str) -> bool {
        value.to_lowercase().contains(query)
    }

    fn tunnel_status_matches(request: &TunnelRequest, query: &str) -> bool {
        request
            .status
            .is_some_and(|status| status.to_string().contains(query))
            || request
                .error
                .as_deref()
                .is_some_and(|error| Self::contains_query(error, query))
            || (request.error.is_some() && "error".contains(query))
            || (request.status.is_none() && request.error.is_none() && "pending".contains(query))
    }

    fn tunnel_headers_match(request: &TunnelRequest, query: &str) -> bool {
        request.headers.iter().any(|(key, value)| {
            Self::contains_query(key, query) || Self::contains_query(value, query)
        })
    }

    fn tunnel_request_matches_field(request: &TunnelRequest, field: &str, query: &str) -> bool {
        match field {
            "method" => Self::contains_query(&request.method, query),
            "path" | "url" => {
                Self::contains_query(&request.path, query)
                    || Self::contains_query(&request.query_string, query)
            }
            "status" => Self::tunnel_status_matches(request, query),
            "body" => {
                request
                    .body
                    .as_deref()
                    .is_some_and(|body| Self::contains_query(body, query))
                    || request
                        .response_body
                        .as_deref()
                        .is_some_and(|body| Self::contains_query(body, query))
            }
            "header" | "headers" | "from" | "ip" => Self::tunnel_headers_match(request, query),
            "error" => request
                .error
                .as_deref()
                .is_some_and(|error| Self::contains_query(error, query)),
            "pinned" | "pin" => {
                Self::parse_bool_filter(query).is_some_and(|expected| request.pinned == expected)
            }
            _ => false,
        }
    }

    fn tunnel_request_matches_query(request: &TunnelRequest, query: &str) -> bool {
        let query = query.trim().to_lowercase();
        if query.is_empty() {
            return true;
        }

        if matches!(query.as_str(), "pinned" | "pin") {
            return request.pinned;
        }

        if let Some((field, value)) = query.split_once(':') {
            let value = value.trim();
            if !value.is_empty() && Self::tunnel_request_matches_field(request, field.trim(), value)
            {
                return true;
            }
        }

        Self::contains_query(&request.method, &query)
            || Self::contains_query(&request.path, &query)
            || Self::contains_query(&request.query_string, &query)
            || Self::tunnel_status_matches(request, &query)
            || Self::tunnel_headers_match(request, &query)
            || request
                .body
                .as_deref()
                .is_some_and(|body| Self::contains_query(body, &query))
            || request
                .response_body
                .as_deref()
                .is_some_and(|body| Self::contains_query(body, &query))
    }

    fn parse_bool_filter(value: &str) -> Option<bool> {
        match value.trim() {
            "true" | "yes" | "1" | "on" => Some(true),
            "false" | "no" | "0" | "off" => Some(false),
            _ => None,
        }
    }

    pub fn filter_tunnel_requests(requests: &VecDeque<TunnelRequest>, query: &str) -> Vec<usize> {
        requests
            .iter()
            .enumerate()
            .rev()
            .filter(|(_, request)| Self::tunnel_request_matches_query(request, query))
            .map(|(index, _)| index)
            .collect()
    }

    pub fn visible_tunnel_request_indices(&self) -> Vec<usize> {
        Self::filter_tunnel_requests(&self.tunnel_requests, &self.search_query)
            .into_iter()
            .filter(|request_index| {
                !self.tunnel_pinned_only
                    || self
                        .tunnel_requests
                        .get(*request_index)
                        .is_some_and(|request| request.pinned)
            })
            .collect()
    }

    fn find_visible_tunnel_request_position(&self, request_id: &str) -> Option<usize> {
        self.visible_tunnel_request_indices()
            .iter()
            .position(|request_index| {
                self.tunnel_requests
                    .get(*request_index)
                    .is_some_and(|request| request.request_id == request_id)
            })
    }

    fn selected_tunnel_request_index(&self) -> Option<usize> {
        self.visible_tunnel_request_indices()
            .get(self.tunnel_selected_index)
            .copied()
    }

    fn selected_tunnel_request(&self) -> Option<&TunnelRequest> {
        self.selected_tunnel_request_index()
            .and_then(|request_index| self.tunnel_requests.get(request_index))
    }

    fn selected_tunnel_request_id(&self) -> Option<String> {
        self.selected_tunnel_request()
            .map(|request| request.request_id.clone())
    }

    pub fn is_tunnel_follow_pinned(&self) -> bool {
        self.tunnel_selected_index != 0 || self.tunnel_expanded_request_id.is_some()
    }

    pub fn tunnel_pinned_count(&self) -> usize {
        self.tunnel_requests
            .iter()
            .filter(|request| request.pinned)
            .count()
    }

    fn tunnel_request_public_path(request: &TunnelRequest) -> String {
        format_path_with_query(&request.path, &request.query_string)
    }

    fn selected_tunnel_request_url(&self) -> Option<String> {
        let subdomain = self
            .tunnel_subdomain
            .as_deref()
            .map(str::trim)
            .filter(|subdomain| !subdomain.is_empty())?
            .trim_end_matches('/');
        let request = self.selected_tunnel_request()?;

        Some(format!(
            "https://{}{}",
            subdomain,
            Self::tunnel_request_public_path(request)
        ))
    }

    fn selected_tunnel_replay_request(&self) -> Option<TunnelReplayRequest> {
        let request = self.selected_tunnel_request()?;

        Some(TunnelReplayRequest {
            request_id: request.request_id.clone(),
            method: request.method.clone(),
            path: request.path.clone(),
            query_string: request.query_string.clone(),
            headers: request.headers.clone(),
            body: request.body.clone(),
            local_host: self.tunnel_local_host.clone(),
            local_port: self.tunnel_local_port,
        })
    }

    fn request_selected_tunnel_replay(&mut self) {
        let Some(replay_request) = self.selected_tunnel_replay_request() else {
            self.set_tunnel_feedback(FeedbackKind::Warning, "No request selected to replay");
            return;
        };

        if is_truncated_body(replay_request.body.as_deref()) {
            self.set_tunnel_feedback(FeedbackKind::Warning, "Replay unavailable: body truncated");
            return;
        }

        self.tunnel_replay_requested = Some(replay_request);
        self.set_tunnel_feedback(FeedbackKind::Info, "Replaying selected request...");
    }

    fn request_tunnel_reconnect(&mut self) {
        self.tunnel_reconnect_requested = true;
        self.tunnel_connected = false;
        self.tunnel_connected_at = None;
        self.tunnel_error = Some("Manual reconnect requested...".to_string());
        self.set_tunnel_feedback(FeedbackKind::Info, "Restarting tunnel connection...");
    }

    fn copy_tunnel_base_url(&mut self) {
        if let Some(subdomain) = &self.tunnel_subdomain {
            let url = format!("https://{}", subdomain);
            self.copy_tunnel_text_to_clipboard(&url, "Base URL copied to clipboard!");
        } else {
            self.set_tunnel_feedback(FeedbackKind::Warning, "Tunnel URL is not ready yet");
        }
    }

    fn copy_selected_tunnel_request_url(&mut self) {
        if let Some(url) = self.selected_tunnel_request_url() {
            self.copy_tunnel_text_to_clipboard(&url, "Request URL copied to clipboard!");
        } else {
            self.set_tunnel_feedback(FeedbackKind::Warning, "No request URL selected to copy");
        }
    }

    fn copy_selected_tunnel_request_id(&mut self) {
        if let Some(request_id) = self.selected_tunnel_request_id() {
            self.copy_tunnel_text_to_clipboard(&request_id, "Request ID copied to clipboard!");
        } else {
            self.set_tunnel_feedback(FeedbackKind::Warning, "No request ID selected to copy");
        }
    }

    fn toggle_selected_tunnel_request_pin(&mut self) {
        let Some(request_index) = self.selected_tunnel_request_index() else {
            self.set_tunnel_feedback(FeedbackKind::Warning, "No request selected to pin");
            return;
        };

        let Some(request) = self.tunnel_requests.get_mut(request_index) else {
            return;
        };
        request.pinned = !request.pinned;
        let request_id = request.request_id.clone();
        let pinned = request.pinned;

        let status_message = if pinned {
            "Request pinned"
        } else {
            "Request unpinned"
        };
        self.set_tunnel_feedback(FeedbackKind::Info, status_message);

        if let Some(position) = self.find_visible_tunnel_request_position(&request_id) {
            self.tunnel_selected_index = position;
        } else {
            self.clamp_tunnel_selection();
            if self.tunnel_expanded_request_id.as_deref() == Some(request_id.as_str()) {
                self.tunnel_expanded_request_id = None;
            }
        }
    }

    fn toggle_tunnel_pinned_view(&mut self) {
        let selected_request_id = self.selected_tunnel_request_id();
        self.tunnel_pinned_only = !self.tunnel_pinned_only;

        if let Some(selected_request_id) = selected_request_id
            && let Some(position) = self.find_visible_tunnel_request_position(&selected_request_id)
        {
            self.tunnel_selected_index = position;
        } else {
            self.clamp_tunnel_selection();
        }

        if let Some(expanded_id) = &self.tunnel_expanded_request_id
            && self
                .find_visible_tunnel_request_position(expanded_id)
                .is_none()
        {
            self.tunnel_expanded_request_id = None;
        }

        let status_message = if self.tunnel_pinned_only {
            "Showing pinned requests"
        } else {
            "Showing all requests"
        };
        self.set_tunnel_feedback(FeedbackKind::Info, status_message);
    }

    fn copy_tunnel_text_to_clipboard(&mut self, text: &str, success_message: &'static str) {
        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(text)) {
            Ok(_) => {
                self.set_tunnel_feedback(FeedbackKind::Success, success_message);
            }
            Err(e) => {
                self.set_tunnel_feedback(FeedbackKind::Error, format!("Failed to copy: {e}"));
            }
        }
    }

    pub fn set_tunnel_feedback(&mut self, kind: FeedbackKind, message: impl Into<String>) {
        self.tunnel_status_message = Some(FeedbackMessage::new(kind, message));
    }

    fn set_status_feedback(&mut self, kind: FeedbackKind, message: impl Into<String>) {
        self.status_message = Some(FeedbackMessage::new(kind, message));
    }

    fn clamp_tunnel_selection(&mut self) {
        let filtered_len = self.visible_tunnel_request_indices().len();
        if filtered_len == 0 {
            self.tunnel_selected_index = 0;
        } else {
            self.tunnel_selected_index = self.tunnel_selected_index.min(filtered_len - 1);
        }
    }

    pub fn push_tunnel_request(&mut self, request: TunnelRequest) {
        let selected_request_id = if self.is_tunnel_follow_pinned() {
            self.selected_tunnel_request_id()
        } else {
            None
        };

        self.tunnel_requests.push_back(request);
        if self.tunnel_requests.len() > MAX_TUNNEL_REQUESTS {
            self.tunnel_requests.pop_front();
        }

        if let Some(expanded_id) = &self.tunnel_expanded_request_id
            && !self
                .tunnel_requests
                .iter()
                .any(|request| request.request_id == *expanded_id)
        {
            self.tunnel_expanded_request_id = None;
        }

        if let Some(selected_request_id) = selected_request_id
            && let Some(position) = self.find_visible_tunnel_request_position(&selected_request_id)
        {
            self.tunnel_selected_index = position;
            return;
        }

        self.clamp_tunnel_selection();
    }

    fn toggle_selected_tunnel_request_expansion(&mut self) {
        let Some(request_index) = self.selected_tunnel_request_index() else {
            return;
        };
        let Some(request) = self.tunnel_requests.get(request_index) else {
            return;
        };

        if self.tunnel_expanded_request_id.as_deref() == Some(request.request_id.as_str()) {
            self.tunnel_expanded_request_id = None;
        } else {
            self.tunnel_expanded_request_id = Some(request.request_id.clone());
        }
    }

    fn open_selected_tunnel_request_detail(&mut self) {
        if let Some(request_index) = self.selected_tunnel_request_index() {
            self.open_tunnel_request_detail(request_index);
        } else {
            self.set_tunnel_feedback(FeedbackKind::Warning, "No request selected for details");
        }
    }

    fn open_tunnel_request_detail(&mut self, request_index: usize) {
        let Some(tunnel_req) = self.tunnel_requests.get(request_index).cloned() else {
            return;
        };
        let public_path = Self::tunnel_request_public_path(&tunnel_req);

        let webhook_req = WebhookRequest {
            id: tunnel_req.request_id,
            timestamp: 0,
            remote_addr: "Tunnel".to_string(),
            headers: tunnel_req.headers,
            content_length: tunnel_req
                .body
                .as_ref()
                .map(|b| b.len() as i64)
                .unwrap_or(0),
            method: tunnel_req.method,
            url: public_path,
            path: Some(tunnel_req.path),
            query_params: parse_query_string(&tunnel_req.query_string),
            created_at: chrono::Utc::now().to_rfc3339(),
            body_preview: tunnel_req.body.clone(),
            body: tunnel_req.body,
        };
        self.selected_request = Some(webhook_req);

        let duration_ms = tunnel_req
            .completed_at
            .map(|completed| completed.duration_since(tunnel_req.received_at).as_millis() as u64);
        self.selected_tunnel_response = Some(TunnelResponseData {
            status: tunnel_req.status,
            headers: tunnel_req.response_headers.unwrap_or_default(),
            body: tunnel_req.response_body,
            duration_ms,
            error: tunnel_req.error,
        });

        self.current_tab = 0;
        self.headers_scroll_offset = 0;
        self.body_scroll_offset = 0;
        self.response_headers_scroll_offset = 0;
        self.response_scroll_offset = 0;
        self.detail_return_state = Some(DetailReturnTarget::Tunneling);
        self.state = AppState::ShowRequestDetail;
    }

    pub fn tick(&mut self) {
        // Update loading animation frame
        self.loading_frame = (self.loading_frame + 1) % 8;

        // Expire tunnel status message after 2s
        if let Some(feedback) = &self.tunnel_status_message
            && feedback.created_at.elapsed() > std::time::Duration::from_secs(2)
        {
            self.tunnel_status_message = None;
        }

        // Expire general status message after 3s
        if let Some(feedback) = &self.status_message
            && feedback.created_at.elapsed() > std::time::Duration::from_secs(3)
        {
            self.status_message = None;
        }
    }

    pub fn take_tunnel_reconnect_request(&mut self) -> bool {
        std::mem::take(&mut self.tunnel_reconnect_requested)
    }

    pub fn take_tunnel_replay_request(&mut self) -> Option<TunnelReplayRequest> {
        self.tunnel_replay_requested.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn make_config() -> Config {
        Config {
            access_token: Some("test-token".to_string()),
            ..Config::default()
        }
    }

    fn key_event(code: KeyCode) -> KeyEvent {
        KeyEvent {
            code,
            modifiers: KeyModifiers::NONE,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn key_event_with_modifiers(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent {
            code,
            modifiers,
            kind: KeyEventKind::Press,
            state: KeyEventState::NONE,
        }
    }

    fn make_app_with_state(state: AppState) -> App {
        let mut app = App::with_config(make_config());
        app.state = state;
        app
    }

    fn make_tunnel_request(
        id: &str,
        method: &str,
        path: &str,
        status: Option<u16>,
    ) -> TunnelRequest {
        TunnelRequest {
            request_id: id.to_string(),
            method: method.to_string(),
            path: path.to_string(),
            received_at: std::time::Instant::now(),
            status,
            completed_at: status.map(|_| std::time::Instant::now()),
            error: None,
            headers: HashMap::new(),
            body: None,
            query_string: String::new(),
            response_headers: None,
            response_body: None,
            pinned: false,
        }
    }

    // is_valid_url tests
    #[test]
    fn test_is_valid_url_http() {
        let app = App::with_config(make_config());
        assert!(app.is_valid_url("http://localhost:3000"));
    }

    #[test]
    fn test_is_valid_url_https() {
        let app = App::with_config(make_config());
        assert!(app.is_valid_url("https://example.com/webhook"));
    }

    #[test]
    fn test_is_valid_url_ftp_invalid() {
        let app = App::with_config(make_config());
        assert!(!app.is_valid_url("ftp://example.com"));
    }

    #[test]
    fn test_is_valid_url_empty() {
        let app = App::with_config(make_config());
        assert!(!app.is_valid_url(""));
    }

    #[test]
    fn test_is_valid_url_garbage() {
        let app = App::with_config(make_config());
        assert!(!app.is_valid_url("not a url"));
    }

    #[test]
    fn test_is_valid_url_requires_host() {
        let app = App::with_config(make_config());
        assert!(!app.is_valid_url("http://"));
    }

    // tick tests
    #[test]
    fn test_tick_advances_loading_frame() {
        let mut app = App::with_config(make_config());
        assert_eq!(app.loading_frame, 0);
        app.tick();
        assert_eq!(app.loading_frame, 1);
    }

    #[test]
    fn test_tick_wraps_at_8() {
        let mut app = App::with_config(make_config());
        app.loading_frame = 7;
        app.tick();
        assert_eq!(app.loading_frame, 0);
    }

    // with_config tests
    #[test]
    fn test_with_config_defaults_to_listening() {
        let app = App::with_config(make_config());
        assert!(matches!(app.state, AppState::Listening));
    }

    #[test]
    fn test_tunnel_stats_record_response_tracks_status_buckets() {
        let mut stats = TunnelStats::default();

        stats.record_response(200, 20);
        stats.record_response(404, 30);
        stats.record_response(503, 40);

        assert_eq!(stats.success, 3);
        assert_eq!(stats.status_2xx, 1);
        assert_eq!(stats.status_4xx, 1);
        assert_eq!(stats.status_5xx, 1);
        assert_eq!(stats.total_duration_ms, 90);
    }

    #[test]
    fn test_filter_tunnel_requests_empty_query_returns_newest_first() {
        let mut requests = VecDeque::new();
        requests.push_back(make_tunnel_request("old", "GET", "/old", Some(200)));
        requests.push_back(make_tunnel_request("new", "POST", "/new", Some(201)));

        assert_eq!(App::filter_tunnel_requests(&requests, ""), vec![1, 0]);
    }

    #[test]
    fn test_filter_tunnel_requests_supports_field_queries() {
        let mut requests = VecDeque::new();
        requests.push_back(make_tunnel_request("ok", "GET", "/health", Some(200)));
        requests.push_back(make_tunnel_request(
            "fail",
            "POST",
            "/webhooks/stripe",
            Some(500),
        ));
        requests[0].pinned = true;

        assert_eq!(
            App::filter_tunnel_requests(&requests, "method:post"),
            vec![1]
        );
        assert_eq!(
            App::filter_tunnel_requests(&requests, "status:500"),
            vec![1]
        );
        assert_eq!(
            App::filter_tunnel_requests(&requests, "path:health"),
            vec![0]
        );
        assert_eq!(
            App::filter_tunnel_requests(&requests, "pinned:true"),
            vec![0]
        );
        assert_eq!(
            App::filter_tunnel_requests(&requests, "pinned:false"),
            vec![1]
        );
    }

    #[test]
    fn test_visible_tunnel_request_indices_respects_pinned_view() {
        let mut app = make_app_with_state(AppState::Tunneling);
        let mut pinned = make_tunnel_request("pinned", "GET", "/important", Some(200));
        pinned.pinned = true;
        app.tunnel_requests
            .push_back(make_tunnel_request("normal", "GET", "/normal", Some(200)));
        app.tunnel_requests.push_back(pinned);

        assert_eq!(app.visible_tunnel_request_indices(), vec![1, 0]);

        app.tunnel_pinned_only = true;
        assert_eq!(app.visible_tunnel_request_indices(), vec![1]);
    }

    #[test]
    fn test_slash_activates_search_in_tunneling() {
        let mut app = make_app_with_state(AppState::Tunneling);

        app.handle_key_event(key_event(KeyCode::Char('/'))).unwrap();
        app.handle_key_event(key_event(KeyCode::Char('p'))).unwrap();

        assert!(app.search_active);
        assert_eq!(app.search_query, "p");
        assert_eq!(app.tunnel_selected_index, 0);
    }

    #[test]
    fn test_esc_from_tunneling_clears_inactive_filter() {
        let mut app = make_app_with_state(AppState::Tunneling);
        app.search_query = "status:500".to_string();
        app.tunnel_selected_index = 2;

        app.handle_key_event(key_event(KeyCode::Esc)).unwrap();

        assert!(!app.should_quit);
        assert!(app.search_query.is_empty());
        assert_eq!(app.tunnel_selected_index, 0);
    }

    #[test]
    fn test_enter_from_tunneling_expands_filtered_selection() {
        let mut app = make_app_with_state(AppState::Tunneling);
        app.tunnel_requests
            .push_back(make_tunnel_request("get", "GET", "/health", Some(200)));
        app.tunnel_requests
            .push_back(make_tunnel_request("post", "POST", "/webhooks", Some(202)));
        app.search_query = "method:get".to_string();

        app.handle_key_event(key_event(KeyCode::Enter)).unwrap();

        assert!(matches!(app.state, AppState::Tunneling));
        assert_eq!(app.tunnel_expanded_request_id.as_deref(), Some("get"));
    }

    // handle_key_event state transitions
    #[test]
    fn test_esc_from_input_forward_url_goes_to_request_detail() {
        let mut app = make_app_with_state(AppState::InputForwardUrl);
        app.handle_key_event(key_event(KeyCode::Esc)).unwrap();
        assert!(matches!(app.state, AppState::ShowRequestDetail));
    }

    #[test]
    fn test_char_input_in_forward_url() {
        let mut app = make_app_with_state(AppState::InputForwardUrl);
        app.handle_key_event(key_event(KeyCode::Char('h'))).unwrap();
        app.handle_key_event(key_event(KeyCode::Char('t'))).unwrap();
        assert_eq!(app.forward_url_input, "ht");
    }

    #[test]
    fn test_backspace_in_forward_url() {
        let mut app = make_app_with_state(AppState::InputForwardUrl);
        app.forward_url_input = "http".to_string();
        app.handle_key_event(key_event(KeyCode::Backspace)).unwrap();
        assert_eq!(app.forward_url_input, "htt");
    }

    #[test]
    fn test_tab_cycling_in_request_detail() {
        let mut app = make_app_with_state(AppState::ShowRequestDetail);
        app.selected_request = Some(WebhookRequest {
            id: "r1".to_string(),
            timestamp: 0,
            remote_addr: "".to_string(),
            headers: std::collections::HashMap::new(),
            content_length: 0,
            method: "GET".to_string(),
            url: "/".to_string(),
            path: None,
            query_params: std::collections::HashMap::new(),
            created_at: "".to_string(),
            body_preview: None,
            body: None,
        });
        assert_eq!(app.current_tab, 0);
        app.handle_key_event(key_event(KeyCode::Tab)).unwrap();
        assert_eq!(app.current_tab, 1);
        app.handle_key_event(key_event(KeyCode::Tab)).unwrap();
        assert_eq!(app.current_tab, 2);
        app.handle_key_event(key_event(KeyCode::Tab)).unwrap();
        assert_eq!(app.current_tab, 0);
    }

    #[test]
    fn test_q_from_listening_quits() {
        let mut app = make_app_with_state(AppState::Listening);
        app.handle_key_event(key_event(KeyCode::Char('q'))).unwrap();
        assert!(app.should_quit);
    }

    #[test]
    fn test_q_from_tunneling_quits() {
        let mut app = make_app_with_state(AppState::Tunneling);
        app.handle_key_event(key_event(KeyCode::Char('q'))).unwrap();
        assert!(app.should_quit);
    }

    #[test]
    fn test_ctrl_r_from_tunneling_requests_reconnect() {
        let mut app = make_app_with_state(AppState::Tunneling);
        app.handle_key_event(key_event_with_modifiers(
            KeyCode::Char('r'),
            KeyModifiers::CONTROL,
        ))
        .unwrap();
        assert!(app.take_tunnel_reconnect_request());
        assert!(app.tunnel_status_message.is_some());
        assert!(app.tunnel_error.is_some());
    }

    #[test]
    fn test_q_from_error_quits() {
        let mut app = make_app_with_state(AppState::Error {
            message: "test".to_string(),
            hint: None,
        });
        app.handle_key_event(key_event(KeyCode::Char('q'))).unwrap();
        assert!(app.should_quit);
    }

    #[test]
    fn test_back_from_request_detail_goes_to_listening() {
        let mut app = make_app_with_state(AppState::ShowRequestDetail);
        app.detail_return_state = Some(DetailReturnTarget::Listening);
        app.handle_key_event(key_event(KeyCode::Char('b'))).unwrap();
        assert!(matches!(app.state, AppState::Listening));
    }

    #[test]
    fn test_back_from_request_detail_goes_to_tunneling() {
        let mut app = make_app_with_state(AppState::ShowRequestDetail);
        app.detail_return_state = Some(DetailReturnTarget::Tunneling);
        app.handle_key_event(key_event(KeyCode::Char('b'))).unwrap();
        assert!(matches!(app.state, AppState::Tunneling));
    }

    #[test]
    fn test_enter_from_tunneling_toggles_expanded_request() {
        let mut app = make_app_with_state(AppState::Tunneling);
        app.tunnel_requests
            .push_back(make_tunnel_request("req-1", "POST", "/webhook", Some(200)));

        app.handle_key_event(key_event(KeyCode::Enter)).unwrap();
        assert_eq!(app.tunnel_expanded_request_id.as_deref(), Some("req-1"));

        app.handle_key_event(key_event(KeyCode::Enter)).unwrap();
        assert_eq!(app.tunnel_expanded_request_id, None);
    }

    #[test]
    fn test_p_from_tunneling_toggles_selected_pin() {
        let mut app = make_app_with_state(AppState::Tunneling);
        app.tunnel_requests
            .push_back(make_tunnel_request("req-1", "POST", "/webhook", Some(200)));

        app.handle_key_event(key_event(KeyCode::Char('p'))).unwrap();
        assert!(app.tunnel_requests[0].pinned);
        assert_eq!(app.tunnel_pinned_count(), 1);
        assert_eq!(
            app.tunnel_status_message
                .as_ref()
                .map(|feedback| feedback.message.as_str()),
            Some("Request pinned")
        );

        app.handle_key_event(key_event(KeyCode::Char('p'))).unwrap();
        assert!(!app.tunnel_requests[0].pinned);
        assert_eq!(app.tunnel_pinned_count(), 0);
    }

    #[test]
    fn test_p_from_tunneling_without_selection_sets_status_message() {
        let mut app = make_app_with_state(AppState::Tunneling);

        app.handle_key_event(key_event(KeyCode::Char('p'))).unwrap();

        assert_eq!(
            app.tunnel_status_message
                .as_ref()
                .map(|feedback| feedback.message.as_str()),
            Some("No request selected to pin")
        );
    }

    #[test]
    fn test_tab_from_tunneling_toggles_pinned_view_and_preserves_pinned_selection() {
        let mut app = make_app_with_state(AppState::Tunneling);
        let mut pinned = make_tunnel_request("pinned", "GET", "/important", Some(200));
        pinned.pinned = true;
        app.tunnel_requests.push_back(pinned);
        app.tunnel_requests
            .push_back(make_tunnel_request("normal", "GET", "/normal", Some(200)));
        app.tunnel_selected_index = 1;

        app.handle_key_event(key_event(KeyCode::Tab)).unwrap();

        assert!(app.tunnel_pinned_only);
        assert_eq!(app.tunnel_selected_index, 0);
        assert_eq!(app.selected_tunnel_request_id().as_deref(), Some("pinned"));
        assert_eq!(
            app.tunnel_status_message
                .as_ref()
                .map(|feedback| feedback.message.as_str()),
            Some("Showing pinned requests")
        );

        app.handle_key_event(key_event(KeyCode::Tab)).unwrap();

        assert!(!app.tunnel_pinned_only);
        assert_eq!(app.tunnel_selected_index, 1);
        assert_eq!(app.selected_tunnel_request_id().as_deref(), Some("pinned"));
    }

    #[test]
    fn test_tab_from_tunneling_selects_first_pin_when_selection_is_unpinned() {
        let mut app = make_app_with_state(AppState::Tunneling);
        let mut pinned = make_tunnel_request("pinned", "GET", "/important", Some(200));
        pinned.pinned = true;
        app.tunnel_requests.push_back(pinned);
        app.tunnel_requests
            .push_back(make_tunnel_request("normal", "GET", "/normal", Some(200)));
        app.tunnel_selected_index = 0;

        app.handle_key_event(key_event(KeyCode::Tab)).unwrap();

        assert!(app.tunnel_pinned_only);
        assert_eq!(app.selected_tunnel_request_id().as_deref(), Some("pinned"));
    }

    #[test]
    fn test_push_tunnel_request_follows_newest_when_at_top() {
        let mut app = make_app_with_state(AppState::Tunneling);
        app.tunnel_requests
            .push_back(make_tunnel_request("old", "GET", "/old", Some(200)));

        app.push_tunnel_request(make_tunnel_request("new", "GET", "/new", Some(201)));

        assert_eq!(app.tunnel_selected_index, 0);
        assert_eq!(app.selected_tunnel_request_id().as_deref(), Some("new"));
    }

    #[test]
    fn test_push_tunnel_request_preserves_selected_older_request() {
        let mut app = make_app_with_state(AppState::Tunneling);
        app.tunnel_requests
            .push_back(make_tunnel_request("old", "GET", "/old", Some(200)));
        app.tunnel_requests
            .push_back(make_tunnel_request("current", "GET", "/current", Some(201)));
        app.tunnel_selected_index = 1;

        app.push_tunnel_request(make_tunnel_request("new", "GET", "/new", Some(202)));

        assert_eq!(app.tunnel_selected_index, 2);
        assert_eq!(app.selected_tunnel_request_id().as_deref(), Some("old"));
    }

    #[test]
    fn test_push_tunnel_request_preserves_expanded_top_request() {
        let mut app = make_app_with_state(AppState::Tunneling);
        app.tunnel_requests.push_back(make_tunnel_request(
            "current",
            "POST",
            "/webhook",
            Some(200),
        ));
        app.tunnel_expanded_request_id = Some("current".to_string());

        app.push_tunnel_request(make_tunnel_request("new", "POST", "/new", Some(201)));

        assert_eq!(app.tunnel_selected_index, 1);
        assert_eq!(app.selected_tunnel_request_id().as_deref(), Some("current"));
        assert_eq!(app.tunnel_expanded_request_id.as_deref(), Some("current"));
    }

    #[test]
    fn test_tunnel_follow_state_is_live_at_top() {
        let app = make_app_with_state(AppState::Tunneling);

        assert!(!app.is_tunnel_follow_pinned());
    }

    #[test]
    fn test_tunnel_follow_state_is_pinned_for_older_or_expanded_request() {
        let mut app = make_app_with_state(AppState::Tunneling);
        app.tunnel_selected_index = 1;

        assert!(app.is_tunnel_follow_pinned());

        app.tunnel_selected_index = 0;
        app.tunnel_expanded_request_id = Some("req-1".to_string());

        assert!(app.is_tunnel_follow_pinned());
    }

    #[test]
    fn test_selected_tunnel_request_url_includes_path_and_query() {
        let mut app = make_app_with_state(AppState::Tunneling);
        app.tunnel_subdomain = Some("abc123.hook.events".to_string());
        let mut request = make_tunnel_request("req-1", "GET", "search", Some(200));
        request.query_string = "q=hello%20world".to_string();
        app.tunnel_requests.push_back(request);

        assert_eq!(
            app.selected_tunnel_request_url().as_deref(),
            Some("https://abc123.hook.events/search?q=hello%20world")
        );
    }

    #[test]
    fn test_selected_tunnel_request_url_uses_filtered_selection() {
        let mut app = make_app_with_state(AppState::Tunneling);
        app.tunnel_subdomain = Some("abc123.hook.events".to_string());
        app.tunnel_requests
            .push_back(make_tunnel_request("get", "GET", "/health", Some(200)));
        app.tunnel_requests
            .push_back(make_tunnel_request("post", "POST", "/webhook", Some(202)));
        app.search_query = "method:get".to_string();

        assert_eq!(
            app.selected_tunnel_request_url().as_deref(),
            Some("https://abc123.hook.events/health")
        );
        assert_eq!(app.selected_tunnel_request_id().as_deref(), Some("get"));
    }

    #[test]
    fn test_selected_tunnel_request_url_missing_without_subdomain_or_request() {
        let mut app = make_app_with_state(AppState::Tunneling);
        app.tunnel_requests
            .push_back(make_tunnel_request("req-1", "GET", "/health", Some(200)));

        assert_eq!(app.selected_tunnel_request_url(), None);

        app.tunnel_subdomain = Some("abc123.hook.events".to_string());
        app.search_query = "method:post".to_string();

        assert_eq!(app.selected_tunnel_request_url(), None);
    }

    #[test]
    fn test_r_from_tunneling_requests_selected_replay() {
        let mut app = make_app_with_state(AppState::Tunneling);
        app.tunnel_local_host = "127.0.0.1".to_string();
        app.tunnel_local_port = 4567;
        let mut request = make_tunnel_request("req-1", "POST", "/webhook", Some(200));
        request.query_string = "debug=true".to_string();
        request
            .headers
            .insert("content-type".to_string(), "application/json".to_string());
        request.body = Some("{\"ok\":true}".to_string());
        app.tunnel_requests.push_back(request);

        app.handle_key_event(key_event(KeyCode::Char('r'))).unwrap();

        let replay = app
            .take_tunnel_replay_request()
            .expect("selected tunnel request should be queued for replay");
        assert_eq!(replay.request_id, "req-1");
        assert_eq!(replay.method, "POST");
        assert_eq!(
            replay.target_url(),
            "http://127.0.0.1:4567/webhook?debug=true"
        );
        assert_eq!(replay.body.as_deref(), Some("{\"ok\":true}"));
        assert!(!app.take_tunnel_reconnect_request());
    }

    #[test]
    fn test_r_from_tunneling_without_selection_sets_status_message() {
        let mut app = make_app_with_state(AppState::Tunneling);

        app.handle_key_event(key_event(KeyCode::Char('r'))).unwrap();

        assert!(app.take_tunnel_replay_request().is_none());
        assert_eq!(
            app.tunnel_status_message
                .as_ref()
                .map(|feedback| feedback.message.as_str()),
            Some("No request selected to replay")
        );
    }

    #[test]
    fn test_r_from_tunneling_refuses_truncated_body_replay() {
        let mut app = make_app_with_state(AppState::Tunneling);
        let mut request = make_tunnel_request("req-1", "POST", "/webhook", Some(200));
        request.body = Some(format!("partial{}", TRUNCATED_BODY_MARKER));
        app.tunnel_requests.push_back(request);

        app.handle_key_event(key_event(KeyCode::Char('r'))).unwrap();

        assert!(app.take_tunnel_replay_request().is_none());
        assert_eq!(
            app.tunnel_status_message
                .as_ref()
                .map(|feedback| feedback.message.as_str()),
            Some("Replay unavailable: body truncated")
        );
    }

    #[test]
    fn test_a_from_tunneling_opens_actions() {
        let mut app = make_app_with_state(AppState::Tunneling);

        app.handle_key_event(key_event(KeyCode::Char('a'))).unwrap();

        assert!(app.tunnel_actions_open);
    }

    #[test]
    fn test_esc_closes_tunnel_actions_without_quitting() {
        let mut app = make_app_with_state(AppState::Tunneling);
        app.tunnel_actions_open = true;

        app.handle_key_event(key_event(KeyCode::Esc)).unwrap();

        assert!(!app.tunnel_actions_open);
        assert!(!app.should_quit);
    }

    #[test]
    fn test_q_from_action_menu_quits() {
        let mut app = make_app_with_state(AppState::Tunneling);
        app.tunnel_actions_open = true;

        app.handle_key_event(key_event(KeyCode::Char('q'))).unwrap();

        assert!(!app.tunnel_actions_open);
        assert!(app.should_quit);
    }

    #[test]
    fn test_action_menu_r_requests_replay_and_closes() {
        let mut app = make_app_with_state(AppState::Tunneling);
        app.tunnel_actions_open = true;
        app.tunnel_requests
            .push_back(make_tunnel_request("req-1", "GET", "/health", Some(200)));

        app.handle_key_event(key_event(KeyCode::Char('r'))).unwrap();

        assert!(!app.tunnel_actions_open);
        assert_eq!(
            app.take_tunnel_replay_request()
                .map(|request| request.request_id),
            Some("req-1".to_string())
        );
    }

    #[test]
    fn test_action_menu_p_toggles_pin_and_closes() {
        let mut app = make_app_with_state(AppState::Tunneling);
        app.tunnel_actions_open = true;
        app.tunnel_requests
            .push_back(make_tunnel_request("req-1", "GET", "/health", Some(200)));

        app.handle_key_event(key_event(KeyCode::Char('p'))).unwrap();

        assert!(!app.tunnel_actions_open);
        assert!(app.tunnel_requests[0].pinned);
    }

    #[test]
    fn test_action_menu_tab_toggles_pinned_view_and_closes() {
        let mut app = make_app_with_state(AppState::Tunneling);
        app.tunnel_actions_open = true;

        app.handle_key_event(key_event(KeyCode::Tab)).unwrap();

        assert!(!app.tunnel_actions_open);
        assert!(app.tunnel_pinned_only);
    }

    #[test]
    fn test_action_menu_ctrl_r_requests_reconnect_and_closes() {
        let mut app = make_app_with_state(AppState::Tunneling);
        app.tunnel_actions_open = true;

        app.handle_key_event(key_event_with_modifiers(
            KeyCode::Char('r'),
            KeyModifiers::CONTROL,
        ))
        .unwrap();

        assert!(!app.tunnel_actions_open);
        assert!(app.take_tunnel_reconnect_request());
    }

    #[test]
    fn test_tunnel_replay_request_target_url_normalizes_path() {
        let replay = TunnelReplayRequest {
            request_id: "req-1".to_string(),
            method: "GET".to_string(),
            path: "health".to_string(),
            query_string: "check=true".to_string(),
            headers: HashMap::new(),
            body: None,
            local_host: "localhost".to_string(),
            local_port: 3000,
        };

        assert_eq!(
            replay.target_url(),
            "http://localhost:3000/health?check=true"
        );
    }

    #[test]
    fn test_d_from_tunneling_opens_detail() {
        let mut app = make_app_with_state(AppState::Tunneling);
        app.tunnel_requests.push_back(TunnelRequest {
            request_id: "req-1".to_string(),
            method: "POST".to_string(),
            path: "/webhook".to_string(),
            received_at: std::time::Instant::now(),
            status: Some(200),
            completed_at: Some(std::time::Instant::now()),
            error: None,
            headers: HashMap::new(),
            body: Some("{\"test\":true}".to_string()),
            query_string: String::new(),
            response_headers: Some(HashMap::from([(
                "content-type".to_string(),
                "application/json".to_string(),
            )])),
            response_body: Some("{\"ok\":true}".to_string()),
            pinned: false,
        });
        app.tunnel_selected_index = 0;
        app.handle_key_event(key_event(KeyCode::Char('d'))).unwrap();
        assert!(matches!(app.state, AppState::ShowRequestDetail));
        assert_eq!(app.detail_return_state, Some(DetailReturnTarget::Tunneling));
        assert!(app.selected_request.is_some());
        assert!(app.selected_tunnel_response.is_some());
        let resp = app.selected_tunnel_response.unwrap();
        assert_eq!(resp.status, Some(200));
        assert!(resp.body.is_some());
    }

    #[test]
    fn test_d_from_tunneling_decodes_query_params() {
        let mut app = make_app_with_state(AppState::Tunneling);
        app.tunnel_requests.push_back(TunnelRequest {
            request_id: "req-2".to_string(),
            method: "GET".to_string(),
            path: "/search".to_string(),
            received_at: std::time::Instant::now(),
            status: Some(200),
            completed_at: Some(std::time::Instant::now()),
            error: None,
            headers: HashMap::new(),
            body: None,
            query_string: "q=hello%20world&plus=a+b".to_string(),
            response_headers: Some(HashMap::new()),
            response_body: None,
            pinned: false,
        });
        app.tunnel_selected_index = 0;
        app.handle_key_event(key_event(KeyCode::Char('d'))).unwrap();

        let selected = app
            .selected_request
            .expect("selected request should be set");
        assert_eq!(selected.url, "/search?q=hello%20world&plus=a+b");
        assert_eq!(
            selected.query_params.get("q"),
            Some(&"hello world".to_string())
        );
        assert_eq!(selected.query_params.get("plus"), Some(&"a b".to_string()));
    }

    #[test]
    fn test_tab_cycling_with_response_tab() {
        let mut app = make_app_with_state(AppState::ShowRequestDetail);
        app.selected_request = Some(make_request("GET", "/"));
        app.selected_tunnel_response = Some(TunnelResponseData {
            status: Some(200),
            headers: HashMap::new(),
            body: None,
            duration_ms: Some(42),
            error: None,
        });
        assert_eq!(app.current_tab, 0);
        // Cycle through 4 tabs: 0 -> 1 -> 2 -> 3 -> 0
        app.handle_key_event(key_event(KeyCode::Tab)).unwrap();
        assert_eq!(app.current_tab, 1);
        app.handle_key_event(key_event(KeyCode::Tab)).unwrap();
        assert_eq!(app.current_tab, 2);
        app.handle_key_event(key_event(KeyCode::Tab)).unwrap();
        assert_eq!(app.current_tab, 3);
        app.handle_key_event(key_event(KeyCode::Tab)).unwrap();
        assert_eq!(app.current_tab, 0);
    }

    #[test]
    fn test_response_tab_scrolls_headers_then_body() {
        let mut app = make_app_with_state(AppState::ShowRequestDetail);
        app.selected_request = Some(make_request("GET", "/"));

        let headers = (0..10)
            .map(|i| (format!("x-test-{}", i), format!("value-{}", i)))
            .collect();
        let body = (0..30)
            .map(|i| format!("line {}", i))
            .collect::<Vec<_>>()
            .join("\n");

        app.selected_tunnel_response = Some(TunnelResponseData {
            status: Some(200),
            headers,
            body: Some(body),
            duration_ms: Some(42),
            error: None,
        });
        app.current_tab = 3;

        let headers_max = 10usize.saturating_sub(1);
        for _ in 0..headers_max {
            app.handle_key_event(key_event(KeyCode::Down)).unwrap();
        }
        assert_eq!(app.response_headers_scroll_offset, headers_max);
        assert_eq!(app.response_scroll_offset, 0);

        app.handle_key_event(key_event(KeyCode::Down)).unwrap();
        assert_eq!(app.response_headers_scroll_offset, headers_max);
        assert_eq!(app.response_scroll_offset, 1);
    }

    #[test]
    fn test_body_scroll_advances_for_compact_json() {
        let mut app = make_app_with_state(AppState::ShowRequestDetail);
        let mut request = make_request("POST", "/webhook");
        let items = (0..50).map(|i| i.to_string()).collect::<Vec<_>>().join(",");
        request.body = Some(format!(r#"{{"items":[{}]}}"#, items));
        app.selected_request = Some(request);
        app.current_tab = 2;

        app.handle_key_event(key_event(KeyCode::Down)).unwrap();

        assert_eq!(app.body_scroll_offset, 1);
    }

    #[test]
    fn test_header_scroll_advances_for_long_wrapped_value() {
        let mut app = make_app_with_state(AppState::ShowRequestDetail);
        let large_value = "a".repeat(2_500);
        app.selected_request = Some(make_request_with_headers(
            "POST",
            "/webhook",
            vec![("x-large-header", large_value.as_str())],
        ));
        app.current_tab = 1;

        app.handle_key_event(key_event(KeyCode::Down)).unwrap();

        assert_eq!(app.headers_scroll_offset, 1);
    }

    #[test]
    fn test_truncate_body_under_limit() {
        let small = Some("hello".to_string());
        assert_eq!(truncate_body(small), Some("hello".to_string()));
    }

    #[test]
    fn test_truncate_body_over_limit() {
        let large = Some("x".repeat(MAX_BODY_SIZE + 100));
        let result = truncate_body(large).unwrap();
        assert!(result.len() < MAX_BODY_SIZE + 100);
        assert!(result.ends_with("\n...(truncated)"));
    }

    #[test]
    fn test_truncate_body_over_limit_utf8_boundary() {
        // 3-byte codepoint makes MAX_BODY_SIZE likely land mid-character.
        let large = Some("€".repeat((MAX_BODY_SIZE / 3) + 100));
        let result = truncate_body(large).unwrap();

        assert!(result.ends_with("\n...(truncated)"));
        let prefix = result.strip_suffix("\n...(truncated)").unwrap();
        assert!(prefix.len() <= MAX_BODY_SIZE);
        assert!(prefix.is_char_boundary(prefix.len()));
    }

    #[test]
    fn test_truncate_body_none() {
        assert_eq!(truncate_body(None), None);
    }

    // Helper to build test WebhookRequests
    fn make_request(method: &str, url: &str) -> WebhookRequest {
        WebhookRequest {
            id: uuid::Uuid::new_v4().to_string(),
            timestamp: 0,
            remote_addr: "127.0.0.1".to_string(),
            headers: std::collections::HashMap::new(),
            content_length: 0,
            method: method.to_string(),
            url: url.to_string(),
            path: Some(url.to_string()),
            query_params: std::collections::HashMap::new(),
            created_at: "2024-01-01T00:00:00Z".to_string(),
            body_preview: None,
            body: None,
        }
    }

    fn make_request_with_headers(
        method: &str,
        url: &str,
        headers: Vec<(&str, &str)>,
    ) -> WebhookRequest {
        let mut r = make_request(method, url);
        for (k, v) in headers {
            r.headers.insert(k.to_string(), v.to_string());
        }
        r
    }

    // === Tunnel status message tests ===

    #[test]
    fn test_tunnel_status_message_clears_after_tick() {
        let mut app = App::with_config(make_config());
        // Set a message with an instant far in the past
        app.tunnel_status_message = Some(FeedbackMessage {
            kind: FeedbackKind::Info,
            message: "Test".to_string(),
            created_at: std::time::Instant::now() - std::time::Duration::from_secs(5),
        });
        app.tick();
        assert!(app.tunnel_status_message.is_none());
    }

    #[test]
    fn test_tunnel_status_message_persists_when_fresh() {
        let mut app = App::with_config(make_config());
        app.tunnel_status_message = Some(FeedbackMessage::new(FeedbackKind::Info, "Test"));
        app.tick();
        assert!(app.tunnel_status_message.is_some());
    }

    // === Replay tests ===

    #[test]
    fn test_replay_with_valid_url_transitions_to_forwarding() {
        let mut app = make_app_with_state(AppState::ShowRequestDetail);
        app.selected_request = Some(make_request("POST", "/webhook"));
        app.forward_url_input = "http://localhost:3000".to_string();
        app.handle_key_event(key_event(KeyCode::Char('r'))).unwrap();
        assert!(matches!(app.state, AppState::ForwardingRequest));
    }

    #[test]
    fn test_replay_without_url_stays_in_detail() {
        let mut app = make_app_with_state(AppState::ShowRequestDetail);
        app.selected_request = Some(make_request("POST", "/webhook"));
        app.forward_url_input.clear();
        app.handle_key_event(key_event(KeyCode::Char('r'))).unwrap();
        assert!(matches!(app.state, AppState::ShowRequestDetail));
    }

    #[test]
    fn test_replay_with_invalid_url_stays_in_detail() {
        let mut app = make_app_with_state(AppState::ShowRequestDetail);
        app.selected_request = Some(make_request("POST", "/webhook"));
        app.forward_url_input = "not-a-url".to_string();
        app.handle_key_event(key_event(KeyCode::Char('r'))).unwrap();
        assert!(matches!(app.state, AppState::ShowRequestDetail));
    }

    #[test]
    fn invalid_forward_url_reports_error_without_leaving_input() {
        let mut app = make_app_with_state(AppState::InputForwardUrl);
        app.selected_request = Some(make_request("POST", "/webhook"));
        app.forward_url_input = "ftp://example.com".to_string();

        app.handle_key_event(key_event(KeyCode::Enter)).unwrap();

        assert!(matches!(app.state, AppState::InputForwardUrl));
        assert_eq!(
            app.status_message.as_ref().map(|feedback| feedback.kind),
            Some(FeedbackKind::Error)
        );
    }

    #[test]
    fn error_escape_returns_to_request_detail() {
        let mut app = make_app_with_state(AppState::Error {
            message: "Forward failed".to_string(),
            hint: None,
        });

        app.handle_key_event(key_event(KeyCode::Esc)).unwrap();

        assert!(matches!(app.state, AppState::ShowRequestDetail));
        assert!(!app.should_quit);
    }

    #[test]
    fn error_retry_restarts_forward_when_target_is_valid() {
        let mut app = make_app_with_state(AppState::Error {
            message: "Forward failed".to_string(),
            hint: None,
        });
        app.selected_request = Some(make_request("POST", "/webhook"));
        app.forward_url_input = "http://localhost:3000".to_string();

        app.handle_key_event(key_event(KeyCode::Char('r'))).unwrap();

        assert!(matches!(app.state, AppState::ForwardingRequest));
    }

    // === Export tests ===

    #[test]
    fn test_e_key_opens_export_menu() {
        let mut app = make_app_with_state(AppState::ShowRequestDetail);
        app.selected_request = Some(make_request("GET", "/test"));
        app.handle_key_event(key_event(KeyCode::Char('e'))).unwrap();
        assert!(matches!(app.state, AppState::ExportMenu));
    }

    #[test]
    fn test_e_key_no_request_stays_in_detail() {
        let mut app = make_app_with_state(AppState::ShowRequestDetail);
        app.selected_request = None;
        app.handle_key_event(key_event(KeyCode::Char('e'))).unwrap();
        assert!(matches!(app.state, AppState::ShowRequestDetail));
    }

    #[test]
    fn test_esc_from_export_menu_returns_to_detail() {
        let mut app = make_app_with_state(AppState::ExportMenu);
        app.handle_key_event(key_event(KeyCode::Esc)).unwrap();
        assert!(matches!(app.state, AppState::ShowRequestDetail));
    }

    #[test]
    fn test_generate_curl_basic_get() {
        let request = make_request("GET", "https://example.com/hook");
        let curl = App::generate_curl(&request);
        assert!(curl.contains("curl -X GET"));
        assert!(curl.contains("https://example.com/hook"));
        // GET should not have -d
        assert!(!curl.contains("-d"));
    }

    #[test]
    fn test_generate_curl_post_with_body() {
        let request = make_request_with_headers(
            "POST",
            "https://example.com/hook",
            vec![("content-type", "application/json")],
        );
        let mut request = request;
        request.body = Some(r#"{"key":"value"}"#.to_string());

        let curl = App::generate_curl(&request);
        assert!(curl.contains("curl -X POST"));
        assert!(curl.contains("-d"));
        assert!(curl.contains("-H 'content-type: application/json'"));
    }

    #[test]
    fn test_generate_curl_skips_internal_headers() {
        let request = make_request_with_headers(
            "GET",
            "https://example.com/hook",
            vec![
                ("cf-connecting-ip", "1.2.3.4"),
                ("x-forwarded-for", "1.2.3.4"),
                ("host", "example.com"),
                ("content-length", "42"),
                ("x-real-ip", "1.2.3.4"),
                ("authorization", "Bearer tok"),
            ],
        );

        let curl = App::generate_curl(&request);
        assert!(!curl.contains("cf-connecting-ip"));
        assert!(!curl.contains("x-forwarded-for"));
        assert!(!curl.contains("host"));
        assert!(!curl.contains("content-length"));
        assert!(!curl.contains("x-real-ip"));
        // authorization should be kept
        assert!(curl.contains("authorization"));
    }

    #[test]
    fn test_generate_json_export() {
        let request = make_request("POST", "/webhook");
        let json = App::generate_json_export(&request).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(parsed["method"], "POST");
        assert_eq!(parsed["url"], "/webhook");
    }

    // === Search / filter tests ===

    #[test]
    fn test_filter_requests_empty_query_returns_all() {
        let requests = vec![make_request("GET", "/a"), make_request("POST", "/b")].into();
        let result = App::filter_requests(&requests, "");
        assert_eq!(result, vec![0, 1]);
    }

    #[test]
    fn test_filter_requests_by_method() {
        let requests = vec![
            make_request("GET", "/a"),
            make_request("POST", "/b"),
            make_request("GET", "/c"),
        ]
        .into();
        let result = App::filter_requests(&requests, "POST");
        assert_eq!(result, vec![1]);
    }

    #[test]
    fn test_filter_requests_by_path() {
        let requests = vec![
            make_request("GET", "/api/webhook"),
            make_request("POST", "/api/users"),
            make_request("GET", "/webhook/test"),
        ]
        .into();
        let result = App::filter_requests(&requests, "webhook");
        assert_eq!(result, vec![0, 2]);
    }

    #[test]
    fn test_filter_requests_case_insensitive() {
        let requests = vec![
            make_request("GET", "/API/Webhook"),
            make_request("POST", "/other"),
        ]
        .into();
        let result = App::filter_requests(&requests, "webhook");
        assert_eq!(result, vec![0]);
    }

    #[test]
    fn listening_history_is_bounded_while_total_remains_lifetime_count() {
        let mut app = App::with_config(make_config());

        for index in 0..=MAX_LISTENING_REQUESTS {
            let mut request = make_request("POST", "/webhook");
            request.id = format!("req-{index}");
            app.push_listening_request(request);
        }

        assert_eq!(app.listening_requests.len(), MAX_LISTENING_REQUESTS);
        assert_eq!(
            app.listening_stats.total_requests,
            (MAX_LISTENING_REQUESTS + 1) as u64
        );
        assert_eq!(
            app.listening_requests
                .front()
                .map(|request| request.id.as_str()),
            Some("req-1")
        );
    }

    #[test]
    fn listening_history_truncates_oversized_bodies_before_storage() {
        let mut app = App::with_config(make_config());
        let mut request = make_request("POST", "/webhook");
        request.body = Some("x".repeat(MAX_BODY_SIZE + 1));

        app.push_listening_request(request);

        assert!(
            app.listening_requests
                .front()
                .and_then(|request| request.body.as_deref())
                .is_some_and(|body| body.ends_with(TRUNCATED_BODY_MARKER))
        );
    }

    #[test]
    fn export_filename_replaces_unsafe_characters() {
        assert_eq!(
            App::export_filename("req/../../unsafe", "json"),
            "hooklistener-req_______unsafe.json"
        );
    }

    #[test]
    fn test_slash_activates_search_in_listening() {
        let mut app = make_app_with_state(AppState::Listening);
        assert!(!app.search_active);
        app.handle_key_event(key_event(KeyCode::Char('/'))).unwrap();
        assert!(app.search_active);
    }

    #[test]
    fn test_status_message_clears_after_tick() {
        let mut app = App::with_config(make_config());
        app.status_message = Some(FeedbackMessage {
            kind: FeedbackKind::Success,
            message: "Copied!".to_string(),
            created_at: std::time::Instant::now() - std::time::Duration::from_secs(5),
        });
        app.tick();
        assert!(app.status_message.is_none());
    }
}
