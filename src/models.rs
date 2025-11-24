use serde::{Deserialize, Serialize};
use std::collections::HashMap;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Organization {
    pub id: String,
    pub name: String,
    pub updated_at: String,
    pub created_at: String,
    pub signing_secret_prefix: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugEndpoint {
    pub id: String,
    pub name: String,
    pub status: String,
    pub updated_at: String,
    pub created_at: String,
    pub slug: String,
    pub webhook_url: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DebugEndpointsResponse {
    pub data: Vec<DebugEndpoint>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DebugEndpointDetail {
    pub id: String,
    pub name: String,
    pub status: String,
    pub updated_at: String,
    pub created_at: String,
    pub slug: String,
    pub webhook_url: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct DebugEndpointDetailResponse {
    pub data: DebugEndpointDetail,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WebhookRequest {
    pub id: String,
    pub timestamp: i64,
    pub remote_addr: String,
    pub headers: HashMap<String, String>,
    pub content_length: i64,
    pub method: String,
    pub url: String,
    #[serde(default)]
    pub path: Option<String>, // Added for compatibility with tunnel
    pub query_params: HashMap<String, String>,
    pub created_at: String,
    pub body_preview: Option<String>,
    #[serde(default)]
    pub body: Option<String>, // Full body content (fetched separately)
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Pagination {
    pub page: i32,
    pub total_count: i32,
    pub page_size: i32,
    pub total_pages: i32,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WebhookRequestsResponse {
    pub data: Vec<WebhookRequest>,
    pub pagination: Pagination,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WebhookRequestDetailResponse {
    pub data: WebhookRequest,
}

#[derive(Debug, Clone)]
pub struct ForwardResponse {
    pub success: bool,
    pub status_code: Option<u16>,
    pub headers: HashMap<String, String>,
    pub body: String,
    pub error_message: Option<String>,
    pub target_url: String,
    pub duration_ms: u64,
}
