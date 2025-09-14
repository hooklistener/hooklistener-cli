use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Debug, Serialize, Deserialize)]
pub struct Config {
    pub access_token: Option<String>,
    pub token_expires_at: Option<DateTime<Utc>>,
    pub selected_organization_id: Option<String>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = Self::config_path()?;

        if config_path.exists() {
            let content = fs::read_to_string(config_path)?;
            let config: Config = serde_json::from_str(&content)?;
            Ok(config)
        } else {
            Ok(Config {
                access_token: None,
                token_expires_at: None,
                selected_organization_id: None,
            })
        }
    }

    pub fn save(&self) -> Result<()> {
        let config_path = Self::config_path()?;

        if let Some(parent) = config_path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content = serde_json::to_string_pretty(self)?;
        fs::write(config_path, content)?;

        Ok(())
    }

    pub fn config_path() -> Result<PathBuf> {
        let home =
            dirs::config_dir().ok_or_else(|| anyhow::anyhow!("Could not find config directory"))?;

        Ok(home.join("hooklistener").join("config.json"))
    }

    pub fn set_access_token(&mut self, access_token: String, expires_at: DateTime<Utc>) {
        self.access_token = Some(access_token);
        self.token_expires_at = Some(expires_at);
    }

    pub fn is_token_valid(&self) -> bool {
        if let (Some(_), Some(expires_at)) = (&self.access_token, &self.token_expires_at) {
            Utc::now() < *expires_at
        } else {
            false
        }
    }

    pub fn clear_token(&mut self) {
        self.access_token = None;
        self.token_expires_at = None;
    }

    pub fn set_selected_organization(&mut self, organization_id: String) {
        self.selected_organization_id = Some(organization_id);
    }

    pub fn clear_all(&mut self) {
        self.access_token = None;
        self.token_expires_at = None;
        self.selected_organization_id = None;
    }
}
