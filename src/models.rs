use serde::{Deserialize, Serialize};
use std::collections::HashMap;

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
