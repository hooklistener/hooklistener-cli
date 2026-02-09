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

pub struct ApiClient {
    client: Client,
    base_url: Option<String>,
}

impl ApiClient {
    pub fn for_forwarding() -> Self {
        Self {
            client: Client::new(),
            base_url: None,
        }
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

        let base_url = std::env::var("HOOKLISTENER_API_URL")
            .unwrap_or_else(|_| "https://app.hooklistener.com".to_string());

        Ok(Self {
            client,
            base_url: Some(base_url),
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
