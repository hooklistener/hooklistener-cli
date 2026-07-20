use anyhow::Result;
use chrono::Utc;
use std::cmp::Reverse;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
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

        Self::ensure_private_directory(&config.directory)?;
        #[cfg(unix)]
        Self::harden_existing_log_files(&config.directory)?;

        // Clean up old log files
        let _ = Self::cleanup_old_logs(&config.directory, config.max_log_files)?;

        let log_file_path = config.directory.join(format!(
            "hooklistener-{}-{session_id}.log",
            Utc::now().format("%Y%m%d-%H%M%S"),
        ));

        // Supplying the already-opened file prevents tracing-appender from creating it
        // with process-umask-dependent permissions.
        let log_file = Self::open_private_file(&log_file_path)?;
        let (non_blocking, guard) = tracing_appender::non_blocking(log_file);

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

        let bundle_dir = Self::create_unique_bundle_directory(bundle_path)?;

        // Write this first so a useful bundle remains even when optional inputs are corrupt.
        let system_info = self.collect_system_info();
        let system_info_path = bundle_dir.join("system_info.json");
        Self::write_private_file(
            &system_info_path,
            serde_json::to_string_pretty(&system_info)?.as_bytes(),
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
            Self::create_private_directory(&log_bundle_dir)?;
            if let Err(error) = Self::copy_log_files(&log_dir, &log_bundle_dir, &tokens) {
                warn!(error = %error, "Unable to enumerate diagnostic logs");
            }
        }

        // Copy sanitized config
        if let Some(sanitized_config) = sanitized_config {
            let config_bundle_path = bundle_dir.join("config.json");
            if let Err(error) =
                Self::write_private_file(&config_bundle_path, sanitized_config.as_bytes())
            {
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

    fn ensure_private_directory(path: &Path) -> Result<()> {
        fs::create_dir_all(path)?;

        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;

            // Apply permissions through the opened directory descriptor so a later path
            // replacement cannot redirect the chmod operation.
            let directory = File::open(path)?;
            if !directory.metadata()?.is_dir() {
                anyhow::bail!("{} is not a directory", path.display());
            }
            directory.set_permissions(fs::Permissions::from_mode(0o700))?;
        }

        Ok(())
    }

    fn create_private_directory(path: &Path) -> std::io::Result<()> {
        #[cfg(unix)]
        {
            use std::os::unix::fs::{DirBuilderExt, PermissionsExt};

            let mut builder = fs::DirBuilder::new();
            builder.mode(0o700).create(path)?;
            let directory = File::open(path)?;
            directory.set_permissions(fs::Permissions::from_mode(0o700))?;
        }

        #[cfg(not(unix))]
        fs::create_dir(path)?;

        Ok(())
    }

    #[cfg(unix)]
    fn harden_existing_log_files(log_dir: &Path) -> Result<()> {
        use std::os::unix::fs::PermissionsExt;

        for entry in fs::read_dir(log_dir)? {
            let entry = entry?;
            let file_name = entry.file_name();
            if !entry.file_type()?.is_file()
                || !file_name
                    .to_str()
                    .is_some_and(|name| name.starts_with("hooklistener-"))
                || Path::new(&file_name)
                    .extension()
                    .is_none_or(|extension| extension != "log")
            {
                continue;
            }

            // DirEntry::file_type does not follow symlinks. The directory is already
            // owner-only, and chmod is applied through the opened file descriptor.
            let file = File::open(entry.path())?;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
        }

        Ok(())
    }

    fn create_unique_bundle_directory(bundle_path: &Path) -> Result<PathBuf> {
        const MAX_ATTEMPTS: usize = 8;

        fs::create_dir_all(bundle_path)?;
        for _ in 0..MAX_ATTEMPTS {
            let bundle_dir = bundle_path.join(format!(
                "hooklistener-diagnostics-{}-{}",
                Utc::now().format("%Y%m%d-%H%M%S"),
                Uuid::new_v4()
            ));
            match Self::create_private_directory(&bundle_dir) {
                Ok(()) => return Ok(bundle_dir),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error.into()),
            }
        }

        anyhow::bail!("Could not allocate a unique diagnostic bundle directory")
    }

    fn open_private_file(path: &Path) -> std::io::Result<File> {
        let mut options = OpenOptions::new();
        options.write(true).create_new(true);

        #[cfg(unix)]
        {
            use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

            options.mode(0o600);
            let file = options.open(path)?;
            file.set_permissions(fs::Permissions::from_mode(0o600))?;
            Ok(file)
        }

        #[cfg(not(unix))]
        options.open(path)
    }

    fn write_private_file(path: &Path, contents: &[u8]) -> Result<()> {
        let mut file = Self::open_private_file(path)?;
        file.write_all(contents)?;
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
            if !entry.file_type()?.is_file()
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

        Self::write_private_file(dest, &contents)?;
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
        let content = Self::read_regular_file(config_path)?;
        let config: serde_json::Value = serde_json::from_str(&content)?;
        serde_json::from_value::<crate::config::Config>(config.clone())
            .map_err(|_| anyhow::anyhow!("Config unavailable or invalid"))?;
        let (sanitized, tokens) = Self::sanitize_config_with_secrets(config)?;
        Ok((Some(sanitized), tokens))
    }

    fn read_regular_file(path: &Path) -> Result<String> {
        let mut file = File::open(path)?;
        let opened_metadata = file.metadata()?;
        let current_metadata = fs::symlink_metadata(path)?;

        if !opened_metadata.is_file() || !current_metadata.file_type().is_file() {
            anyhow::bail!("Diagnostic config must be a regular file and must not be a symlink");
        }

        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;

            if opened_metadata.dev() != current_metadata.dev()
                || opened_metadata.ino() != current_metadata.ino()
            {
                anyhow::bail!("Diagnostic config changed while it was being opened");
            }
        }

        let mut content = String::new();
        file.read_to_string(&mut content)?;
        Ok(content)
    }

    #[cfg(test)]
    fn create_sanitized_config(config_path: &Path) -> Result<String> {
        let content = fs::read_to_string(config_path)?;
        let config: serde_json::Value = serde_json::from_str(&content)?;
        Self::sanitize_config(config)
    }

    #[cfg(test)]
    fn sanitize_config(mut config: serde_json::Value) -> Result<String> {
        let mut secrets = Vec::new();
        Self::redact_config_value(&mut config, &mut secrets);

        Ok(serde_json::to_string_pretty(&config)?)
    }

    fn sanitize_config_with_secrets(
        mut config: serde_json::Value,
    ) -> Result<(String, Vec<Vec<u8>>)> {
        let mut secrets = Vec::new();
        Self::redact_config_value(&mut config, &mut secrets);
        Ok((serde_json::to_string_pretty(&config)?, secrets))
    }

    fn redact_config_value(value: &mut serde_json::Value, secrets: &mut Vec<Vec<u8>>) {
        match value {
            serde_json::Value::Object(object) => {
                let keys: Vec<_> = object.keys().cloned().collect();
                for key in keys {
                    if Self::is_token_expiry_key(&key) {
                        if let Some(value) = object.get_mut(&key) {
                            *value = serde_json::Value::String("[REDACTED]".to_string());
                        }
                    } else if Self::is_secret_config_key(&key) {
                        if let Some(secret) = object.remove(&key) {
                            Self::collect_secret_values(&secret, secrets);
                        }
                    } else if let Some(value) = object.get_mut(&key) {
                        Self::redact_config_value(value, secrets);
                    }
                }
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    Self::redact_config_value(value, secrets);
                }
            }
            _ => {}
        }
    }

    fn collect_secret_values(value: &serde_json::Value, secrets: &mut Vec<Vec<u8>>) {
        match value {
            serde_json::Value::String(secret) if !secret.is_empty() => {
                secrets.push(secret.as_bytes().to_vec());
            }
            serde_json::Value::Array(values) => {
                for value in values {
                    Self::collect_secret_values(value, secrets);
                }
            }
            serde_json::Value::Object(object) => {
                for value in object.values() {
                    Self::collect_secret_values(value, secrets);
                }
            }
            _ => {}
        }
    }

    fn normalized_config_key(key: &str) -> String {
        key.to_ascii_lowercase().replace(['-', ' '], "_")
    }

    fn is_token_expiry_key(key: &str) -> bool {
        let key = Self::normalized_config_key(key);
        key.contains("token") && (key.contains("expires") || key.contains("expiration"))
    }

    fn is_secret_config_key(key: &str) -> bool {
        let key = Self::normalized_config_key(key);
        key == "token"
            || key == "tokens"
            || key.ends_with("token")
            || key.ends_with("_tokens")
            || key.starts_with("access_token")
            || key.starts_with("refresh_token")
            || key.starts_with("session_token")
            || key.starts_with("auth_token")
            || key.starts_with("bearer_token")
            || key.starts_with("id_token")
            || key == "secret"
            || key.ends_with("_secret")
            || key == "password"
            || key.ends_with("_password")
            || key == "api_key"
            || key.ends_with("_api_key")
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
    fn sanitize_config_recursively_removes_future_secret_fields() {
        let config = serde_json::json!({
            "refreshToken": "refresh-secret",
            "refreshTokenExpiration": "2030-01-01T00:00:00Z",
            "nested": [{
                "session_token": "session-secret",
                "client_secret": "client-secret",
                "api_key": "api-key-secret",
                "token_type": "Bearer"
            }],
            "theme": "dark"
        });

        let sanitized = Logger::sanitize_config(config).unwrap();
        let sanitized: serde_json::Value = serde_json::from_str(&sanitized).unwrap();

        assert_eq!(
            sanitized,
            serde_json::json!({
                "refreshTokenExpiration": "[REDACTED]",
                "nested": [{ "token_type": "Bearer" }],
                "theme": "dark"
            })
        );
    }

    #[test]
    fn read_config_for_bundle_collects_all_removed_secrets_for_log_redaction() {
        use std::collections::BTreeSet;

        let dir = TempDir::new().unwrap();
        let config_path = dir.path().join("config.json");
        fs::write(
            &config_path,
            r#"{
                "access_token": "access-secret",
                "refresh_token": "refresh-secret",
                "nested": { "api_token": "api-secret" }
            }"#,
        )
        .unwrap();

        let (_, secrets) = Logger::read_config_for_bundle(&config_path).unwrap();
        let secrets: BTreeSet<_> = secrets
            .into_iter()
            .map(|secret| String::from_utf8(secret).unwrap())
            .collect();

        assert_eq!(
            secrets,
            BTreeSet::from([
                "access-secret".to_string(),
                "api-secret".to_string(),
                "refresh-secret".to_string(),
            ])
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

    #[cfg(unix)]
    #[test]
    fn read_config_for_bundle_rejects_symlink_without_exposing_target() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let target = dir.path().join("target.json");
        let config_path = dir.path().join("config.json");
        let secret = "symlink-target-secret";
        fs::write(
            &target,
            format!(r#"{{"access_token":"{secret}","leaked":"target contents"}}"#),
        )
        .unwrap();
        symlink(&target, &config_path).unwrap();

        let error = Logger::read_config_for_bundle(&config_path)
            .unwrap_err()
            .to_string();

        assert!(error.contains("must not be a symlink"));
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

    #[cfg(unix)]
    #[test]
    fn ensure_private_directory_sets_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let logs = dir.path().join("logs");
        fs::create_dir(&logs).unwrap();
        fs::set_permissions(&logs, fs::Permissions::from_mode(0o755)).unwrap();

        Logger::ensure_private_directory(&logs).unwrap();

        assert_eq!(
            fs::metadata(logs).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[cfg(unix)]
    #[test]
    fn open_private_file_creates_owner_only_file() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let path = dir.path().join("private.log");

        drop(Logger::open_private_file(&path).unwrap());

        assert_eq!(
            fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn harden_existing_log_files_updates_regular_files_without_following_symlinks() {
        use std::os::unix::fs::{PermissionsExt, symlink};

        let dir = TempDir::new().unwrap();
        let log = dir.path().join("hooklistener-existing.log");
        let victim = dir.path().join("victim");
        let linked_log = dir.path().join("hooklistener-linked.log");
        fs::write(&log, "log").unwrap();
        fs::write(&victim, "victim").unwrap();
        fs::set_permissions(&log, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(&victim, fs::Permissions::from_mode(0o644)).unwrap();
        symlink(&victim, linked_log).unwrap();

        Logger::harden_existing_log_files(dir.path()).unwrap();

        assert_eq!(
            (
                fs::metadata(log).unwrap().permissions().mode() & 0o777,
                fs::metadata(victim).unwrap().permissions().mode() & 0o777,
            ),
            (0o600, 0o644)
        );
    }

    #[cfg(unix)]
    #[test]
    fn write_private_file_refuses_preexisting_symlink() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let victim = dir.path().join("victim");
        let destination = dir.path().join("destination");
        fs::write(&victim, "unchanged").unwrap();
        symlink(&victim, &destination).unwrap();

        let error = Logger::write_private_file(&destination, b"replacement").unwrap_err();

        assert_eq!(
            (
                error
                    .downcast_ref::<std::io::Error>()
                    .map(std::io::Error::kind),
                fs::read_to_string(victim).unwrap(),
            ),
            (
                Some(std::io::ErrorKind::AlreadyExists),
                "unchanged".to_string()
            )
        );
    }

    #[cfg(unix)]
    #[test]
    fn create_unique_bundle_directory_is_private() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();

        let bundle = Logger::create_unique_bundle_directory(dir.path()).unwrap();

        assert_eq!(
            fs::metadata(bundle).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }

    #[test]
    fn create_unique_bundle_directory_uses_distinct_randomized_names() {
        let dir = TempDir::new().unwrap();

        let first = Logger::create_unique_bundle_directory(dir.path()).unwrap();
        let second = Logger::create_unique_bundle_directory(dir.path()).unwrap();

        assert_ne!(first.file_name(), second.file_name());
    }

    #[cfg(unix)]
    #[test]
    fn copy_redacted_log_creates_owner_only_destination() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TempDir::new().unwrap();
        let source = dir.path().join("source.log");
        let destination = dir.path().join("destination.log");
        fs::write(&source, "contents").unwrap();

        Logger::copy_redacted_log(&source, &destination, &[]).unwrap();

        assert_eq!(
            fs::metadata(destination).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[cfg(unix)]
    #[test]
    fn copy_log_files_skips_symlinked_sources() {
        use std::os::unix::fs::symlink;

        let dir = TempDir::new().unwrap();
        let log_dir = dir.path().join("logs");
        let bundle_dir = dir.path().join("bundle");
        fs::create_dir(&log_dir).unwrap();
        fs::create_dir(&bundle_dir).unwrap();
        let victim = dir.path().join("victim");
        fs::write(&victim, "private contents").unwrap();
        symlink(&victim, log_dir.join("hooklistener-linked.log")).unwrap();

        Logger::copy_log_files(&log_dir, &bundle_dir, &[]).unwrap();

        assert_eq!(fs::read_dir(bundle_dir).unwrap().count(), 0);
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
    fn copy_redacted_log_refuses_existing_destination_without_temporary_output() {
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
