use anyhow::Result;
use chrono::Utc;
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
        Self::cleanup_old_logs(&config.directory, config.max_log_files)?;

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

    fn cleanup_old_logs(log_dir: &Path, max_files: usize) -> Result<()> {
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
        log_files.sort_by(|a, b| b.1.cmp(&a.1));

        // Remove old files if we have too many
        if log_files.len() > max_files {
            for (path, _) in log_files.into_iter().skip(max_files) {
                if let Err(e) = fs::remove_file(&path) {
                    warn!(
                        error = %e,
                        file = %path.display(),
                        "Failed to remove old log file"
                    );
                } else {
                    info!(
                        file = %path.display(),
                        "Removed old log file"
                    );
                }
            }
        }

        Ok(())
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

        // Copy recent log files
        let log_dir = dirs::config_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("hooklistener")
            .join("logs");

        if log_dir.exists() {
            let log_bundle_dir = bundle_dir.join("logs");
            fs::create_dir_all(&log_bundle_dir)?;

            let entries = fs::read_dir(&log_dir)?;
            for entry in entries {
                let entry = entry?;
                let path = entry.path();
                if path.is_file()
                    && path
                        .file_name()
                        .unwrap()
                        .to_str()
                        .unwrap()
                        .starts_with("hooklistener-")
                {
                    let dest = log_bundle_dir.join(path.file_name().unwrap());
                    if let Err(e) = fs::copy(&path, &dest) {
                        warn!(
                            error = %e,
                            source = %path.display(),
                            dest = %dest.display(),
                            "Failed to copy log file to diagnostic bundle"
                        );
                    }
                }
            }
        }

        // Copy sanitized config
        let config_path = crate::config::Config::config_path()?;
        if config_path.exists() {
            let sanitized_config = self.create_sanitized_config(&config_path)?;
            let config_bundle_path = bundle_dir.join("config.json");
            fs::write(config_bundle_path, sanitized_config)?;
        }

        // Create system info file
        let system_info = self.collect_system_info();
        let system_info_path = bundle_dir.join("system_info.json");
        fs::write(
            system_info_path,
            serde_json::to_string_pretty(&system_info)?,
        )?;

        info!(
            session_id = %self.session_id,
            bundle_dir = %bundle_dir.display(),
            "Diagnostic bundle created successfully"
        );

        Ok(())
    }

    fn create_sanitized_config(&self, config_path: &Path) -> Result<String> {
        let content = fs::read_to_string(config_path)?;
        let mut config: serde_json::Value = serde_json::from_str(&content)?;

        // Remove sensitive data
        if let Some(obj) = config.as_object_mut() {
            obj.remove("access_token");
            if let Some(token_expires) = obj.get_mut("token_expires_at")
                && token_expires.is_string()
            {
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

// Request ID generator for correlation
pub fn generate_request_id() -> String {
    Uuid::new_v4().to_string()[..8].to_string()
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
