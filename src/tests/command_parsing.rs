//! Command names, aliases, completions, and global flags.

use super::*;

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
