use anyhow::{Result, anyhow};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use std::time::Duration as StdDuration;
use tokio::time::Instant;

const DEFAULT_POLL_INTERVAL: StdDuration = StdDuration::from_secs(5);
const SLOW_DOWN_INCREMENT: StdDuration = StdDuration::from_secs(5);
const MAX_AUTH_RESPONSE_BYTES: usize = 16 * 1024;
const MAX_ERROR_DETAIL_CHARS: usize = 240;

#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub expires_in: u64,
    #[serde(default)]
    pub interval: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub expires_in: Option<u64>,
    #[serde(default)]
    pub refresh_expires_in: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PendingResponse {
    pub error: String,
}

#[derive(Debug)]
pub struct DeviceCodeFlow {
    client: reqwest::Client,
    base_url: String,
    device_code: Option<String>,
    user_code: Option<String>,
    expires_at: Option<DateTime<Utc>>,
    poll_interval: StdDuration,
    next_poll_at: Option<Instant>,
}

impl DeviceCodeFlow {
    pub fn new(base_url: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url,
            device_code: None,
            user_code: None,
            expires_at: None,
            poll_interval: DEFAULT_POLL_INTERVAL,
            next_poll_at: None,
        }
    }

    pub async fn initiate_device_flow(&mut self) -> Result<String> {
        let url = format!("{}/api/v1/device", self.base_url);

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .send()
            .await
            .map_err(|error| Self::request_error("Failed to initiate device flow", error, None))?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "Failed to initiate device flow: {}",
                response.status()
            ));
        }

        let body = Self::read_bounded_response(response, None).await?;
        let device_response: DeviceCodeResponse = serde_json::from_slice(&body).map_err(|_| {
            anyhow!("Authorization server returned an invalid device flow response")
        })?;

        self.device_code = Some(device_response.device_code.clone());
        self.user_code = Some(device_response.user_code.clone());
        self.expires_at = Some(Utc::now() + Duration::seconds(device_response.expires_in as i64));
        self.poll_interval = StdDuration::from_secs(
            device_response
                .interval
                .unwrap_or(DEFAULT_POLL_INTERVAL.as_secs()),
        );
        self.schedule_next_poll()?;

        Ok(device_response.user_code)
    }

    pub async fn poll_for_authorization(&mut self) -> Result<Option<TokenResponse>> {
        let device_code = self
            .device_code
            .clone()
            .ok_or_else(|| anyhow!("No device code available"))?;

        if !self.poll_is_due() {
            return Ok(None);
        }
        self.next_poll_at = None;

        let mut url = reqwest::Url::parse(&format!("{}/api/v1/device", self.base_url))
            .map_err(|_| anyhow!("Invalid authorization server URL"))?;
        url.query_pairs_mut()
            .append_pair("device_code", &device_code);
        // The deployed endpoint contract is GET with a query parameter. Url encodes the
        // opaque code, and every request error below removes both the URL and the code.
        let response = self.client.get(url).send().await.map_err(|error| {
            Self::request_error(
                "Device authorization request failed",
                error,
                Some(&device_code),
            )
        })?;
        let status = response.status();

        if status.as_u16() == 404 {
            return Err(anyhow!("Device code not found or expired"));
        }

        let body = Self::read_bounded_response(response, Some(&device_code)).await?;
        if matches!(status.as_u16(), 200 | 400)
            && let Ok(pending_response) = serde_json::from_slice::<PendingResponse>(&body)
        {
            return self.handle_authorization_error(&pending_response.error);
        }

        if status.is_success() {
            if let Ok(token_response) = serde_json::from_slice::<TokenResponse>(&body) {
                if token_response.access_token.is_empty() {
                    Err(anyhow!("Unexpected response format"))
                } else {
                    Ok(Some(token_response))
                }
            } else {
                Err(anyhow!("Unexpected response format"))
            }
        } else {
            Err(anyhow!("Polling failed: {status}"))
        }
    }

    pub fn time_until_next_poll(&self) -> StdDuration {
        Self::poll_delay_at(self.next_poll_at, Instant::now())
    }

    fn poll_is_due(&self) -> bool {
        self.time_until_next_poll().is_zero()
    }

    fn poll_delay_at(next_poll_at: Option<Instant>, now: Instant) -> StdDuration {
        next_poll_at.map_or(StdDuration::ZERO, |deadline| {
            deadline.saturating_duration_since(now)
        })
    }

    fn schedule_next_poll(&mut self) -> Result<()> {
        self.next_poll_at = Some(
            Instant::now()
                .checked_add(self.poll_interval)
                .ok_or_else(|| anyhow!("Authorization polling interval is too large"))?,
        );
        Ok(())
    }

    fn handle_authorization_error(&mut self, error: &str) -> Result<Option<TokenResponse>> {
        match error {
            "authorization_pending" => {
                self.schedule_next_poll()?;
                Ok(None)
            }
            "slow_down" => {
                self.poll_interval = self
                    .poll_interval
                    .checked_add(SLOW_DOWN_INCREMENT)
                    .ok_or_else(|| anyhow!("Authorization polling interval is too large"))?;
                self.schedule_next_poll()?;
                Ok(None)
            }
            "access_denied" => Err(anyhow!("Device authorization was denied")),
            "expired_token" => Err(anyhow!("Device code not found or expired")),
            _ => Err(anyhow!(
                "Authorization server returned an unrecognized error"
            )),
        }
    }

    async fn read_bounded_response(
        mut response: reqwest::Response,
        secret: Option<&str>,
    ) -> Result<Vec<u8>> {
        if response
            .content_length()
            .is_some_and(|length| length > MAX_AUTH_RESPONSE_BYTES as u64)
        {
            return Err(anyhow!(
                "Authorization server response exceeded {MAX_AUTH_RESPONSE_BYTES} bytes"
            ));
        }

        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|error| {
            Self::request_error("Failed to read authorization response", error, secret)
        })? {
            if chunk.len() > MAX_AUTH_RESPONSE_BYTES.saturating_sub(body.len()) {
                return Err(anyhow!(
                    "Authorization server response exceeded {MAX_AUTH_RESPONSE_BYTES} bytes"
                ));
            }
            body.extend_from_slice(&chunk);
        }
        Ok(body)
    }

    fn request_error(context: &str, error: reqwest::Error, secret: Option<&str>) -> anyhow::Error {
        let mut detail = error.without_url().to_string();
        if let Some(secret) = secret.filter(|secret| !secret.is_empty()) {
            detail = detail.replace(secret, "[REDACTED]");
        }
        anyhow!("{context}: {}", Self::sanitize_error_detail(&detail))
    }

    fn sanitize_error_detail(detail: &str) -> String {
        let mut sanitized = String::new();
        let mut chars = detail.chars();
        for character in chars.by_ref().take(MAX_ERROR_DETAIL_CHARS) {
            if character.is_control() {
                sanitized.push('\u{fffd}');
            } else {
                sanitized.push(character);
            }
        }
        if chars.next().is_some() {
            sanitized.push('…');
        }
        sanitized
    }

    pub fn format_user_code(&self) -> Option<String> {
        self.user_code.as_ref().map(|code| {
            if code.len() == 8 {
                format!("{}-{}", &code[0..4], &code[4..8])
            } else {
                code.clone()
            }
        })
    }

    pub fn time_remaining(&self) -> Option<Duration> {
        self.expires_at.map(|expires| {
            let remaining = expires - Utc::now();
            if remaining > Duration::zero() {
                remaining
            } else {
                Duration::zero()
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn test_initiate_device_flow_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/device")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"device_code":"dev123","user_code":"ABCD1234","expires_in":600}"#)
            .create_async()
            .await;

        let mut flow = DeviceCodeFlow::new(server.url());
        let user_code = flow.initiate_device_flow().await.unwrap();
        assert_eq!(user_code, "ABCD1234");
        assert_eq!(flow.device_code.as_deref(), Some("dev123"));
        assert!(flow.expires_at.is_some());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_initiate_device_flow_server_error() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/device")
            .with_status(500)
            .create_async()
            .await;

        let mut flow = DeviceCodeFlow::new(server.url());
        let result = flow.initiate_device_flow().await;
        assert!(result.is_err());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn initiate_device_flow_uses_server_poll_interval() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/device")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(
                r#"{"device_code":"dev123","user_code":"ABCD1234","expires_in":600,"interval":11}"#,
            )
            .create_async()
            .await;

        let mut flow = DeviceCodeFlow::new(server.url());
        flow.initiate_device_flow().await.unwrap();

        assert_eq!(
            (flow.poll_interval, flow.next_poll_at.is_some()),
            (StdDuration::from_secs(11), true)
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn initiate_device_flow_defaults_to_five_second_poll_interval() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("POST", "/api/v1/device")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"device_code":"dev123","user_code":"ABCD1234","expires_in":600}"#)
            .create_async()
            .await;

        let mut flow = DeviceCodeFlow::new(server.url());
        flow.initiate_device_flow().await.unwrap();

        assert_eq!(flow.poll_interval, DEFAULT_POLL_INTERVAL);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_poll_for_authorization_pending() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/v1/device?device_code=dev123")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":"authorization_pending"}"#)
            .create_async()
            .await;

        let mut flow = DeviceCodeFlow::new(server.url());
        flow.device_code = Some("dev123".to_string());
        let result = flow.poll_for_authorization().await.unwrap();
        assert!(result.is_none());
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_poll_for_authorization_success() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/v1/device?device_code=dev123")
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"access_token":"tok_secret_abc","refresh_token":"ref_tok","expires_in":3600,"refresh_expires_in":2592000,"token_type":"Bearer"}"#)
            .create_async()
            .await;

        let mut flow = DeviceCodeFlow::new(server.url());
        flow.device_code = Some("dev123".to_string());
        let result = flow.poll_for_authorization().await.unwrap();
        let token_response = result.unwrap();
        assert_eq!(token_response.access_token, "tok_secret_abc");
        assert_eq!(token_response.refresh_token.as_deref(), Some("ref_tok"));
        assert_eq!(token_response.expires_in, Some(3600));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn poll_for_authorization_percent_encodes_device_code_query_value() {
        let mut server = mockito::Server::new_async().await;
        let device_code = "device code&admin=true";
        let mock = server
            .mock("GET", "/api/v1/device")
            .match_query(mockito::Matcher::UrlEncoded(
                "device_code".to_string(),
                device_code.to_string(),
            ))
            .with_status(200)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":"authorization_pending"}"#)
            .create_async()
            .await;

        let mut flow = DeviceCodeFlow::new(server.url());
        flow.device_code = Some(device_code.to_string());
        flow.poll_for_authorization().await.unwrap();

        mock.assert_async().await;
    }

    #[tokio::test]
    async fn poll_for_authorization_slow_down_adds_five_seconds() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/v1/device?device_code=dev123")
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":"slow_down"}"#)
            .create_async()
            .await;

        let mut flow = DeviceCodeFlow::new(server.url());
        flow.device_code = Some("dev123".to_string());
        let result = flow.poll_for_authorization().await.unwrap();

        assert_eq!(
            (
                result.is_none(),
                flow.poll_interval,
                flow.next_poll_at.is_some(),
            ),
            (true, StdDuration::from_secs(10), true)
        );
        mock.assert_async().await;
    }

    #[test]
    fn repeated_slow_down_responses_keep_increasing_poll_interval() {
        let mut flow = DeviceCodeFlow::new("http://localhost".to_string());

        flow.handle_authorization_error("slow_down").unwrap();
        flow.handle_authorization_error("slow_down").unwrap();

        assert_eq!(flow.poll_interval, StdDuration::from_secs(15));
    }

    #[test]
    fn authorization_pending_preserves_poll_interval() {
        let mut flow = DeviceCodeFlow::new("http://localhost".to_string());
        flow.poll_interval = StdDuration::from_secs(9);

        flow.handle_authorization_error("authorization_pending")
            .unwrap();

        assert_eq!(flow.poll_interval, StdDuration::from_secs(9));
    }

    #[test]
    fn time_until_next_poll_reports_configured_interval() {
        let now = Instant::now();
        let deadline = now + StdDuration::from_secs(9);

        let delay = DeviceCodeFlow::poll_delay_at(Some(deadline), now);

        assert_eq!(delay, StdDuration::from_secs(9));
    }

    #[tokio::test]
    async fn poll_transport_error_does_not_expose_device_code() {
        let device_code = "sensitive-device-code";
        let mut flow = DeviceCodeFlow::new("://invalid-base-url".to_string());
        flow.device_code = Some(device_code.to_string());

        let error = flow.poll_for_authorization().await.unwrap_err().to_string();

        assert!(
            !error.contains(device_code),
            "error exposed device code: {error}"
        );
    }

    #[tokio::test]
    async fn poll_before_deadline_returns_without_sending_request() {
        let mut flow = DeviceCodeFlow::new("://invalid-base-url".to_string());
        flow.device_code = Some("dev123".to_string());
        flow.next_poll_at = Some(Instant::now() + StdDuration::from_secs(60));

        let result = flow.poll_for_authorization().await.unwrap();

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn poll_unknown_remote_error_is_not_exposed() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/v1/device?device_code=dev123")
            .with_status(400)
            .with_header("content-type", "application/json")
            .with_body(r#"{"error":"secret-body\u001b]52;c;clipboard"}"#)
            .create_async()
            .await;

        let mut flow = DeviceCodeFlow::new(server.url());
        flow.device_code = Some("dev123".to_string());
        let error = flow.poll_for_authorization().await.unwrap_err().to_string();

        assert_eq!(error, "Authorization server returned an unrecognized error");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn poll_rejects_oversized_remote_error_body_without_exposing_it() {
        let mut server = mockito::Server::new_async().await;
        let secret = "remote-error-secret";
        let body = format!("{secret}{}", "x".repeat(MAX_AUTH_RESPONSE_BYTES));
        let mock = server
            .mock("GET", "/api/v1/device?device_code=dev123")
            .with_status(500)
            .with_body(body)
            .create_async()
            .await;

        let mut flow = DeviceCodeFlow::new(server.url());
        flow.device_code = Some("dev123".to_string());
        let error = flow.poll_for_authorization().await.unwrap_err().to_string();

        assert_eq!(
            error,
            format!("Authorization server response exceeded {MAX_AUTH_RESPONSE_BYTES} bytes")
        );
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn poll_http_error_does_not_expose_bounded_remote_body() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/v1/device?device_code=dev123")
            .with_status(500)
            .with_body("secret remote body\u{1b}]52;c;clipboard")
            .create_async()
            .await;

        let mut flow = DeviceCodeFlow::new(server.url());
        flow.device_code = Some("dev123".to_string());
        let error = flow.poll_for_authorization().await.unwrap_err().to_string();

        assert_eq!(error, "Polling failed: 500 Internal Server Error");
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_poll_for_authorization_expired() {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", "/api/v1/device?device_code=dev123")
            .with_status(404)
            .create_async()
            .await;

        let mut flow = DeviceCodeFlow::new(server.url());
        flow.device_code = Some("dev123".to_string());
        let result = flow.poll_for_authorization().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("expired"));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn test_poll_without_device_code_errors() {
        let server = mockito::Server::new_async().await;
        let mut flow = DeviceCodeFlow::new(server.url());
        let result = flow.poll_for_authorization().await;
        assert!(result.is_err());
        assert!(result.unwrap_err().to_string().contains("No device code"));
    }

    #[test]
    fn test_format_user_code_8_chars() {
        let mut flow = DeviceCodeFlow::new("http://localhost".to_string());
        flow.user_code = Some("ABCD1234".to_string());
        assert_eq!(flow.format_user_code(), Some("ABCD-1234".to_string()));
    }

    #[test]
    fn test_format_user_code_other_length() {
        let mut flow = DeviceCodeFlow::new("http://localhost".to_string());
        flow.user_code = Some("ABC".to_string());
        assert_eq!(flow.format_user_code(), Some("ABC".to_string()));
    }

    #[test]
    fn test_format_user_code_none() {
        let flow = DeviceCodeFlow::new("http://localhost".to_string());
        assert_eq!(flow.format_user_code(), None);
    }

    #[test]
    fn test_time_remaining_future() {
        let mut flow = DeviceCodeFlow::new("http://localhost".to_string());
        flow.expires_at = Some(Utc::now() + Duration::minutes(5));
        let remaining = flow.time_remaining().unwrap();
        assert!(remaining > Duration::zero());
    }

    #[test]
    fn test_time_remaining_past() {
        let mut flow = DeviceCodeFlow::new("http://localhost".to_string());
        flow.expires_at = Some(Utc::now() - Duration::minutes(5));
        let remaining = flow.time_remaining().unwrap();
        assert_eq!(remaining, Duration::zero());
    }

    #[test]
    fn test_time_remaining_not_set() {
        let flow = DeviceCodeFlow::new("http://localhost".to_string());
        assert!(flow.time_remaining().is_none());
    }
}
