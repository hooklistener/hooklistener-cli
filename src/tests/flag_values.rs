//! Value enums and flag validation.

use super::*;

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
