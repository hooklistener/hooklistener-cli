//! `cases run` target and flag handling.

use super::*;

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
