use super::*;
use crate::cli::*;
use crate::commands::anon::*;
use crate::commands::cases::*;
use crate::commands::completions::*;
use crate::commands::config::*;
use crate::commands::endpoint::*;
use crate::commands::monitor::*;
use crate::commands::org::*;
use crate::commands::share::*;
use crate::commands::static_tunnel::*;
use crate::commands::tunnel::*;
use crate::commands::{WorkerCompletion, confirmation_is_yes, supervise_json_worker};
use crate::credentials::*;
use crate::errors::*;
use crate::receipts::*;
use crate::render::*;
use crate::tui::*;
use crate::tunnel::TunnelEvent;
use chrono::{Duration as ChronoDuration, Utc};
use clap::ValueEnum;
use std::path::PathBuf;
use tokio::sync::watch;

fn assert_no_emoji(output: &str) {
    assert!(
        !output.chars().any(|ch| {
            let code = ch as u32;
            (0x1F300..=0x1FAFF).contains(&code) || (0x2600..=0x27BF).contains(&code)
        }),
        "output contains emoji-like glyphs: {output}"
    );
}

fn assert_no_ansi_escape(output: &str) {
    assert!(
        !output.contains("\u{1b}["),
        "output contains ANSI escape sequences: {output:?}"
    );
}

#[test]
fn sanitize_terminal_escapes_ansi_csi_and_c1_csi_introducers() {
    let input = "before\u{1b}[31mred\u{1b}[0m\u{9b}2Jafter";

    assert_eq!(
        sanitize_terminal(input, TerminalTextLayout::Inline),
        r"before\u{1b}[31mred\u{1b}[0m\u{9b}2Jafter"
    );
}

#[test]
fn sanitize_terminal_inline_neutralizes_every_c0_del_and_c1_control() {
    let input = (0..=0x9f).filter_map(char::from_u32).collect::<String>();
    let sanitized = sanitize_terminal(&input, TerminalTextLayout::Inline);

    assert!(!sanitized.chars().any(char::is_control));
}

#[test]
fn sanitize_terminal_escapes_osc_52_with_bel_terminator() {
    let input = "\u{1b}]52;c;SGVsbG8=\u{7}";

    assert_eq!(
        sanitize_terminal(input, TerminalTextLayout::Inline),
        r"\u{1b}]52;c;SGVsbG8=\u{7}"
    );
}

#[test]
fn sanitize_terminal_escapes_osc_8_with_st_terminator() {
    let input = "\u{1b}]8;;https://evil.example\u{1b}\\click\u{1b}]8;;\u{1b}\\";

    assert_eq!(
        sanitize_terminal(input, TerminalTextLayout::Inline),
        r"\u{1b}]8;;https://evil.example\u{1b}\click\u{1b}]8;;\u{1b}\"
    );
}

#[test]
fn sanitize_terminal_escapes_carriage_return_backspace_and_inline_layout() {
    let input = "legitimate\r[OK] forged\u{8}!\n\tnext";

    assert_eq!(
        sanitize_terminal(input, TerminalTextLayout::Inline),
        r"legitimate\r[OK] forged\u{8}!\n\tnext"
    );
}

#[test]
fn sanitize_terminal_block_layout_preserves_only_newline_and_tab_controls() {
    let input = "line one\n\tline two\rrewritten\u{7}";

    assert_eq!(
        sanitize_terminal(input, TerminalTextLayout::Block),
        "line one\n\tline two\\rrewritten\\u{7}"
    );
}

#[test]
fn sanitize_terminal_borrows_benign_unicode_text_unchanged() {
    let input = "Café 東京 — webhook payload";
    let sanitized = sanitize_terminal(input, TerminalTextLayout::Inline);

    assert!(matches!(sanitized, std::borrow::Cow::Borrowed(value) if value == input));
}

#[test]
fn format_terminal_body_gutters_forged_status_and_blank_trailing_lines() {
    let body = "legitimate\n\n[OK] forged\n";

    assert_eq!(
        format_terminal_body(body),
        "│ legitimate\n│ \n│ [OK] forged\n│ "
    );
}

#[test]
fn sanitize_terminal_display_neutralizes_server_error_controls_and_newlines() {
    let error = anyhow!("upstream \u{1b}[31mfailed\u{1b}[0m\n[OK] forged\r");

    assert_eq!(
        sanitize_terminal_display(&error),
        r"upstream \u{1b}[31mfailed\u{1b}[0m\n[OK] forged\r"
    );
}

#[test]
fn tunnel_lifecycle_event_output_neutralizes_every_remote_text_field() {
    let event = api::TunnelLifecycleEvent {
        id: "event-id".to_string(),
        position: 7,
        cursor: "cursor".to_string(),
        organization_id: "organization".to_string(),
        capture_id: "capture\rforged".to_string(),
        delivery_id: Some("delivery\u{9b}2J".to_string()),
        sequence: 1,
        fence: None,
        event_type: "opened\n[OK]\u{1b}]52;c;x\u{7}".to_string(),
        metadata: serde_json::json!({}),
        created_at: "2026-07-20T00:00:00Z".to_string(),
    };

    assert_eq!(
        format_tunnel_lifecycle_event(&event),
        r"7  opened\n[OK]\u{1b}]52;c;x\u{7}  capture=capture\rforged  attempt=delivery\u{9b}2J"
    );
}

#[test]
fn monitor_output_neutralizes_status_controls_and_truncates_unicode_safely() {
    let styled = style_monitor_status(Some("pending\n[OK]\u{1b}]52;c;x\u{7}"));

    assert!(!styled.contains('\n'));
    assert!(!styled.contains("\u{1b}]52"));
    assert!(styled.contains(r"pending\n[OK]\u{1b}]52;c;x\u{7}"));
    assert_eq!(truncate_terminal_inline("東京 webhook", 4), "東京 …");
    assert_eq!(
        truncate_terminal_inline("\u{1b}]52;c;x\u{7}", 64),
        r"\u{1b}]52;c;x\u{7}"
    );
}

fn render_table<'a, const C: usize>(
    headers: &[&str],
    rows: impl IntoIterator<Item = [&'a str; C]>,
) -> String {
    let mut table = new_table(headers);
    for row in rows {
        table.add_row(row);
    }
    table.to_string()
}

fn render_field_block(fields: &[OutputField]) -> String {
    let mut output = String::new();
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        output.push_str(&format_field_line(field.label, &field.value));
    }
    output.push('\n');
    output
}

fn render_snapshot_sections(sections: Vec<(&str, String)>) -> String {
    let mut output = String::new();
    for (index, (title, body)) in sections.into_iter().enumerate() {
        if index > 0 {
            output.push('\n');
            output.push('\n');
        }
        output.push_str(&output_title(title));
        output.push('\n');
        output.push_str(body.trim_end());
        output.push('\n');
    }
    output
}

fn render_empty_status(title: &str, action: &str) -> String {
    format!(
        "{}\n\n{}\n",
        format_status_line(OutputStatus::Info, title),
        format_wrapped_field_lines("ACTION", action, 80)
    )
}

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
async fn json_worker_is_cancelled_and_awaited_on_injected_shutdown() {
    let (dropped_tx, dropped_rx) = tokio::sync::oneshot::channel();
    struct NotifyDrop(Option<tokio::sync::oneshot::Sender<()>>);
    impl Drop for NotifyDrop {
        fn drop(&mut self) {
            let _ = self.0.take().expect("sender").send(());
        }
    }
    let worker = tokio::spawn(async move {
        let _guard = NotifyDrop(Some(dropped_tx));
        std::future::pending::<()>().await;
    });
    tokio::task::yield_now().await;
    let result = supervise_json_worker(
        worker,
        std::future::pending::<()>(),
        std::future::ready(Ok(())),
    )
    .await
    .unwrap();
    assert_eq!(result, WorkerCompletion::Shutdown);
    dropped_rx.await.expect("worker drop notification");
}

#[test]
fn terminal_initialization_cleanup_state_can_be_disarmed_without_terminal_io() {
    let mut cleanup = TerminalInitCleanup::raw_mode_enabled();
    assert!(cleanup.raw_mode);
    cleanup.alternate_screen = true;
    cleanup.disarm();
    assert_eq!(cleanup, TerminalInitCleanup::default());
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

fn parsed_tunnel_target(args: &[&str]) -> TunnelTarget {
    let cli = Cli::try_parse_from(args).expect("tunnel command");
    match cli.command.expect("parsed command") {
        Commands::Tunnel {
            action: None,
            target,
        } => target.resolve(),
        Commands::Tunnel {
            action: Some(TunnelAction::Start(action_target) | TunnelAction::Prepare(action_target)),
            target,
        } => target.merge(action_target).resolve(),
        _ => panic!("expected a tunnel target command"),
    }
}

#[test]
fn json_error_receipt_has_stable_machine_readable_shape() {
    let receipt = json_error_receipt(&anyhow!("Something failed"));

    assert_eq!(receipt["$schema"], COMMAND_ERROR_SCHEMA);
    assert_eq!(receipt["type"], "error");
    assert_eq!(receipt["ok"], false);
    assert_eq!(receipt["error"]["code"], "command_failed");
    assert_eq!(receipt["error"]["message"], "Something failed");
    assert!(receipt["error"]["causes"].is_array());
}

#[test]
fn command_events_declare_document_specific_schemas() {
    let listen = listen_event_base(
        "connected",
        "connected",
        "endpoint-123",
        "http://localhost:3000",
    );
    let tunnel = tunnel_event_base("connected", "connected");

    assert_eq!(listen["$schema"], LISTEN_EVENT_SCHEMA);
    assert_eq!(listen["type"], "event");
    assert_eq!(tunnel["$schema"], TUNNEL_EVENT_SCHEMA);
    assert_eq!(tunnel["type"], "event");
}

fn lifecycle_contract(major: u64) -> api::TunnelLifecycleContract {
    api::TunnelLifecycleContract {
        id: "hooklistener.tunnel.lifecycle".to_string(),
        version: format!("{major}.0.0"),
        schema: api::TunnelSchemaVersion { major, minor: 0 },
    }
}

#[test]
fn tunnel_lifecycle_subcommands_preserve_default_start_syntax() {
    let default = Cli::try_parse_from(["hooklistener", "tunnel", "--port", "4000"])
        .expect("legacy tunnel syntax");
    assert!(matches!(
        default.command,
        Some(Commands::Tunnel {
            action: None,
            target: TunnelTargetArgs {
                port: Some(4000),
                ..
            }
        })
    ));

    let before_start = parsed_tunnel_target(&[
        "hooklistener",
        "tunnel",
        "--port",
        "4000",
        "--host",
        "127.0.0.1",
        "--org",
        "org_123",
        "--slug",
        "billing",
        "--allow-non-loopback",
        "--no-replay-buffered",
        "start",
    ]);
    let after_start = parsed_tunnel_target(&[
        "hooklistener",
        "tunnel",
        "start",
        "--port",
        "4000",
        "--host",
        "127.0.0.1",
        "--org",
        "org_123",
        "--slug",
        "billing",
        "--allow-non-loopback",
        "--no-replay-buffered",
    ]);
    assert_eq!(before_start, after_start);
    assert_eq!(before_start.port, 4000);
    assert_eq!(before_start.host, "127.0.0.1");
    assert_eq!(before_start.org.as_deref(), Some("org_123"));
    assert_eq!(before_start.slug.as_deref(), Some("billing"));
    assert!(before_start.allow_non_loopback);
    assert!(before_start.no_replay_buffered);

    for action in ["prepare", "start", "activate"] {
        let before = parsed_tunnel_target(&["hooklistener", "tunnel", "--port", "4001", action]);
        let after = parsed_tunnel_target(&["hooklistener", "tunnel", action, "--port", "4001"]);
        assert_eq!(before, after);
    }

    let events = Cli::try_parse_from([
        "hooklistener",
        "tunnel",
        "events",
        "--cursor",
        "opaque",
        "--follow",
        "--json",
    ])
    .expect("events command");
    assert!(events.json);
    assert!(matches!(
        events.command,
        Some(Commands::Tunnel {
            action: Some(TunnelAction::Events {
                cursor: Some(cursor),
                follow: true,
                ..
            }),
            ..
        }) if cursor == "opaque"
    ));
}

#[test]
fn anonymous_tunnel_claim_and_detach_commands_parse_explicit_credentials() {
    let anonymous = Cli::try_parse_from([
        "hooklistener",
        "anon",
        "tunnel",
        "--port",
        "4000",
        "--name",
        "stable-demo",
        "--ttl",
        "1200",
    ])
    .expect("anonymous tunnel command");
    assert!(matches!(
        anonymous.command,
        Some(Commands::Anon {
            action: AnonAction::Tunnel {
                port: 4000,
                name: Some(name),
                ttl,
                ..
            }
        }) if name == "stable-demo" && ttl == Duration::from_secs(1200)
    ));

    let claim = Cli::try_parse_from([
        "hooklistener",
        "anon",
        "claim",
        "route-123",
        "--token",
        "hkac_secret",
        "--org",
        "org-123",
    ])
    .expect("anonymous claim command");
    assert!(matches!(
        claim.command,
        Some(Commands::Anon {
            action: AnonAction::Claim {
                route_id,
                token,
                org: Some(org),
            }
        }) if route_id == "route-123" && token == "hkac_secret" && org == "org-123"
    ));

    let detach = Cli::try_parse_from([
        "hooklistener",
        "tunnel",
        "detach",
        "session-123",
        "--reason",
        "switching-machines",
    ])
    .expect("tunnel detach command");
    assert!(matches!(
        detach.command,
        Some(Commands::Tunnel {
            action: Some(TunnelAction::Detach {
                session_id,
                reason: Some(reason),
                ..
            }),
            ..
        }) if session_id == "session-123" && reason == "switching-machines"
    ));

    assert!(Cli::try_parse_from(["hooklistener", "anon", "tunnel", "--ttl", "59"]).is_err());
}

#[test]
fn tunnel_schema_mismatch_is_typed_before_activation() {
    let error = validate_tunnel_schema(&lifecycle_contract(2)).unwrap_err();

    assert_eq!(error_code(&error), "incompatible_schema");
    assert_eq!(command_exit_code(&error), 3);
    assert_eq!(
        json_error_receipt(&error)["error"]["details"]["actual_major"],
        2
    );
}

#[test]
fn expired_tunnel_cursor_has_stable_exit_code_and_resync_details() {
    let error = anyhow::Error::new(errors::TunnelLifecycleError::CursorExpired {
        earliest_cursor: Some("earliest".to_string()),
        resync: serde_json::json!({"sessions": "/api/v1/tunnel/sessions"}),
    });
    let receipt = json_error_receipt(&error);

    assert_eq!(error_code(&error), "cursor_expired");
    assert_eq!(command_exit_code(&error), 4);
    assert_eq!(receipt["error"]["details"]["earliest_cursor"], "earliest");
    assert_eq!(
        receipt["error"]["details"]["resync"]["sessions"],
        "/api/v1/tunnel/sessions"
    );

    let without_cursor = anyhow::Error::new(errors::TunnelLifecycleError::CursorExpired {
        earliest_cursor: None,
        resync: serde_json::json!({}),
    });
    assert_eq!(
        error_hint(&without_cursor),
        Some(
            "Resync sessions, captures, attempts, and outcomes, then request a fresh event cursor."
        )
    );
}

#[test]
fn tunnel_receipts_are_versioned_and_do_not_expose_credentials() {
    let receipt = tunnel_lifecycle_receipt(
        "prepare",
        "prepared",
        "org_123",
        None,
        serde_json::json!({
            "local_target_url": "http://localhost:3000",
            "requested_slug": "billing"
        }),
    );
    let serialized = serde_json::to_string(&receipt).unwrap();

    assert_eq!(receipt["$schema"], TUNNEL_RECEIPT_SCHEMA);
    assert_eq!(receipt["schema_version"], 1);
    assert!(receipt["event_id"].is_string());
    assert_eq!(receipt["sequence"], 0);
    assert!(!serialized.contains("access_token"));
    assert!(!serialized.contains("resume_token"));
}

#[test]
fn tunnel_event_envelope_carries_cursor_identity_sequence_and_resources() {
    let event = api::TunnelLifecycleEvent {
        id: "event_123".to_string(),
        position: 42,
        cursor: "opaque-cursor".to_string(),
        organization_id: "org_123".to_string(),
        capture_id: "capture_123".to_string(),
        delivery_id: Some("attempt_123".to_string()),
        sequence: 3,
        fence: Some(2),
        event_type: "forward_started".to_string(),
        metadata: serde_json::json!({
            "source": "tunnel",
            "status_code": 202,
            "headers": {"authorization": "Bearer secret-header"},
            "body": "secret-body",
            "access_token": "secret-access-token",
            "credentials": {"password": "secret-password"},
            "error_code": {"token": "secret-nested-token"}
        }),
        created_at: "2026-07-14T20:00:00Z".to_string(),
    };

    let envelope = tunnel_lifecycle_event_envelope(&event);

    assert_eq!(envelope["$schema"], TUNNEL_EVENT_SCHEMA);
    assert_eq!(envelope["event_id"], "event_123");
    assert_eq!(envelope["sequence"], 3);
    assert_eq!(envelope["cursor"], "opaque-cursor");
    assert_eq!(envelope["metadata"]["source"], "tunnel");
    assert_eq!(envelope["metadata"]["status_code"], 202);
    assert!(envelope["metadata"].get("headers").is_none());
    assert!(envelope["metadata"].get("body").is_none());
    assert!(envelope["metadata"].get("access_token").is_none());
    assert!(envelope["metadata"].get("credentials").is_none());
    assert!(envelope["metadata"].get("error_code").is_none());
    let serialized = serde_json::to_string(&envelope).unwrap();
    assert!(!serialized.contains("secret"));
    assert_eq!(
        envelope["resources"]["attempt"],
        "hooklistener://tunnel/attempts/attempt_123"
    );
}

#[test]
fn confirmation_requires_the_full_yes_token() {
    assert!(confirmation_is_yes("yes\n"));
    assert!(!confirmation_is_yes("y"));
}

#[test]
fn destructive_commands_accept_global_yes_flag() {
    let cli =
        Cli::try_parse_from(["hooklistener", "endpoint", "delete", "ep_123", "--yes"]).unwrap();

    assert!(cli.yes);
}

#[test]
fn listen_accepts_explicit_insecure_dev_server_opt_in_as_global_flag() {
    let cli = Cli::try_parse_from([
        "hooklistener",
        "listen",
        "example",
        "--ws-url",
        "ws://dev.example.com",
        "--allow-insecure-dev-server",
    ])
    .unwrap();

    assert!(cli.allow_insecure_dev_server);
}

#[test]
fn cli_definition_is_consistent() {
    Cli::command().debug_assert();
}

fn render_help_snapshot(path: &[&str]) -> String {
    let mut cli = Cli::command()
        .term_width(100)
        .color(clap::ColorChoice::Never);
    cli.build();
    let mut command = &mut cli;
    for name in path {
        command = command
            .find_subcommand_mut(name)
            .unwrap_or_else(|| panic!("subcommand `{name}` exists"));
    }
    let help = command.render_help().to_string();
    assert_no_emoji(&help);
    help
}

#[test]
fn help_snapshot_top_level() {
    insta::assert_snapshot!("help_top_level", render_help_snapshot(&[]));
}

#[test]
fn help_snapshot_endpoint() {
    insta::assert_snapshot!("help_endpoint", render_help_snapshot(&["endpoint"]));
}

#[test]
fn help_snapshot_endpoint_list() {
    insta::assert_snapshot!(
        "help_endpoint_list",
        render_help_snapshot(&["endpoint", "list"])
    );
}

#[test]
fn help_snapshot_tunnel() {
    insta::assert_snapshot!("help_tunnel", render_help_snapshot(&["tunnel"]));
}

#[test]
fn help_snapshot_tunnel_events() {
    insta::assert_snapshot!(
        "help_tunnel_events",
        render_help_snapshot(&["tunnel", "events"])
    );
}

#[test]
fn help_snapshot_anon() {
    insta::assert_snapshot!("help_anon", render_help_snapshot(&["anon"]));
}

#[test]
fn help_snapshot_anon_create() {
    insta::assert_snapshot!(
        "help_anon_create",
        render_help_snapshot(&["anon", "create"])
    );
}

#[test]
fn help_snapshot_anon_tunnel() {
    insta::assert_snapshot!(
        "help_anon_tunnel",
        render_help_snapshot(&["anon", "tunnel"])
    );
}

#[test]
fn help_snapshot_monitor() {
    insta::assert_snapshot!("help_monitor", render_help_snapshot(&["monitor"]));
}

#[test]
fn help_snapshot_monitor_create() {
    insta::assert_snapshot!(
        "help_monitor_create",
        render_help_snapshot(&["monitor", "create"])
    );
}

#[test]
fn help_snapshot_cases_run() {
    insta::assert_snapshot!("help_cases_run", render_help_snapshot(&["cases", "run"]));
}

#[test]
fn help_snapshot_cases() {
    insta::assert_snapshot!("help_cases", render_help_snapshot(&["cases"]));
}

#[test]
fn help_snapshot_cases_replay() {
    insta::assert_snapshot!(
        "help_cases_replay",
        render_help_snapshot(&["cases", "replay"])
    );
}

#[test]
fn help_snapshot_cases_runs_wait() {
    insta::assert_snapshot!(
        "help_cases_runs_wait",
        render_help_snapshot(&["cases", "runs", "wait"])
    );
}

#[test]
fn help_snapshot_share_create() {
    insta::assert_snapshot!(
        "help_share_create",
        render_help_snapshot(&["share", "create"])
    );
}

#[test]
fn help_snapshot_completions() {
    let help = render_help_snapshot(&["completions"]);
    assert!(
        help.contains("[possible values: bash, zsh, fish, powershell, elvish]"),
        "completions help must list the visible shell names:\n{help}"
    );
    assert!(
        !help.contains("power-shell"),
        "power-shell is a hidden alias and must not appear in help:\n{help}"
    );
    insta::assert_snapshot!("help_completions", help);
}

#[test]
fn org_flag_accepts_short_o_on_every_command() {
    let cli = Cli::try_parse_from(["hooklistener", "endpoint", "list", "-o", "org_1"])
        .expect("endpoint list -o parses");
    assert!(matches!(
        cli.command,
        Some(Commands::Endpoint {
            action: EndpointAction::List { ref org },
        }) if org.as_deref() == Some("org_1")
    ));

    let cli = Cli::try_parse_from(["hooklistener", "monitor", "list", "-o", "org_1"])
        .expect("monitor list -o parses");
    assert!(matches!(
        cli.command,
        Some(Commands::Monitor {
            action: MonitorAction::List { ref org },
        }) if org.as_deref() == Some("org_1")
    ));

    let cli = Cli::try_parse_from(["hooklistener", "share", "list", "req_1", "-o", "org_1"])
        .expect("share list -o parses");
    assert!(matches!(
        cli.command,
        Some(Commands::Share {
            action: ShareAction::List { ref request_id, ref org },
        }) if request_id == "req_1" && org.as_deref() == Some("org_1")
    ));

    let cli = Cli::try_parse_from([
        "hooklistener",
        "static-tunnel",
        "delete",
        "st_1",
        "-o",
        "org_1",
    ])
    .expect("static-tunnel delete -o parses");
    assert!(matches!(
        cli.command,
        Some(Commands::StaticTunnel {
            action: StaticTunnelAction::Delete { ref static_tunnel_id, ref org },
        }) if static_tunnel_id == "st_1" && org.as_deref() == Some("org_1")
    ));

    let cli = Cli::try_parse_from(["hooklistener", "cases", "run", "ep_1", "-o", "org_1"])
        .expect("cases run -o parses");
    assert!(matches!(
        cli.command,
        Some(Commands::Cases {
            action: CasesAction::Run { ref org, .. },
        }) if org.as_deref() == Some("org_1")
    ));
}

#[test]
fn completions_power_shell_is_alias_of_powershell() {
    for spelling in ["powershell", "power-shell", "PowerShell"] {
        let cli = Cli::try_parse_from(["hooklistener", "completions", spelling])
            .unwrap_or_else(|err| panic!("completions {spelling} parses: {err}"));
        assert!(
            matches!(
                cli.command,
                Some(Commands::Completions {
                    shell: CompletionShell::PowerShell,
                })
            ),
            "completions {spelling} must yield PowerShell"
        );
    }
}

#[test]
fn completions_generate_for_every_shell() {
    for shell in CompletionShell::value_variants() {
        let mut buf: Vec<u8> = Vec::new();
        write_completions(*shell, &mut buf)
            .unwrap_or_else(|err| panic!("completions {shell:?} writes: {err}"));
        assert!(!buf.is_empty(), "completions {shell:?} must not be empty");
        let script = String::from_utf8(buf)
            .unwrap_or_else(|err| panic!("completions {shell:?} is UTF-8: {err}"));
        assert!(
            script.contains("hooklistener"),
            "completions {shell:?} must mention the binary name"
        );
    }
}

struct BrokenPipeWriter;

impl io::Write for BrokenPipeWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::from(io::ErrorKind::BrokenPipe))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

struct FailingWriter;

impl io::Write for FailingWriter {
    fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
        Err(io::Error::other("disk full"))
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn completions_broken_pipe_is_not_an_error() {
    for shell in CompletionShell::value_variants() {
        let mut out = BrokenPipeWriter;
        assert!(
            print_completions(*shell, &mut out).is_ok(),
            "completions {shell:?} must treat a closed pipe as success"
        );
    }
}

#[test]
fn completions_other_write_errors_propagate() {
    let mut out = FailingWriter;
    let err = print_completions(CompletionShell::Bash, &mut out)
        .expect_err("a non-pipe write error must propagate");
    assert_eq!(err.kind(), io::ErrorKind::Other);
    assert_eq!(err.to_string(), "disk full");
}

#[test]
fn renamed_positionals_keep_their_positions() {
    let cli = Cli::try_parse_from(["hooklistener", "listen", "my-endpoint"]).unwrap();
    assert!(matches!(
        cli.command,
        Some(Commands::Listen { ref endpoint_slug, .. }) if endpoint_slug == "my-endpoint"
    ));

    let cli = Cli::try_parse_from(["hooklistener", "org", "use", "org_1"]).unwrap();
    assert!(matches!(
        cli.command,
        Some(Commands::Org {
            action: OrgAction::Use { ref org_id },
        }) if org_id == "org_1"
    ));

    let cli = Cli::try_parse_from(["hooklistener", "anon", "show", "ep_1"]).unwrap();
    assert!(matches!(
        cli.command,
        Some(Commands::Anon {
            action: AnonAction::Show { ref endpoint_id },
        }) if endpoint_id == "ep_1"
    ));

    let cli = Cli::try_parse_from(["hooklistener", "share", "show", "tok_1"]).unwrap();
    assert!(matches!(
        cli.command,
        Some(Commands::Share {
            action: ShareAction::Show { ref share_token },
        }) if share_token == "tok_1"
    ));

    let cli = Cli::try_parse_from(["hooklistener", "monitor", "show", "mon_1"]).unwrap();
    assert!(matches!(
        cli.command,
        Some(Commands::Monitor {
            action: MonitorAction::Show { ref monitor_id, .. },
        }) if monitor_id == "mon_1"
    ));
}

#[test]
fn endpoint_requests_is_alias_of_list_requests() {
    for spelling in ["requests", "list-requests"] {
        let cli = Cli::try_parse_from(["hooklistener", "endpoint", spelling, "ep_1"])
            .unwrap_or_else(|err| panic!("endpoint {spelling} parses: {err}"));
        assert!(
            matches!(
                cli.command,
                Some(Commands::Endpoint {
                    action: EndpointAction::ListRequests {
                        ref endpoint_id,
                        page: 1,
                        page_size: 50,
                        org: None,
                    },
                }) if endpoint_id == "ep_1"
            ),
            "endpoint {spelling} must yield ListRequests"
        );
    }
}

#[test]
fn endpoint_request_is_alias_of_show_request() {
    for spelling in ["request", "show-request"] {
        let cli = Cli::try_parse_from(["hooklistener", "endpoint", spelling, "ep_1", "req_1"])
            .unwrap_or_else(|err| panic!("endpoint {spelling} parses: {err}"));
        assert!(
            matches!(
                cli.command,
                Some(Commands::Endpoint {
                    action: EndpointAction::ShowRequest {
                        ref endpoint_id,
                        ref request_id,
                        org: None,
                    },
                }) if endpoint_id == "ep_1" && request_id == "req_1"
            ),
            "endpoint {spelling} must yield ShowRequest"
        );
    }
}

#[test]
fn endpoint_forwards_is_alias_of_list_forwards() {
    for spelling in ["forwards", "list-forwards"] {
        let cli = Cli::try_parse_from([
            "hooklistener",
            "endpoint",
            spelling,
            "ep_1",
            "req_1",
            "--page",
            "2",
        ])
        .unwrap_or_else(|err| panic!("endpoint {spelling} parses: {err}"));
        assert!(
            matches!(
                cli.command,
                Some(Commands::Endpoint {
                    action: EndpointAction::ListForwards {
                        ref endpoint_id,
                        ref request_id,
                        page: 2,
                        page_size: 50,
                        org: None,
                    },
                }) if endpoint_id == "ep_1" && request_id == "req_1"
            ),
            "endpoint {spelling} must yield ListForwards"
        );
    }
}

#[test]
fn endpoint_forward_is_alias_of_show_forward() {
    for spelling in ["forward", "show-forward"] {
        let cli = Cli::try_parse_from(["hooklistener", "endpoint", spelling, "fwd_1"])
            .unwrap_or_else(|err| panic!("endpoint {spelling} parses: {err}"));
        assert!(
            matches!(
                cli.command,
                Some(Commands::Endpoint {
                    action: EndpointAction::ShowForward {
                        ref forward_id,
                        org: None,
                    },
                }) if forward_id == "fwd_1"
            ),
            "endpoint {spelling} must yield ShowForward"
        );
    }
}

#[test]
fn endpoint_forward_aliases_do_not_capture_forward_request_or_delete_request() {
    let cli = Cli::try_parse_from([
        "hooklistener",
        "endpoint",
        "forward-request",
        "ep_1",
        "req_1",
        "http://localhost:3000/hook",
    ])
    .expect("endpoint forward-request parses");
    assert!(matches!(
        cli.command,
        Some(Commands::Endpoint {
            action: EndpointAction::ForwardRequest { .. },
        })
    ));

    let cli = Cli::try_parse_from([
        "hooklistener",
        "endpoint",
        "delete-request",
        "ep_1",
        "req_1",
    ])
    .expect("endpoint delete-request parses");
    assert!(matches!(
        cli.command,
        Some(Commands::Endpoint {
            action: EndpointAction::DeleteRequest { .. },
        })
    ));
}

#[test]
fn anon_events_is_alias_of_list_events() {
    for spelling in ["events", "list-events"] {
        let cli = Cli::try_parse_from([
            "hooklistener",
            "anon",
            spelling,
            "ep_1",
            "--token",
            "viewer_token",
        ])
        .unwrap_or_else(|err| panic!("anon {spelling} parses: {err}"));
        assert!(
            matches!(
                cli.command,
                Some(Commands::Anon {
                    action: AnonAction::ListEvents {
                        ref endpoint_id,
                        ref token,
                        page: 1,
                        page_size: 50,
                    },
                }) if endpoint_id == "ep_1" && token == "viewer_token"
            ),
            "anon {spelling} must yield ListEvents"
        );
    }
}

#[test]
fn anon_event_is_alias_of_show_event() {
    for spelling in ["event", "show-event"] {
        let cli = Cli::try_parse_from([
            "hooklistener",
            "anon",
            spelling,
            "ep_1",
            "evt_1",
            "--token",
            "viewer_token",
        ])
        .unwrap_or_else(|err| panic!("anon {spelling} parses: {err}"));
        assert!(
            matches!(
                cli.command,
                Some(Commands::Anon {
                    action: AnonAction::ShowEvent {
                        ref endpoint_id,
                        ref event_id,
                        ref token,
                    },
                }) if endpoint_id == "ep_1" && event_id == "evt_1" && token == "viewer_token"
            ),
            "anon {spelling} must yield ShowEvent"
        );
    }
}

#[test]
fn tunnel_activate_is_alias_of_start() {
    let cli = Cli::try_parse_from(["hooklistener", "tunnel", "activate", "--port", "5000"])
        .expect("tunnel activate parses");
    assert!(matches!(
        cli.command,
        Some(Commands::Tunnel {
            action: Some(TunnelAction::Start(TunnelTargetArgs {
                port: Some(5000),
                ..
            })),
            ..
        })
    ));

    let alias = parsed_tunnel_target(&["hooklistener", "tunnel", "activate", "--port", "5000"]);
    let start = parsed_tunnel_target(&["hooklistener", "tunnel", "start", "--port", "5000"]);
    assert_eq!(alias, start);
}

#[test]
fn tunnel_list_accepts_status_limit_and_short_org() {
    let cli = Cli::try_parse_from([
        "hooklistener",
        "tunnel",
        "list",
        "--status",
        "active",
        "--limit",
        "10",
        "-o",
        "org_1",
    ])
    .expect("tunnel list parses");
    assert!(matches!(
        cli.command,
        Some(Commands::Tunnel {
            action: Some(TunnelAction::List {
                limit: 10,
                status: Some(status),
                org: Some(org),
            }),
            ..
        }) if status == "active" && org == "org_1"
    ));
}

#[test]
fn tunnel_lifecycle_subcommands_accept_short_org() {
    for args in [
        vec!["tunnel", "status", "session-1", "-o", "org_1"],
        vec!["tunnel", "events", "-o", "org_1"],
        vec!["tunnel", "capture", "capture-1", "-o", "org_1"],
        vec!["tunnel", "attempt", "attempt-1", "-o", "org_1"],
        vec!["tunnel", "stop", "session-1", "-o", "org_1"],
        vec!["tunnel", "detach", "session-1", "-o", "org_1"],
        vec!["anon", "claim", "route-1", "--token", "t", "-o", "org_1"],
    ] {
        let mut full = vec!["hooklistener"];
        full.extend(args.iter().copied());
        let cli = Cli::try_parse_from(&full).unwrap_or_else(|err| panic!("{args:?}: {err}"));
        let org = match cli.command.expect("parsed command") {
            Commands::Tunnel {
                action:
                    Some(
                        TunnelAction::Status { org, .. }
                        | TunnelAction::Events { org, .. }
                        | TunnelAction::Capture { org, .. }
                        | TunnelAction::Attempt { org, .. }
                        | TunnelAction::Stop { org, .. }
                        | TunnelAction::Detach { org, .. },
                    ),
                ..
            } => org,
            Commands::Anon {
                action: AnonAction::Claim { org, .. },
            } => org,
            _ => panic!("{args:?}: unexpected command"),
        };
        assert_eq!(org.as_deref(), Some("org_1"), "{args:?}");
    }
}

#[test]
fn top_level_commands_are_listed_in_grouped_order() {
    let command = Cli::command();
    let names: Vec<&str> = command
        .get_subcommands()
        .map(|subcommand| subcommand.get_name())
        .collect();

    assert_eq!(
        names,
        [
            "listen",
            "tunnel",
            "endpoint",
            "static-tunnel",
            "anon",
            "cases",
            "share",
            "monitor",
            "login",
            "logout",
            "org",
            "config",
            "diagnostics",
            "clean-logs",
            "completions",
            "update",
        ]
    );
    assert_eq!(
        command.get_about().map(ToString::to_string).as_deref(),
        Some("Inspect webhooks, replay failures, and expose localhost from your terminal")
    );
}

#[test]
fn log_level_is_global_after_tunnel_subcommand() {
    let cli =
        Cli::try_parse_from(["hooklistener", "tunnel", "--log-level", "debug", "prepare"]).unwrap();

    assert_eq!(cli.log_level, LogLevel::Debug);
}

#[test]
fn log_flags_are_global_after_endpoint_list() {
    let cli = Cli::try_parse_from([
        "hooklistener",
        "endpoint",
        "list",
        "--log-stdout",
        "--log-dir",
        "/tmp/x",
    ])
    .unwrap();

    assert!(cli.log_stdout);
    assert_eq!(cli.log_dir, Some(PathBuf::from("/tmp/x")));
}

#[test]
fn log_flags_are_global_after_diagnostics() {
    let cli = Cli::try_parse_from([
        "hooklistener",
        "diagnostics",
        "--log-level",
        "debug",
        "--log-dir",
        "/tmp/x",
        "--output",
        "/tmp/bundle",
    ])
    .unwrap();

    assert_eq!(cli.log_level, LogLevel::Debug);
    assert_eq!(cli.log_dir, Some(PathBuf::from("/tmp/x")));
    assert!(matches!(
        cli.command,
        Some(Commands::Diagnostics { ref output }) if output == &PathBuf::from("/tmp/bundle")
    ));
}

#[test]
fn log_level_ignores_case() {
    let cli =
        Cli::try_parse_from(["hooklistener", "--log-level", "WARN", "endpoint", "list"]).unwrap();

    assert_eq!(cli.log_level, LogLevel::Warn);
}

#[test]
fn log_level_defaults_to_info() {
    let cli = Cli::try_parse_from(["hooklistener", "endpoint", "list"]).unwrap();

    assert_eq!(cli.log_level, LogLevel::Info);
    assert_eq!(cli.log_level.as_str(), "info");
}

#[test]
fn log_level_rejects_unknown_value() {
    let result =
        Cli::try_parse_from(["hooklistener", "--log-level", "verbose", "endpoint", "list"]);

    match result {
        Ok(_) => panic!("expected --log-level verbose to be rejected"),
        Err(error) => assert_eq!(error.kind(), clap::error::ErrorKind::InvalidValue),
    }
}

#[test]
fn insecure_dev_server_flag_is_hidden_from_help() {
    let mut command = Cli::command().term_width(100);
    let help = command.render_help().to_string();

    assert!(!help.contains("insecure"), "{help}");
    assert_eq!(help.matches("Global options:").count(), 1, "{help}");
    for flag in [
        "--json",
        "--color",
        "--yes",
        "--log-level",
        "--log-dir",
        "--log-stdout",
    ] {
        assert!(help.contains(flag), "missing {flag} in {help}");
    }

    let mut root = Cli::command().term_width(100);
    root.build();
    let help = root
        .find_subcommand_mut("endpoint")
        .and_then(|endpoint| endpoint.find_subcommand_mut("list"))
        .expect("endpoint list command")
        .render_help()
        .to_string();

    assert!(!help.contains("insecure"), "{help}");
    assert_eq!(help.matches("Global options:").count(), 1, "{help}");
}

fn parse_error<I, T>(args: I) -> clap::Error
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    match Cli::try_parse_from(args) {
        Ok(_) => panic!("expected argument parsing to fail"),
        Err(err) => err,
    }
}

fn parse_error_kind<I, T>(args: I) -> clap::error::ErrorKind
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    parse_error(args).kind()
}

#[test]
fn forward_request_method_is_a_value_enum_rendered_uppercase() {
    let cli = Cli::try_parse_from([
        "hooklistener",
        "endpoint",
        "forward-request",
        "ep_1",
        "req_1",
        "http://localhost:3000/hook",
        "--method",
        "post",
    ])
    .unwrap();

    match cli.command {
        Some(Commands::Endpoint {
            action: EndpointAction::ForwardRequest { method, .. },
        }) => {
            assert_eq!(method, Some(HttpMethod::Post));
            assert_eq!(method.unwrap().as_uppercase(), "POST");
        }
        _ => panic!("expected endpoint forward-request command"),
    }
}

#[test]
fn forward_request_method_rejects_unknown_values() {
    let kind = parse_error_kind([
        "hooklistener",
        "endpoint",
        "forward-request",
        "ep_1",
        "req_1",
        "http://localhost:3000/hook",
        "--method",
        "trace",
    ]);
    assert_eq!(kind, clap::error::ErrorKind::InvalidValue);
}

#[test]
fn monitor_create_method_defaults_to_get_and_accepts_any_case() {
    let cli = Cli::try_parse_from([
        "hooklistener",
        "monitor",
        "create",
        "API",
        "https://example.com/health",
    ])
    .unwrap();
    match cli.command {
        Some(Commands::Monitor {
            action:
                MonitorAction::Create {
                    method,
                    expected_status,
                    failure_threshold,
                    ..
                },
        }) => {
            assert_eq!(method, MonitorMethod::Get);
            assert_eq!(method.as_lowercase(), "get");
            assert_eq!(method.to_string(), "GET");
            assert_eq!(expected_status, 200);
            assert_eq!(failure_threshold, 2);
        }
        _ => panic!("expected monitor create command"),
    }

    let cli = Cli::try_parse_from([
        "hooklistener",
        "monitor",
        "create",
        "API",
        "https://example.com/health",
        "--method",
        "Post",
    ])
    .unwrap();
    match cli.command {
        Some(Commands::Monitor {
            action: MonitorAction::Create { method, .. },
        }) => {
            assert_eq!(method, MonitorMethod::Post);
            assert_eq!(method.as_lowercase(), "post");
        }
        _ => panic!("expected monitor create command"),
    }
}

#[test]
fn monitor_method_rejects_options() {
    for args in [
        vec![
            "hooklistener",
            "monitor",
            "create",
            "API",
            "https://example.com/health",
            "--method",
            "options",
        ],
        vec![
            "hooklistener",
            "monitor",
            "update",
            "mon_1",
            "--method",
            "OPTIONS",
        ],
    ] {
        assert_eq!(parse_error_kind(args), clap::error::ErrorKind::InvalidValue);
    }
}

#[test]
fn monitor_update_method_parses_case_insensitively() {
    let cli = Cli::try_parse_from([
        "hooklistener",
        "monitor",
        "update",
        "mon_1",
        "--method",
        "head",
    ])
    .unwrap();
    match cli.command {
        Some(Commands::Monitor {
            action: MonitorAction::Update { method, .. },
        }) => assert_eq!(method, Some(MonitorMethod::Head)),
        _ => panic!("expected monitor update command"),
    }
}

#[test]
fn monitor_expected_status_must_be_an_http_status_code() {
    let kind = parse_error_kind([
        "hooklistener",
        "monitor",
        "create",
        "API",
        "https://example.com/health",
        "--expected-status",
        "42",
    ]);
    assert_eq!(kind, clap::error::ErrorKind::ValueValidation);

    let kind = parse_error_kind([
        "hooklistener",
        "monitor",
        "update",
        "mon_1",
        "--expected-status",
        "600",
    ]);
    assert_eq!(kind, clap::error::ErrorKind::ValueValidation);
}

#[test]
fn monitor_failure_threshold_rejects_zero() {
    let kind = parse_error_kind([
        "hooklistener",
        "monitor",
        "create",
        "API",
        "https://example.com/health",
        "--failure-threshold",
        "0",
    ]);
    assert_eq!(kind, clap::error::ErrorKind::ValueValidation);
}

#[test]
fn paginated_lists_reject_page_zero() {
    for args in [
        vec![
            "hooklistener",
            "endpoint",
            "list-requests",
            "ep_1",
            "--page",
            "0",
        ],
        vec![
            "hooklistener",
            "endpoint",
            "list-forwards",
            "ep_1",
            "req_1",
            "--page-size",
            "0",
        ],
        vec![
            "hooklistener",
            "anon",
            "list-events",
            "ep_1",
            "--token",
            "t",
            "--page",
            "0",
        ],
        vec!["hooklistener", "monitor", "checks", "mon_1", "--page", "0"],
    ] {
        assert_eq!(
            parse_error_kind(args.clone()),
            clap::error::ErrorKind::ValueValidation,
            "{args:?} must fail range validation"
        );
    }

    let cli = Cli::try_parse_from([
        "hooklistener",
        "monitor",
        "checks",
        "mon_1",
        "--page",
        "2",
        "--page-size",
        "10",
    ])
    .unwrap();
    match cli.command {
        Some(Commands::Monitor {
            action: MonitorAction::Checks {
                page, page_size, ..
            },
        }) => {
            assert_eq!(page, 2);
            assert_eq!(page_size, 10);
        }
        _ => panic!("expected monitor checks command"),
    }
}

#[test]
fn config_set_key_is_a_value_enum() {
    let cli = Cli::try_parse_from([
        "hooklistener",
        "config",
        "set",
        "selected_organization_id",
        "org_1",
    ])
    .unwrap();
    match cli.command {
        Some(Commands::Config {
            action: ConfigAction::Set { key, value },
        }) => {
            assert_eq!(key, ConfigKey::SelectedOrganizationId);
            assert_eq!(value, "org_1");
        }
        _ => panic!("expected config set command"),
    }

    assert_eq!(
        parse_error_kind(["hooklistener", "config", "set", "other", "x"]),
        clap::error::ErrorKind::InvalidValue
    );
}

#[test]
fn monitor_email_accepts_explicit_false() {
    let cli = Cli::try_parse_from([
        "hooklistener",
        "monitor",
        "create",
        "API",
        "https://example.com/health",
        "--email=false",
    ])
    .unwrap();

    match cli.command {
        Some(Commands::Monitor {
            action: MonitorAction::Create { email, .. },
        }) => assert!(!email),
        _ => panic!("expected monitor create command"),
    }
}

#[test]
fn monitor_no_email_disables_notifications() {
    let cli = Cli::try_parse_from([
        "hooklistener",
        "monitor",
        "create",
        "API",
        "https://example.com/health",
        "--no-email",
    ])
    .unwrap();

    match cli.command {
        Some(Commands::Monitor {
            action: MonitorAction::Create {
                email, no_email, ..
            },
        }) => {
            assert!(email);
            assert!(no_email);
            assert!(!monitor_email_enabled(email, no_email));
        }
        _ => panic!("expected monitor create command"),
    }

    assert!(monitor_email_enabled(true, false));
    assert!(!monitor_email_enabled(false, false));
}

#[test]
fn monitor_email_and_no_email_conflict() {
    assert_eq!(
        parse_error_kind([
            "hooklistener",
            "monitor",
            "create",
            "API",
            "https://example.com/health",
            "--email",
            "--no-email",
        ]),
        clap::error::ErrorKind::ArgumentConflict
    );
}

#[test]
fn monitor_update_enable_disable_conflict() {
    assert_eq!(
        parse_error_kind([
            "hooklistener",
            "monitor",
            "update",
            "mon_1",
            "--enable",
            "--disable",
        ]),
        clap::error::ErrorKind::ArgumentConflict
    );
    assert_eq!(
        parse_error_kind([
            "hooklistener",
            "monitor",
            "update",
            "mon_1",
            "--enable",
            "--enabled",
            "true",
        ]),
        clap::error::ErrorKind::ArgumentConflict
    );
}

#[test]
fn monitor_update_enable_and_disable_set_enabled() {
    for (flag, expected) in [("--enable", Some(true)), ("--disable", Some(false))] {
        let cli =
            Cli::try_parse_from(["hooklistener", "monitor", "update", "mon_1", flag]).unwrap();
        match cli.command {
            Some(Commands::Monitor {
                action:
                    MonitorAction::Update {
                        enable,
                        disable,
                        enabled,
                        ..
                    },
            }) => {
                assert_eq!(enable, expected == Some(true));
                assert_eq!(disable, expected == Some(false));
                assert_eq!(enabled, None);
                assert_eq!(monitor_enabled_update(enable, disable, enabled), expected);
            }
            _ => panic!("expected monitor update command"),
        }
    }

    assert_eq!(monitor_enabled_update(false, false, None), None);
}

#[test]
fn monitor_update_enabled_hidden_flag_still_parses() {
    let cli = Cli::try_parse_from([
        "hooklistener",
        "monitor",
        "update",
        "mon_1",
        "--enabled",
        "false",
    ])
    .unwrap();

    match cli.command {
        Some(Commands::Monitor {
            action:
                MonitorAction::Update {
                    enable,
                    disable,
                    enabled,
                    ..
                },
        }) => {
            assert!(!enable);
            assert!(!disable);
            assert_eq!(enabled, Some(false));
            assert_eq!(
                monitor_enabled_update(enable, disable, enabled),
                Some(false)
            );
        }
        _ => panic!("expected monitor update command"),
    }
}

#[test]
fn cases_run_hidden_target_flags_still_parse() {
    let cli = Cli::try_parse_from([
        "hooklistener",
        "cases",
        "run",
        "ep_123",
        "--target-url",
        "http://localhost:3000",
    ])
    .unwrap();
    match cli.command {
        Some(Commands::Cases {
            action:
                CasesAction::Run {
                    destination:
                        commands::cases::DestinationArgs {
                            target,
                            target_url,
                            target_id,
                        },
                    ..
                },
        }) => {
            assert_eq!(target, None);
            assert_eq!(target_url.as_deref(), Some("http://localhost:3000"));
            assert_eq!(target_id, None);
        }
        _ => panic!("expected cases run command"),
    }

    let cli = Cli::try_parse_from([
        "hooklistener",
        "cases",
        "run",
        "ep_123",
        "--target-id",
        "t_1",
    ])
    .unwrap();
    match cli.command {
        Some(Commands::Cases {
            action:
                CasesAction::Run {
                    destination:
                        commands::cases::DestinationArgs {
                            target,
                            target_url,
                            target_id,
                        },
                    ..
                },
        }) => {
            assert_eq!(target, None);
            assert_eq!(target_url, None);
            assert_eq!(target_id.as_deref(), Some("t_1"));
        }
        _ => panic!("expected cases run command"),
    }
}

#[test]
fn cases_run_help_shows_only_the_target_flag() {
    let mut cmd = Cli::command();
    let run = cmd
        .find_subcommand_mut("cases")
        .unwrap()
        .find_subcommand_mut("run")
        .unwrap();
    let help = run.render_help().to_string();
    assert!(help.contains("--target <TARGET>"));
    assert!(!help.contains("--target-url"));
    assert!(!help.contains("--target-id"));
    assert!(help.contains("--target-name <NAME>"));
}

#[test]
fn cases_run_parser_rejects_conflicting_target_options() {
    let result = Cli::try_parse_from([
        "hooklistener",
        "cases",
        "run",
        "ep_123",
        "--target",
        "cli",
        "--target-url",
        "http://localhost:3000",
    ]);

    assert!(result.is_err());
}

#[test]
fn command_tables_fit_the_default_terminal_width() {
    let output = render_table(
        &["ID", "Method", "Status", "Webhook URL", "Name"],
        [[
            "endpoint_identifier_123",
            "POST",
            "active",
            "https://example.hooklistener.dev/a/very/long/webhook/path",
            "Production webhook receiver",
        ]],
    );

    assert!(output.lines().all(|line| line.chars().count() <= 80));
}

#[test]
fn command_output_endpoint_created_snapshot() {
    let output = render_status_block(
        OutputStatus::Ok,
        "endpoint created",
        &[
            output_field("ID", "ep_123"),
            output_field("SLUG", "github-webhooks"),
            output_field(
                "WEBHOOK URL",
                "https://example.hooklistener.dev/github-webhooks",
            ),
            output_field("ORGANIZATION", "org_123"),
            output_field("ACTION", "Run `hooklistener listen github-webhooks`"),
        ],
    );

    assert_no_emoji(&output);
    assert!(!output.contains("ID:"));
    insta::assert_snapshot!("command_output_endpoint_created", output);
}

#[test]
fn command_output_request_deleted_snapshot() {
    let output = render_status_block(
        OutputStatus::Ok,
        "request deleted",
        &[
            output_field("REQUEST", "req_123"),
            output_field("ENDPOINT", "ep_123"),
            output_field("ORGANIZATION", "org_123"),
        ],
    );

    assert_no_emoji(&output);
    assert!(output.starts_with("[OK] REQUEST DELETED\n\n"));
    insta::assert_snapshot!("command_output_request_deleted", output);
}

#[test]
fn command_output_error_block_snapshot() {
    let output = render_status_block(
        OutputStatus::Err,
        "command failed",
        &[
            output_field("MESSAGE", "Not authenticated"),
            output_field("ACTION", "Run `hooklistener login`"),
        ],
    );

    assert_no_emoji(&output);
    assert!(output.starts_with("[ERR] COMMAND FAILED\n\n"));
    insta::assert_snapshot!("command_output_error_block", output);
}

#[test]
fn command_output_table_headers_snapshot() {
    let output = render_table(
        &["ID", "Method", "Status", "Checked At"],
        [["req_123", "POST", "200", "2026-05-29T12:00:00Z"]],
    );

    assert!(output.contains("METHOD"));
    assert!(output.contains("CHECKED AT"));
    assert!(!output.contains("Method"));
    assert_no_ansi_escape(&output);
    insta::assert_snapshot!("command_output_table_headers", output);
}

#[test]
fn cases_run_target_shorthand_maps_to_expected_body_fields() {
    let url = build_case_run_params(CaseRunInput {
        target: Some("https://example.test/webhooks".to_string()),
        target_url: None,
        target_id: None,
        target_name: None,
        wait: true,
        timeout: Some(Duration::from_secs(60)),
        timeout_ms: None,
        interval: None,
        interval_ms: None,
    })
    .unwrap();
    assert_eq!(
        url.target_url.as_deref(),
        Some("https://example.test/webhooks")
    );
    assert_eq!(url.wait, Some(true));
    assert_eq!(url.timeout_ms, Some(60_000));

    let cli = build_case_run_params(CaseRunInput {
        target: Some("cli".to_string()),
        target_url: None,
        target_id: None,
        target_name: Some("Local CLI".to_string()),
        wait: false,
        timeout: None,
        timeout_ms: None,
        interval: None,
        interval_ms: Some(500),
    })
    .unwrap();
    assert_eq!(cli.target.as_deref(), Some("cli"));
    assert_eq!(cli.target_name.as_deref(), Some("Local CLI"));
    assert_eq!(cli.interval_ms, Some(500));

    let saved = build_case_run_params(CaseRunInput {
        target: Some("rt_123".to_string()),
        target_url: None,
        target_id: None,
        target_name: None,
        wait: false,
        timeout: None,
        timeout_ms: None,
        interval: None,
        interval_ms: None,
    })
    .unwrap();
    assert_eq!(saved.target_id.as_deref(), Some("rt_123"));
}

#[test]
fn cases_run_rejects_ambiguous_targets() {
    let err = build_case_run_params(CaseRunInput {
        target: Some("cli".to_string()),
        target_url: Some("http://localhost:3000".to_string()),
        target_id: None,
        target_name: None,
        wait: false,
        timeout: None,
        timeout_ms: None,
        interval: None,
        interval_ms: None,
    })
    .unwrap_err();
    assert!(err.to_string().contains("Use --target by itself"));

    let err = build_case_run_params(CaseRunInput {
        target: None,
        target_url: None,
        target_id: None,
        target_name: None,
        wait: false,
        timeout: None,
        timeout_ms: None,
        interval: None,
        interval_ms: None,
    })
    .unwrap_err();
    assert!(err.to_string().contains("Target is required"));
}

#[test]
fn parse_duration_accepts_bare_seconds_and_units() {
    assert_eq!(parse_duration("60").unwrap(), Duration::from_secs(60));
    assert_eq!(parse_duration("60s").unwrap(), Duration::from_secs(60));
    assert_eq!(parse_duration("2m").unwrap(), Duration::from_secs(120));
    assert_eq!(
        parse_duration("1500ms").unwrap(),
        Duration::from_millis(1_500)
    );
    assert_eq!(parse_duration("1h").unwrap(), Duration::from_secs(3_600));
    assert_eq!(parse_duration("7d").unwrap(), Duration::from_secs(604_800));
    assert_eq!(
        parse_duration(" 2 Hours ").unwrap(),
        Duration::from_secs(7_200)
    );
}

#[test]
fn parse_duration_reports_errors_in_cli_voice() {
    assert_eq!(parse_duration("").unwrap_err(), "Duration cannot be empty.");
    assert_eq!(
        parse_duration("abc").unwrap_err(),
        "Duration must start with a number."
    );
    assert_eq!(
        parse_duration("5w").unwrap_err(),
        "Invalid duration unit 'w'. Use ms, s, m, h, or d."
    );
    assert_eq!(
        parse_duration("99999999999999999999").unwrap_err(),
        "Duration is too large."
    );
    assert_eq!(
        parse_duration("9999999999999999d").unwrap_err(),
        "Duration is too large."
    );
}

#[test]
fn format_duration_uses_the_largest_exact_unit() {
    assert_eq!(format_duration(Duration::from_millis(1_500)), "1500ms");
    assert_eq!(format_duration(Duration::from_secs(1)), "1s");
    assert_eq!(format_duration(Duration::from_secs(90)), "90s");
    assert_eq!(format_duration(Duration::from_secs(1_800)), "30m");
    assert_eq!(format_duration(Duration::from_secs(86_400)), "1d");
    assert_eq!(format_duration(Duration::from_secs(90_000)), "25h");
}

#[test]
fn parse_whole_hours_rejects_partial_hours_and_zero() {
    assert_eq!(
        parse_whole_hours("24h").unwrap(),
        Duration::from_secs(86_400)
    );
    assert_eq!(
        parse_whole_hours("86400").unwrap(),
        Duration::from_secs(86_400)
    );
    assert_eq!(
        parse_whole_hours("7d").unwrap(),
        Duration::from_secs(604_800)
    );
    for raw in ["90m", "0", "3601s", "500ms"] {
        assert!(
            parse_whole_hours(raw)
                .unwrap_err()
                .contains("whole number of hours"),
            "{raw}"
        );
    }
}

fn cases_run_args(cli: Cli) -> (Option<Duration>, Option<u64>, Option<Duration>, Option<u64>) {
    match cli.command {
        Some(Commands::Cases {
            action:
                CasesAction::Run {
                    wait_options:
                        commands::cases::WaitArgs {
                            timeout,
                            timeout_ms,
                            interval,
                            interval_ms,
                        },
                    ..
                },
        }) => (timeout, timeout_ms, interval, interval_ms),
        _ => panic!("expected cases run command"),
    }
}

#[test]
fn cases_run_duration_flags_replace_millisecond_flags() {
    let cli = Cli::try_parse_from([
        "hooklistener",
        "cases",
        "run",
        "ep_1",
        "--target",
        "cli",
        "--timeout",
        "2m",
        "--interval",
        "500ms",
    ])
    .unwrap();
    assert_eq!(
        cases_run_args(cli),
        (
            Some(Duration::from_secs(120)),
            None,
            Some(Duration::from_millis(500)),
            None
        )
    );

    let cli = Cli::try_parse_from([
        "hooklistener",
        "cases",
        "run",
        "ep_1",
        "--target",
        "cli",
        "--timeout",
        "60",
    ])
    .unwrap();
    assert_eq!(cases_run_args(cli).0, Some(Duration::from_secs(60)));

    let cli = Cli::try_parse_from([
        "hooklistener",
        "cases",
        "run",
        "ep_1",
        "--target",
        "cli",
        "--timeout-ms",
        "500",
        "--interval-ms",
        "250",
    ])
    .unwrap();
    assert_eq!(cases_run_args(cli), (None, Some(500), None, Some(250)));

    assert_eq!(
        parse_error_kind([
            "hooklistener",
            "cases",
            "run",
            "ep_1",
            "--target",
            "cli",
            "--timeout",
            "1s",
            "--timeout-ms",
            "5",
        ]),
        clap::error::ErrorKind::ArgumentConflict
    );
    assert_eq!(
        parse_error_kind([
            "hooklistener",
            "cases",
            "run",
            "ep_1",
            "--target",
            "cli",
            "--interval",
            "1s",
            "--interval-ms",
            "500",
        ]),
        clap::error::ErrorKind::ArgumentConflict
    );
}

#[test]
fn cases_run_params_convert_durations_to_milliseconds() {
    let params = build_case_run_params(CaseRunInput {
        target: Some("cli".to_string()),
        target_url: None,
        target_id: None,
        target_name: None,
        wait: true,
        timeout: Some(Duration::from_secs(120)),
        timeout_ms: None,
        interval: Some(Duration::from_millis(500)),
        interval_ms: None,
    })
    .unwrap();
    assert_eq!(params.timeout_ms, Some(120_000));
    assert_eq!(params.interval_ms, Some(500));
}

#[test]
fn hidden_millisecond_flags_win_over_defaulted_duration_flags() {
    assert_eq!(
        resolve_millis_flag(
            Some(Duration::from_secs(1)),
            Some(250),
            "interval-ms",
            "interval"
        ),
        Some(250)
    );
    assert_eq!(
        resolve_millis_flag(
            Some(Duration::from_secs(2)),
            None,
            "interval-ms",
            "interval"
        ),
        Some(2_000)
    );
    assert_eq!(
        resolve_millis_flag(None, None, "timeout-ms", "timeout"),
        None
    );
}

#[test]
fn tunnel_events_interval_accepts_durations_and_hidden_milliseconds() {
    fn interval_args(cli: Cli) -> (Duration, Option<u64>) {
        match cli.command {
            Some(Commands::Tunnel {
                action:
                    Some(TunnelAction::Events {
                        interval,
                        interval_ms,
                        ..
                    }),
                ..
            }) => (interval, interval_ms),
            _ => panic!("expected tunnel events command"),
        }
    }

    let cli = Cli::try_parse_from(["hooklistener", "tunnel", "events"]).unwrap();
    assert_eq!(interval_args(cli), (Duration::from_secs(1), None));

    let cli = Cli::try_parse_from([
        "hooklistener",
        "tunnel",
        "events",
        "--follow",
        "--interval",
        "2s",
    ])
    .unwrap();
    assert_eq!(interval_args(cli), (Duration::from_secs(2), None));

    let cli =
        Cli::try_parse_from(["hooklistener", "tunnel", "events", "--interval-ms", "250"]).unwrap();
    let (interval, interval_ms) = interval_args(cli);
    assert_eq!(interval_ms, Some(250));
    assert_eq!(
        resolve_millis_flag(Some(interval), interval_ms, "interval-ms", "interval"),
        Some(250)
    );

    assert_eq!(
        parse_error_kind([
            "hooklistener",
            "tunnel",
            "events",
            "--interval",
            "2s",
            "--interval-ms",
            "1",
        ]),
        clap::error::ErrorKind::ArgumentConflict
    );
}

#[test]
fn anon_ttl_flags_accept_seconds_and_units() {
    fn create_ttl(args: &[&str]) -> Duration {
        match Cli::try_parse_from(args).unwrap().command {
            Some(Commands::Anon {
                action: AnonAction::Create { ttl },
            }) => ttl,
            _ => panic!("expected anon create command"),
        }
    }
    fn tunnel_ttl(args: &[&str]) -> Duration {
        match Cli::try_parse_from(args).unwrap().command {
            Some(Commands::Anon {
                action: AnonAction::Tunnel { ttl, .. },
            }) => ttl,
            _ => panic!("expected anon tunnel command"),
        }
    }

    assert_eq!(
        create_ttl(&["hooklistener", "anon", "create"]),
        Duration::from_secs(86_400)
    );
    assert_eq!(
        create_ttl(&["hooklistener", "anon", "create", "--ttl", "3600"]),
        Duration::from_secs(3_600)
    );
    assert_eq!(
        create_ttl(&["hooklistener", "anon", "create", "--ttl", "7d"]),
        Duration::from_secs(604_800)
    );

    assert_eq!(
        tunnel_ttl(&["hooklistener", "anon", "tunnel"]),
        Duration::from_secs(900)
    );
    assert_eq!(
        tunnel_ttl(&["hooklistener", "anon", "tunnel", "--ttl", "900"]),
        Duration::from_secs(900)
    );
    assert_eq!(
        tunnel_ttl(&["hooklistener", "anon", "tunnel", "--ttl", "10m"]),
        Duration::from_secs(600)
    );

    let error = parse_error(["hooklistener", "anon", "tunnel", "--ttl", "31m"]);
    assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
    assert!(error.to_string().contains("between 1m and 30m"), "{error}");
}

#[test]
fn share_create_expires_in_accepts_whole_hours_and_hidden_hours_flag() {
    fn expiry(args: &[&str]) -> (Option<Duration>, Option<u64>) {
        match Cli::try_parse_from(args).unwrap().command {
            Some(Commands::Share {
                action:
                    ShareAction::Create {
                        expires_in,
                        expires_in_hours,
                        ..
                    },
            }) => (expires_in, expires_in_hours),
            _ => panic!("expected share create command"),
        }
    }

    assert_eq!(
        expiry(&["hooklistener", "share", "create", "req_1"]),
        (None, None)
    );
    assert_eq!(
        expiry(&[
            "hooklistener",
            "share",
            "create",
            "req_1",
            "--expires-in",
            "24h"
        ]),
        (Some(Duration::from_secs(86_400)), None)
    );
    assert_eq!(
        expiry(&[
            "hooklistener",
            "share",
            "create",
            "req_1",
            "--expires-in-hours",
            "24"
        ]),
        (None, Some(24))
    );
    assert_eq!(duration_hours(Duration::from_secs(604_800)), 168);

    let error = parse_error([
        "hooklistener",
        "share",
        "create",
        "req_1",
        "--expires-in",
        "90m",
    ]);
    assert!(
        error.to_string().contains("whole number of hours"),
        "{error}"
    );

    assert_eq!(
        parse_error_kind([
            "hooklistener",
            "share",
            "create",
            "req_1",
            "--expires-in",
            "24h",
            "--expires-in-hours",
            "24",
        ]),
        clap::error::ErrorKind::ArgumentConflict
    );
}

#[test]
fn monitor_interval_is_a_closed_set_of_minutes() {
    fn create_interval(raw: &str) -> MonitorInterval {
        match Cli::try_parse_from([
            "hooklistener",
            "monitor",
            "create",
            "API",
            "https://example.com/health",
            "--interval",
            raw,
        ])
        .unwrap()
        .command
        {
            Some(Commands::Monitor {
                action: MonitorAction::Create { interval, .. },
            }) => interval,
            _ => panic!("expected monitor create command"),
        }
    }

    assert_eq!(create_interval("5"), MonitorInterval::M5);
    assert_eq!(create_interval("1h"), MonitorInterval::M60);
    assert_eq!(create_interval("60M"), MonitorInterval::M60);
    assert_eq!(create_interval("10m"), MonitorInterval::M10);
    assert_eq!(MonitorInterval::M30.minutes(), 30);

    match Cli::try_parse_from([
        "hooklistener",
        "monitor",
        "create",
        "API",
        "https://example.com/health",
    ])
    .unwrap()
    .command
    {
        Some(Commands::Monitor {
            action: MonitorAction::Create { interval, .. },
        }) => assert_eq!(interval, MonitorInterval::M5),
        _ => panic!("expected monitor create command"),
    }

    match Cli::try_parse_from([
        "hooklistener",
        "monitor",
        "update",
        "mon_1",
        "--interval",
        "10m",
    ])
    .unwrap()
    .command
    {
        Some(Commands::Monitor {
            action: MonitorAction::Update { interval, .. },
        }) => assert_eq!(interval, Some(MonitorInterval::M10)),
        _ => panic!("expected monitor update command"),
    }

    for raw in ["7", "2h", "90s"] {
        assert_eq!(
            parse_error_kind([
                "hooklistener",
                "monitor",
                "create",
                "API",
                "https://example.com/health",
                "--interval",
                raw,
            ]),
            clap::error::ErrorKind::InvalidValue,
            "{raw}"
        );
    }
}

#[test]
fn cases_run_clap_shape_matches_expected_command() {
    let cli = Cli::try_parse_from([
        "hooklistener",
        "cases",
        "run",
        "ep_123",
        "--target",
        "cli",
        "--wait",
        "--timeout",
        "60s",
        "--json",
    ])
    .unwrap();

    assert!(cli.json);
    match cli.command.unwrap() {
        Commands::Cases {
            action:
                CasesAction::Run {
                    endpoint_id,
                    destination,
                    wait,
                    wait_options,
                    ..
                },
        } => {
            assert_eq!(endpoint_id, "ep_123");
            assert_eq!(destination.target.as_deref(), Some("cli"));
            assert!(wait);
            assert_eq!(wait_options.timeout, Some(Duration::from_secs(60)));
        }
        _ => panic!("expected cases run command"),
    }
}

#[test]
fn command_output_monitor_table_snapshot() {
    let output = render_table(
        &["ID", "Status", "Method", "Int", "URL", "Name"],
        [[
            "mon_123",
            monitor_status_label(Some("up")),
            "GET",
            "5m",
            "https://serpgoblin.com",
            "SerpGoblin",
        ]],
    );

    assert_no_emoji(&output);
    assert_no_ansi_escape(&output);
    insta::assert_snapshot!("command_output_monitor_table", output);
}

#[test]
fn command_output_table_family_snapshot() {
    let output = render_snapshot_sections(vec![
        (
            "org list",
            render_table(
                &["", "ID", "Name"],
                [
                    ["*", "org_123", "Hooklistener Labs"],
                    ["", "org_456", "Platform Team"],
                ],
            ),
        ),
        (
            "endpoint list",
            render_table(
                &["ID", "Slug", "Status", "Webhook URL", "Name"],
                [[
                    "ep_123",
                    "github-webhooks",
                    "active",
                    "https://hooks.example.dev/github-webhooks",
                    "GitHub",
                ]],
            ),
        ),
        (
            "endpoint request list",
            render_table(
                &["ID", "Method", "URL", "Remote"],
                [[
                    "req_123",
                    "POST",
                    "/webhooks/github/push?delivery=1c07ce58",
                    "172.71.190.83",
                ]],
            ),
        ),
        (
            "request forwards",
            render_table(
                &["ID", "Method", "Status", "Duration", "Target"],
                [
                    [
                        "fwd_123",
                        "POST",
                        "200",
                        "42ms",
                        "http://localhost:3000/webhooks",
                    ],
                    [
                        "fwd_456",
                        "POST",
                        "-",
                        "-",
                        "http://localhost:3001/webhooks\n  [ERR] connection refused",
                    ],
                ],
            ),
        ),
        (
            "static tunnel list",
            render_table(
                &["ID", "Slug", "Name"],
                [["tun_123", "acme-dev", "Development tunnel"]],
            ),
        ),
        (
            "anon events",
            render_table(
                &["ID", "Method", "Received At"],
                [["evt_123", "POST", "2026-05-29T12:00:00Z"]],
            ),
        ),
        (
            "share list",
            render_table(
                &["ID", "Token", "Fwds", "Views", "Protected", "Expires At"],
                [[
                    "shr_123",
                    "share_abcdef123456",
                    "yes",
                    "14",
                    "no",
                    "2026-06-05T12:00:00Z",
                ]],
            ),
        ),
        (
            "monitor checks",
            render_table(
                &["ID", "Status", "Code", "Response", "Checked At", "Error"],
                [
                    ["chk_123", "up", "200", "184ms", "2026-05-29T12:00:00Z", "-"],
                    [
                        "chk_456",
                        "down",
                        "500",
                        "901ms",
                        "2026-05-29T12:05:00Z",
                        "timeout",
                    ],
                ],
            ),
        ),
    ]);

    insta::assert_snapshot!("command_output_table_family", output);
}

#[test]
fn command_output_detail_blocks_snapshot() {
    let output = render_snapshot_sections(vec![
        (
            "endpoint show",
            render_field_block(&[
                output_field("ID", "ep_123"),
                output_field("SLUG", "github-webhooks"),
                output_field("STATUS", "active"),
                output_field("WEBHOOK URL", "https://hooks.example.dev/github-webhooks"),
                output_field("NAME", "GitHub"),
                output_field("CREATED AT", "2026-05-29T12:00:00Z"),
            ]),
        ),
        (
            "request show",
            format!(
                "{}\nHEADERS\n  content-type: \"application/json\"\n  x-github-delivery: \"1c07ce58\"\n\nQUERY PARAMS\n  delivery=\"1c07ce58\"\n\nBODY\n{}",
                render_field_block(&[
                    output_field("REQUEST ID", "req_123"),
                    output_field("METHOD", "POST"),
                    output_field("PATH", "/webhooks/github/push"),
                    output_field("URL", "/webhooks/github/push?delivery=1c07ce58"),
                    output_field("REMOTE", "172.71.190.83"),
                    output_field("CONTENT LEN", "10354"),
                    output_field("CREATED AT", "2026-05-29T12:00:00Z"),
                ])
                .trim_end(),
                r#"{"ref":"refs/heads/main","repository":"hooklistener"}"#
            ),
        ),
        (
            "forward show",
            format!(
                "{}\nREQUEST HEADERS\n  content-type: \"application/json\"\n\nRESPONSE HEADERS\n  server: \"local-dev\"\n\nRESPONSE BODY\n{}",
                render_field_block(&[
                    output_field("FORWARD ID", "fwd_123"),
                    output_field("REQUEST ID", "req_123"),
                    output_field("TARGET URL", "http://localhost:3000/webhooks"),
                    output_field("METHOD", "POST"),
                    output_field("STATUS", "200"),
                    output_field("DURATION", "42ms"),
                    output_field("ATTEMPTED AT", "2026-05-29T12:00:01Z"),
                ])
                .trim_end(),
                r#"{"ok":true}"#
            ),
        ),
        (
            "monitor show",
            render_field_block(&[
                output_field("ID", "mon_123"),
                output_field("NAME", "SerpGoblin"),
                output_field("URL", "https://serpgoblin.com"),
                output_field("METHOD", "GET"),
                output_field("STATUS", "up"),
                output_field("ENABLED", "yes"),
                output_field("EXPECTED", "200"),
                output_field("INTERVAL", "5m"),
                output_field("THRESHOLD", "3"),
                output_field("FAILURES", "0"),
                output_field("NOTIFY", "email=on, slack=off"),
                output_field("LAST CHECK", "2026-05-29T12:00:00Z"),
            ]),
        ),
    ]);

    insta::assert_snapshot!("command_output_detail_blocks", output);
}

#[test]
fn command_output_empty_states_snapshot() {
    let output = render_snapshot_sections(vec![
        (
            "endpoint list",
            render_empty_status(
                "NO DEBUG ENDPOINTS FOUND",
                "Run `hooklistener endpoint create <name>` to create one.",
            ),
        ),
        (
            "request list",
            render_empty_status(
                "NO REQUESTS FOUND",
                "Send a webhook, then run `hooklistener endpoint list-requests <endpoint-id>` again.",
            ),
        ),
        (
            "forward list",
            render_empty_status(
                "NO FORWARDS FOUND",
                "Run `hooklistener endpoint forward-request <endpoint-id> <request-id> <url>`.",
            ),
        ),
        (
            "share list",
            render_empty_status(
                "NO SHARES FOUND",
                "Run `hooklistener share create <request-id>` to create one.",
            ),
        ),
        (
            "monitor list",
            render_empty_status(
                "NO UPTIME MONITORS FOUND",
                "Run `hooklistener monitor create <name> <url>` to create one.",
            ),
        ),
        (
            "monitor checks",
            render_empty_status(
                "NO CHECKS RECORDED",
                "Wait for the first interval, then run `hooklistener monitor checks <monitor-id>`.",
            ),
        ),
        (
            "static tunnel list",
            render_empty_status(
                "NO STATIC TUNNELS FOUND",
                "Run `hooklistener static-tunnel create <slug>` to reserve one.",
            ),
        ),
        (
            "anon events",
            render_empty_status(
                "NO EVENTS CAPTURED",
                "Send a webhook, then run `hooklistener anon list-events <endpoint-id> --token <viewer-token>`.",
            ),
        ),
    ]);

    assert!(output.lines().all(|line| line.chars().count() <= 80));
    insta::assert_snapshot!("command_output_empty_states", output);
}

#[test]
fn command_output_pagination_snapshot() {
    let output = format_pagination_line(&api::Pagination {
        page: 2,
        page_size: 25,
        total_count: 120,
        total_pages: 5,
    });

    assert_eq!(output, "PAGE 2/5  PAGE SIZE 25  TOTAL 120");
    insta::assert_snapshot!("command_output_pagination", output);
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

#[test]
fn forward_request_receipt_includes_agent_links() {
    let response = api::EndpointRequestForwardResponse {
        forward_id: "fwd_123".to_string(),
        debug_request_id: "req_123".to_string(),
        target_url: "https://example.com/webhook".to_string(),
        status: "pending".to_string(),
    };

    let receipt = forward_request_receipt("org_123", "ep_123", "req_123", &response);

    assert_eq!(receipt["status"], "pending");
    assert_eq!(receipt["delivery_status"], "queued");
    assert_eq!(receipt["resource_uri"], "hooklistener://forwards/fwd_123");
    assert_eq!(
        receipt["request_resource_uri"],
        "hooklistener://requests/req_123"
    );
    assert_eq!(receipt["poll_url"], "/api/v1/forwards/fwd_123");
    assert_eq!(
        receipt["resources"]["request_forwards"],
        "hooklistener://requests/req_123/forwards"
    );
    assert_eq!(
        receipt["next_actions"][0],
        "hooklistener endpoint show-forward fwd_123"
    );
}

#[test]
fn listen_started_receipt_includes_endpoint_resources() {
    let endpoint = api::DebugEndpointSummary {
        id: "ep_123".to_string(),
        name: "GitHub".to_string(),
        slug: "github-webhooks".to_string(),
        status: "active".to_string(),
        webhook_url: "https://hooks.example.dev/github-webhooks".to_string(),
        created_at: None,
        updated_at: None,
    };

    let receipt = listen_started_receipt(
        "github-webhooks",
        "http://localhost:3000/webhooks",
        "wss://api.example.dev/socket/websocket",
        Some(&endpoint),
    );

    assert_eq!(receipt["type"], "receipt");
    assert_eq!(receipt["event"], "listen_started");
    assert_eq!(receipt["status"], "running");
    assert_eq!(
        receipt["resource_uri"],
        "hooklistener://cli/listen/github-webhooks"
    );
    assert_eq!(receipt["endpoint"]["id"], "ep_123");
    assert_eq!(
        receipt["resources"]["endpoint"],
        "hooklistener://endpoints/ep_123"
    );
    assert_eq!(
        receipt["resources"]["requests"],
        "hooklistener://endpoints/ep_123/requests"
    );
}

#[test]
fn listen_webhook_event_includes_request_resource_uri() {
    let request = models::WebhookRequest {
        id: "req_123".to_string(),
        timestamp: 1_781_000_000,
        remote_addr: "Tunnel".to_string(),
        headers: std::collections::HashMap::new(),
        content_length: 42,
        method: "POST".to_string(),
        url: "/webhooks/github".to_string(),
        path: Some("/webhooks/github".to_string()),
        query_params: std::collections::HashMap::new(),
        created_at: "2026-07-08T10:00:00Z".to_string(),
        body_preview: None,
        body: None,
    };
    let event = TunnelEvent::WebhookReceived(Box::new(request));
    let receipt = listen_event_receipt(
        &event,
        "github-webhooks",
        "http://localhost:3000/webhooks",
        None,
    );

    assert_eq!(receipt["event"], "webhook_received");
    assert_eq!(receipt["request_id"], "req_123");
    assert_eq!(receipt["resource_uri"], "hooklistener://requests/req_123");
    assert_eq!(
        receipt["endpoint_resource_uri"],
        "hooklistener://endpoints/by-slug/github-webhooks"
    );
    assert_eq!(receipt["request"]["method"], "POST");
}

#[test]
fn listen_forward_success_event_includes_delivery_receipt() {
    let event = TunnelEvent::ForwardSuccess {
        request_id: "req_123".to_string(),
        target_url: "http://localhost:3000/webhooks/github".to_string(),
        status: 204,
        duration_ms: 37,
    };
    let receipt = listen_event_receipt(
        &event,
        "github-webhooks",
        "http://localhost:3000/webhooks",
        None,
    );

    assert_eq!(receipt["event"], "forward_succeeded");
    assert_eq!(receipt["status"], "succeeded");
    assert_eq!(receipt["status_code"], 204);
    assert_eq!(receipt["duration_ms"], 37);
    assert_eq!(
        receipt["request_resource_uri"],
        "hooklistener://requests/req_123"
    );
}

#[test]
fn tunnel_established_event_includes_public_url_and_resource() {
    let event = TunnelEvent::TunnelEstablished {
        subdomain: "plant-07.hook.events".to_string(),
        tunnel_id: "tun_123".to_string(),
        is_static: false,
    };
    let receipt = tunnel_event_receipt(&event, "localhost", 3000, Some("org_123"), None);

    assert_eq!(receipt["type"], "receipt");
    assert_eq!(receipt["event"], "tunnel_established");
    assert_eq!(receipt["resource_uri"], "hooklistener://tunnels/tun_123");
    assert_eq!(receipt["public_url"], "https://plant-07.hook.events");
    assert_eq!(receipt["local_target_url"], "http://localhost:3000");
    assert_eq!(receipt["organization_id"], "org_123");
}

#[test]
fn tunnel_request_event_includes_local_target_and_request_resource() {
    let mut headers = std::collections::HashMap::new();
    headers.insert("content-type".to_string(), "application/json".to_string());
    headers.insert("authorization".to_string(), "Bearer secret".to_string());
    let event = TunnelEvent::RequestReceived {
        request_id: "req_123".to_string(),
        method: "POST".to_string(),
        path: "webhooks/github".to_string(),
        headers,
        body: Some("{\"ok\":true}".to_string()),
        query_string: "delivery=abc".to_string(),
        replay: false,
    };
    let receipt = tunnel_event_receipt(&event, "127.0.0.1", 8080, None, Some("dev"));

    assert_eq!(receipt["event"], "request_received");
    assert_eq!(receipt["resource_uri"], "hooklistener://requests/req_123");
    assert_eq!(
        receipt["local_target_url"],
        "http://127.0.0.1:8080/webhooks/github?delivery=abc"
    );
    assert_eq!(receipt["body_size"], 11);
    assert_eq!(receipt["headers"]["content-type"], "application/json");
    assert_eq!(receipt["headers"]["authorization"], "[REDACTED]");
    assert!(!receipt.to_string().contains("Bearer secret"));
}

#[test]
fn tunnel_request_event_redacts_sensitive_headers() {
    let headers = std::collections::HashMap::from([
        (
            "authorization".to_string(),
            "Bearer request-secret".to_string(),
        ),
        ("cookie".to_string(), "session=request-secret".to_string()),
        ("x-api-key".to_string(), "request-api-key".to_string()),
        ("content-type".to_string(), "application/json".to_string()),
    ]);
    let event = TunnelEvent::RequestReceived {
        request_id: "req_secure".to_string(),
        method: "POST".to_string(),
        path: "/webhook".to_string(),
        headers,
        body: None,
        query_string: String::new(),
        replay: false,
    };

    let receipt = tunnel_event_receipt(&event, "127.0.0.1", 8080, None, None);

    assert_eq!(receipt["headers"]["authorization"], "[REDACTED]");
    assert_eq!(receipt["headers"]["cookie"], "[REDACTED]");
    assert_eq!(receipt["headers"]["x-api-key"], "[REDACTED]");
    assert_eq!(receipt["headers"]["content-type"], "application/json");
    let output = receipt.to_string();
    assert!(!output.contains("request-secret"));
    assert!(!output.contains("request-api-key"));
}

#[test]
fn tunnel_response_event_redacts_sensitive_headers() {
    let event = TunnelEvent::RequestForwarded {
        request_id: "req_secure".to_string(),
        status: 200,
        duration_ms: 10,
        response_headers: std::collections::HashMap::from([
            (
                "set-cookie".to_string(),
                "session=response-secret".to_string(),
            ),
            ("x-auth-token".to_string(), "response-token".to_string()),
            ("content-type".to_string(), "application/json".to_string()),
        ]),
        response_body: None,
    };

    let receipt = tunnel_event_receipt(&event, "127.0.0.1", 8080, None, None);

    assert_eq!(receipt["response_headers"]["set-cookie"], "[REDACTED]");
    assert_eq!(receipt["response_headers"]["x-auth-token"], "[REDACTED]");
    assert_eq!(
        receipt["response_headers"]["content-type"],
        "application/json"
    );
    let output = receipt.to_string();
    assert!(!output.contains("response-secret"));
    assert!(!output.contains("response-token"));
}

#[test]
fn buffered_summary_event_receipt_includes_count_and_oldest() {
    let event = TunnelEvent::BufferedSummary {
        count: 3,
        oldest_captured_at: Some("2026-07-14T09:00:00Z".to_string()),
    };

    let receipt = tunnel_event_receipt(&event, "localhost", 3000, None, None);

    assert_eq!(receipt["event"], "buffered_summary");
    assert_eq!(receipt["count"], 3);
    assert_eq!(receipt["oldest_captured_at"], "2026-07-14T09:00:00Z");
}

#[test]
fn buffered_replayed_event_receipt_includes_status() {
    let event = TunnelEvent::BufferedReplayed {
        capture_id: "cap_123".to_string(),
        status: 204,
    };

    let receipt = tunnel_event_receipt(&event, "localhost", 3000, None, None);

    assert_eq!(receipt["event"], "buffered_replayed");
    assert_eq!(receipt["capture_id"], "cap_123");
    assert_eq!(receipt["status_code"], 204);
    assert_eq!(receipt["resource_uri"], "hooklistener://requests/cap_123");
}

#[test]
fn buffered_replay_failed_event_receipt_includes_reason() {
    let event = TunnelEvent::BufferedReplayFailed {
        capture_id: "cap_123".to_string(),
        reason: "local_unreachable".to_string(),
    };

    let receipt = tunnel_event_receipt(&event, "localhost", 3000, None, None);

    assert_eq!(receipt["event"], "buffered_replay_failed");
    assert_eq!(receipt["capture_id"], "cap_123");
    assert_eq!(receipt["error"], "local_unreachable");
}

#[test]
fn tunnel_request_event_marks_replay() {
    let event = TunnelEvent::RequestReceived {
        request_id: "cap_456".to_string(),
        method: "POST".to_string(),
        path: "/webhook".to_string(),
        headers: std::collections::HashMap::new(),
        body: None,
        query_string: String::new(),
        replay: true,
    };

    let receipt = tunnel_event_receipt(&event, "localhost", 3000, None, None);

    assert_eq!(receipt["event"], "request_received");
    assert_eq!(receipt["replay"], true);
}

#[test]
fn tunnel_stream_gap_is_recoverable_and_does_not_affect_delivery() {
    let event = TunnelEvent::StreamGap { dropped_events: 7 };
    let receipt = tunnel_event_receipt(&event, "127.0.0.1", 8080, None, None);

    assert_eq!(receipt["event"], "stream_gap");
    assert_eq!(receipt["status"], "recoverable");
    assert_eq!(receipt["dropped_events"], 7);
    assert_eq!(receipt["delivery_affected"], false);
}

#[test]
fn validate_forward_target_url_requires_http_or_https() {
    assert!(validate_forward_target_url("http://localhost:3000/webhook").is_ok());
    assert!(validate_forward_target_url("https://example.com/webhook").is_ok());

    let err = validate_forward_target_url("ftp://example.com/webhook").unwrap_err();
    assert!(
        err.to_string().contains("Use http or https"),
        "unexpected error: {}",
        err
    );
}
