use crate::{
    errors::TunnelLifecycleError,
    models::{ForwardResponse, WebhookRequest},
};
use anyhow::{Context, Result, anyhow};
use reqwest::{
    Client, Response, Url,
    header::{AUTHORIZATION, HeaderMap, HeaderValue},
};
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::HashMap;
use std::time::{Duration, Instant};

const RELAY_TICKET_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const TOKEN_REFRESH_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
const API_CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const API_REQUEST_TIMEOUT: Duration = Duration::from_secs(120);

fn deserialize_map_or_default<'de, D>(
    deserializer: D,
) -> std::result::Result<HashMap<String, Value>, D::Error>
where
    D: Deserializer<'de>,
{
    Option::<HashMap<String, Value>>::deserialize(deserializer)
        .map(|maybe_map| maybe_map.unwrap_or_default())
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Organization {
    pub id: String,
    pub name: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct RelayTicket {
    pub ticket: String,
    pub scope: String,
    pub plan_fingerprint: String,
    pub expires_at: String,
}

#[derive(Clone, Serialize, Deserialize)]
pub struct AnonymousTunnelRouteCreated {
    pub id: String,
    pub slug: String,
    pub url: String,
    pub stable_name: bool,
    pub expires_at: String,
    pub route_token: String,
    pub claim_token: String,
    pub relay_ticket: RelayTicket,
    pub limits: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ClaimedAnonymousTunnelRoute {
    pub id: String,
    pub slug: String,
    pub kind: String,
    pub status: String,
    pub claimed_at: String,
    pub privacy: Value,
}

#[derive(Deserialize)]
struct RelayTicketEnvelope {
    data: RelayTicket,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugEndpointSummary {
    pub id: String,
    pub name: String,
    pub slug: String,
    pub status: String,
    pub webhook_url: String,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugRequestSummary {
    pub id: String,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub remote_addr: String,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub timestamp: Option<i64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pagination {
    pub page: u64,
    pub page_size: u64,
    pub total_count: u64,
    pub total_pages: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointRequestsResponse {
    pub data: Vec<DebugRequestSummary>,
    pub pagination: Pagination,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugRequestDetail {
    pub id: String,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub scheme: Option<String>,
    #[serde(default)]
    pub host: Option<String>,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub path: Option<String>,
    #[serde(default)]
    pub url: String,
    #[serde(default)]
    pub protocol_version: Option<String>,
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_map_or_default")]
    pub headers: HashMap<String, Value>,
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_map_or_default")]
    pub cookies: HashMap<String, Value>,
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_map_or_default")]
    pub query_params: HashMap<String, Value>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub body_preview: Option<String>,
    #[serde(default)]
    pub content_length: Option<i64>,
    #[serde(default)]
    pub remote_addr: Option<String>,
    #[serde(default)]
    pub timestamp: Option<i64>,
    #[serde(default)]
    pub tls_version: Option<String>,
    #[serde(default)]
    pub tls_cipher: Option<String>,
    #[serde(default)]
    pub debug_endpoint_id: Option<String>,
    #[serde(default)]
    pub organization_id: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointRequestForwardResponse {
    pub forward_id: String,
    pub debug_request_id: String,
    pub target_url: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseRunParams {
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_url: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub target_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wait: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub timeout_ms: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub interval_ms: Option<u64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseRunFailure {
    pub case_id: String,
    pub reason: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseRunTarget {
    #[serde(default)]
    pub r#type: Option<String>,
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseRunForward {
    pub id: String,
    pub debug_request_id: String,
    #[serde(default)]
    pub debug_request_case_id: Option<String>,
    #[serde(default)]
    pub case_suite_run_id: Option<String>,
    #[serde(default)]
    pub target_url: Option<String>,
    pub status: String,
    #[serde(default)]
    pub status_code: Option<u16>,
    #[serde(default)]
    pub error_message: Option<Value>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub attempted_at: Option<String>,
    #[serde(default)]
    pub assertion_status: Option<String>,
    #[serde(default)]
    pub assertion_details: Option<Value>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub poll_url: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CaseRunResult {
    #[serde(default)]
    pub id: Option<String>,
    #[serde(default)]
    pub case_suite_run_id: Option<String>,
    #[serde(default)]
    pub case_suite_run_url: Option<String>,
    #[serde(default)]
    pub source: Option<String>,
    pub status: String,
    pub result_status: String,
    #[serde(rename = "async")]
    pub async_run: bool,
    #[serde(default)]
    pub waited: Option<bool>,
    #[serde(default)]
    pub timed_out: Option<bool>,
    pub endpoint_id: String,
    pub target: CaseRunTarget,
    #[serde(default)]
    pub total_count: u64,
    #[serde(default)]
    pub queued_count: u64,
    #[serde(default)]
    pub failed_count: u64,
    #[serde(default)]
    pub completed_count: u64,
    #[serde(default)]
    pub waiting_count: u64,
    #[serde(default)]
    pub queue_failed_count: u64,
    #[serde(default)]
    pub delivery_failed_count: u64,
    #[serde(default)]
    pub passed_count: u64,
    #[serde(default)]
    pub assertion_failed_count: u64,
    #[serde(default)]
    pub assertion_error_count: u64,
    #[serde(default)]
    pub not_configured_count: u64,
    #[serde(default)]
    pub forwards: Vec<CaseRunForward>,
    #[serde(default)]
    pub failures: Vec<CaseRunFailure>,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub started_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugRequestForwardSummary {
    pub id: String,
    pub debug_request_id: String,
    pub target_url: String,
    pub method: String,
    #[serde(default)]
    pub status_code: Option<u16>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub error_message: Option<String>,
    #[serde(default)]
    pub attempted_at: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EndpointRequestForwardsResponse {
    pub data: Vec<DebugRequestForwardSummary>,
    pub pagination: Pagination,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugRequestForwardDetail {
    pub id: String,
    pub debug_request_id: String,
    #[serde(default)]
    pub organization_id: Option<String>,
    pub target_url: String,
    pub method: String,
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_map_or_default")]
    pub request_headers: HashMap<String, Value>,
    #[serde(default)]
    pub request_body: Option<String>,
    #[serde(default)]
    pub request_body_object_key: Option<String>,
    #[serde(default)]
    pub status_code: Option<u16>,
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_map_or_default")]
    pub response_headers: HashMap<String, Value>,
    #[serde(default)]
    pub response_body: Option<String>,
    #[serde(default)]
    pub response_body_object_key: Option<String>,
    #[serde(default)]
    pub error_message: Option<String>,
    #[serde(default)]
    pub duration_ms: Option<u64>,
    #[serde(default)]
    pub attempted_at: Option<String>,
    #[serde(default)]
    pub created_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticTunnelSummary {
    pub id: String,
    pub slug: String,
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticTunnelsResponse {
    pub static_tunnels: Vec<StaticTunnelSummary>,
    pub limit: u64,
    pub used: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct StaticTunnelCreateResponse {
    pub static_tunnel: StaticTunnelSummary,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelLifecycleContract {
    pub id: String,
    pub version: String,
    pub schema: TunnelSchemaVersion,
    pub receipts: Value,
    pub events: Value,
    pub resources: Value,
    pub lifecycle: Value,
    pub exit_codes: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelSchemaVersion {
    pub major: u64,
    pub minor: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelRouteResource {
    pub id: String,
    pub slug: String,
    pub kind: String,
    pub mode: String,
    pub status: String,
    #[serde(default)]
    pub expires_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelSessionResource {
    pub id: String,
    pub organization_id: String,
    pub status: String,
    pub fence: u64,
    #[serde(default)]
    pub lease_expires_at: Option<String>,
    #[serde(default)]
    pub opened_at: Option<String>,
    #[serde(default)]
    pub closed_at: Option<String>,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub route: Option<TunnelRouteResource>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelCaptureResource {
    pub id: String,
    pub organization_id: String,
    pub route_id: String,
    #[serde(default)]
    pub session_id: Option<String>,
    #[serde(default)]
    pub endpoint_id: Option<String>,
    pub origin: String,
    pub method: String,
    pub path: String,
    #[serde(default)]
    pub request_content_length: Option<u64>,
    pub request_body_captured: bool,
    #[serde(default)]
    pub provider_response_status: Option<u16>,
    pub captured_at: String,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelAttemptResource {
    pub id: String,
    pub organization_id: String,
    pub capture_id: String,
    pub session_id: String,
    pub attempt_number: u64,
    pub fence: u64,
    pub status: String,
    pub deadline_at: String,
    #[serde(default)]
    pub forward_started_at: Option<String>,
    #[serde(default)]
    pub response_observed_at: Option<String>,
    #[serde(default)]
    pub completed_at: Option<String>,
    #[serde(default)]
    pub error_code: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelLifecycleEvent {
    pub id: String,
    pub position: u64,
    pub cursor: String,
    pub organization_id: String,
    pub capture_id: String,
    #[serde(default)]
    pub delivery_id: Option<String>,
    pub sequence: u64,
    #[serde(default)]
    pub fence: Option<u64>,
    #[serde(rename = "type")]
    pub event_type: String,
    #[serde(default)]
    pub metadata: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelCollection<T> {
    pub data: Vec<T>,
    pub meta: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelEventPage {
    pub data: Vec<TunnelLifecycleEvent>,
    pub meta: TunnelEventPageMeta,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelEventPageMeta {
    pub cursor: String,
    pub has_more: bool,
    pub retention_days: u64,
    pub resync: Value,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TunnelReconnectDescriptor {
    pub session: TunnelSessionResource,
    pub topic: String,
    pub resume_token: String,
    pub resume_token_expires_in: u64,
    pub cursor: String,
    pub ownership: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageResponse {
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct DataResponse<T> {
    data: T,
}

// ── Anonymous Endpoint models ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnonEndpointCreated {
    pub id: String,
    pub viewer_token: String,
    pub expires_at: String,
    pub webhook_url: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnonEndpointStatus {
    pub id: String,
    pub active: bool,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub webhook_url: Option<String>,
    #[serde(default)]
    pub error: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnonEvent {
    pub id: String,
    pub endpoint_id: String,
    #[serde(default)]
    pub method: String,
    #[serde(default)]
    pub status: Option<String>,
    #[serde(default)]
    #[serde(deserialize_with = "deserialize_map_or_default")]
    pub headers: HashMap<String, Value>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub inserted_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AnonEventsResponse {
    pub data: Vec<AnonEvent>,
    pub pagination: Pagination,
}

// ── Shared Request models ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SharedRequestSummary {
    pub id: String,
    pub share_token: String,
    #[serde(default)]
    pub share_url: Option<String>,
    pub debug_request_id: String,
    #[serde(default)]
    pub include_forwards: bool,
    #[serde(default)]
    pub password_protected: bool,
    #[serde(default)]
    pub expires_at: Option<String>,
    #[serde(default)]
    pub view_count: u64,
    #[serde(default)]
    pub last_viewed_at: Option<String>,
    #[serde(default)]
    pub created_by_user_id: Option<String>,
    #[serde(default)]
    pub inserted_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

// ── Uptime Monitor models ───────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UptimeMonitor {
    pub id: String,
    pub name: String,
    pub url: String,
    #[serde(default = "default_method")]
    pub method: String,
    #[serde(default)]
    pub headers: Option<HashMap<String, Value>>,
    #[serde(default)]
    pub body: Option<String>,
    #[serde(default)]
    pub expected_status_code: Option<u16>,
    #[serde(default)]
    pub body_contains: Option<String>,
    #[serde(default)]
    pub check_interval: Option<u32>,
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub current_status: Option<String>,
    #[serde(default)]
    pub last_checked_at: Option<String>,
    #[serde(default)]
    pub last_status_change_at: Option<String>,
    #[serde(default)]
    pub consecutive_failures: Option<u32>,
    #[serde(default)]
    pub failure_threshold: Option<u32>,
    #[serde(default)]
    pub email_enabled: bool,
    #[serde(default)]
    pub slack_enabled: bool,
    #[serde(default)]
    pub created_at: Option<String>,
    #[serde(default)]
    pub updated_at: Option<String>,
}

fn default_method() -> String {
    "get".to_string()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UptimeCheck {
    pub id: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub response_time_ms: Option<u64>,
    #[serde(default)]
    pub status_code: Option<u16>,
    #[serde(default)]
    pub error_message: Option<String>,
    #[serde(default)]
    pub checked_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UptimeChecksStats {
    #[serde(default)]
    pub uptime_percentage: Option<f64>,
    #[serde(default)]
    pub avg_response_time_ms: Option<f64>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UptimeChecksResponse {
    pub data: Vec<UptimeCheck>,
    pub pagination: Pagination,
    #[serde(default)]
    pub stats: Option<UptimeChecksStats>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct TokenRefreshResponse {
    pub access_token: String,
    pub expires_in: u64,
}

pub struct ApiClient {
    client: Client,
    base_url: Option<String>,
}

pub fn default_base_url() -> String {
    std::env::var("HOOKLISTENER_API_URL")
        .unwrap_or_else(|_| "https://app.hooklistener.com".to_string())
}

/// Refresh an expired CLI access token using a refresh token (no auth needed).
pub async fn refresh_access_token(
    refresh_token: &str,
    base_url: &str,
) -> Result<TokenRefreshResponse> {
    let url = format!("{}/api/v1/auth/refresh", base_url.trim_end_matches('/'));
    let body = serde_json::json!({ "refresh_token": refresh_token });

    let client = Client::new();
    let response = client
        .post(&url)
        .timeout(TOKEN_REFRESH_REQUEST_TIMEOUT)
        .json(&body)
        .send()
        .await
        .context("Failed to refresh access token")?;

    let status = response.status();
    let text = response.text().await.unwrap_or_default();
    if !status.is_success() {
        return Err(anyhow!("Token refresh failed (HTTP {}): {}", status, text));
    }

    serde_json::from_str(&text).context("Failed to parse refresh response")
}

/// Revoke a CLI refresh token server-side (best-effort, no auth needed).
pub async fn revoke_refresh_token(refresh_token: &str) -> Result<()> {
    let base_url = default_base_url();
    let url = format!("{}/api/v1/auth/revoke", base_url.trim_end_matches('/'));
    let body = serde_json::json!({ "refresh_token": refresh_token });

    let client = Client::new();
    let _ = client.post(&url).json(&body).send().await;
    Ok(())
}

/// Exchange a long-lived HTTP credential for a short-lived relay handshake ticket.
pub async fn issue_relay_ticket(
    access_token: &str,
    base_url: &str,
    plan: &Value,
) -> Result<RelayTicket> {
    let http_base = base_url
        .replacen("wss://", "https://", 1)
        .replacen("ws://", "http://", 1);
    let url = format!(
        "{}/api/v1/tunnel/relay-tickets",
        http_base.trim_end_matches('/')
    );

    let response = Client::new()
        .post(url)
        .bearer_auth(access_token)
        .json(&serde_json::json!({"plan": plan}))
        .timeout(RELAY_TICKET_REQUEST_TIMEOUT)
        .send()
        .await
        .context("Failed to request relay handshake ticket")?;
    let status = response.status();
    let text = response.text().await.unwrap_or_default();

    if !status.is_success() {
        let permanent_client_error =
            status.is_client_error() && !matches!(status.as_u16(), 408 | 429);
        let failure = if permanent_client_error {
            "Relay handshake rejected"
        } else {
            "Relay handshake ticket request failed"
        };
        return Err(anyhow!("{} (HTTP {}): {}", failure, status, text));
    }

    serde_json::from_str::<RelayTicketEnvelope>(&text)
        .map(|envelope| envelope.data)
        .context("Failed to parse relay handshake ticket")
}

impl ApiClient {
    pub fn for_forwarding() -> Self {
        Self {
            client: Client::new(),
            base_url: None,
        }
    }

    /// Create a client with no authentication (for anonymous endpoints).
    pub fn unauthenticated() -> Result<Self> {
        Self::unauthenticated_at(default_base_url())
    }

    pub fn unauthenticated_at(base_url: String) -> Result<Self> {
        Ok(Self {
            client: Client::new(),
            base_url: Some(
                base_url
                    .replacen("wss://", "https://", 1)
                    .replacen("ws://", "http://", 1),
            ),
        })
    }

    pub fn with_organization(
        access_token: String,
        organization_id: Option<String>,
    ) -> Result<Self> {
        Self::with_base_url(access_token, default_base_url(), organization_id)
    }

    pub fn with_base_url(
        access_token: String,
        base_url: String,
        organization_id: Option<String>,
    ) -> Result<Self> {
        let mut headers = HeaderMap::new();
        let auth = format!("Bearer {}", access_token);
        headers.insert(
            AUTHORIZATION,
            HeaderValue::from_str(&auth).context("Invalid authorization header value")?,
        );

        if let Some(org_id) = organization_id {
            headers.insert(
                "x-organization-id",
                HeaderValue::from_str(&org_id).context("Invalid x-organization-id header value")?,
            );
        }

        let client = Client::builder()
            .default_headers(headers)
            .connect_timeout(API_CONNECT_TIMEOUT)
            .timeout(API_REQUEST_TIMEOUT)
            .build()
            .context("Failed to build API client")?;

        Ok(Self {
            client,
            base_url: Some(base_url),
        })
    }

    #[cfg(test)]
    pub fn with_unauthenticated_base_url(base_url: String) -> Self {
        Self {
            client: Client::new(),
            base_url: Some(base_url),
        }
    }

    fn api_url(&self, path: &str) -> Result<String> {
        let base = self
            .base_url
            .as_ref()
            .ok_or_else(|| anyhow!("API base URL is not configured for this client"))?;
        Ok(format!(
            "{}/{}",
            base.trim_end_matches('/'),
            path.trim_start_matches('/')
        ))
    }

    async fn parse_json_response<T: DeserializeOwned>(
        &self,
        response: Response,
        context: &str,
    ) -> Result<T> {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if !status.is_success() {
            return Err(anyhow!("{} failed (HTTP {}): {}", context, status, text));
        }

        serde_json::from_str(&text).with_context(|| format!("Failed to parse {} response", context))
    }

    async fn parse_tunnel_response<T: DeserializeOwned>(
        &self,
        response: Response,
        context: &str,
    ) -> Result<T> {
        let status = response.status();
        let text = response.text().await.unwrap_or_default();
        if status.is_success() {
            return serde_json::from_str(&text)
                .with_context(|| format!("Failed to parse {context} response"));
        }

        let body: Value = serde_json::from_str(&text).unwrap_or(Value::Null);
        let error = body.get("error").unwrap_or(&Value::Null);
        let code = error
            .get("code")
            .and_then(Value::as_str)
            .unwrap_or("tunnel_api_error");
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or(context);

        if code == "cursor_expired" {
            return Err(TunnelLifecycleError::CursorExpired {
                earliest_cursor: error
                    .get("earliest_cursor")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                resync: error.get("resync").cloned().unwrap_or(Value::Null),
            }
            .into());
        }

        Err(TunnelLifecycleError::Api {
            status: status.as_u16(),
            code: code.to_string(),
            message: message.to_string(),
        }
        .into())
    }

    async fn get_tunnel_json<T: DeserializeOwned>(
        &self,
        path: &str,
        query: &[(&str, Option<String>)],
        context: &str,
    ) -> Result<T> {
        let mut url = Url::parse(&self.api_url(path)?)?;
        if query.iter().any(|(_, value)| value.is_some()) {
            let mut pairs = url.query_pairs_mut();
            for (key, value) in query {
                if let Some(value) = value {
                    pairs.append_pair(key, value);
                }
            }
        }
        let response = self
            .client
            .get(url)
            .send()
            .await
            .with_context(|| format!("Failed to {context}"))?;
        self.parse_tunnel_response(response, context).await
    }

    async fn post_tunnel_json<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
        context: &str,
    ) -> Result<T> {
        let response = self
            .client
            .post(self.api_url(path)?)
            .json(body)
            .send()
            .await
            .with_context(|| format!("Failed to {context}"))?;
        self.parse_tunnel_response(response, context).await
    }

    async fn get_json<T: DeserializeOwned>(&self, path: &str, context: &str) -> Result<T> {
        let url = self.api_url(path)?;
        let response = self
            .client
            .get(url)
            .send()
            .await
            .with_context(|| format!("Failed to {}", context))?;
        self.parse_json_response(response, context).await
    }

    async fn post_json<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
        context: &str,
    ) -> Result<T> {
        let url = self.api_url(path)?;
        let response = self
            .client
            .post(url)
            .json(body)
            .send()
            .await
            .with_context(|| format!("Failed to {}", context))?;
        self.parse_json_response(response, context).await
    }

    async fn patch_json<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
        context: &str,
    ) -> Result<T> {
        let url = self.api_url(path)?;
        let response = self
            .client
            .patch(url)
            .json(body)
            .send()
            .await
            .with_context(|| format!("Failed to {}", context))?;
        self.parse_json_response(response, context).await
    }

    async fn delete_json<T: DeserializeOwned>(&self, path: &str, context: &str) -> Result<T> {
        let url = self.api_url(path)?;
        let response = self
            .client
            .delete(url)
            .send()
            .await
            .with_context(|| format!("Failed to {}", context))?;
        self.parse_json_response(response, context).await
    }

    async fn delete_empty(&self, path: &str, context: &str) -> Result<()> {
        let url = self.api_url(path)?;
        let response = self
            .client
            .delete(url)
            .send()
            .await
            .with_context(|| format!("Failed to {}", context))?;

        let status = response.status();
        if status.is_success() {
            return Ok(());
        }

        let text = response.text().await.unwrap_or_default();
        Err(anyhow!("{} failed (HTTP {}): {}", context, status, text))
    }

    pub async fn list_organizations(&self) -> Result<Vec<Organization>> {
        self.get_json("/api/v1/organizations", "list organizations")
            .await
    }

    pub async fn list_endpoints(&self) -> Result<Vec<DebugEndpointSummary>> {
        let response: DataResponse<Vec<DebugEndpointSummary>> =
            self.get_json("/api/v1/endpoints", "list endpoints").await?;
        Ok(response.data)
    }

    pub async fn list_endpoint_requests(
        &self,
        endpoint_id: &str,
        page: u32,
        page_size: u32,
    ) -> Result<EndpointRequestsResponse> {
        let path = format!(
            "/api/v1/endpoints/{}/requests?page={}&page_size={}",
            endpoint_id, page, page_size
        );
        self.get_json(&path, "list endpoint requests").await
    }

    pub async fn create_endpoint(
        &self,
        name: &str,
        slug: Option<&str>,
    ) -> Result<DebugEndpointSummary> {
        let mut endpoint_body = serde_json::Map::new();
        endpoint_body.insert("name".to_string(), Value::String(name.to_string()));
        if let Some(slug_value) = slug {
            endpoint_body.insert("slug".to_string(), Value::String(slug_value.to_string()));
        }

        let body = Value::Object(
            [("debug_endpoint".to_string(), Value::Object(endpoint_body))]
                .into_iter()
                .collect(),
        );

        let response: DataResponse<DebugEndpointSummary> = self
            .post_json("/api/v1/endpoints", &body, "create endpoint")
            .await?;
        Ok(response.data)
    }

    pub async fn get_endpoint(&self, endpoint_id: &str) -> Result<DebugEndpointSummary> {
        let path = format!("/api/v1/endpoints/{}", endpoint_id);
        let response: DataResponse<DebugEndpointSummary> =
            self.get_json(&path, "get endpoint").await?;
        Ok(response.data)
    }

    pub async fn delete_endpoint(&self, endpoint_id: &str) -> Result<()> {
        let path = format!("/api/v1/endpoints/{}", endpoint_id);
        self.delete_empty(&path, "delete endpoint").await
    }

    pub async fn get_endpoint_request(
        &self,
        endpoint_id: &str,
        request_id: &str,
    ) -> Result<DebugRequestDetail> {
        let path = format!("/api/v1/endpoints/{}/requests/{}", endpoint_id, request_id);
        let response: DataResponse<DebugRequestDetail> =
            self.get_json(&path, "get endpoint request").await?;
        Ok(response.data)
    }

    pub async fn delete_endpoint_request(&self, endpoint_id: &str, request_id: &str) -> Result<()> {
        let path = format!("/api/v1/endpoints/{}/requests/{}", endpoint_id, request_id);
        self.delete_empty(&path, "delete endpoint request").await
    }

    pub async fn forward_endpoint_request(
        &self,
        endpoint_id: &str,
        request_id: &str,
        target_url: &str,
        method: Option<&str>,
    ) -> Result<EndpointRequestForwardResponse> {
        let path = format!(
            "/api/v1/endpoints/{}/requests/{}/forward",
            endpoint_id, request_id
        );
        let mut body = serde_json::Map::new();
        body.insert(
            "target_url".to_string(),
            Value::String(target_url.to_string()),
        );
        if let Some(method_value) = method {
            body.insert(
                "method".to_string(),
                Value::String(method_value.to_string()),
            );
        }
        self.post_json(&path, &Value::Object(body), "forward endpoint request")
            .await
    }

    pub async fn run_endpoint_cases(
        &self,
        endpoint_id: &str,
        params: &CaseRunParams,
    ) -> Result<CaseRunResult> {
        let path = format!("/api/v1/endpoints/{}/cases/run", endpoint_id);
        let body = serde_json::to_value(params)?;
        let response: DataResponse<CaseRunResult> =
            self.post_json(&path, &body, "run endpoint cases").await?;
        Ok(response.data)
    }

    pub async fn list_endpoint_request_forwards(
        &self,
        endpoint_id: &str,
        request_id: &str,
        page: u32,
        page_size: u32,
    ) -> Result<EndpointRequestForwardsResponse> {
        let path = format!(
            "/api/v1/endpoints/{}/requests/{}/forwards?page={}&page_size={}",
            endpoint_id, request_id, page, page_size
        );
        self.get_json(&path, "list endpoint request forwards").await
    }

    pub async fn get_forward(&self, forward_id: &str) -> Result<DebugRequestForwardDetail> {
        let path = format!("/api/v1/forwards/{}", forward_id);
        let response: DataResponse<DebugRequestForwardDetail> =
            self.get_json(&path, "get forward").await?;
        Ok(response.data)
    }

    pub async fn list_static_tunnels(
        &self,
        organization_id: &str,
    ) -> Result<StaticTunnelsResponse> {
        let path = format!("/api/v1/organizations/{}/static-tunnels", organization_id);
        self.get_json(&path, "list static tunnels").await
    }

    pub async fn create_static_tunnel(
        &self,
        organization_id: &str,
        slug: &str,
        name: Option<&str>,
    ) -> Result<StaticTunnelCreateResponse> {
        let path = format!("/api/v1/organizations/{}/static-tunnels", organization_id);
        let mut body = serde_json::Map::new();
        body.insert("slug".to_string(), Value::String(slug.to_string()));
        if let Some(name_value) = name {
            body.insert("name".to_string(), Value::String(name_value.to_string()));
        }
        self.post_json(&path, &Value::Object(body), "create static tunnel")
            .await
    }

    pub async fn delete_static_tunnel(
        &self,
        organization_id: &str,
        slug_id: &str,
    ) -> Result<MessageResponse> {
        let path = format!(
            "/api/v1/organizations/{}/static-tunnels/{}",
            organization_id, slug_id
        );
        self.delete_json(&path, "delete static tunnel").await
    }

    pub async fn tunnel_lifecycle_contract(&self) -> Result<TunnelLifecycleContract> {
        let response: DataResponse<TunnelLifecycleContract> = self
            .get_tunnel_json("/api/v1/tunnel/contract", &[], "read tunnel contract")
            .await?;
        Ok(response.data)
    }

    pub async fn list_tunnel_sessions(
        &self,
        limit: u16,
        status: Option<&str>,
    ) -> Result<TunnelCollection<TunnelSessionResource>> {
        self.get_tunnel_json(
            "/api/v1/tunnel/sessions",
            &[
                ("limit", Some(limit.to_string())),
                ("status", status.map(str::to_string)),
            ],
            "list tunnel sessions",
        )
        .await
    }

    pub async fn get_tunnel_session(&self, id: &str) -> Result<TunnelSessionResource> {
        let response: DataResponse<TunnelSessionResource> = self
            .get_tunnel_json(
                &format!("/api/v1/tunnel/sessions/{id}"),
                &[],
                "read tunnel session",
            )
            .await?;
        Ok(response.data)
    }

    pub async fn stop_tunnel_session(
        &self,
        id: &str,
        reason: Option<&str>,
    ) -> Result<TunnelSessionResource> {
        let response: DataResponse<TunnelSessionResource> = self
            .post_tunnel_json(
                &format!("/api/v1/tunnel/sessions/{id}/close"),
                &serde_json::json!({"reason": reason}),
                "stop tunnel session",
            )
            .await?;
        Ok(response.data)
    }

    pub async fn detach_tunnel_session(
        &self,
        id: &str,
        reason: Option<&str>,
    ) -> Result<TunnelSessionResource> {
        let response: DataResponse<TunnelSessionResource> = self
            .post_tunnel_json(
                &format!("/api/v1/tunnel/sessions/{id}/detach"),
                &serde_json::json!({"reason": reason}),
                "detach tunnel session",
            )
            .await?;
        Ok(response.data)
    }

    pub async fn create_anonymous_tunnel_route(
        &self,
        target: &Value,
        name: Option<&str>,
        ttl_seconds: u64,
    ) -> Result<AnonymousTunnelRouteCreated> {
        let response: DataResponse<AnonymousTunnelRouteCreated> = self
            .post_tunnel_json(
                "/api/v1/tunnel/anonymous-routes",
                &serde_json::json!({
                    "target": target,
                    "name": name,
                    "ttl_seconds": ttl_seconds,
                }),
                "create anonymous tunnel route",
            )
            .await?;
        Ok(response.data)
    }

    pub async fn issue_anonymous_tunnel_ticket(
        &self,
        id: &str,
        route_token: &str,
    ) -> Result<RelayTicket> {
        let response: DataResponse<RelayTicket> = self
            .post_tunnel_json(
                &format!("/api/v1/tunnel/anonymous-routes/{id}/relay-tickets"),
                &serde_json::json!({"route_token": route_token}),
                "rotate anonymous tunnel relay ticket",
            )
            .await?;
        Ok(response.data)
    }

    pub async fn claim_anonymous_tunnel_route(
        &self,
        id: &str,
        claim_token: &str,
    ) -> Result<ClaimedAnonymousTunnelRoute> {
        let response: DataResponse<ClaimedAnonymousTunnelRoute> = self
            .post_tunnel_json(
                &format!("/api/v1/tunnel/anonymous-routes/{id}/claim"),
                &serde_json::json!({"claim_token": claim_token}),
                "claim anonymous tunnel route",
            )
            .await?;
        Ok(response.data)
    }

    pub async fn reconnect_tunnel_session(&self, id: &str) -> Result<TunnelReconnectDescriptor> {
        let response: DataResponse<TunnelReconnectDescriptor> = self
            .post_tunnel_json(
                &format!("/api/v1/tunnel/sessions/{id}/reconnect"),
                &serde_json::json!({}),
                "prepare tunnel session reconnect",
            )
            .await?;
        Ok(response.data)
    }

    pub async fn list_tunnel_events(
        &self,
        cursor: Option<&str>,
        limit: u16,
        capture_id: Option<&str>,
        attempt_id: Option<&str>,
    ) -> Result<TunnelEventPage> {
        self.get_tunnel_json(
            "/api/v1/tunnel/events",
            &[
                ("cursor", cursor.map(str::to_string)),
                ("limit", Some(limit.to_string())),
                ("capture_id", capture_id.map(str::to_string)),
                ("delivery_id", attempt_id.map(str::to_string)),
            ],
            "read tunnel events",
        )
        .await
    }

    pub async fn get_tunnel_capture(&self, id: &str) -> Result<TunnelCaptureResource> {
        let response: DataResponse<TunnelCaptureResource> = self
            .get_tunnel_json(
                &format!("/api/v1/tunnel/captures/{id}"),
                &[],
                "read tunnel capture",
            )
            .await?;
        Ok(response.data)
    }

    pub async fn get_tunnel_attempt(&self, id: &str) -> Result<TunnelAttemptResource> {
        let response: DataResponse<TunnelAttemptResource> = self
            .get_tunnel_json(
                &format!("/api/v1/tunnel/deliveries/{id}"),
                &[],
                "read tunnel delivery attempt",
            )
            .await?;
        Ok(response.data)
    }

    // ── Uptime Monitor methods ──────────────────────────────────────────────

    pub async fn list_uptime_monitors(&self) -> Result<Vec<UptimeMonitor>> {
        let response: DataResponse<Vec<UptimeMonitor>> = self
            .get_json("/api/v1/uptime-monitors", "list uptime monitors")
            .await?;
        Ok(response.data)
    }

    pub async fn get_uptime_monitor(&self, id: &str) -> Result<UptimeMonitor> {
        let path = format!("/api/v1/uptime-monitors/{}", id);
        let response: DataResponse<UptimeMonitor> =
            self.get_json(&path, "get uptime monitor").await?;
        Ok(response.data)
    }

    pub async fn create_uptime_monitor(&self, params: &Value) -> Result<UptimeMonitor> {
        let body = serde_json::json!({ "uptime_monitor": params });
        let response: DataResponse<UptimeMonitor> = self
            .post_json("/api/v1/uptime-monitors", &body, "create uptime monitor")
            .await?;
        Ok(response.data)
    }

    pub async fn update_uptime_monitor(&self, id: &str, params: &Value) -> Result<UptimeMonitor> {
        let path = format!("/api/v1/uptime-monitors/{}", id);
        let body = serde_json::json!({ "uptime_monitor": params });
        let response: DataResponse<UptimeMonitor> = self
            .patch_json(&path, &body, "update uptime monitor")
            .await?;
        Ok(response.data)
    }

    pub async fn delete_uptime_monitor(&self, id: &str) -> Result<()> {
        let path = format!("/api/v1/uptime-monitors/{}", id);
        self.delete_empty(&path, "delete uptime monitor").await
    }

    pub async fn list_uptime_checks(
        &self,
        monitor_id: &str,
        page: u32,
        page_size: u32,
    ) -> Result<UptimeChecksResponse> {
        let path = format!(
            "/api/v1/uptime-monitors/{}/checks?page={}&page_size={}",
            monitor_id, page, page_size
        );
        self.get_json(&path, "list uptime checks").await
    }

    // ── Anonymous Endpoint methods ────────────────────────────────────────────

    pub async fn create_anon_endpoint(
        &self,
        ttl_seconds: Option<u64>,
    ) -> Result<AnonEndpointCreated> {
        let mut body = serde_json::Map::new();
        if let Some(ttl) = ttl_seconds {
            body.insert("ttl_seconds".to_string(), Value::Number(ttl.into()));
        }
        self.post_json(
            "/api/v1/anon/endpoints",
            &Value::Object(body),
            "create anonymous endpoint",
        )
        .await
    }

    pub async fn get_anon_endpoint(&self, id: &str) -> Result<AnonEndpointStatus> {
        let path = format!("/api/v1/anon/endpoints/{}", id);
        self.get_json(&path, "get anonymous endpoint").await
    }

    pub async fn list_anon_events(
        &self,
        endpoint_id: &str,
        page: u32,
        page_size: u32,
    ) -> Result<AnonEventsResponse> {
        let path = format!(
            "/api/v1/anon/endpoints/{}/events?page={}&page_size={}",
            endpoint_id, page, page_size
        );
        self.get_json(&path, "list anonymous endpoint events").await
    }

    pub async fn get_anon_event(&self, endpoint_id: &str, event_id: &str) -> Result<AnonEvent> {
        let path = format!("/api/v1/anon/endpoints/{}/events/{}", endpoint_id, event_id);
        self.get_json(&path, "get anonymous endpoint event").await
    }

    // ── Shared Request methods ──────────────────────────────────────────────

    pub async fn create_shared_request(
        &self,
        debug_request_id: &str,
        expires_in_hours: Option<u64>,
        password: Option<&str>,
        include_forwards: bool,
    ) -> Result<SharedRequestSummary> {
        let path = format!("/api/v1/debug-requests/{}/share", debug_request_id);
        let mut share_body = serde_json::Map::new();
        if let Some(hours) = expires_in_hours {
            share_body.insert("expires_in_hours".to_string(), Value::Number(hours.into()));
        }
        if let Some(pw) = password {
            share_body.insert("password".to_string(), Value::String(pw.to_string()));
        }
        share_body.insert(
            "include_forwards".to_string(),
            Value::Bool(include_forwards),
        );

        let body = Value::Object(
            [("share".to_string(), Value::Object(share_body))]
                .into_iter()
                .collect(),
        );

        let response: DataResponse<SharedRequestSummary> = self
            .post_json(&path, &body, "create shared request")
            .await?;
        Ok(response.data)
    }

    pub async fn list_shared_requests(
        &self,
        debug_request_id: &str,
    ) -> Result<Vec<SharedRequestSummary>> {
        let path = format!("/api/v1/debug-requests/{}/shares", debug_request_id);
        let response: DataResponse<Vec<SharedRequestSummary>> =
            self.get_json(&path, "list shared requests").await?;
        Ok(response.data)
    }

    pub async fn get_shared_request(&self, token: &str) -> Result<Value> {
        let path = format!("/api/v1/shared/r/{}", token);
        let response: DataResponse<Value> = self.get_json(&path, "get shared request").await?;
        Ok(response.data)
    }

    pub async fn revoke_shared_request(&self, token: &str) -> Result<()> {
        let path = format!("/api/v1/shared/r/{}", token);
        self.delete_empty(&path, "revoke shared request").await
    }

    pub async fn forward_request(
        &self,
        original_request: &WebhookRequest,
        target_url: &str,
    ) -> Result<ForwardResponse> {
        let start_time = Instant::now();

        // Build the forwarding request
        let method = match original_request.method.as_str() {
            "GET" => reqwest::Method::GET,
            "POST" => reqwest::Method::POST,
            "PUT" => reqwest::Method::PUT,
            "DELETE" => reqwest::Method::DELETE,
            "PATCH" => reqwest::Method::PATCH,
            "HEAD" => reqwest::Method::HEAD,
            "OPTIONS" => reqwest::Method::OPTIONS,
            _ => reqwest::Method::GET,
        };

        // Build URL with query parameters
        let mut url = target_url.parse::<Url>()?;
        if !original_request.query_params.is_empty() {
            for (key, value) in &original_request.query_params {
                url.query_pairs_mut().append_pair(key, &value.to_string());
            }
        }

        let mut request_builder = self.client.request(method, url);

        // Add headers (excluding host-related ones)
        for (key, value) in &original_request.headers {
            let key_lower = key.to_lowercase();
            if !key_lower.starts_with("host")
                && !key_lower.starts_with("x-forwarded")
                && !key_lower.starts_with("cf-")
                && key_lower != "content-length"
            {
                request_builder = request_builder.header(key, value);
            }
        }

        // Add body if present (for POST, PUT, PATCH requests)
        // Use full body if available, otherwise fall back to preview
        let body_content = original_request
            .body
            .as_ref()
            .or(original_request.body_preview.as_ref());
        if let Some(body) = body_content
            && !body.is_empty()
            && original_request.method != "GET"
            && original_request.method != "HEAD"
        {
            request_builder = request_builder.body(body.clone());
        }

        // Execute the request
        match request_builder.send().await {
            Ok(response) => {
                let status_code = response.status().as_u16();

                // Extract response headers
                let mut response_headers = HashMap::new();
                for (key, value) in response.headers() {
                    if let Ok(value_str) = value.to_str() {
                        response_headers.insert(key.to_string(), value_str.to_string());
                    }
                }

                // Decode a bounded, content-aware preview for display. The tunnel path
                // separately preserves original response bytes for the public caller.
                let body = match response.bytes().await {
                    Ok(bytes) => {
                        crate::tunnel::body_preview(&bytes, &response_headers).unwrap_or_default()
                    }
                    Err(_) => "(Failed to read response body)".to_string(),
                };

                let duration = start_time.elapsed();

                Ok(ForwardResponse {
                    success: true,
                    status_code: Some(status_code),
                    headers: response_headers,
                    body,
                    error_message: None,
                    target_url: target_url.to_string(),
                    duration_ms: duration.as_millis() as u64,
                })
            }
            Err(e) => {
                let duration = start_time.elapsed();

                Ok(ForwardResponse {
                    success: false,
                    status_code: None,
                    headers: HashMap::new(),
                    body: String::new(),
                    error_message: Some(e.to_string()),
                    target_url: target_url.to_string(),
                    duration_ms: duration.as_millis() as u64,
                })
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use flate2::{Compression, write::GzEncoder};
    use std::io::Write;

    #[tokio::test]
    async fn relay_ticket_exchange_uses_authorization_header_and_parses_safe_receipt() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/tunnel/relay-tickets")
            .match_header("authorization", "Bearer access-secret")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "plan": {"mode": "capture_forward"}
            })))
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"data":{"ticket":"hktr_once","scope":"relay:listen","plan_fingerprint":"fingerprint","expires_at":"2026-07-14T20:01:00Z"}}"#,
            )
            .create_async()
            .await;

        let ticket = issue_relay_ticket(
            "access-secret",
            &server.url(),
            &serde_json::json!({"mode": "capture_forward"}),
        )
        .await
        .unwrap();

        assert_eq!(ticket.ticket, "hktr_once");
        assert_eq!(ticket.scope, "relay:listen");
        assert_eq!(ticket.plan_fingerprint, "fingerprint");
        assert_eq!(ticket.expires_at, "2026-07-14T20:01:00Z");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn relay_ticket_exchange_classifies_permanent_http_rejections() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/tunnel/relay-tickets")
            .with_status(401)
            .with_body(r#"{"error":{"code":"unauthorized"}}"#)
            .create_async()
            .await;

        let error = match issue_relay_ticket(
            "expired-secret",
            &server.url(),
            &serde_json::json!({"mode": "capture_forward"}),
        )
        .await
        {
            Ok(_) => panic!("expected relay-ticket rejection"),
            Err(error) => error,
        };

        assert!(error.to_string().contains("Relay handshake rejected"));
        assert!(error.to_string().contains("HTTP 401 Unauthorized"));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn anonymous_tunnel_api_bootstraps_rotates_and_claims_without_mixing_credentials() {
        let mut server = mockito::Server::new_async().await;
        let create_mock = server
            .mock("POST", "/api/v1/tunnel/anonymous-routes")
            .match_body(mockito::Matcher::PartialJson(serde_json::json!({
                "name": "stable-demo",
                "ttl_seconds": 900,
                "target": {"host": "localhost", "port": 3000}
            })))
            .with_status(201)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"data":{"id":"anon-123","slug":"stable-demo","url":"https://stable-demo.hook.events","stable_name":true,"expires_at":"2026-07-14T20:15:00Z","route_token":"hkar_route-secret","claim_token":"hkac_claim-secret","relay_ticket":{"ticket":"hktr_first","scope":"relay:tunnel","plan_fingerprint":"first-fingerprint","expires_at":"2026-07-14T20:01:00Z"},"limits":{"body_bytes":1048576}}}"#,
            )
            .create_async()
            .await;
        let rotate_mock = server
            .mock(
                "POST",
                "/api/v1/tunnel/anonymous-routes/anon-123/relay-tickets",
            )
            .match_body(mockito::Matcher::Json(serde_json::json!({
                "route_token": "hkar_route-secret"
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"data":{"ticket":"hktr_second","scope":"relay:tunnel","plan_fingerprint":"second-fingerprint","expires_at":"2026-07-14T20:02:00Z"}}"#,
            )
            .create_async()
            .await;
        let claim_mock = server
            .mock("POST", "/api/v1/tunnel/anonymous-routes/anon-123/claim")
            .match_header("authorization", "Bearer access-secret")
            .match_header("x-organization-id", "org-123")
            .match_body(mockito::Matcher::Json(serde_json::json!({
                "claim_token": "hkac_claim-secret"
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"data":{"id":"route-claimed","slug":"stable-demo","kind":"static","status":"active","claimed_at":"2026-07-14T20:03:00Z","privacy":{"pre_claim_captures_discarded":true,"captures_transferred":0}}}"#,
            )
            .create_async()
            .await;

        let public = ApiClient::with_unauthenticated_base_url(server.url());
        let created = public
            .create_anonymous_tunnel_route(
                &serde_json::json!({"host": "localhost", "port": 3000}),
                Some("stable-demo"),
                900,
            )
            .await
            .unwrap();
        assert_eq!(created.route_token, "hkar_route-secret");
        assert_eq!(created.claim_token, "hkac_claim-secret");

        let rotated = public
            .issue_anonymous_tunnel_ticket(&created.id, &created.route_token)
            .await
            .unwrap();
        assert_eq!(rotated.ticket, "hktr_second");

        let authenticated = ApiClient::with_base_url(
            "access-secret".to_string(),
            server.url(),
            Some("org-123".to_string()),
        )
        .unwrap();
        let claimed = authenticated
            .claim_anonymous_tunnel_route(&created.id, &created.claim_token)
            .await
            .unwrap();
        assert_eq!(claimed.kind, "static");
        assert_eq!(claimed.privacy["captures_transferred"], 0);

        create_mock.assert_async().await;
        rotate_mock.assert_async().await;
        claim_mock.assert_async().await;
    }

    #[tokio::test]
    async fn detach_tunnel_session_posts_reason_and_returns_preserved_route() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/tunnel/sessions/session-123/detach")
            .match_header("authorization", "Bearer test-token")
            .match_header("x-organization-id", "org-123")
            .match_body(mockito::Matcher::Json(serde_json::json!({
                "reason": "switching machines"
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"data":{"id":"session-123","organization_id":"org-123","status":"active","fence":2,"created_at":"2026-07-14T20:00:00Z","updated_at":"2026-07-14T20:00:01Z","route":{"id":"route-123","slug":"stable-demo","kind":"static","mode":"direct_response","status":"active","expires_at":null}}}"#,
            )
            .create_async()
            .await;
        let client = ApiClient::with_base_url(
            "test-token".to_string(),
            server.url(),
            Some("org-123".to_string()),
        )
        .unwrap();

        let session = client
            .detach_tunnel_session("session-123", Some("switching machines"))
            .await
            .unwrap();
        assert_eq!(session.fence, 2);
        assert_eq!(session.route.unwrap().status, "active");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_forward_request_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/webhook")
            .with_status(200)
            .with_body("ok")
            .create_async()
            .await;

        let client =
            ApiClient::with_base_url("test-token".to_string(), server.url(), None).unwrap();

        let request = WebhookRequest {
            id: "req-1".to_string(),
            timestamp: 0,
            remote_addr: "127.0.0.1".to_string(),
            headers: HashMap::new(),
            content_length: 0,
            method: "POST".to_string(),
            url: "/webhook".to_string(),
            path: Some("/webhook".to_string()),
            query_params: HashMap::new(),
            created_at: "2024-01-01".to_string(),
            body_preview: Some("{}".to_string()),
            body: Some("{}".to_string()),
        };

        let target_url = format!("{}/webhook", server.url());
        let result = client.forward_request(&request, &target_url).await.unwrap();
        assert!(result.success);
        assert_eq!(result.status_code, Some(200));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_forward_request_decodes_compressed_response_for_display() {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::default());
        encoder.write_all(b"compressed forward response").unwrap();
        let compressed = encoder.finish().unwrap();
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/webhook")
            .with_status(200)
            .with_header("content-type", "text/plain; charset=utf-8")
            .with_header("content-encoding", "gzip")
            .with_body(compressed)
            .create_async()
            .await;

        let client =
            ApiClient::with_base_url("test-token".to_string(), server.url(), None).unwrap();
        let request = WebhookRequest {
            id: "req-1".to_string(),
            timestamp: 0,
            remote_addr: "127.0.0.1".to_string(),
            headers: HashMap::new(),
            content_length: 0,
            method: "GET".to_string(),
            url: "/webhook".to_string(),
            path: Some("/webhook".to_string()),
            query_params: HashMap::new(),
            created_at: "2024-01-01".to_string(),
            body_preview: None,
            body: None,
        };

        let target_url = format!("{}/webhook", server.url());
        let result = client.forward_request(&request, &target_url).await.unwrap();

        assert!(result.success);
        assert_eq!(result.body, "compressed forward response");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_forward_request_connection_refused() {
        let client = ApiClient::with_base_url(
            "test-token".to_string(),
            "http://localhost:1".to_string(),
            None,
        )
        .unwrap();

        let request = WebhookRequest {
            id: "req-1".to_string(),
            timestamp: 0,
            remote_addr: "127.0.0.1".to_string(),
            headers: HashMap::new(),
            content_length: 0,
            method: "POST".to_string(),
            url: "/webhook".to_string(),
            path: Some("/webhook".to_string()),
            query_params: HashMap::new(),
            created_at: "2024-01-01".to_string(),
            body_preview: None,
            body: None,
        };

        let result = client
            .forward_request(&request, "http://localhost:1/webhook")
            .await
            .unwrap();
        assert!(!result.success);
        assert!(result.error_message.is_some());
    }

    #[tokio::test]
    async fn test_run_endpoint_cases_posts_params_and_returns_data() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/endpoints/ep_123/cases/run")
            .match_header("authorization", "Bearer test-token")
            .match_body(mockito::Matcher::Json(serde_json::json!({
                "target_url": "http://localhost:3000/webhooks",
                "wait": true,
                "timeout_ms": 60000
            })))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"data":{"id":"run_123","case_suite_run_id":"run_123","case_suite_run_url":"/api/v1/case-runs/run_123","status":"completed","result_status":"passed","async":false,"waited":true,"endpoint_id":"ep_123","target":{"type":"custom","url":"http://localhost:3000/webhooks"},"total_count":1,"queued_count":0,"failed_count":0,"completed_count":1,"passed_count":1,"forwards":[],"failures":[]}}"#,
            )
            .create_async()
            .await;

        let client =
            ApiClient::with_base_url("test-token".to_string(), server.url(), None).unwrap();
        let result = client
            .run_endpoint_cases(
                "ep_123",
                &CaseRunParams {
                    target_url: Some("http://localhost:3000/webhooks".to_string()),
                    target_id: None,
                    target: None,
                    target_name: None,
                    wait: Some(true),
                    timeout_ms: Some(60_000),
                    interval_ms: None,
                },
            )
            .await
            .unwrap();

        assert_eq!(result.result_status, "passed");
        assert_eq!(result.case_suite_run_id.as_deref(), Some("run_123"));
        assert_eq!(
            result.target.url.as_deref(),
            Some("http://localhost:3000/webhooks")
        );
        assert_eq!(result.passed_count, 1);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_tunnel_contract_and_resources_use_authenticated_cloud_state() {
        let mut server = mockito::Server::new_async().await;
        let contract_mock = server
            .mock("GET", "/api/v1/tunnel/contract")
            .match_header("authorization", "Bearer test-token")
            .match_header("x-organization-id", "org-123")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"data":{"id":"hooklistener.tunnel.lifecycle","version":"1.0.0","schema":{"major":1,"minor":0},"receipts":{},"events":{},"resources":{},"lifecycle":{},"exit_codes":{}}}"#,
            )
            .create_async()
            .await;
        let session_mock = server
            .mock("GET", "/api/v1/tunnel/sessions/session-123")
            .match_header("authorization", "Bearer test-token")
            .match_header("x-organization-id", "org-123")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"data":{"id":"session-123","organization_id":"org-123","status":"active","fence":1,"created_at":"2026-07-14T20:00:00Z","updated_at":"2026-07-14T20:00:01Z","route":null}}"#,
            )
            .create_async()
            .await;
        let reconnect_mock = server
            .mock("POST", "/api/v1/tunnel/sessions/session-123/reconnect")
            .match_header("authorization", "Bearer test-token")
            .match_header("x-organization-id", "org-123")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"data":{"session":{"id":"session-123","organization_id":"org-123","status":"active","fence":1,"created_at":"2026-07-14T20:00:00Z","updated_at":"2026-07-14T20:00:01Z","route":null},"topic":"tunnel:connect","resume_token":"secret-resume-token","resume_token_expires_in":300,"cursor":"opaque","ownership":"lost"}}"#,
            )
            .create_async()
            .await;
        let client = ApiClient::with_base_url(
            "test-token".to_string(),
            server.url(),
            Some("org-123".to_string()),
        )
        .unwrap();

        assert_eq!(
            client.tunnel_lifecycle_contract().await.unwrap().version,
            "1.0.0"
        );
        assert_eq!(
            client
                .get_tunnel_session("session-123")
                .await
                .unwrap()
                .status,
            "active"
        );
        let reconnect = client
            .reconnect_tunnel_session("session-123")
            .await
            .unwrap();
        assert_eq!(reconnect.cursor, "opaque");
        assert_eq!(reconnect.resume_token, "secret-resume-token");
        contract_mock.assert_async().await;
        session_mock.assert_async().await;
        reconnect_mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_tunnel_cursor_expiry_is_a_typed_error() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/v1/tunnel/events")
            .match_query(mockito::Matcher::Any)
            .with_status(409)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"error":{"code":"cursor_expired","message":"expired","earliest_cursor":"earliest","resync":{"sessions":"/api/v1/tunnel/sessions"}}}"#,
            )
            .create_async()
            .await;
        let client =
            ApiClient::with_base_url("test-token".to_string(), server.url(), None).unwrap();

        let error = client
            .list_tunnel_events(Some("expired"), 50, None, None)
            .await
            .unwrap_err();
        let lifecycle = error.downcast_ref::<TunnelLifecycleError>().unwrap();
        assert!(matches!(
            lifecycle,
            TunnelLifecycleError::CursorExpired {
                earliest_cursor: Some(cursor),
                ..
            } if cursor == "earliest"
        ));
        mock.assert_async().await;
    }

    #[test]
    fn test_tunnel_capture_projection_drops_unrecognized_sensitive_fields() {
        let capture: TunnelCaptureResource = serde_json::from_value(serde_json::json!({
            "id": "capture-123",
            "organization_id": "org-123",
            "route_id": "route-123",
            "session_id": "session-123",
            "endpoint_id": null,
            "origin": "tunnel",
            "method": "POST",
            "path": "/billing",
            "request_content_length": 6,
            "request_body_captured": true,
            "provider_response_status": 200,
            "captured_at": "2026-07-14T20:00:00Z",
            "created_at": "2026-07-14T20:00:00Z",
            "updated_at": "2026-07-14T20:00:01Z",
            "request_headers": {"authorization": "Bearer secret"},
            "request_body": "secret",
            "object_storage_key": "private/key"
        }))
        .unwrap();

        let output = serde_json::to_string(&capture).unwrap();
        assert!(!output.contains("authorization"));
        assert!(!output.contains("Bearer secret"));
        assert!(!output.contains("private/key"));
    }
}
