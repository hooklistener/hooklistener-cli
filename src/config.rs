use anyhow::Result;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use std::fs;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Config {
    pub access_token: Option<String>,
    pub token_expires_at: Option<DateTime<Utc>>,
    pub selected_organization_id: Option<String>,
    #[serde(default)]
    pub last_update_check: Option<DateTime<Utc>>,
    #[serde(default)]
    pub latest_known_version: Option<String>,
}

impl Config {
    pub fn load() -> Result<Self> {
        let config_path = Self::config_path()?;
        Self::load_from(&config_path)
    }

    pub fn load_from(path: &Path) -> Result<Self> {
        if path.exists() {
            let content = fs::read_to_string(path)?;
            let config: Config = serde_json::from_str(&content)?;
            Ok(config)
        } else {
            Ok(Config::default())
        }
    }

    pub fn save(&self) -> Result<()> {
        let config_path = Self::config_path()?;
        self.save_to(&config_path)
    }

    pub fn save_to(&self, path: &Path) -> Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let content = serde_json::to_string_pretty(self)?;
        fs::write(path, content)?;

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

    #[cfg(test)]
    pub fn set_selected_organization(&mut self, organization_id: String) {
        self.selected_organization_id = Some(organization_id);
    }

    #[cfg(test)]
    pub fn clear_all(&mut self) {
        *self = Config::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;
    use tempfile::TempDir;

    fn config_path_in(dir: &TempDir) -> PathBuf {
        dir.path().join("config.json")
    }

    #[test]
    fn test_no_token_is_invalid() {
        let config = Config::default();
        assert!(!config.is_token_valid());
    }

    #[test]
    fn test_token_without_expiry_is_invalid() {
        let config = Config {
            access_token: Some("tok".to_string()),
            ..Config::default()
        };
        assert!(!config.is_token_valid());
    }

    #[test]
    fn test_expired_token_is_invalid() {
        let config = Config {
            access_token: Some("tok".to_string()),
            token_expires_at: Some(Utc::now() - Duration::hours(1)),
            ..Config::default()
        };
        assert!(!config.is_token_valid());
    }

    #[test]
    fn test_valid_token() {
        let config = Config {
            access_token: Some("tok".to_string()),
            token_expires_at: Some(Utc::now() + Duration::hours(1)),
            ..Config::default()
        };
        assert!(config.is_token_valid());
    }

    #[test]
    fn test_set_access_token() {
        let mut config = Config::default();
        let expires = Utc::now() + Duration::hours(24);
        config.set_access_token("my_token".to_string(), expires);
        assert_eq!(config.access_token.as_deref(), Some("my_token"));
        assert_eq!(config.token_expires_at, Some(expires));
    }

    #[test]
    fn test_clear_token_preserves_org() {
        let mut config = Config {
            access_token: Some("tok".to_string()),
            token_expires_at: Some(Utc::now()),
            selected_organization_id: Some("org-1".to_string()),
            ..Config::default()
        };
        config.clear_token();
        assert!(config.access_token.is_none());
        assert!(config.token_expires_at.is_none());
        assert_eq!(config.selected_organization_id.as_deref(), Some("org-1"));
    }

    #[test]
    fn test_clear_all() {
        let mut config = Config {
            access_token: Some("tok".to_string()),
            token_expires_at: Some(Utc::now()),
            selected_organization_id: Some("org-1".to_string()),
            ..Config::default()
        };
        config.clear_all();
        assert!(config.access_token.is_none());
        assert!(config.token_expires_at.is_none());
        assert!(config.selected_organization_id.is_none());
    }

    #[test]
    fn test_set_selected_organization() {
        let mut config = Config::default();
        config.set_selected_organization("org-42".to_string());
        assert_eq!(config.selected_organization_id.as_deref(), Some("org-42"));
    }

    #[test]
    fn test_save_load_roundtrip() {
        let dir = TempDir::new().unwrap();
        let path = config_path_in(&dir);

        let expires = Utc::now() + Duration::hours(24);
        let config = Config {
            access_token: Some("roundtrip_token".to_string()),
            token_expires_at: Some(expires),
            selected_organization_id: Some("org-rt".to_string()),
            ..Config::default()
        };
        config.save_to(&path).unwrap();

        let loaded = Config::load_from(&path).unwrap();
        assert_eq!(loaded.access_token.as_deref(), Some("roundtrip_token"));
        assert_eq!(loaded.selected_organization_id.as_deref(), Some("org-rt"));
        assert!(loaded.is_token_valid());
    }

    #[test]
    fn test_load_missing_file_returns_defaults() {
        let dir = TempDir::new().unwrap();
        let path = dir.path().join("nonexistent.json");

        let config = Config::load_from(&path).unwrap();
        assert!(config.access_token.is_none());
        assert!(config.token_expires_at.is_none());
        assert!(config.selected_organization_id.is_none());
    }

    #[test]
    fn test_load_corrupted_json_returns_error() {
        let dir = TempDir::new().unwrap();
        let path = config_path_in(&dir);

        fs::write(&path, "not valid json {{{").unwrap();

        let result = Config::load_from(&path);
        assert!(result.is_err());
    }
}
