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
const CHECKSUMS_ASSET_NAME: &str = "SHA256SUMS.txt";
const MAX_CHECKSUM_MANIFEST_BYTES: usize = 64 * 1024;

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
            InstallMethod::Npm => "npm update -g hooklistener",
            InstallMethod::Cargo => "cargo install hooklistener-cli",
            InstallMethod::DirectBinary => "hooklistener update",
        }
    }
}

#[derive(Debug, Deserialize)]
struct GitHubRelease {
    tag_name: String,
    #[serde(default)]
    assets: Vec<GitHubAsset>,
}

#[derive(Debug, Deserialize)]
struct GitHubAsset {
    name: String,
    browser_download_url: String,
}

#[derive(Debug)]
struct PreparedBinaryUpdate {
    release_tag: String,
    archive_name: String,
    checksum: String,
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
    check_latest_version_from("https://api.github.com").await
}

async fn check_latest_version_from(base_url: &str) -> Result<Option<String>, UpdateError> {
    let release = fetch_latest_release_from(base_url)
        .await
        .map_err(UpdateError::CheckFailed)?;
    let remote_version = normalize_version(&release.tag_name).to_string();

    if is_newer(&remote_version, CURRENT_VERSION) {
        Ok(Some(remote_version))
    } else {
        Ok(None)
    }
}

fn github_client() -> Result<reqwest::Client, String> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(REQUEST_TIMEOUT_SECS))
        .user_agent(format!("hooklistener-cli/{CURRENT_VERSION}"))
        .redirect(reqwest::redirect::Policy::custom(|attempt| {
            if attempt.url().scheme() == "https" {
                attempt.follow()
            } else {
                attempt.error("refusing a non-HTTPS release redirect")
            }
        }))
        .build()
        .map_err(|error| error.to_string())
}

async fn fetch_latest_release_from(base_url: &str) -> Result<GitHubRelease, String> {
    let url = format!(
        "{}/repos/{}/{}/releases/latest",
        base_url.trim_end_matches('/'),
        GITHUB_REPO_OWNER,
        GITHUB_REPO_NAME
    );

    let client = github_client()?;

    let response = client
        .get(&url)
        .send()
        .await
        .map_err(|error| error.to_string())?;

    if !response.status().is_success() {
        return Err(format!("GitHub API returned {}", response.status()));
    }

    response.json().await.map_err(|error| error.to_string())
}

fn current_archive_name() -> String {
    let extension = if cfg!(windows) { "zip" } else { "tar.gz" };
    format!(
        "hooklistener{}-{}.{}",
        std::env::consts::EXE_SUFFIX,
        self_update::get_target(),
        extension
    )
}

fn find_unique_asset<'a>(
    release: &'a GitHubRelease,
    asset_name: &str,
) -> Result<&'a GitHubAsset, String> {
    let mut matching_assets = release
        .assets
        .iter()
        .filter(|asset| asset.name == asset_name);
    let asset = matching_assets
        .next()
        .ok_or_else(|| format!("release is missing required asset {asset_name}"))?;

    if matching_assets.next().is_some() {
        return Err(format!(
            "release contains duplicate assets named {asset_name}"
        ));
    }

    Ok(asset)
}

fn parse_checksum_manifest(manifest: &str, archive_name: &str) -> Result<String, String> {
    let mut expected_checksum = None;

    for line in manifest.lines() {
        let mut fields = line.split_whitespace();
        let Some(checksum) = fields.next() else {
            continue;
        };
        let Some(listed_name) = fields.next() else {
            continue;
        };
        let listed_name = listed_name.strip_prefix('*').unwrap_or(listed_name);

        if listed_name != archive_name {
            continue;
        }

        if fields.next().is_some()
            || checksum.len() != 64
            || !checksum.bytes().all(|byte| byte.is_ascii_hexdigit())
        {
            return Err(format!(
                "checksum manifest has a malformed entry for {archive_name}"
            ));
        }

        if expected_checksum.is_some() {
            return Err(format!(
                "checksum manifest has duplicate entries for {archive_name}"
            ));
        }

        expected_checksum = Some(checksum.to_ascii_lowercase());
    }

    expected_checksum
        .ok_or_else(|| format!("checksum manifest is missing an entry for {archive_name}"))
}

async fn download_checksum_manifest(url: &str) -> Result<String, String> {
    let parsed_url =
        reqwest::Url::parse(url).map_err(|error| format!("invalid checksum asset URL: {error}"))?;
    if parsed_url.scheme() != "https" {
        return Err("checksum asset URL must use HTTPS".to_string());
    }

    let client = github_client()?;
    let mut response = client
        .get(parsed_url)
        .send()
        .await
        .map_err(|error| format!("failed to download checksum manifest: {error}"))?;

    if !response.status().is_success() {
        return Err(format!(
            "checksum manifest download returned {}",
            response.status()
        ));
    }

    if response
        .content_length()
        .is_some_and(|length| length > MAX_CHECKSUM_MANIFEST_BYTES as u64)
    {
        return Err("checksum manifest exceeds the size limit".to_string());
    }

    let mut manifest_bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| format!("failed to read checksum manifest: {error}"))?
    {
        if manifest_bytes.len().saturating_add(chunk.len()) > MAX_CHECKSUM_MANIFEST_BYTES {
            return Err("checksum manifest exceeds the size limit".to_string());
        }
        manifest_bytes.extend_from_slice(&chunk);
    }

    String::from_utf8(manifest_bytes)
        .map_err(|_| "checksum manifest is not valid UTF-8".to_string())
}

async fn prepare_binary_update() -> Result<Option<PreparedBinaryUpdate>, UpdateError> {
    let release = fetch_latest_release_from("https://api.github.com")
        .await
        .map_err(UpdateError::UpdateFailed)?;
    let version = normalize_version(&release.tag_name).to_string();
    let update_available = self_update::version::bump_is_greater(CURRENT_VERSION, &version)
        .map_err(|error| {
            UpdateError::UpdateFailed(format!("release has an invalid version: {error}"))
        })?;

    if !update_available {
        return Ok(None);
    }

    let archive_name = current_archive_name();
    find_unique_asset(&release, &archive_name).map_err(UpdateError::UpdateFailed)?;
    let checksums_asset =
        find_unique_asset(&release, CHECKSUMS_ASSET_NAME).map_err(UpdateError::UpdateFailed)?;
    let manifest = download_checksum_manifest(&checksums_asset.browser_download_url)
        .await
        .map_err(UpdateError::UpdateFailed)?;
    let checksum =
        parse_checksum_manifest(&manifest, &archive_name).map_err(UpdateError::UpdateFailed)?;

    Ok(Some(PreparedBinaryUpdate {
        release_tag: release.tag_name,
        archive_name,
        checksum,
    }))
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
    let notification = render_update_notification(CURRENT_VERSION, new_version, &method);
    eprint!("{notification}");
}

fn render_update_notification(
    current_version: &str,
    new_version: &str,
    method: &InstallMethod,
) -> String {
    let status = crate::format_status_line(crate::OutputStatus::Info, "UPDATE AVAILABLE")
        .yellow()
        .bold();
    let current = crate::format_field_line("CURRENT", current_version.dim());
    let latest = crate::format_field_line("LATEST", new_version.green().bold());
    let action = crate::format_field_line(
        "ACTION",
        format!("Run {} to update.", method.upgrade_command().bold()),
    );

    format!("\n{}\n\n{}\n{}\n{}\n\n", status, current, latest, action)
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
                    "{}",
                    crate::format_status_line(crate::OutputStatus::Info, "MANUAL UPDATE REQUIRED")
                        .yellow()
                        .bold()
                );
                println!();
                println!(
                    "{}",
                    crate::format_field_line("METHOD", method.to_string().bold())
                );
                println!(
                    "{}",
                    crate::format_field_line("COMMAND", cmd.green().bold())
                );
            }
            Ok(())
        }
        InstallMethod::DirectBinary => run_binary_self_update(json).await,
    }
}

async fn run_binary_self_update(json: bool) -> Result<()> {
    if !json {
        println!(
            "{}",
            crate::format_status_line(crate::OutputStatus::Info, "CHECKING FOR UPDATES").bold()
        );
    }

    let status = match prepare_binary_update().await? {
        None => self_update::VersionStatus::UpToDate(CURRENT_VERSION.to_string()),
        Some(prepared) => {
            let PreparedBinaryUpdate {
                release_tag,
                archive_name,
                checksum,
            } = prepared;
            let archive_name_for_matcher = archive_name;

            tokio::task::spawn_blocking(move || {
                self_update::backends::github::Update::configure()
                    .repo_owner(GITHUB_REPO_OWNER)
                    .repo_name(GITHUB_REPO_NAME)
                    .bin_name("hooklistener")
                    .release_tag(release_tag)
                    .asset_matcher(move |assets| {
                        assets
                            .iter()
                            .find(|asset| asset.name() == archive_name_for_matcher)
                            .cloned()
                    })
                    .verify_checksum(self_update::Checksum::Sha256(checksum))
                    .show_download_progress(!json)
                    .show_output(!json)
                    .no_confirm(json)
                    .current_version(CURRENT_VERSION)
                    .build()
                    .map_err(|error| UpdateError::UpdateFailed(error.to_string()))?
                    .update()
                    .map_err(|error| UpdateError::UpdateFailed(error.to_string()))
            })
            .await
            .map_err(|error| UpdateError::UpdateFailed(error.to_string()))??
        }
    };

    let new_version = normalize_version(status.version());

    // Persist the fact that we're now up to date
    persist_check_result(None);

    if json {
        crate::print_json(&serde_json::json!({
            "status": if status.is_updated() { "updated" } else { "up_to_date" },
            "current_version": CURRENT_VERSION,
            "latest_version": new_version,
        }))?;
    } else if status.is_updated() {
        println!(
            "\n{}",
            crate::format_status_line(crate::OutputStatus::Ok, "UPDATED")
                .green()
                .bold(),
        );
        println!();
        println!(
            "{}",
            crate::format_field_line("VERSION", new_version.bold())
        );
    } else {
        println!(
            "\n{}",
            crate::format_status_line(crate::OutputStatus::Ok, "UP TO DATE")
                .green()
                .bold(),
        );
        println!();
        println!("{}", crate::format_field_line("VERSION", CURRENT_VERSION));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const RELEASE_PATH: &str = "/repos/hooklistener/hooklistener-cli/releases/latest";

    async fn mock_release(status: usize, body: &str) -> (mockito::ServerGuard, mockito::Mock) {
        let mut server = mockito::Server::new_async().await;
        let mock = server
            .mock("GET", RELEASE_PATH)
            .with_status(status)
            .with_header("content-type", "application/json")
            .with_body(body)
            .create_async()
            .await;
        (server, mock)
    }

    #[tokio::test]
    async fn check_latest_version_returns_newer_release() {
        let (server, mock) = mock_release(200, r#"{"tag_name":"v999.0.0"}"#).await;

        let result = check_latest_version_from(&server.url()).await;

        assert_eq!(result.unwrap(), Some("999.0.0".to_string()));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn check_latest_version_returns_none_for_equal_release() {
        let body = format!(r#"{{"tag_name":"v{CURRENT_VERSION}"}}"#);
        let (server, mock) = mock_release(200, &body).await;

        let result = check_latest_version_from(&server.url()).await;

        assert_eq!(result.unwrap(), None);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn check_latest_version_returns_none_for_older_release() {
        let (server, mock) = mock_release(200, r#"{"tag_name":"v0.0.1"}"#).await;

        let result = check_latest_version_from(&server.url()).await;

        assert_eq!(result.unwrap(), None);
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn check_latest_version_returns_typed_error_for_non_success_status() {
        let (server, mock) = mock_release(503, r#"{"message":"unavailable"}"#).await;

        let result = check_latest_version_from(&server.url()).await;

        assert!(matches!(
            result,
            Err(UpdateError::CheckFailed(message)) if message.contains("503 Service Unavailable")
        ));
        mock.assert_async().await;
    }

    #[tokio::test]
    async fn check_latest_version_returns_typed_error_for_malformed_json() {
        let (server, mock) = mock_release(200, "not JSON").await;

        let result = check_latest_version_from(&server.url()).await;

        assert!(matches!(result, Err(UpdateError::CheckFailed(_))));
        mock.assert_async().await;
    }

    #[test]
    fn parse_checksum_manifest_returns_exact_archive_entry() {
        let archive_name = "hooklistener-x86_64-unknown-linux-gnu.tar.gz";
        let expected = "a".repeat(64);
        let manifest = format!(
            "{}  {}.sig\n{}  {}\n",
            "b".repeat(64),
            archive_name,
            expected.to_uppercase(),
            archive_name
        );

        let checksum = parse_checksum_manifest(&manifest, archive_name).unwrap();

        assert_eq!(checksum, expected);
    }

    #[test]
    fn parse_checksum_manifest_rejects_missing_archive_entry() {
        let manifest = format!("{}  another-archive.tar.gz\n", "a".repeat(64));

        let error = parse_checksum_manifest(&manifest, "hooklistener.tar.gz").unwrap_err();

        assert!(
            error.contains("missing an entry"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn parse_checksum_manifest_rejects_malformed_digest() {
        let archive_name = "hooklistener.tar.gz";
        let manifest = format!("not-a-sha256  {archive_name}\n");

        let error = parse_checksum_manifest(&manifest, archive_name).unwrap_err();

        assert!(
            error.contains("malformed entry"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn parse_checksum_manifest_rejects_duplicate_archive_entries() {
        let archive_name = "hooklistener.tar.gz";
        let manifest = format!(
            "{}  {archive_name}\n{}  {archive_name}\n",
            "a".repeat(64),
            "b".repeat(64)
        );

        let error = parse_checksum_manifest(&manifest, archive_name).unwrap_err();

        assert!(
            error.contains("duplicate entries"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn find_unique_asset_rejects_duplicate_exact_names() {
        let release = GitHubRelease {
            tag_name: "v2.0.0".to_string(),
            assets: vec![
                GitHubAsset {
                    name: CHECKSUMS_ASSET_NAME.to_string(),
                    browser_download_url: "https://example.com/one".to_string(),
                },
                GitHubAsset {
                    name: CHECKSUMS_ASSET_NAME.to_string(),
                    browser_download_url: "https://example.com/two".to_string(),
                },
            ],
        };

        let error = find_unique_asset(&release, CHECKSUMS_ASSET_NAME).unwrap_err();

        assert!(
            error.contains("duplicate assets"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn download_checksum_manifest_rejects_plain_http() {
        let error = download_checksum_manifest("http://example.com/SHA256SUMS.txt")
            .await
            .unwrap_err();

        assert!(
            error.contains("must use HTTPS"),
            "unexpected error: {error}"
        );
    }

    #[tokio::test]
    async fn recent_newer_cached_version_is_returned_without_network_check() {
        let config = Config {
            last_update_check: Some(Utc::now()),
            latest_known_version: Some("999.0.0".to_string()),
            ..Config::default()
        };

        let result = spawn_version_check(&config).unwrap().await.unwrap();

        assert_eq!(result, Some("999.0.0".to_string()));
    }

    #[test]
    fn recent_non_newer_cached_version_skips_check() {
        let config = Config {
            last_update_check: Some(Utc::now()),
            latest_known_version: Some(CURRENT_VERSION.to_string()),
            ..Config::default()
        };

        assert!(spawn_version_check(&config).is_none());
    }

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
            "npm update -g hooklistener"
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

    #[test]
    fn update_notification_snapshot() {
        let output = render_update_notification("1.2.3", "1.4.0", &InstallMethod::Cargo);

        assert!(output.starts_with('\n'));
        assert!(output.ends_with("\n\n"));
        insta::assert_snapshot!(strip_ansi_sequences(&output));
    }

    fn strip_ansi_sequences(input: &str) -> String {
        let mut stripped = String::with_capacity(input.len());
        let mut chars = input.chars().peekable();

        while let Some(ch) = chars.next() {
            if ch == '\x1b' && chars.peek() == Some(&'[') {
                chars.next();
                for ch in chars.by_ref() {
                    if ch.is_ascii_alphabetic() {
                        break;
                    }
                }
            } else {
                stripped.push(ch);
            }
        }

        stripped
    }
}
