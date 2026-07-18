use anyhow::Result;
use chrono::Utc;
use std::cmp::Reverse;
use std::fs;
use std::path::{Path, PathBuf};
use tracing::{info, warn};
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{
    EnvFilter, Registry, fmt::time::ChronoUtc, layer::SubscriberExt, util::SubscriberInitExt,
};
use uuid::Uuid;

pub struct LogConfig {
    pub level: String,
    pub directory: PathBuf,
    pub output_to_stdout: bool,
    pub max_log_files: usize,
    #[allow(dead_code)] // Reserved for future log file size management
    pub max_file_size_mb: u64,
}

impl Default for LogConfig {
    fn default() -> Self {
        let log_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("hooklistener")
            .join("logs");

        Self {
            level: "info".to_string(),
            directory: log_dir,
            output_to_stdout: false,
            max_log_files: 10,
            max_file_size_mb: 10,
        }
    }
}

pub struct Logger {
    session_id: Uuid,
    _guard: WorkerGuard,
}

impl Logger {
    pub fn new(config: LogConfig) -> Result<Self> {
        let session_id = Uuid::new_v4();

        // Create log directory if it doesn't exist
        fs::create_dir_all(&config.directory)?;

        // Clean up old log files
        let _ = Self::cleanup_old_logs(&config.directory, config.max_log_files)?;

        let log_file_path = config.directory.join(format!(
            "hooklistener-{}.log",
            Utc::now().format("%Y%m%d-%H%M%S")
        ));

        // Create file appender
        let file_appender =
            tracing_appender::rolling::never(&config.directory, log_file_path.file_name().unwrap());
        let (non_blocking, guard) = tracing_appender::non_blocking(file_appender);

        // Create filter
        let filter =
            EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(&config.level));

        // Create subscriber with both console and file output
        let registry = Registry::default().with(filter);

        if config.output_to_stdout {
            let stdout_layer = tracing_subscriber::fmt::layer()
                .with_writer(std::io::stdout)
                .with_timer(ChronoUtc::rfc_3339())
                .with_target(true)
                .with_thread_ids(true)
                .with_line_number(true)
                .with_file(true);

            let file_layer = tracing_subscriber::fmt::layer()
                .with_writer(non_blocking)
                .with_timer(ChronoUtc::rfc_3339())
                .with_target(true)
                .with_thread_ids(true)
                .with_line_number(true)
                .with_file(true)
                .json();

            registry.with(stdout_layer).with(file_layer).init();
        } else {
            let file_layer = tracing_subscriber::fmt::layer()
                .with_writer(non_blocking)
                .with_timer(ChronoUtc::rfc_3339())
                .with_target(true)
                .with_thread_ids(true)
                .with_line_number(true)
                .with_file(true)
                .json();

            registry.with(file_layer).init();
        }

        info!(
            session_id = %session_id,
            version = env!("CARGO_PKG_VERSION"),
            "Starting HookListener CLI session"
        );

        Ok(Logger {
            session_id,
            _guard: guard,
        })
    }

    #[allow(dead_code)] // Reserved for external session tracking
    pub fn session_id(&self) -> &Uuid {
        &self.session_id
    }

    pub fn cleanup_old_logs(log_dir: &Path, max_files: usize) -> Result<usize> {
        let entries = fs::read_dir(log_dir)?;
        let mut log_files: Vec<_> = entries
            .filter_map(|entry| {
                let entry = entry.ok()?;
                let path = entry.path();
                if path.is_file()
                    && path.file_name()?.to_str()?.starts_with("hooklistener-")
                    && path.extension()? == "log"
                {
                    let metadata = entry.metadata().ok()?;
                    Some((path, metadata.modified().ok()?))
                } else {
                    None
                }
            })
            .collect();

        // Sort by modification time (newest first)
        log_files.sort_by_key(|(_, modified)| Reverse(*modified));

        // Remove old files if we have too many
        let mut removed_count = 0;
        if log_files.len() > max_files {
            for (path, _) in log_files.into_iter().skip(max_files) {
                if let Err(e) = fs::remove_file(&path) {
                    warn!(
                        error = %e,
                        file = %path.display(),
                        "Failed to remove old log file"
                    );
                } else {
                    removed_count += 1;
                    info!(
                        file = %path.display(),
                        "Removed old log file"
                    );
                }
            }
        }

        Ok(removed_count)
    }

    pub fn create_diagnostic_bundle(&self, bundle_path: &Path) -> Result<()> {
        info!(
            session_id = %self.session_id,
            bundle_path = %bundle_path.display(),
            "Creating diagnostic bundle"
        );

        // Create a directory for the diagnostic bundle
        let bundle_dir = bundle_path.join(format!(
            "hooklistener-diagnostics-{}",
            Utc::now().format("%Y%m%d-%H%M%S")
        ));
        fs::create_dir_all(&bundle_dir)?;

        // Write this first so a useful bundle remains even when optional inputs are corrupt.
        let system_info = self.collect_system_info();
        let system_info_path = bundle_dir.join("system_info.json");
        fs::write(
            system_info_path,
            serde_json::to_string_pretty(&system_info)?,
        )?;

        let config_path = crate::config::Config::config_path()?;
        let (sanitized_config, tokens) = if config_path.exists() {
            match Self::read_config_for_bundle(&config_path) {
                Ok(contents) => contents,
                Err(error) => {
                    warn!(error = %error, "Skipping unreadable diagnostic config");
                    (
                        Some("{\n  \"status\": \"config unavailable or invalid\"\n}".to_string()),
                        Vec::new(),
                    )
                }
            }
        } else {
            (None, Vec::new())
        };

        // Copy recent log files
        let log_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("hooklistener")
            .join("logs");

        if log_dir.exists() {
            let log_bundle_dir = bundle_dir.join("logs");
            fs::create_dir_all(&log_bundle_dir)?;
            if let Err(error) = Self::copy_log_files(&log_dir, &log_bundle_dir, &tokens) {
                warn!(error = %error, "Unable to enumerate diagnostic logs");
            }
        }

        // Copy sanitized config
        if let Some(sanitized_config) = sanitized_config {
            let config_bundle_path = bundle_dir.join("config.json");
            if let Err(error) = fs::write(config_bundle_path, sanitized_config) {
                warn!(error = %error, "Unable to write sanitized diagnostic config");
            }
        }

        info!(
            session_id = %self.session_id,
            bundle_dir = %bundle_dir.display(),
            "Diagnostic bundle created successfully"
        );

        Ok(())
    }

    fn copy_log_files(log_dir: &Path, log_bundle_dir: &Path, tokens: &[Vec<u8>]) -> Result<()> {
        for entry in fs::read_dir(log_dir)? {
            let entry = match entry {
                Ok(entry) => entry,
                Err(error) => {
                    warn!(error = %error, "Skipping unreadable log directory entry");
                    continue;
                }
            };
            let path = entry.path();
            let Some(file_name) = path.file_name() else {
                continue;
            };
            if !path.is_file()
                || !file_name
                    .to_str()
                    .is_some_and(|name| name.starts_with("hooklistener-"))
                || path.extension().is_none_or(|extension| extension != "log")
            {
                continue;
            }

            let dest = log_bundle_dir.join(file_name);
            if let Err(e) = Self::copy_redacted_log(&path, &dest, tokens) {
                warn!(
                    error = %e,
                    source = %path.display(),
                    dest = %dest.display(),
                    "Failed to copy log file to diagnostic bundle"
                );
            }
        }
        Ok(())
    }

    fn copy_redacted_log(source: &Path, dest: &Path, tokens: &[Vec<u8>]) -> Result<()> {
        let mut contents = fs::read(source)?;
        for token in tokens.iter().filter(|token| !token.is_empty()) {
            contents = Self::replace_bytes(&contents, token, b"[REDACTED]");
        }

        let temp = dest.with_file_name(format!(
            ".{}.{}.tmp",
            dest.file_name().unwrap_or_default().to_string_lossy(),
            Uuid::new_v4()
        ));
        if let Err(error) = fs::write(&temp, contents).and_then(|()| fs::rename(&temp, dest)) {
            let _ = fs::remove_file(&temp);
            return Err(error.into());
        }
        Ok(())
    }

    fn replace_bytes(input: &[u8], needle: &[u8], replacement: &[u8]) -> Vec<u8> {
        let mut output = Vec::with_capacity(input.len());
        let mut remaining = input;
        while let Some(position) = remaining
            .windows(needle.len())
            .position(|part| part == needle)
        {
            output.extend_from_slice(&remaining[..position]);
            output.extend_from_slice(replacement);
            remaining = &remaining[position + needle.len()..];
        }
        output.extend_from_slice(remaining);
        output
    }

    fn read_config_for_bundle(config_path: &Path) -> Result<(Option<String>, Vec<Vec<u8>>)> {
        let content = fs::read_to_string(config_path)?;
        let config: serde_json::Value = serde_json::from_str(&content)?;
        serde_json::from_value::<crate::config::Config>(config.clone())
            .map_err(|_| anyhow::anyhow!("Config unavailable or invalid"))?;
        let tokens = ["access_token", "refresh_token"]
            .into_iter()
            .filter_map(|key| config.get(key)?.as_str())
            .map(|token| token.as_bytes().to_vec())
            .collect();
        Ok((Some(Self::sanitize_config(config)?), tokens))
    }

    #[cfg(test)]
    fn create_sanitized_config(config_path: &Path) -> Result<String> {
        let content = fs::read_to_string(config_path)?;
        let config: serde_json::Value = serde_json::from_str(&content)?;
        Self::sanitize_config(config)
    }

    fn sanitize_config(mut config: serde_json::Value) -> Result<String> {
        // Remove sensitive data
        if let Some(obj) = config.as_object_mut() {
            obj.remove("access_token");
            obj.remove("refresh_token");
            if let Some(token_expires) = obj.get_mut("token_expires_at") {
                *token_expires = serde_json::Value::String("[REDACTED]".to_string());
            }
            if let Some(token_expires) = obj.get_mut("refresh_token_expires_at") {
                *token_expires = serde_json::Value::String("[REDACTED]".to_string());
            }
        }

        Ok(serde_json::to_string_pretty(&config)?)
    }

    fn collect_system_info(&self) -> serde_json::Value {
        serde_json::json!({
            "session_id": self.session_id,
            "timestamp": Utc::now().to_rfc3339(),
            "version": env!("CARGO_PKG_VERSION"),
            "os": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "rust_version": std::env::var("RUSTC_VERSION").unwrap_or_else(|_| "unknown".to_string()),
        })
    }
}

// Macros for structured logging with automatic context
#[macro_export]
macro_rules! log_api_request {
    ($method:expr, $url:expr, $request_id:expr) => {
        tracing::info!(
            request_id = $request_id,
            method = $method,
            url = $url,
            "API request initiated"
        );
    };
}

#[macro_export]
macro_rules! log_api_response {
    ($request_id:expr, $status:expr, $duration_ms:expr) => {
        tracing::info!(
            request_id = $request_id,
            status = $status,
            duration_ms = $duration_ms,
            "API response received"
        );
    };
}

#[macro_export]
macro_rules! log_api_error {
    ($request_id:expr, $error:expr, $duration_ms:expr) => {
        tracing::error!(
            request_id = $request_id,
            error = %$error,
            duration_ms = $duration_ms,
            "API request failed"
        );
    };
}

#[macro_export]
macro_rules! log_state_transition {
    ($from:expr, $to:expr, $context:expr) => {
        tracing::debug!(
            from_state = ?$from,
            to_state = ?$to,
            context = %$context,
            "State transition"
        );
    };
}

#[macro_export]
macro_rules! log_user_action {
    ($action:expr, $context:expr) => {
        tracing::info!(
            user_action = $action,
            context = %$context,
            "User action performed"
        );
    };
}

#[macro_export]
macro_rules! log_performance {
    ($operation:expr, $duration_ms:expr, $details:expr) => {
        tracing::debug!(
            operation = $operation,
            duration_ms = $duration_ms,
            details = %$details,
            "Performance measurement"
        );
    };
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn create_sanitized_config_removes_tokens_and_redacts_expiry_values() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.json");
        fs::write(
            &config_path,
            r#"{
                "access_token": "access-secret",
                "refresh_token": "refresh-secret",
                "token_expires_at": "2030-01-01T00:00:00Z",
                "refresh_token_expires_at": 1893456000,
                "api_url": "https://example.test",
                "theme": "dark"
            }"#,
        )
        .unwrap();

        let sanitized = Logger::create_sanitized_config(&config_path).unwrap();
        let config: serde_json::Value = serde_json::from_str(&sanitized).unwrap();

        assert!(config.get("access_token").is_none());
        assert!(config.get("refresh_token").is_none());
        assert_eq!(config["token_expires_at"], "[REDACTED]");
        assert_eq!(config["refresh_token_expires_at"], "[REDACTED]");
        assert_eq!(config["api_url"], "https://example.test");
        assert_eq!(config["theme"], "dark");
    }

    #[test]
    fn create_sanitized_config_malformed_json_error_does_not_expose_secret() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.json");
        let secret = "highly-sensitive-access-token";
        fs::write(
            &config_path,
            format!(r#"{{"access_token":"{secret}","theme":"dark","#),
        )
        .unwrap();

        let error = Logger::create_sanitized_config(&config_path)
            .unwrap_err()
            .to_string();

        assert!(
            !error.contains(secret),
            "error exposed config secret: {error}"
        );
    }

    #[test]
    fn read_config_for_bundle_rejects_wrong_json_shape_without_exposing_content() {
        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.json");
        let secret = "valid-json-but-not-a-config-secret";
        fs::write(&config_path, format!(r#"["{secret}"]"#)).unwrap();

        let error = Logger::read_config_for_bundle(&config_path)
            .unwrap_err()
            .to_string();

        assert_eq!(error, "Config unavailable or invalid");
        assert!(!error.contains(secret));
    }

    #[test]
    fn copy_log_files_only_copies_hooklistener_log_files() {
        let dir = TempDir::new().unwrap();
        let logs = dir.path().join("logs");
        let bundle = dir.path().join("bundle");
        fs::create_dir_all(&logs).unwrap();
        fs::create_dir_all(&bundle).unwrap();
        fs::write(logs.join("hooklistener-current.log"), "wanted").unwrap();
        fs::write(logs.join("hooklistener-not-a-log.txt"), "unwanted").unwrap();
        fs::write(logs.join("other.log"), "unwanted").unwrap();
        fs::create_dir(logs.join("hooklistener-directory.log")).unwrap();

        Logger::copy_log_files(&logs, &bundle, &[]).unwrap();

        let copied: Vec<_> = fs::read_dir(bundle)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(copied, vec!["hooklistener-current.log"]);
    }

    #[test]
    fn copy_log_files_continues_after_one_file_cannot_be_copied() {
        let dir = TempDir::new().unwrap();
        let logs = dir.path().join("logs");
        let bundle = dir.path().join("bundle");
        fs::create_dir_all(&logs).unwrap();
        fs::create_dir_all(&bundle).unwrap();
        fs::write(logs.join("hooklistener-blocked.log"), "blocked").unwrap();
        fs::write(logs.join("hooklistener-copied.log"), "copied").unwrap();
        fs::create_dir(bundle.join("hooklistener-blocked.log")).unwrap();

        Logger::copy_log_files(&logs, &bundle, &[]).unwrap();

        assert_eq!(
            fs::read_to_string(bundle.join("hooklistener-copied.log")).unwrap(),
            "copied"
        );
        assert!(bundle.join("hooklistener-blocked.log").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn copy_log_files_skips_non_utf8_names_instead_of_panicking() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;

        let dir = TempDir::new().unwrap();
        let logs = dir.path().join("logs");
        let bundle = dir.path().join("bundle");
        fs::create_dir_all(&logs).unwrap();
        fs::create_dir_all(&bundle).unwrap();
        fs::write(logs.join("hooklistener-valid.log"), "valid").unwrap();
        fs::write(
            logs.join(OsString::from_vec(vec![0xff, b'.', b'l', b'o', b'g'])),
            "invalid",
        )
        .unwrap();

        Logger::copy_log_files(&logs, &bundle, &[]).unwrap();

        let copied: Vec<_> = fs::read_dir(bundle)
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(copied, vec![OsString::from("hooklistener-valid.log")]);
    }

    #[test]
    fn copy_log_files_redacts_known_tokens_in_utf8_and_non_utf8_logs() {
        let dir = TempDir::new().unwrap();
        let logs = dir.path().join("logs");
        let bundle = dir.path().join("bundle");
        fs::create_dir_all(&logs).unwrap();
        fs::create_dir_all(&bundle).unwrap();
        let token = b"secret-token".to_vec();
        fs::write(
            logs.join("hooklistener-utf8.log"),
            b"Authorization: Bearer secret-token",
        )
        .unwrap();
        fs::write(
            logs.join("hooklistener-bytes.log"),
            b"\xff before secret-token \xfe after",
        )
        .unwrap();

        Logger::copy_log_files(&logs, &bundle, &[token]).unwrap();

        for name in ["hooklistener-utf8.log", "hooklistener-bytes.log"] {
            let copied = fs::read(bundle.join(name)).unwrap();
            assert!(
                !copied
                    .windows(b"secret-token".len())
                    .any(|part| part == b"secret-token")
            );
            assert!(
                copied
                    .windows(b"[REDACTED]".len())
                    .any(|part| part == b"[REDACTED]")
            );
        }
    }

    #[test]
    fn invalid_config_cannot_be_copied_and_does_not_prevent_log_processing() {
        let dir = TempDir::new().unwrap();
        let config = dir.path().join("config.json");
        let logs = dir.path().join("logs");
        let bundle = dir.path().join("bundle");
        fs::write(&config, b"{\xff raw malformed secret").unwrap();
        fs::create_dir_all(&logs).unwrap();
        fs::create_dir_all(&bundle).unwrap();
        fs::write(logs.join("hooklistener-ok.log"), b"usable log").unwrap();

        assert!(Logger::read_config_for_bundle(&config).is_err());
        Logger::copy_log_files(&logs, &bundle, &[]).unwrap();

        assert_eq!(
            fs::read(bundle.join("hooklistener-ok.log")).unwrap(),
            b"usable log"
        );
    }

    #[test]
    fn failed_log_rename_removes_temporary_output() {
        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source.log");
        let destination = dir.path().join("destination.log");
        fs::write(&source, b"contents").unwrap();
        fs::create_dir(&destination).unwrap();

        assert!(Logger::copy_redacted_log(&source, &destination, &[]).is_err());
        let entries: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert!(
            !entries
                .iter()
                .any(|name| name.to_string_lossy().ends_with(".tmp"))
        );
        assert!(destination.is_dir());
    }
}
