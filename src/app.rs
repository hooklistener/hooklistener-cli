use anyhow::Result;
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};
use crate::api::ApiClient;
use crate::config::Config;
use crate::models::{DebugEndpoint, DebugEndpointDetail, WebhookRequest, ForwardResponse};

#[derive(Debug)]
pub enum AppState {
    InputApiKey,
    Loading,
    ShowEndpoints,
    ShowEndpointDetail,
    ShowRequests,
    ShowRequestDetail,
    InputForwardUrl,
    ForwardingRequest,
    ForwardResult,
    Error(String),
}

pub struct App {
    pub state: AppState,
    pub config: Config,
    pub api_key_input: String,
    pub endpoints: Vec<DebugEndpoint>,
    pub selected_index: usize,
    pub selected_endpoint: Option<DebugEndpointDetail>,
    pub requests: Vec<WebhookRequest>,
    pub requests_pagination: Option<crate::models::Pagination>,
    pub selected_request_index: usize,
    pub selected_request: Option<WebhookRequest>,
    pub current_page: i32,
    pub forward_url_input: String,
    pub forward_result: Option<ForwardResponse>,
    pub current_tab: usize,
    pub headers_scroll_offset: usize,
    pub body_scroll_offset: usize,
    pub should_quit: bool,
    pub loading_frame: usize, // For animated loading spinner
}

impl App {
    pub fn new() -> Result<Self> {
        let config = Config::load()?;
        
        let state = if config.api_key.is_some() {
            AppState::Loading
        } else {
            AppState::InputApiKey
        };

        Ok(Self {
            state,
            config,
            api_key_input: String::new(),
            endpoints: Vec::new(),
            selected_index: 0,
            selected_endpoint: None,
            requests: Vec::new(),
            requests_pagination: None,
            selected_request_index: 0,
            selected_request: None,
            current_page: 1,
            forward_url_input: String::new(),
            forward_result: None,
            current_tab: 0,
            headers_scroll_offset: 0,
            body_scroll_offset: 0,
            should_quit: false,
            loading_frame: 0,
        })
    }

    pub async fn load_endpoints(&mut self) -> Result<()> {
        if let Some(api_key) = &self.config.api_key {
            let client = ApiClient::new(api_key.clone());
            
            match client.fetch_debug_endpoints().await {
                Ok(endpoints) => {
                    self.endpoints = endpoints;
                    self.state = AppState::ShowEndpoints;
                }
                Err(e) => {
                    self.state = AppState::Error(format!("Failed to fetch endpoints: {}", e));
                }
            }
        } else {
            self.state = AppState::InputApiKey;
        }
        
        Ok(())
    }

    pub async fn load_endpoint_detail(&mut self, endpoint_id: &str) -> Result<()> {
        if let Some(api_key) = &self.config.api_key {
            let client = ApiClient::new(api_key.clone());
            
            match client.fetch_endpoint_detail(endpoint_id).await {
                Ok(detail) => {
                    self.selected_endpoint = Some(detail);
                    self.state = AppState::ShowEndpointDetail;
                }
                Err(e) => {
                    self.state = AppState::Error(format!("Failed to fetch endpoint detail: {}", e));
                }
            }
        }
        
        Ok(())
    }

    pub async fn load_requests(&mut self, endpoint_id: &str) -> Result<()> {
        if let Some(api_key) = &self.config.api_key {
            let client = ApiClient::new(api_key.clone());
            
            match client.fetch_endpoint_requests(endpoint_id, self.current_page, 50).await {
                Ok(response) => {
                    self.requests = response.data;
                    self.requests_pagination = Some(response.pagination);
                    self.selected_request_index = 0;
                    self.state = AppState::ShowRequests;
                }
                Err(e) => {
                    self.state = AppState::Error(format!("Failed to fetch requests: {}", e));
                }
            }
        }
        
        Ok(())
    }

    pub async fn load_request_details(&mut self, endpoint_id: &str, request_id: &str) -> Result<()> {
        if let Some(api_key) = &self.config.api_key {
            let client = ApiClient::new(api_key.clone());
            
            match client.fetch_request_details(endpoint_id, request_id).await {
                Ok(request_detail) => {
                    self.selected_request = Some(request_detail);
                    self.current_tab = 0;
                    self.headers_scroll_offset = 0;
                    self.body_scroll_offset = 0;
                    self.state = AppState::ShowRequestDetail;
                }
                Err(_) => {
                    // Fallback: Use the request from the list with just the preview
                    if let Some(request) = self.requests.get(self.selected_request_index) {
                        self.selected_request = Some(request.clone());
                        self.current_tab = 0;
                        self.headers_scroll_offset = 0;
                        self.body_scroll_offset = 0;
                        self.state = AppState::ShowRequestDetail;
                    } else {
                        self.state = AppState::Error("Request not found in the current list".to_string());
                    }
                }
            }
        }
        
        Ok(())
    }

    pub fn handle_key_event(&mut self, key: KeyEvent) -> Result<()> {
        if key.kind != KeyEventKind::Press {
            return Ok(());
        }

        match &self.state {
            AppState::InputApiKey => {
                match key.code {
                    KeyCode::Enter => {
                        if !self.api_key_input.is_empty() {
                            self.config.set_api_key(self.api_key_input.clone());
                            self.config.save()?;
                            self.state = AppState::Loading;
                        }
                    }
                    KeyCode::Char(c) => {
                        self.api_key_input.push(c);
                    }
                    KeyCode::Backspace => {
                        self.api_key_input.pop();
                    }
                    KeyCode::Esc => {
                        self.should_quit = true;
                    }
                    _ => {}
                }
            }
            AppState::ShowEndpoints => {
                match key.code {
                    KeyCode::Up => {
                        if self.selected_index > 0 {
                            self.selected_index -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if self.selected_index < self.endpoints.len().saturating_sub(1) {
                            self.selected_index += 1;
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(_endpoint) = self.endpoints.get(self.selected_index) {
                            self.state = AppState::Loading;
                        }
                    }
                    KeyCode::Char('q') => {
                        self.should_quit = true;
                    }
                    KeyCode::Char('r') => {
                        self.state = AppState::Loading;
                    }
                    _ => {}
                }
            }
            AppState::ShowEndpointDetail => {
                match key.code {
                    KeyCode::Char('q') => {
                        self.should_quit = true;
                    }
                    KeyCode::Char('b') | KeyCode::Esc => {
                        self.state = AppState::ShowEndpoints;
                    }
                    KeyCode::Char('r') => {
                        if let Some(_endpoint) = &self.selected_endpoint {
                            self.current_page = 1;
                            self.state = AppState::Loading;
                        }
                    }
                    _ => {}
                }
            }
            AppState::ShowRequests => {
                match key.code {
                    KeyCode::Up => {
                        if self.selected_request_index > 0 {
                            self.selected_request_index -= 1;
                        }
                    }
                    KeyCode::Down => {
                        if self.selected_request_index < self.requests.len().saturating_sub(1) {
                            self.selected_request_index += 1;
                        }
                    }
                    KeyCode::Enter => {
                        if let Some(_request) = self.requests.get(self.selected_request_index) {
                            self.state = AppState::Loading;
                        }
                    }
                    KeyCode::Left => {
                        if self.current_page > 1 {
                            self.current_page -= 1;
                            if let Some(_endpoint) = &self.selected_endpoint {
                                self.state = AppState::Loading;
                            }
                        }
                    }
                    KeyCode::Right => {
                        if let Some(pagination) = &self.requests_pagination
                            && self.current_page < pagination.total_pages {
                                self.current_page += 1;
                                if let Some(_endpoint) = &self.selected_endpoint {
                                    self.state = AppState::Loading;
                                }
                            }
                    }
                    KeyCode::Char('q') => {
                        self.should_quit = true;
                    }
                    KeyCode::Char('b') | KeyCode::Esc => {
                        self.state = AppState::ShowEndpointDetail;
                    }
                    _ => {}
                }
            }
            AppState::ShowRequestDetail => {
                match key.code {
                    KeyCode::Char('q') => {
                        self.should_quit = true;
                    }
                    KeyCode::Char('b') | KeyCode::Esc => {
                        self.current_tab = 0;
                        self.headers_scroll_offset = 0;
                        self.body_scroll_offset = 0;
                        self.state = AppState::ShowRequests;
                    }
                    KeyCode::Char('f') => {
                        self.forward_url_input.clear();
                        self.state = AppState::InputForwardUrl;
                    }
                    KeyCode::Tab => {
                        self.current_tab = (self.current_tab + 1) % 3;
                    }
                    KeyCode::BackTab => {
                        self.current_tab = if self.current_tab == 0 { 2 } else { self.current_tab - 1 };
                    }
                    KeyCode::Left => {
                        self.current_tab = if self.current_tab == 0 { 2 } else { self.current_tab - 1 };
                    }
                    KeyCode::Right => {
                        self.current_tab = (self.current_tab + 1) % 3;
                    }
                    KeyCode::Up | KeyCode::Char('k') => {
                        match self.current_tab {
                            1 => { // Headers tab
                                if self.headers_scroll_offset > 0 {
                                    self.headers_scroll_offset -= 1;
                                }
                            }
                            2 => { // Body tab
                                if self.body_scroll_offset > 0 {
                                    self.body_scroll_offset -= 1;
                                }
                            }
                            _ => {} // Info tab - no scrolling
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        match self.current_tab {
                            1 => { // Headers tab
                                if let Some(request) = &self.selected_request {
                                    let max_scroll = request.headers.len().saturating_sub(1);
                                    if self.headers_scroll_offset < max_scroll {
                                        self.headers_scroll_offset += 1;
                                    }
                                }
                            }
                            2 => { // Body tab
                                if let Some(request) = &self.selected_request {
                                    let body_text = request.body.as_ref().or(request.body_preview.as_ref());
                                    if let Some(body) = body_text {
                                        let lines: Vec<&str> = body.lines().collect();
                                        // Conservative estimate for available lines
                                        // Terminal height is typically ~24-40 lines, minus tabs, headers, help text
                                        let viewport_lines = 20;
                                        if lines.len() > viewport_lines {
                                            let max_scroll = lines.len() - viewport_lines;
                                            if self.body_scroll_offset < max_scroll {
                                                self.body_scroll_offset += 1;
                                            }
                                        }
                                    }
                                }
                            }
                            _ => {} // Info tab - no scrolling
                        }
                    }
                    KeyCode::PageUp => {
                        match self.current_tab {
                            1 => { // Headers tab
                                self.headers_scroll_offset = self.headers_scroll_offset.saturating_sub(10);
                            }
                            2 => { // Body tab
                                self.body_scroll_offset = self.body_scroll_offset.saturating_sub(10);
                            }
                            _ => {}
                        }
                    }
                    KeyCode::PageDown => {
                        match self.current_tab {
                            1 => { // Headers tab
                                if let Some(request) = &self.selected_request {
                                    let max_scroll = request.headers.len().saturating_sub(1);
                                    self.headers_scroll_offset = (self.headers_scroll_offset + 10).min(max_scroll);
                                }
                            }
                            2 => { // Body tab
                                if let Some(request) = &self.selected_request {
                                    let body_text = request.body.as_ref().or(request.body_preview.as_ref());
                                    if let Some(body) = body_text {
                                        let lines: Vec<&str> = body.lines().collect();
                                        let viewport_lines = 20;
                                        if lines.len() > viewport_lines {
                                            let max_scroll = lines.len() - viewport_lines;
                                            self.body_scroll_offset = (self.body_scroll_offset + 10).min(max_scroll);
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    KeyCode::Home => {
                        match self.current_tab {
                            1 => self.headers_scroll_offset = 0,
                            2 => self.body_scroll_offset = 0,
                            _ => {}
                        }
                    }
                    KeyCode::End => {
                        match self.current_tab {
                            1 => {
                                if let Some(request) = &self.selected_request {
                                    self.headers_scroll_offset = request.headers.len().saturating_sub(1);
                                }
                            }
                            2 => {
                                if let Some(request) = &self.selected_request {
                                    let body_text = request.body.as_ref().or(request.body_preview.as_ref());
                                    if let Some(body) = body_text {
                                        let lines: Vec<&str> = body.lines().collect();
                                        let viewport_lines = 20;
                                        if lines.len() > viewport_lines {
                                            self.body_scroll_offset = lines.len() - viewport_lines;
                                        } else {
                                            self.body_scroll_offset = 0;
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    _ => {}
                }
            }
            AppState::InputForwardUrl => {
                match key.code {
                    KeyCode::Enter => {
                        if !self.forward_url_input.is_empty() && self.is_valid_url(&self.forward_url_input) {
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
                }
            }
            AppState::ForwardResult => {
                match key.code {
                    KeyCode::Char('q') => {
                        self.should_quit = true;
                    }
                    KeyCode::Char('b') | KeyCode::Esc => {
                        self.state = AppState::ShowRequestDetail;
                    }
                    _ => {}
                }
            }
            AppState::Error(_) => {
                match key.code {
                    KeyCode::Char('q') => {
                        self.should_quit = true;
                    }
                    KeyCode::Char('r') => {
                        self.state = AppState::Loading;
                    }
                    KeyCode::Char('c') => {
                        self.api_key_input.clear();
                        self.config.api_key = None;
                        self.config.save()?;
                        self.state = AppState::InputApiKey;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        
        Ok(())
    }

    pub fn get_selected_endpoint_id(&self) -> Option<String> {
        self.endpoints.get(self.selected_index).map(|e| e.id.clone())
    }

    pub fn is_valid_url(&self, url: &str) -> bool {
        url.starts_with("http://") || url.starts_with("https://")
    }

    pub async fn forward_request(&mut self) -> Result<()> {
        if let (Some(request), Some(api_key)) = (&self.selected_request, &self.config.api_key) {
            let client = ApiClient::new(api_key.clone());
            
            match client.forward_request(request, &self.forward_url_input).await {
                Ok(response) => {
                    self.forward_result = Some(response);
                    self.state = AppState::ForwardResult;
                }
                Err(e) => {
                    self.state = AppState::Error(format!("Failed to forward request: {}", e));
                }
            }
        }
        
        Ok(())
    }
    
    pub fn tick(&mut self) {
        // Update loading animation frame
        self.loading_frame = (self.loading_frame + 1) % 8;
    }
}