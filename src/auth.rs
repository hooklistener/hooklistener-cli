use anyhow::{Result, anyhow};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
pub struct DeviceCodeResponse {
    pub device_code: String,
    pub user_code: String,
    pub expires_in: u64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct TokenResponse {
    pub access_token: String,
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
}

impl DeviceCodeFlow {
    pub fn new(base_url: String) -> Self {
        Self {
            client: reqwest::Client::new(),
            base_url,
            device_code: None,
            user_code: None,
            expires_at: None,
        }
    }

    pub async fn initiate_device_flow(&mut self) -> Result<String> {
        let url = format!("{}/api/v1/device", self.base_url);

        let response = self
            .client
            .post(&url)
            .header("Content-Type", "application/json")
            .send()
            .await?;

        if !response.status().is_success() {
            return Err(anyhow!(
                "Failed to initiate device flow: {}",
                response.status()
            ));
        }

        let device_response: DeviceCodeResponse = response.json().await?;

        self.device_code = Some(device_response.device_code.clone());
        self.user_code = Some(device_response.user_code.clone());
        self.expires_at = Some(Utc::now() + Duration::seconds(device_response.expires_in as i64));

        Ok(device_response.user_code)
    }

    pub async fn poll_for_authorization(&self) -> Result<Option<String>> {
        let device_code = self
            .device_code
            .as_ref()
            .ok_or_else(|| anyhow!("No device code available"))?;

        let url = format!(
            "{}/api/v1/device?device_code={}",
            self.base_url, device_code
        );

        let response = self.client.get(&url).send().await?;

        match response.status().as_u16() {
            200 => {
                let text = response.text().await?;

                if let Ok(token_response) = serde_json::from_str::<TokenResponse>(&text) {
                    Ok(Some(token_response.access_token))
                } else if let Ok(pending_response) = serde_json::from_str::<PendingResponse>(&text)
                {
                    if pending_response.error == "authorization_pending" {
                        Ok(None)
                    } else {
                        Err(anyhow!("Authorization error: {}", pending_response.error))
                    }
                } else {
                    Err(anyhow!("Unexpected response format"))
                }
            }
            404 => Err(anyhow!("Device code not found or expired")),
            _ => Err(anyhow!("Polling failed: {}", response.status())),
        }
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
