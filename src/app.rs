use crate::api::ApiClient;
use crate::auth::DeviceCodeFlow;
use crate::config::Config;
use crate::models::{
    DebugEndpoint, DebugEndpointDetail, ForwardResponse, Organization, WebhookRequest,
};
use anyhow::Result;
use chrono::{Duration, Utc};
use crossterm::event::{KeyCode, KeyEvent, KeyEventKind};

#[derive(Debug)]
pub enum AppState {
    InitiatingDeviceFlow,
    DisplayingDeviceCode,
    Loading,
    ShowOrganizations,
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
    pub device_flow: Option<DeviceCodeFlow>,
    pub auth_poll_counter: u64,
    pub organizations: Vec<Organization>,
    pub selected_organization_index: usize,
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
    pub loading_frame: usize,
    pub just_authenticated: bool,
}

impl App {
    pub fn new() -> Result<Self> {
        let config = Config::load()?;

        let state = if config.is_token_valid() {
            // Start by loading organizations
            AppState::Loading
        } else {
            AppState::InitiatingDeviceFlow
        };

        Ok(Self {
            state,
            config,
            device_flow: None,
            auth_poll_counter: 0,
            organizations: Vec::new(),
            selected_organization_index: 0,
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
            just_authenticated: false,
        })
    }

    pub async fn load_organizations(&mut self) -> Result<()> {
        log::info!("load_organizations: Starting");
        if let Some(access_token) = &self.config.access_token {
            if self.config.is_token_valid() {
                let client = ApiClient::new(access_token.clone());

                match client.fetch_organizations().await {
                    Ok(organizations) => {
                        log::info!(
                            "load_organizations: Loaded {} organizations",
                            organizations.len()
                        );
                        self.organizations = organizations;
                        if self.config.selected_organization_id.is_some() {
                            log::info!(
                                "load_organizations: User has selected org, loading endpoints directly"
                            );
                            // User has a selected organization, load endpoints directly
                            self.load_endpoints().await?;
                        } else {
                            log::info!(
                                "load_organizations: No selected org, showing organization selection"
                            );
                            // User needs to select an organization
                            self.state = AppState::ShowOrganizations;
                        }
                    }
                    Err(_e) => {
                        // Token might be invalid, trigger re-authentication
                        self.config.clear_token();
                        self.config.save()?;
                        self.state = AppState::InitiatingDeviceFlow;
                    }
                }
            } else {
                // Token expired, clear and re-authenticate
                self.config.clear_token();
                self.config.save()?;
                self.state = AppState::InitiatingDeviceFlow;
            }
        } else {
            self.state = AppState::InitiatingDeviceFlow;
        }

        Ok(())
    }

    pub async fn load_endpoints(&mut self) -> Result<()> {
        log::info!("load_endpoints: Starting");
        if let Some(access_token) = &self.config.access_token {
            if self.config.is_token_valid() {
                let client = ApiClient::with_organization(
                    access_token.clone(),
                    self.config.selected_organization_id.clone(),
                );

                match client.fetch_debug_endpoints().await {
                    Ok(endpoints) => {
                        log::info!("load_endpoints: Loaded {} endpoints", endpoints.len());
                        self.endpoints = endpoints;
                        self.state = AppState::ShowEndpoints;
                        log::info!("load_endpoints: State set to ShowEndpoints");
                    }
                    Err(_e) => {
                        // Token might be invalid, trigger re-authentication
                        self.config.clear_token();
                        self.config.save()?;
                        self.state = AppState::InitiatingDeviceFlow;
                    }
                }
            } else {
                // Token expired, clear and re-authenticate
                self.config.clear_token();
                self.config.save()?;
                self.state = AppState::InitiatingDeviceFlow;
            }
        } else {
            self.state = AppState::InitiatingDeviceFlow;
        }

        Ok(())
    }

    pub async fn load_endpoint_detail(&mut self, endpoint_id: &str) -> Result<()> {
        if let Some(access_token) = &self.config.access_token {
            let client = ApiClient::with_organization(
                access_token.clone(),
                self.config.selected_organization_id.clone(),
            );

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
        if let Some(access_token) = &self.config.access_token {
            let client = ApiClient::with_organization(
                access_token.clone(),
                self.config.selected_organization_id.clone(),
            );

            match client
                .fetch_endpoint_requests(endpoint_id, self.current_page, 50)
                .await
            {
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

    pub async fn load_request_details(
        &mut self,
        endpoint_id: &str,
        request_id: &str,
    ) -> Result<()> {
        if let Some(access_token) = &self.config.access_token {
            let client = ApiClient::with_organization(
                access_token.clone(),
                self.config.selected_organization_id.clone(),
            );

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
                        self.state =
                            AppState::Error("Request not found in the current list".to_string());
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
            AppState::ShowOrganizations => match key.code {
                KeyCode::Up => {
                    if self.selected_organization_index > 0 {
                        self.selected_organization_index -= 1;
                    }
                }
                KeyCode::Down => {
                    if self.selected_organization_index < self.organizations.len().saturating_sub(1)
                    {
                        self.selected_organization_index += 1;
                    }
                }
                KeyCode::Enter => {
                    self.state = AppState::Loading;
                }
                KeyCode::Char('q') => {
                    self.should_quit = true;
                }
                KeyCode::Char('r') => {
                    self.state = AppState::Loading;
                }
                _ => {}
            },
            AppState::DisplayingDeviceCode => match key.code {
                KeyCode::Char('q') | KeyCode::Esc => {
                    self.should_quit = true;
                }
                KeyCode::Char('r') => {
                    self.state = AppState::InitiatingDeviceFlow;
                }
                _ => {}
            },
            AppState::ShowEndpoints => match key.code {
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
                KeyCode::Char('o') => {
                    self.state = AppState::ShowOrganizations;
                }
                KeyCode::Char('l') => {
                    // Logout and redirect to authentication
                    let _ = self.logout();
                }
                _ => {}
            },
            AppState::ShowEndpointDetail => match key.code {
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
            },
            AppState::ShowRequests => match key.code {
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
                        && self.current_page < pagination.total_pages
                    {
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
            },
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
                        self.current_tab = if self.current_tab == 0 {
                            2
                        } else {
                            self.current_tab - 1
                        };
                    }
                    KeyCode::Left => {
                        self.current_tab = if self.current_tab == 0 {
                            2
                        } else {
                            self.current_tab - 1
                        };
                    }
                    KeyCode::Right => {
                        self.current_tab = (self.current_tab + 1) % 3;
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
                            _ => {} // Info tab - no scrolling
                        }
                    }
                    KeyCode::Down | KeyCode::Char('j') => {
                        match self.current_tab {
                            1 => {
                                // Headers tab
                                if let Some(request) = &self.selected_request {
                                    let max_scroll = request.headers.len().saturating_sub(1);
                                    if self.headers_scroll_offset < max_scroll {
                                        self.headers_scroll_offset += 1;
                                    }
                                }
                            }
                            2 => {
                                // Body tab
                                if let Some(request) = &self.selected_request {
                                    let body_text =
                                        request.body.as_ref().or(request.body_preview.as_ref());
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
                            1 => {
                                // Headers tab
                                self.headers_scroll_offset =
                                    self.headers_scroll_offset.saturating_sub(10);
                            }
                            2 => {
                                // Body tab
                                self.body_scroll_offset =
                                    self.body_scroll_offset.saturating_sub(10);
                            }
                            _ => {}
                        }
                    }
                    KeyCode::PageDown => {
                        match self.current_tab {
                            1 => {
                                // Headers tab
                                if let Some(request) = &self.selected_request {
                                    let max_scroll = request.headers.len().saturating_sub(1);
                                    self.headers_scroll_offset =
                                        (self.headers_scroll_offset + 10).min(max_scroll);
                                }
                            }
                            2 => {
                                // Body tab
                                if let Some(request) = &self.selected_request {
                                    let body_text =
                                        request.body.as_ref().or(request.body_preview.as_ref());
                                    if let Some(body) = body_text {
                                        let lines: Vec<&str> = body.lines().collect();
                                        let viewport_lines = 20;
                                        if lines.len() > viewport_lines {
                                            let max_scroll = lines.len() - viewport_lines;
                                            self.body_scroll_offset =
                                                (self.body_scroll_offset + 10).min(max_scroll);
                                        }
                                    }
                                }
                            }
                            _ => {}
                        }
                    }
                    KeyCode::Home => match self.current_tab {
                        1 => self.headers_scroll_offset = 0,
                        2 => self.body_scroll_offset = 0,
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
                            if let Some(request) = &self.selected_request {
                                let body_text =
                                    request.body.as_ref().or(request.body_preview.as_ref());
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
                    },
                    _ => {}
                }
            }
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
            AppState::Error(_) => match key.code {
                KeyCode::Char('q') => {
                    self.should_quit = true;
                }
                KeyCode::Char('r') => {
                    self.state = AppState::Loading;
                }
                KeyCode::Char('c') => {
                    self.config.clear_token();
                    self.config.save()?;
                    self.state = AppState::InitiatingDeviceFlow;
                }
                _ => {}
            },
            _ => {}
        }

        Ok(())
    }

    pub fn get_selected_endpoint_id(&self) -> Option<String> {
        self.endpoints
            .get(self.selected_index)
            .map(|e| e.id.clone())
    }

    pub fn get_selected_organization_id(&self) -> Option<String> {
        self.organizations
            .get(self.selected_organization_index)
            .map(|org| org.id.clone())
    }

    pub async fn select_organization(&mut self) -> Result<()> {
        log::info!("select_organization: Starting");
        if let Some(org_id) = self.get_selected_organization_id() {
            log::info!("select_organization: Selected org ID: {}", org_id);
            self.config.set_selected_organization(org_id);
            self.config.save()?;
            log::info!("select_organization: Config saved, loading endpoints");
            // Now load endpoints with the selected organization
            self.load_endpoints().await?;
            log::info!("select_organization: Endpoints loaded successfully");
        } else {
            log::warn!("select_organization: No organization selected");
        }
        Ok(())
    }

    pub fn is_valid_url(&self, url: &str) -> bool {
        url.starts_with("http://") || url.starts_with("https://")
    }

    pub async fn forward_request(&mut self) -> Result<()> {
        if let (Some(request), Some(access_token)) =
            (&self.selected_request, &self.config.access_token)
        {
            let client = ApiClient::with_organization(
                access_token.clone(),
                self.config.selected_organization_id.clone(),
            );

            match client
                .forward_request(request, &self.forward_url_input)
                .await
            {
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

    pub async fn initiate_device_flow(&mut self) -> Result<()> {
        let mut device_flow = DeviceCodeFlow::new("https://api.hooklistener.com".to_string());

        match device_flow.initiate_device_flow().await {
            Ok(_user_code) => {
                self.device_flow = Some(device_flow);
                self.state = AppState::DisplayingDeviceCode;
                // Start polling immediately
                self.auth_poll_counter = 0;
            }
            Err(e) => {
                self.state = AppState::Error(format!("Failed to initiate device flow: {}", e));
            }
        }

        Ok(())
    }

    pub async fn poll_device_authentication(&mut self) -> Result<()> {
        if let Some(device_flow) = &self.device_flow {
            // Only poll every 50 ticks (roughly every 5 seconds at 100ms tick rate)
            self.auth_poll_counter += 1;
            if self.auth_poll_counter % 50 == 0 {
                match device_flow.poll_for_authorization().await {
                    Ok(Some(access_token)) => {
                        // Authentication successful!
                        let expires_at = Utc::now() + Duration::hours(24);
                        self.config.set_access_token(access_token, expires_at);
                        self.config.save()?;
                        self.device_flow = None;
                        self.state = AppState::Loading;
                        self.just_authenticated = true;
                    }
                    Ok(None) => {
                        // Still pending, keep waiting
                    }
                    Err(e) => {
                        self.state = AppState::Error(format!("Authentication failed: {}", e));
                    }
                }
            }
        }
        Ok(())
    }

    pub fn get_device_code_info(&self) -> Option<(String, Option<Duration>)> {
        self.device_flow.as_ref().and_then(|flow| {
            flow.format_user_code()
                .map(|code| (code, flow.time_remaining()))
        })
    }

    pub fn tick(&mut self) {
        // Update loading animation frame
        self.loading_frame = (self.loading_frame + 1) % 8;
    }

    pub fn logout(&mut self) -> Result<()> {
        // Clear all authentication data
        self.config.clear_all();
        self.config.save()?;

        // Reset app state to authentication flow
        self.state = AppState::InitiatingDeviceFlow;
        self.organizations.clear();
        self.endpoints.clear();
        self.selected_organization_index = 0;
        self.selected_index = 0;

        Ok(())
    }
}
