//! Tunnel lifecycle commands, contracts, errors, and confirmation.

use super::*;

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
