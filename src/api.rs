use crate::models::{
    DebugEndpoint, DebugEndpointDetail, DebugEndpointDetailResponse, DebugEndpointsResponse,
    ForwardResponse, Organization, WebhookRequest, WebhookRequestDetailResponse,
    WebhookRequestsResponse,
};
use anyhow::Result;
use reqwest::Client;
use std::collections::HashMap;
use std::time::Instant;

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
        Self {
            client: Client::new(),
            access_token,
            base_url: "https://api.hooklistener.com".to_string(),
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

        let request_builder = self.client.get(&url);
        let response = self.add_headers(request_builder).send().await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to fetch organizations: {}",
                response.status()
            ));
        }

        // The API returns an array directly based on the provided example
        let organizations: Vec<Organization> = response.json().await?;
        Ok(organizations)
    }

    pub async fn fetch_debug_endpoints(&self) -> Result<Vec<DebugEndpoint>> {
        let url = format!("{}/api/v1/debug-endpoints", self.base_url);

        let request_builder = self.client.get(&url);
        let response = self.add_headers(request_builder).send().await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to fetch debug endpoints: {}",
                response.status()
            ));
        }

        let endpoints_response: DebugEndpointsResponse = response.json().await?;
        Ok(endpoints_response.data)
    }

    pub async fn fetch_endpoint_detail(&self, endpoint_id: &str) -> Result<DebugEndpointDetail> {
        let url = format!("{}/api/v1/debug-endpoints/{}", self.base_url, endpoint_id);

        let request_builder = self.client.get(&url);
        let response = self.add_headers(request_builder).send().await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to fetch endpoint detail: {}",
                response.status()
            ));
        }

        let detail_response: DebugEndpointDetailResponse = response.json().await?;
        Ok(detail_response.data)
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
        let response = self.add_headers(request_builder).send().await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "Failed to fetch endpoint requests: {}",
                response.status()
            ));
        }

        let requests_response: WebhookRequestsResponse = response.json().await?;
        Ok(requests_response)
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
        let response = self.add_headers(request_builder).send().await?;

        if !response.status().is_success() {
            return Err(anyhow::anyhow!(
                "API endpoint returned status: {}. This endpoint may not be supported by the API.",
                response.status()
            ));
        }

        // Try to parse as wrapped response first (consistent with other endpoints)
        match response.json::<WebhookRequestDetailResponse>().await {
            Ok(wrapped_response) => Ok(wrapped_response.data),
            Err(_) => {
                // If that fails, the endpoint might not exist or return different format
                Err(anyhow::anyhow!(
                    "Unable to parse response. The API may not support individual request details."
                ))
            }
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
