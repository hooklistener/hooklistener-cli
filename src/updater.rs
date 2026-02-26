use anyhow::Result;
use chrono::Utc;
use crossterm::style::Stylize;
use serde::Deserialize;
use std::fmt;
use std::time::Duration;
use tokio::task::JoinHandle;

use crate::config::Config;
use crate::errors::UpdateError;

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const GITHUB_REPO_OWNER: &str = "hooklistener";
const GITHUB_REPO_NAME: &str = "hooklistener-cli";
const CHECK_INTERVAL_HOURS: i64 = 24;
const REQUEST_TIMEOUT_SECS: u64 = 5;

#[derive(Debug)]
enum InstallMethod {
    Homebrew,
    Npm,
    Cargo,
    DirectBinary,
}

impl fmt::Display for InstallMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            InstallMethod::Homebrew => write!(f, "homebrew"),
            InstallMethod::Npm => write!(f, "npm"),
            InstallMethod::Cargo => write!(f, "cargo"),
            InstallMethod::DirectBinary => write!(f, "binary"),
        }
    }
}

impl InstallMethod {
    fn detect() -> Self {
        let exe_path = std::env::current_exe()
            .ok()
            .and_then(|p| p.canonicalize().ok())
            .map(|p| p.to_string_lossy().to_string())
            .unwrap_or_default();

        if exe_path.contains("/Cellar/") || exe_path.contains("/homebrew/") {
            InstallMethod::Homebrew
        } else if exe_path.contains("node_modules") || exe_path.contains("/npm/") {
            InstallMethod::Npm
        } else if exe_path.contains(".cargo/bin/")
            || exe_path.contains("/target/debug/")
            || exe_path.contains("/target/release/")
        {
            InstallMethod::Cargo
        } else {
            InstallMethod::DirectBinary
        }
    }

    fn upgrade_command(&self) -> &str {
        match self {
            InstallMethod::Homebrew => "brew upgrade hooklistener",
            InstallMethod::Npm => "npm update -g hooklistener-cli",
            InstallMethod::Cargo => "cargo install hooklistener-cli",
            InstallMethod::DirectBinary => "hooklistener update",
        }
    }
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
}

fn normalize_version(tag: &str) -> &str {
    tag.strip_prefix('v').unwrap_or(tag)
}

fn is_newer(remote: &str, current: &str) -> bool {
    let parse = |s: &str| -> Vec<u64> {
        s.split('.')
            .filter_map(|part| part.parse::<u64>().ok())
            .collect()
    };
    let r = parse(remote);
    let c = parse(current);
    r > c
}

/// Spawns a background task that checks for a new version.
/// Returns the JoinHandle so the caller can await it with a timeout.
pub fn spawn_version_check(config: &Config) -> Option<JoinHandle<Option<String>>> {
    // If we checked recently, use cached result
    if let Some(last_check) = config.last_update_check {
        let elapsed = Utc::now().signed_duration_since(last_check);
        if elapsed.num_hours() < CHECK_INTERVAL_HOURS {
            // Return cached version if it's newer
            if let Some(ref cached) = config.latest_known_version
                && is_newer(cached, CURRENT_VERSION)
            {
                let cached = cached.clone();
                return Some(tokio::spawn(async move { Some(cached) }));
            }
            return None;
        }
    }

    Some(tokio::spawn(async move {
        check_latest_version().await.ok().flatten()
    }))
}

async fn check_latest_version() -> Result<Option<String>, UpdateError> {
    let url = format!(
        "https://api.github.com/repos/{}/{}/releases/latest",
        GITHUB_REPO_OWNER, GITHUB_REPO_NAME
    );

    let client = reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .user_agent(format!("hooklistener-cli/{}", CURRENT_VERSION))
        .build()
        .map_err(|e| UpdateError::CheckFailed(e.to_string()))?;

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|e| UpdateError::CheckFailed(e.to_string()))?;

    if !response.status().is_success() {
        return Err(UpdateError::CheckFailed(format!(
            "GitHub API returned {}",
            response.status()
        )));
    }

    let release: GitHubRelease = response
        .json()
        .await
        .map_err(|e| UpdateError::CheckFailed(e.to_string()))?;

    let remote_version = normalize_version(&release.tag_name).to_string();

    if is_newer(&remote_version, CURRENT_VERSION) {
        Ok(Some(remote_version))
    } else {
        Ok(None)
    }
}

/// Persist the version check result to config. Silently ignores save errors.
pub fn persist_check_result(latest_version: Option<&str>) {
    if let Ok(mut config) = Config::load() {
        config.last_update_check = Some(Utc::now());
        config.latest_known_version = latest_version.map(String::from);
        let _ = config.save();
    }
}

/// Print an update notification to stderr (won't interfere with --json stdout).
pub fn print_update_notification(new_version: &str) {
    let method = InstallMethod::detect();
    eprintln!();
    eprintln!(
        "{} A new version of hooklistener is available: {} -> {}",
        "Update available!".yellow().bold(),
        CURRENT_VERSION.dim(),
        new_version.green().bold()
    );
    eprintln!("  Run {} to update.", method.upgrade_command().bold());
    eprintln!();
}

/// Run the self-update command.
pub async fn run_self_update(json: bool) -> Result<()> {
    let method = InstallMethod::detect();

    match method {
        InstallMethod::Homebrew | InstallMethod::Npm | InstallMethod::Cargo => {
            let cmd = method.upgrade_command();
            if json {
                crate::print_json(&serde_json::json!({
                    "status": "manual_update_required",
                    "install_method": method.to_string(),
                    "command": cmd,
                    "current_version": CURRENT_VERSION,
                }))?;
            } else {
                println!(
                    "Installed via {}. Update with:\n\n  {}\n",
                    method.to_string().bold(),
                    cmd.green().bold()
                );
            }
            Ok(())
        }
        InstallMethod::DirectBinary => run_binary_self_update(json).await,
    }
}

async fn run_binary_self_update(json: bool) -> Result<()> {
    if !json {
        println!("{} Checking for updates...", "Updating:".bold());
    }

    let status = tokio::task::spawn_blocking(move || {
        self_update::backends::github::Update::configure()
            .repo_owner(GITHUB_REPO_OWNER)
            .repo_name(GITHUB_REPO_NAME)
            .bin_name("hooklistener")
            .show_download_progress(!json)
            .current_version(CURRENT_VERSION)
            .build()
            .map_err(|e| UpdateError::UpdateFailed(e.to_string()))?
            .update()
            .map_err(|e| UpdateError::UpdateFailed(e.to_string()))
    })
    .await
    .map_err(|e| UpdateError::UpdateFailed(e.to_string()))??;

    let new_version = normalize_version(status.version());

    // Persist the fact that we're now up to date
    persist_check_result(None);

    if json {
        crate::print_json(&serde_json::json!({
            "status": if status.updated() { "updated" } else { "up_to_date" },
            "current_version": CURRENT_VERSION,
            "latest_version": new_version,
        }))?;
    } else if status.updated() {
        println!(
            "\n{} Updated to version {}",
            "Success!".green().bold(),
            new_version.bold()
        );
    } else {
        println!(
            "\n{} Already on the latest version ({})",
            "Up to date.".green().bold(),
            CURRENT_VERSION
        );
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_newer() {
        assert!(is_newer("0.2.0", "0.1.2"));
        assert!(is_newer("1.0.0", "0.9.9"));
        assert!(is_newer("0.1.3", "0.1.2"));
        assert!(!is_newer("0.1.2", "0.1.2"));
        assert!(!is_newer("0.1.1", "0.1.2"));
    }

    #[test]
    fn test_normalize_version() {
        assert_eq!(normalize_version("v1.2.3"), "1.2.3");
        assert_eq!(normalize_version("1.2.3"), "1.2.3");
    }

    #[test]
    fn test_install_method_display() {
        assert_eq!(InstallMethod::Homebrew.to_string(), "homebrew");
        assert_eq!(InstallMethod::Npm.to_string(), "npm");
        assert_eq!(InstallMethod::Cargo.to_string(), "cargo");
        assert_eq!(InstallMethod::DirectBinary.to_string(), "binary");
    }

    #[test]
    fn test_install_method_upgrade_command() {
        assert_eq!(
            InstallMethod::Homebrew.upgrade_command(),
            "brew upgrade hooklistener"
        );
        assert_eq!(
            InstallMethod::Npm.upgrade_command(),
            "npm update -g hooklistener-cli"
        );
        assert_eq!(
            InstallMethod::Cargo.upgrade_command(),
            "cargo install hooklistener-cli"
        );
        assert_eq!(
            InstallMethod::DirectBinary.upgrade_command(),
            "hooklistener update"
        );
    }
}
