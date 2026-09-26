//! Access-token refresh, persistence, and organization resolution.

use super::*;

fn make_config(selected_org: Option<&str>) -> config::Config {
    config::Config {
        selected_organization_id: selected_org.map(String::from),
        ..config::Config::default()
    }
}

fn refreshable_config() -> config::Config {
    config::Config {
        access_token: Some("old-token".into()),
        token_expires_at: Some(Utc::now() + ChronoDuration::minutes(5)),
        refresh_token: Some("refresh-token".into()),
        refresh_token_expires_at: Some(Utc::now() + ChronoDuration::hours(1)),
        ..config::Config::default()
    }
}

#[test]
fn access_token_refresh_delay_applies_skew_and_clamps_expired_tokens() {
    let mut config = refreshable_config();
    config.token_expires_at = Some(Utc::now() + ChronoDuration::seconds(90));
    let delay = access_token_refresh_delay(&config);
    assert!((29..=30).contains(&delay.as_secs()));

    config.token_expires_at = Some(Utc::now() - ChronoDuration::seconds(1));
    assert_eq!(access_token_refresh_delay(&config), Duration::ZERO);
}

#[tokio::test]
async fn refresh_loop_terminates_when_receiver_is_closed() {
    let config = refreshable_config();
    let (tx, rx) = watch::channel("old-token".to_string());
    let refresh_loop = tokio::spawn(refresh_access_token_loop(config, tx));
    tokio::task::yield_now().await;
    drop(rx);
    tokio::time::timeout(Duration::from_secs(1), refresh_loop)
        .await
        .expect("loop should interrupt its refresh sleep")
        .unwrap();
}

#[tokio::test]
async fn refresh_loop_cancels_hanging_request_when_receiver_is_closed() {
    let listener = tokio::net::TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
        .await
        .unwrap();
    let base_url = format!("http://{}", listener.local_addr().unwrap());
    let (accepted_tx, accepted_rx) = tokio::sync::oneshot::channel();
    let server = tokio::spawn(async move {
        let (connection, _) = listener.accept().await.unwrap();
        accepted_tx.send(()).unwrap();
        let _connection = connection;
        std::future::pending::<()>().await;
    });
    let mut config = refreshable_config();
    config.token_expires_at = Some(Utc::now() - ChronoDuration::seconds(1));
    let (tx, rx) = watch::channel("old-token".to_string());
    let refresh_loop = tokio::spawn(async move {
        refresh_access_token_loop_with(config, tx, &base_url).await;
    });

    tokio::time::timeout(Duration::from_secs(5), accepted_rx)
        .await
        .expect("refresh request should reach server")
        .unwrap();
    drop(rx);
    tokio::time::timeout(Duration::from_secs(5), refresh_loop)
        .await
        .expect("loop should cancel its in-flight refresh request")
        .unwrap();
    server.abort();
    server.await.unwrap_err();
}

#[tokio::test]
async fn refresh_persists_before_returning_new_token() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("POST", "/api/v1/auth/refresh")
        .with_status(200)
        .with_body(r#"{"access_token":"new-token","expires_in":3600}"#)
        .create_async()
        .await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut config = refreshable_config();
    let token = refresh_access_token_from_config_with(&mut config, &server.url(), Some(&path))
        .await
        .unwrap();
    mock.assert_async().await;
    let saved = config::Config::load_from(&path).unwrap();
    assert_eq!(
        (token.as_str(), saved.access_token.as_deref()),
        ("new-token", Some("new-token"))
    );
}

#[tokio::test]
async fn expired_refresh_token_does_not_call_server() {
    let mut config = refreshable_config();
    config.refresh_token_expires_at = Some(Utc::now() - ChronoDuration::seconds(1));
    let err = refresh_access_token_from_config_with(&mut config, "http://unused", None)
        .await
        .unwrap_err();
    assert!(err.to_string().contains("Refresh token expired"));
}

#[tokio::test]
async fn save_failure_does_not_publish_or_mutate_new_token() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("POST", "/api/v1/auth/refresh")
        .with_status(200)
        .with_body(r#"{"access_token":"must-not-publish","expires_in":3600}"#)
        .create_async()
        .await;
    let dir = tempfile::tempdir().unwrap();
    let mut config = refreshable_config();
    let err = refresh_access_token_from_config_with(&mut config, &server.url(), Some(dir.path()))
        .await
        .unwrap_err();
    assert!(!err.to_string().is_empty());
    assert_eq!(config.access_token.as_deref(), Some("old-token"));
}

async fn mock_refresh_success(server: &mut mockito::Server) -> mockito::Mock {
    server
        .mock("POST", "/api/v1/auth/refresh")
        .with_status(200)
        .with_body(r#"{"access_token":"new-token","expires_in":3600}"#)
        .create_async()
        .await
}

#[tokio::test]
async fn refresh_preserves_organization_selected_by_another_process() {
    let mut server = mockito::Server::new_async().await;
    mock_refresh_success(&mut server).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut config = refreshable_config();
    config.selected_organization_id = Some("org-a".into());
    config.save_to(&path).unwrap();

    let mut other_process = config::Config::load_from(&path).unwrap();
    other_process.selected_organization_id = Some("org-b".into());
    other_process.save_to(&path).unwrap();

    refresh_access_token_from_config_with(&mut config, &server.url(), Some(&path))
        .await
        .unwrap();

    let saved = config::Config::load_from(&path).unwrap();
    assert_eq!(saved.selected_organization_id.as_deref(), Some("org-b"));
    assert_eq!(saved.access_token.as_deref(), Some("new-token"));
    assert_eq!(config.selected_organization_id.as_deref(), Some("org-b"));
    assert_eq!(config.access_token.as_deref(), Some("new-token"));
}

#[tokio::test]
async fn refresh_preserves_refresh_token_rotated_by_another_process() {
    let mut server = mockito::Server::new_async().await;
    mock_refresh_success(&mut server).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut config = refreshable_config();
    config.save_to(&path).unwrap();

    let rotated_expiry = Utc::now() + ChronoDuration::days(30);
    let mut other_process = config::Config::load_from(&path).unwrap();
    other_process.set_tokens(
        "relogin-token".into(),
        Utc::now() + ChronoDuration::hours(1),
        Some("rotated-refresh".into()),
        Some(rotated_expiry),
    );
    other_process.save_to(&path).unwrap();

    refresh_access_token_from_config_with(&mut config, &server.url(), Some(&path))
        .await
        .unwrap();

    let saved = config::Config::load_from(&path).unwrap();
    assert_eq!(saved.refresh_token.as_deref(), Some("rotated-refresh"));
    assert_eq!(saved.refresh_token_expires_at, Some(rotated_expiry));
    assert_eq!(saved.access_token.as_deref(), Some("new-token"));
    assert_eq!(config.refresh_token.as_deref(), Some("rotated-refresh"));
}

#[tokio::test]
async fn refresh_does_not_resurrect_session_after_logout_elsewhere() {
    let mut server = mockito::Server::new_async().await;
    mock_refresh_success(&mut server).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut config = refreshable_config();
    config.selected_organization_id = Some("org-a".into());
    config.save_to(&path).unwrap();

    let mut other_process = config::Config::load_from(&path).unwrap();
    other_process.clear_token();
    other_process.save_to(&path).unwrap();

    let token = refresh_access_token_from_config_with(&mut config, &server.url(), Some(&path))
        .await
        .unwrap();

    assert_eq!(token, "new-token");
    let saved = config::Config::load_from(&path).unwrap();
    assert_eq!(saved.access_token, None);
    assert_eq!(saved.refresh_token, None);
    assert_eq!(saved.selected_organization_id.as_deref(), Some("org-a"));
    assert_eq!(config.refresh_token, None);
}

#[tokio::test]
async fn refresh_writes_in_memory_config_when_file_is_missing() {
    let mut server = mockito::Server::new_async().await;
    mock_refresh_success(&mut server).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut config = refreshable_config();
    config.selected_organization_id = Some("org-a".into());

    refresh_access_token_from_config_with(&mut config, &server.url(), Some(&path))
        .await
        .unwrap();

    let saved = config::Config::load_from(&path).unwrap();
    assert_eq!(saved.access_token.as_deref(), Some("new-token"));
    assert_eq!(saved.refresh_token.as_deref(), Some("refresh-token"));
    assert_eq!(saved.selected_organization_id.as_deref(), Some("org-a"));
}

#[tokio::test]
async fn refresh_falls_back_to_in_memory_config_when_file_is_corrupt() {
    let mut server = mockito::Server::new_async().await;
    mock_refresh_success(&mut server).await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    std::fs::write(&path, "{ not json").unwrap();
    let mut config = refreshable_config();
    config.selected_organization_id = Some("org-a".into());

    let token = refresh_access_token_from_config_with(&mut config, &server.url(), Some(&path))
        .await
        .unwrap();

    assert_eq!(token, "new-token");
    let saved = config::Config::load_from(&path).unwrap();
    assert_eq!(saved.access_token.as_deref(), Some("new-token"));
    assert_eq!(saved.refresh_token.as_deref(), Some("refresh-token"));
    assert_eq!(saved.selected_organization_id.as_deref(), Some("org-a"));
    assert_eq!(config.access_token.as_deref(), Some("new-token"));
}

#[tokio::test]
async fn refresh_rejects_absurd_expires_in_with_clear_error() {
    let mut server = mockito::Server::new_async().await;
    server
        .mock("POST", "/api/v1/auth/refresh")
        .with_status(200)
        .with_body(r#"{"access_token":"new-token","expires_in":18446744073709551615}"#)
        .create_async()
        .await;
    let dir = tempfile::tempdir().unwrap();
    let path = dir.path().join("config.json");
    let mut config = refreshable_config();

    let err = refresh_access_token_from_config_with(&mut config, &server.url(), Some(&path))
        .await
        .unwrap_err();

    assert!(
        err.to_string()
            .contains("Authorization server returned an invalid token lifetime"),
        "{err}"
    );
    assert_eq!(config.access_token.as_deref(), Some("old-token"));
    assert!(!path.exists());
}

#[test]
fn resolve_tunnel_org_prefers_cli_arg() {
    let config = make_config(Some("org-config"));
    let resolved = resolve_tunnel_org(Some("org-cli".to_string()), &config);
    assert_eq!(resolved.as_deref(), Some("org-cli"));
}

#[test]
fn resolve_tunnel_org_falls_back_to_config() {
    let config = make_config(Some("org-config"));
    let resolved = resolve_tunnel_org(None, &config);
    assert_eq!(resolved.as_deref(), Some("org-config"));
}

#[test]
fn resolve_tunnel_org_none_when_not_set() {
    let config = make_config(None);
    let resolved = resolve_tunnel_org(None, &config);
    assert!(resolved.is_none());
}

#[test]
fn require_organization_uses_cli_value() {
    let config = make_config(Some("org-config"));
    let org = require_organization(Some("org-cli".to_string()), &config).unwrap();
    assert_eq!(org, "org-cli");
}

#[test]
fn require_organization_errors_when_missing() {
    let config = make_config(None);
    let err = require_organization(None, &config).unwrap_err();
    assert!(
        err.to_string().contains("No organization selected"),
        "unexpected error: {}",
        err
    );
}

#[tokio::test]
async fn ensure_valid_token_returns_error_when_expired() {
    let mut config = make_config(Some("org-config"));
    let err = ensure_valid_token(&mut config).await.unwrap_err();
    assert!(
        err.to_string().contains("Session expired"),
        "unexpected error: {}",
        err
    );
}

#[tokio::test]
async fn ensure_valid_token_prefers_environment_token() {
    let mut config = make_config(Some("org-config"));
    let token = ensure_valid_token_with(&mut config, Some("env-token".to_string()))
        .await
        .unwrap();
    assert_eq!(token, "env-token");
}

#[tokio::test]
async fn ensure_valid_token_without_environment_token_uses_saved_login() {
    let mut config = make_config(Some("org-config"));
    let err = ensure_valid_token_with(&mut config, None)
        .await
        .unwrap_err();
    assert!(
        err.to_string().contains("Session expired"),
        "unexpected error: {}",
        err
    );
}

#[test]
fn resolve_org_from_prefers_cli_then_environment_then_config() {
    let config = make_config(Some("org-config"));
    let env = || Some("org-env".to_string());
    assert_eq!(
        resolve_org_from(Some("org-cli".to_string()), env(), &config).as_deref(),
        Some("org-cli")
    );
    assert_eq!(
        resolve_org_from(None, env(), &config).as_deref(),
        Some("org-env")
    );
    assert_eq!(
        resolve_org_from(None, None, &config).as_deref(),
        Some("org-config")
    );
}
