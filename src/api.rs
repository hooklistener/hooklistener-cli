use crate::models::{ForwardResponse, WebhookRequest};
use anyhow::Result;
use reqwest::Client;
use std::collections::HashMap;
use std::time::Instant;

pub struct ApiClient {
    client: Client,
}

impl ApiClient {
    pub fn with_organization(_access_token: String, _organization_id: Option<String>) -> Self {
        Self {
            client: Client::new(),
        }
    }

    #[cfg(test)]
    pub fn with_base_url(
        _access_token: String,
        _base_url: String,
        _organization_id: Option<String>,
    ) -> Self {
        Self {
            client: Client::new(),
        }
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

        let client = ApiClient::with_base_url("test-token".to_string(), server.url(), None);

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
        );

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
