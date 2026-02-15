use crate::api::ApiClient;
use crate::config::Config;
use crate::errors::ApiError;
use crate::models::{ForwardResponse, WebhookRequest};
use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use std::collections::{HashMap, VecDeque};

pub const MAX_TUNNEL_REQUESTS: usize = 500;
pub const MAX_BODY_SIZE: usize = 256 * 1024;

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
    pub total_duration_ms: u64,
    #[allow(dead_code)]
    pub bytes_in: u64,
    #[allow(dead_code)]
    pub bytes_out: u64,
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
}

#[derive(Debug, Clone)]
pub struct TunnelResponseData {
    pub status: Option<u16>,
    pub headers: HashMap<String, String>,
    pub body: Option<String>,
    pub duration_ms: Option<u64>,
    pub error: Option<String>,
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
            truncated.push_str("\n...(truncated)");
            truncated
        } else {
            b
        }
    })
}

const VIEWPORT_LINES: usize = 20;

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
    let line_count = text.lines().count();
    line_count.saturating_sub(VIEWPORT_LINES)
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

    // Listening mode state (debug endpoints)
    pub listening_requests: Vec<WebhookRequest>,
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

    // Status messages (auto-expire)
    pub tunnel_status_message: Option<(String, std::time::Instant)>,
    pub status_message: Option<(String, std::time::Instant)>,

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
            listening_requests: Vec::new(),
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
            tunnel_status_message: None,
            status_message: None,
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
            .map(|r| r.headers.len().saturating_sub(1))
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
                        self.state = AppState::InputForwardUrl;
                    }
                    KeyCode::Char('r') => {
                        if !self.forward_url_input.is_empty()
                            && self.is_valid_url(&self.forward_url_input)
                            && self.selected_request.is_some()
                        {
                            self.state = AppState::ForwardingRequest;
                        }
                    }
                    KeyCode::Char('e') => {
                        if self.selected_request.is_some() {
                            self.state = AppState::ExportMenu;
                        }
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
                            1 => {
                                // Headers tab
                                if self.headers_scroll_offset > 0 {
                                    self.headers_scroll_offset -= 1;
                                }
                            }
                            2 => {
                                // Body tab
                                if self.body_scroll_offset > 0 {
                                    self.body_scroll_offset -= 1;
                                }
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
                                let max = request.headers.len().saturating_sub(1);
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
                                let max = request.headers.len().saturating_sub(1);
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
                                self.headers_scroll_offset =
                                    request.headers.len().saturating_sub(1);
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
                KeyCode::Char('q') | KeyCode::Esc => {
                    self.should_quit = true;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    if self.tunnel_selected_index > 0 {
                        self.tunnel_selected_index -= 1;
                    }
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    if !self.tunnel_requests.is_empty()
                        && self.tunnel_selected_index < self.tunnel_requests.len() - 1
                    {
                        self.tunnel_selected_index += 1;
                    }
                }
                KeyCode::PageUp => {
                    self.tunnel_selected_index = self.tunnel_selected_index.saturating_sub(10);
                }
                KeyCode::PageDown => {
                    if !self.tunnel_requests.is_empty() {
                        self.tunnel_selected_index =
                            (self.tunnel_selected_index + 10).min(self.tunnel_requests.len() - 1);
                    }
                }
                KeyCode::Home => {
                    self.tunnel_selected_index = 0;
                }
                KeyCode::End => {
                    if !self.tunnel_requests.is_empty() {
                        self.tunnel_selected_index = self.tunnel_requests.len() - 1;
                    }
                }
                KeyCode::Enter => {
                    if !self.tunnel_requests.is_empty() {
                        // Reversed list: index 0 = newest = last element in deque
                        let reversed_idx =
                            self.tunnel_requests.len() - 1 - self.tunnel_selected_index;
                        if let Some(tunnel_req) = self.tunnel_requests.get(reversed_idx) {
                            // Build a WebhookRequest from the TunnelRequest
                            let webhook_req = WebhookRequest {
                                id: tunnel_req.request_id.clone(),
                                timestamp: 0,
                                remote_addr: "Tunnel".to_string(),
                                headers: tunnel_req.headers.clone(),
                                content_length: tunnel_req
                                    .body
                                    .as_ref()
                                    .map(|b| b.len() as i64)
                                    .unwrap_or(0),
                                method: tunnel_req.method.clone(),
                                url: tunnel_req.path.clone(),
                                path: Some(tunnel_req.path.clone()),
                                query_params: parse_query_string(&tunnel_req.query_string),
                                created_at: chrono::Utc::now().to_rfc3339(),
                                body_preview: tunnel_req.body.clone(),
                                body: tunnel_req.body.clone(),
                            };
                            self.selected_request = Some(webhook_req);

                            // Build response data if available
                            let duration_ms = tunnel_req.completed_at.map(|completed| {
                                completed.duration_since(tunnel_req.received_at).as_millis() as u64
                            });
                            self.selected_tunnel_response = Some(TunnelResponseData {
                                status: tunnel_req.status,
                                headers: tunnel_req.response_headers.clone().unwrap_or_default(),
                                body: tunnel_req.response_body.clone(),
                                duration_ms,
                                error: tunnel_req.error.clone(),
                            });

                            self.current_tab = 0;
                            self.headers_scroll_offset = 0;
                            self.body_scroll_offset = 0;
                            self.response_headers_scroll_offset = 0;
                            self.response_scroll_offset = 0;
                            self.detail_return_state = Some(DetailReturnTarget::Tunneling);
                            self.state = AppState::ShowRequestDetail;
                        }
                    }
                }
                KeyCode::Char('c') => {
                    if let Some(subdomain) = &self.tunnel_subdomain {
                        let url = format!("https://{}", subdomain);
                        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(&url)) {
                            Ok(_) => {
                                self.tunnel_status_message = Some((
                                    "URL copied to clipboard!".into(),
                                    std::time::Instant::now(),
                                ));
                            }
                            Err(e) => {
                                self.tunnel_status_message = Some((
                                    format!("Failed to copy: {}", e),
                                    std::time::Instant::now(),
                                ));
                            }
                        }
                    }
                }
                KeyCode::Char('r') => {
                    self.tunnel_reconnect_requested = true;
                    self.tunnel_connected = false;
                    self.tunnel_connected_at = None;
                    self.tunnel_error = Some("Manual reconnect requested...".to_string());
                    self.tunnel_status_message = Some((
                        "Restarting tunnel connection...".into(),
                        std::time::Instant::now(),
                    ));
                }
                _ => {}
            },
            AppState::ExportMenu => match key.code {
                KeyCode::Char('1') | KeyCode::Char('c') => {
                    if let Some(request) = &self.selected_request {
                        let curl = Self::generate_curl(request);
                        match arboard::Clipboard::new().and_then(|mut cb| cb.set_text(&curl)) {
                            Ok(_) => {
                                self.status_message = Some((
                                    "cURL command copied to clipboard!".into(),
                                    std::time::Instant::now(),
                                ));
                            }
                            Err(e) => {
                                self.status_message = Some((
                                    format!("Failed to copy: {}", e),
                                    std::time::Instant::now(),
                                ));
                            }
                        }
                    }
                    self.state = AppState::ShowRequestDetail;
                }
                KeyCode::Char('2') | KeyCode::Char('j') => {
                    if let Some(request) = &self.selected_request {
                        match Self::generate_json_export(request) {
                            Ok(json) => {
                                match arboard::Clipboard::new()
                                    .and_then(|mut cb| cb.set_text(&json))
                                {
                                    Ok(_) => {
                                        self.status_message = Some((
                                            "JSON copied to clipboard!".into(),
                                            std::time::Instant::now(),
                                        ));
                                    }
                                    Err(e) => {
                                        self.status_message = Some((
                                            format!("Failed to copy: {}", e),
                                            std::time::Instant::now(),
                                        ));
                                    }
                                }
                            }
                            Err(e) => {
                                self.status_message = Some((
                                    format!("Failed to serialize: {}", e),
                                    std::time::Instant::now(),
                                ));
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
                    if !self.forward_url_input.is_empty()
                        && self.is_valid_url(&self.forward_url_input)
                    {
                        self.state = AppState::ForwardingRequest;
                    }
                }
                KeyCode::Char(c) => {
                    self.forward_url_input.push(c);
                }
                KeyCode::Backspace => {
                    self.forward_url_input.pop();
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
                KeyCode::Char('q') | KeyCode::Esc => {
                    self.should_quit = true;
                }
                _ => {}
            },
            _ => {}
        }

        Ok(())
    }

    pub fn is_valid_url(&self, url: &str) -> bool {
        url.starts_with("http://") || url.starts_with("https://")
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

    pub fn filter_requests(requests: &[WebhookRequest], query: &str) -> Vec<usize> {
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

    pub fn tick(&mut self) {
        // Update loading animation frame
        self.loading_frame = (self.loading_frame + 1) % 8;

        // Expire tunnel status message after 2s
        if let Some((_, created_at)) = &self.tunnel_status_message
            && created_at.elapsed() > std::time::Duration::from_secs(2)
        {
            self.tunnel_status_message = None;
        }

        // Expire general status message after 3s
        if let Some((_, created_at)) = &self.status_message
            && created_at.elapsed() > std::time::Duration::from_secs(3)
        {
            self.status_message = None;
        }
    }

    pub fn take_tunnel_reconnect_request(&mut self) -> bool {
        std::mem::take(&mut self.tunnel_reconnect_requested)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers};

    fn make_config() -> Config {
        Config {
            access_token: Some("test-token".to_string()),
            token_expires_at: None,
            selected_organization_id: None,
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

    fn make_app_with_state(state: AppState) -> App {
        let mut app = App::with_config(make_config());
        app.state = state;
        app
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
    fn test_r_from_tunneling_requests_reconnect() {
        let mut app = make_app_with_state(AppState::Tunneling);
        app.handle_key_event(key_event(KeyCode::Char('r'))).unwrap();
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
    fn test_enter_from_tunneling_opens_detail() {
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
        });
        app.tunnel_selected_index = 0;
        app.handle_key_event(key_event(KeyCode::Enter)).unwrap();
        assert!(matches!(app.state, AppState::ShowRequestDetail));
        assert_eq!(app.detail_return_state, Some(DetailReturnTarget::Tunneling));
        assert!(app.selected_request.is_some());
        assert!(app.selected_tunnel_response.is_some());
        let resp = app.selected_tunnel_response.unwrap();
        assert_eq!(resp.status, Some(200));
        assert!(resp.body.is_some());
    }

    #[test]
    fn test_enter_from_tunneling_decodes_query_params() {
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
        });
        app.tunnel_selected_index = 0;
        app.handle_key_event(key_event(KeyCode::Enter)).unwrap();

        let selected = app
            .selected_request
            .expect("selected request should be set");
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
        app.tunnel_status_message = Some((
            "Test".to_string(),
            std::time::Instant::now() - std::time::Duration::from_secs(5),
        ));
        app.tick();
        assert!(app.tunnel_status_message.is_none());
    }

    #[test]
    fn test_tunnel_status_message_persists_when_fresh() {
        let mut app = App::with_config(make_config());
        app.tunnel_status_message = Some(("Test".to_string(), std::time::Instant::now()));
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
        let requests = vec![make_request("GET", "/a"), make_request("POST", "/b")];
        let result = App::filter_requests(&requests, "");
        assert_eq!(result, vec![0, 1]);
    }

    #[test]
    fn test_filter_requests_by_method() {
        let requests = vec![
            make_request("GET", "/a"),
            make_request("POST", "/b"),
            make_request("GET", "/c"),
        ];
        let result = App::filter_requests(&requests, "POST");
        assert_eq!(result, vec![1]);
    }

    #[test]
    fn test_filter_requests_by_path() {
        let requests = vec![
            make_request("GET", "/api/webhook"),
            make_request("POST", "/api/users"),
            make_request("GET", "/webhook/test"),
        ];
        let result = App::filter_requests(&requests, "webhook");
        assert_eq!(result, vec![0, 2]);
    }

    #[test]
    fn test_filter_requests_case_insensitive() {
        let requests = vec![
            make_request("GET", "/API/Webhook"),
            make_request("POST", "/other"),
        ];
        let result = App::filter_requests(&requests, "webhook");
        assert_eq!(result, vec![0]);
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
        app.status_message = Some((
            "Copied!".to_string(),
            std::time::Instant::now() - std::time::Duration::from_secs(5),
        ));
        app.tick();
        assert!(app.status_message.is_none());
    }
}
