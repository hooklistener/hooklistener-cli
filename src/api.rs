use crate::models::{ForwardResponse, WebhookRequest};
use anyhow::{Context, Result, anyhow};
use reqwest::{
    Client, Response,
    header::{AUTHORIZATION, HeaderMap, HeaderValue},
};
use serde::{Deserialize, Deserializer, Serialize, de::DeserializeOwned};
use serde_json::Value;
use std::collections::HashMap;
use std::time::Instant;

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
pub async fn refresh_access_token(refresh_token: &str) -> Result<TokenRefreshResponse> {
    let base_url = default_base_url();
    let url = format!("{}/api/v1/auth/refresh", base_url.trim_end_matches('/'));
    let body = serde_json::json!({ "refresh_token": refresh_token });

    let client = Client::new();
    let response = client
        .post(&url)
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

impl ApiClient {
    pub fn for_forwarding() -> Self {
        Self {
            client: Client::new(),
            base_url: None,
        }
    }

    /// Create a client with no authentication (for anonymous endpoints).
    pub fn unauthenticated() -> Result<Self> {
        Ok(Self {
            client: Client::new(),
            base_url: Some(default_base_url()),
        })
    }

    pub fn with_organization(
        access_token: String,
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
            .build()
            .context("Failed to build API client")?;

        Ok(Self {
            client,
            base_url: Some(default_base_url()),
        })
    }

    #[cfg(test)]
    pub fn with_base_url(
        access_token: String,
        base_url: String,
        organization_id: Option<String>,
    ) -> Result<Self> {
        let mut client = Self::with_organization(access_token, organization_id)?;
        client.base_url = Some(base_url);
        Ok(client)
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

    pub async fn update_uptime_monitor(
        &self,
        id: &str,
        params: &Value,
    ) -> Result<UptimeMonitor> {
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
        self.post_json("/api/v1/anon/endpoints", &Value::Object(body), "create anonymous endpoint")
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

    pub async fn get_anon_event(
        &self,
        endpoint_id: &str,
        event_id: &str,
    ) -> Result<AnonEvent> {
        let path = format!(
            "/api/v1/anon/endpoints/{}/events/{}",
            endpoint_id, event_id
        );
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
            share_body.insert(
                "expires_in_hours".to_string(),
                Value::Number(hours.into()),
            );
        }
        if let Some(pw) = password {
            share_body.insert("password".to_string(), Value::String(pw.to_string()));
        }
        share_body.insert("include_forwards".to_string(), Value::Bool(include_forwards));

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
        let response: DataResponse<Value> =
            self.get_json(&path, "get shared request").await?;
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

        let mut request_builder = self.client.request(method, target_url);

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

        // Add query parameters
        if !original_request.query_params.is_empty() {
            request_builder = request_builder.query(&original_request.query_params);
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

                // Get response body
                let body = response
                    .text()
                    .await
                    .unwrap_or_else(|_| "(Failed to read response body)".to_string());

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
}
