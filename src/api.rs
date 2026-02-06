use crate::errors::{ApiError, classify_http_error};
use crate::logger::generate_request_id;
use crate::models::{
    DebugEndpoint, DebugEndpointDetail, DebugEndpointDetailResponse, DebugEndpointsResponse,
    ForwardResponse, Organization, WebhookRequest, WebhookRequestDetailResponse,
    WebhookRequestsResponse,
};
use anyhow::Result;
use reqwest::Client;
use std::collections::HashMap;
use std::time::Instant;
use tracing::{debug, error, info};

pub struct ApiClient {
    client: Client,
    access_token: String,
    base_url: String,
    organization_id: Option<String>,
}

impl ApiClient {
    pub fn new(access_token: String) -> Self {
        Self::with_organization(access_token, None)
    }

    pub fn with_organization(access_token: String, organization_id: Option<String>) -> Self {
        // Check environment variable for local development
        let base_url = std::env::var("HOOKLISTENER_API_URL")
            .unwrap_or_else(|_| "https://app.hooklistener.com".to_string());

        Self {
            client: Client::new(),
            access_token,
            base_url,
            organization_id,
        }
    }

    #[cfg(test)]
    pub fn with_base_url(
        access_token: String,
        base_url: String,
        organization_id: Option<String>,
    ) -> Self {
        Self {
            client: Client::new(),
            access_token,
            base_url,
            organization_id,
        }
    }

    fn add_headers(&self, mut request_builder: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        request_builder =
            request_builder.header("Authorization", format!("Bearer {}", self.access_token));

        if let Some(org_id) = &self.organization_id {
            request_builder = request_builder.header("x-organization-id", org_id);
        }

        request_builder
    }

    pub async fn fetch_organizations(&self) -> Result<Vec<Organization>> {
        let url = format!("{}/api/v1/organizations", self.base_url);
        let request_id = generate_request_id();
        let start_time = Instant::now();

        crate::log_api_request!("GET", &url, &request_id);

        let request_builder = self.client.get(&url);
        let response = self.add_headers(request_builder).send().await;

        let duration_ms = start_time.elapsed().as_millis() as u64;

        match response {
            Ok(response) => {
                let status = response.status().as_u16();
                crate::log_api_response!(&request_id, status, duration_ms);

                if !response.status().is_success() {
                    let api_err = classify_http_error(status, "Organizations");
                    error!(
                        request_id = %request_id,
                        status = status,
                        url = %url,
                        "API request failed with non-success status"
                    );
                    return Err(api_err.into());
                }

                match response.json::<Vec<Organization>>().await {
                    Ok(organizations) => {
                        info!(
                            request_id = %request_id,
                            count = organizations.len(),
                            "Successfully fetched organizations"
                        );
                        Ok(organizations)
                    }
                    Err(e) => {
                        error!(
                            request_id = %request_id,
                            error = %e,
                            "Failed to parse organizations response"
                        );
                        Err(ApiError::ParseError(e.to_string()).into())
                    }
                }
            }
            Err(e) => {
                crate::log_api_error!(&request_id, &e, duration_ms);
                Err(ApiError::NetworkError(e.to_string()).into())
            }
        }
    }

    pub async fn fetch_debug_endpoints(&self) -> Result<Vec<DebugEndpoint>> {
        let url = format!("{}/api/v1/debug-endpoints", self.base_url);
        let request_id = generate_request_id();
        let start_time = Instant::now();

        crate::log_api_request!("GET", &url, &request_id);

        let request_builder = self.client.get(&url);
        let response = self.add_headers(request_builder).send().await;

        let duration_ms = start_time.elapsed().as_millis() as u64;

        match response {
            Ok(response) => {
                let status = response.status().as_u16();
                crate::log_api_response!(&request_id, status, duration_ms);

                if !response.status().is_success() {
                    let api_err = classify_http_error(status, "Debug endpoints");
                    error!(
                        request_id = %request_id,
                        status = status,
                        url = %url,
                        "Failed to fetch debug endpoints"
                    );
                    return Err(api_err.into());
                }

                match response.json::<DebugEndpointsResponse>().await {
                    Ok(endpoints_response) => {
                        info!(
                            request_id = %request_id,
                            count = endpoints_response.data.len(),
                            "Successfully fetched debug endpoints"
                        );
                        Ok(endpoints_response.data)
                    }
                    Err(e) => {
                        error!(
                            request_id = %request_id,
                            error = %e,
                            "Failed to parse debug endpoints response"
                        );
                        Err(ApiError::ParseError(e.to_string()).into())
                    }
                }
            }
            Err(e) => {
                crate::log_api_error!(&request_id, &e, duration_ms);
                Err(ApiError::NetworkError(e.to_string()).into())
            }
        }
    }

    pub async fn fetch_endpoint_detail(&self, endpoint_id: &str) -> Result<DebugEndpointDetail> {
        let url = format!("{}/api/v1/debug-endpoints/{}", self.base_url, endpoint_id);
        let request_id = generate_request_id();
        let start_time = Instant::now();

        crate::log_api_request!("GET", &url, &request_id);
        debug!(request_id = %request_id, endpoint_id = %endpoint_id, "Fetching endpoint detail");

        let request_builder = self.client.get(&url);
        let response = self.add_headers(request_builder).send().await;

        let duration_ms = start_time.elapsed().as_millis() as u64;

        match response {
            Ok(response) => {
                let status = response.status().as_u16();
                crate::log_api_response!(&request_id, status, duration_ms);

                if !response.status().is_success() {
                    let api_err = classify_http_error(status, "Endpoint detail");
                    error!(
                        request_id = %request_id,
                        endpoint_id = %endpoint_id,
                        status = status,
                        url = %url,
                        "Failed to fetch endpoint detail"
                    );
                    return Err(api_err.into());
                }

                match response.json::<DebugEndpointDetailResponse>().await {
                    Ok(detail_response) => {
                        info!(
                            request_id = %request_id,
                            endpoint_id = %endpoint_id,
                            "Successfully fetched endpoint detail"
                        );
                        Ok(detail_response.data)
                    }
                    Err(e) => {
                        error!(
                            request_id = %request_id,
                            endpoint_id = %endpoint_id,
                            error = %e,
                            "Failed to parse endpoint detail response"
                        );
                        Err(ApiError::ParseError(e.to_string()).into())
                    }
                }
            }
            Err(e) => {
                crate::log_api_error!(&request_id, &e, duration_ms);
                Err(ApiError::NetworkError(e.to_string()).into())
            }
        }
    }

    pub async fn fetch_endpoint_requests(
        &self,
        endpoint_id: &str,
        page: i32,
        page_size: i32,
    ) -> Result<WebhookRequestsResponse> {
        let url = format!(
            "{}/api/v1/debug-endpoints/{}/requests?page={}&page_size={}",
            self.base_url, endpoint_id, page, page_size
        );

        let request_builder = self.client.get(&url);
        let response = self.add_headers(request_builder).send().await;

        match response {
            Ok(response) => {
                if !response.status().is_success() {
                    let api_err =
                        classify_http_error(response.status().as_u16(), "Endpoint requests");
                    return Err(api_err.into());
                }

                let requests_response: WebhookRequestsResponse = response.json().await?;
                Ok(requests_response)
            }
            Err(e) => Err(ApiError::NetworkError(e.to_string()).into()),
        }
    }

    pub async fn fetch_request_details(
        &self,
        endpoint_id: &str,
        request_id: &str,
    ) -> Result<WebhookRequest> {
        let url = format!(
            "{}/api/v1/debug-endpoints/{}/requests/{}",
            self.base_url, endpoint_id, request_id
        );

        let request_builder = self.client.get(&url);
        let response = self.add_headers(request_builder).send().await;

        match response {
            Ok(response) => {
                if !response.status().is_success() {
                    let api_err = classify_http_error(response.status().as_u16(), "Request detail");
                    return Err(api_err.into());
                }

                // Try to parse as wrapped response first (consistent with other endpoints)
                match response.json::<WebhookRequestDetailResponse>().await {
                    Ok(wrapped_response) => Ok(wrapped_response.data),
                    Err(e) => Err(ApiError::ParseError(e.to_string()).into()),
                }
            }
            Err(e) => Err(ApiError::NetworkError(e.to_string()).into()),
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
    async fn test_fetch_organizations_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/v1/organizations")
            .match_header("Authorization", "Bearer test-token")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"[{"id":"org-1","name":"Test Org","updated_at":"2024-01-01","created_at":"2024-01-01","signing_secret_prefix":null}]"#,
            )
            .create_async()
            .await;

        let client = ApiClient::with_base_url("test-token".to_string(), server.url(), None);
        let orgs = client.fetch_organizations().await.unwrap();
        assert_eq!(orgs.len(), 1);
        assert_eq!(orgs[0].name, "Test Org");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_fetch_organizations_unauthorized() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/v1/organizations")
            .with_status(401)
            .create_async()
            .await;

        let client = ApiClient::with_base_url("bad-token".to_string(), server.url(), None);
        let result = client.fetch_organizations().await;
        assert!(result.is_err());
        let err = result.unwrap_err();
        assert!(err.downcast_ref::<ApiError>().is_some());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_fetch_organizations_server_error() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/v1/organizations")
            .with_status(500)
            .create_async()
            .await;

        let client = ApiClient::with_base_url("test-token".to_string(), server.url(), None);
        let result = client.fetch_organizations().await;
        assert!(result.is_err());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_fetch_debug_endpoints_with_org_header() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/v1/debug-endpoints")
            .match_header("x-organization-id", "org-1")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":[]}"#)
            .create_async()
            .await;

        let client = ApiClient::with_base_url(
            "test-token".to_string(),
            server.url(),
            Some("org-1".to_string()),
        );
        let endpoints = client.fetch_debug_endpoints().await.unwrap();
        assert!(endpoints.is_empty());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_fetch_endpoint_requests_pagination() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/v1/debug-endpoints/ep-1/requests?page=2&page_size=10")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"data":[],"pagination":{"page":2,"total_count":15,"page_size":10,"total_pages":2}}"#)
            .create_async()
            .await;

        let client = ApiClient::with_base_url("test-token".to_string(), server.url(), None);
        let response = client.fetch_endpoint_requests("ep-1", 2, 10).await.unwrap();
        assert_eq!(response.pagination.page, 2);
        assert_eq!(response.pagination.total_pages, 2);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_fetch_request_details_not_found() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/v1/debug-endpoints/ep-1/requests/req-1")
            .with_status(404)
            .create_async()
            .await;

        let client = ApiClient::with_base_url("test-token".to_string(), server.url(), None);
        let result = client.fetch_request_details("ep-1", "req-1").await;
        assert!(result.is_err());
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
