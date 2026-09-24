//! Regression tests for the 2026-09 security audit findings.
//! Run with: cargo test audit_findings
use super::*;
use crate::credentials::*;
use chrono::Duration as ChronoDuration;
use chrono::Utc;

fn mock_refresh(server: &mut mockito::ServerGuard, body: &str) -> mockito::Mock {
    server
        .mock("POST", "/api/v1/auth/refresh")
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(body)
        .create()
}

fn config_with_refresh_token() -> config::Config {
    config::Config {
        access_token: Some("old-access".to_string()),
        token_expires_at: Some(Utc::now() - ChronoDuration::minutes(1)),
        refresh_token: Some("refresh-1".to_string()),
        refresh_token_expires_at: Some(Utc::now() + ChronoDuration::days(1)),
        selected_organization_id: Some("org-a".to_string()),
        ..config::Config::default()
    }
}

/// Regression test for finding 3: the background refresh loop (and
/// `ensure_valid_token`) used to rebuild the whole config from the copy
/// loaded at process start and write it back, silently reverting anything
/// another process saved in the meantime (`org use`, `login --force`,
/// `config set`). The refresh must now only touch the fields it owns.
#[tokio::test]
async fn token_refresh_preserves_config_changes_made_by_other_processes() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("config.json");
    let mut server = mockito::Server::new_async().await;
    let _mock = mock_refresh(
        &mut server,
        r#"{"access_token":"new-access","expires_in":3600}"#,
    );

    // Long-running process loads its config at startup.
    let mut in_memory = config_with_refresh_token();
    in_memory.save_to(&path).unwrap();

    // Meanwhile another process changes the selected organization and
    // rotates the credentials via `hooklistener login --force`.
    let mut other_process = config::Config::load_from(&path).unwrap();
    other_process.selected_organization_id = Some("org-b".to_string());
    other_process.set_tokens(
        "relogin-access".to_string(),
        Utc::now() + ChronoDuration::hours(1),
        Some("refresh-2".to_string()),
        Some(Utc::now() + ChronoDuration::days(30)),
    );
    other_process.save_to(&path).unwrap();

    refresh_access_token_from_config_with(&mut in_memory, &server.url(), Some(&path))
        .await
        .unwrap();

    let on_disk = config::Config::load_from(&path).unwrap();
    assert_eq!(on_disk.selected_organization_id.as_deref(), Some("org-b"));
    assert_eq!(on_disk.refresh_token.as_deref(), Some("refresh-2"));
}

/// Regression test for the login-path instance of finding 3:
/// `run_login_flow` used to save the config copy it loaded before the
/// minutes-long device-authorization wait, reverting anything another
/// process (`org use`, `config set`, an update check) saved meanwhile.
/// The login must now write only the token fields on top of what is on
/// disk.
#[test]
fn login_preserves_config_changes_made_by_other_processes() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("config.json");

    // `hooklistener login` loads its config before waiting on the browser.
    let loaded = config::Config {
        selected_organization_id: Some("org-a".to_string()),
        ..config::Config::default()
    };
    loaded.save_to(&path).unwrap();

    // Meanwhile another process switches organizations and records an
    // update check.
    let mut other_process = config::Config::load_from(&path).unwrap();
    other_process.selected_organization_id = Some("org-b".to_string());
    other_process.latest_known_version = Some("9.9.9".to_string());
    other_process.save_to(&path).unwrap();

    save_login_tokens(
        &loaded,
        "new-access".to_string(),
        Utc::now() + ChronoDuration::hours(1),
        Some("new-refresh".to_string()),
        Some(Utc::now() + ChronoDuration::days(30)),
        Some(&path),
    )
    .unwrap();

    let on_disk = config::Config::load_from(&path).unwrap();
    assert_eq!(on_disk.selected_organization_id.as_deref(), Some("org-b"));
    assert_eq!(on_disk.latest_known_version.as_deref(), Some("9.9.9"));
    assert_eq!(on_disk.access_token.as_deref(), Some("new-access"));
    assert_eq!(on_disk.refresh_token.as_deref(), Some("new-refresh"));
}

#[test]
fn login_saves_tokens_when_config_file_was_removed() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("config.json");
    let loaded = config::Config {
        selected_organization_id: Some("org-a".to_string()),
        ..config::Config::default()
    };

    save_login_tokens(
        &loaded,
        "new-access".to_string(),
        Utc::now() + ChronoDuration::hours(1),
        None,
        None,
        Some(&path),
    )
    .unwrap();

    let on_disk = config::Config::load_from(&path).unwrap();
    assert_eq!(on_disk.selected_organization_id.as_deref(), Some("org-a"));
    assert_eq!(on_disk.access_token.as_deref(), Some("new-access"));
}

/// Regression test for finding 7 (second site):
/// `refresh_access_token_from_config_with` used to cast the
/// server-supplied `expires_in` with `as i64` and add it to now, which
/// panicked for large values. It must now fail with a clean error.
#[tokio::test]
async fn token_refresh_survives_absurd_expires_in() {
    let dir = tempfile::TempDir::new().unwrap();
    let path = dir.path().join("config.json");
    let mut server = mockito::Server::new_async().await;
    let _mock = mock_refresh(
        &mut server,
        r#"{"access_token":"new-access","expires_in":9000000000000000000}"#,
    );
    let mut in_memory = config_with_refresh_token();
    let base_url = server.url();

    let result = tokio::task::spawn(async move {
        refresh_access_token_from_config_with(&mut in_memory, &base_url, Some(&path)).await
    })
    .await
    .expect("token refresh must not panic");

    let _ = result;
}
