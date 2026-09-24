use super::*;
use clap::Parser;

#[test]
fn idempotency_keys_are_bounded_and_terminal_safe() {
    for key in [
        "",
        "short",
        "has a space",
        "has\nnewline",
        "control\u{1b}key",
        "unicode-é-key",
    ] {
        assert!(validate_action_key(key).is_err());
    }
    assert!(validate_action_key(&"k".repeat(201)).is_err());
    assert!(validate_action_key(&"k".repeat(200)).is_ok());
    assert!(validate_action_key("job-482-smoke").is_ok());
}

#[test]
fn preview_rejects_execution_keys() {
    for operation in ["run", "replay"] {
        assert!(
            crate::cli::Cli::try_parse_from([
                "hooklistener",
                "cases",
                operation,
                "resource-id",
                "--target",
                "cli",
                "--dry-run",
                "--idempotency-key",
                "job-482-smoke"
            ])
            .is_err()
        );
    }
}

#[test]
fn case_update_rejects_malformed_json_without_echoing_secrets() {
    let options = CaseOptions {
        headers: Some("{secret-credential".into()),
        ..Default::default()
    };
    let error = options.attrs().unwrap_err().to_string();
    assert_eq!(error, "--headers requires valid JSON.");
}

#[test]
fn case_assertions_reject_empty_and_non_object_subsets() {
    for value in ["{}", "[]", "true", "null"] {
        let options = CaseOptions {
            expect_json: Some(value.into()),
            ..Default::default()
        };
        assert!(options.attrs().is_err(), "accepted {value}");
    }
}

#[test]
fn headers_reject_injection_and_non_string_values() {
    for value in [
        r#"{"x-test":"value\r\nAuthorization: secret"}"#,
        r#"{"x-test":3}"#,
        r#"{"bad name":"v"}"#,
    ] {
        assert!(header_object(value).is_err());
    }
}

#[test]
fn clearing_assertions_is_explicit_in_request_body() {
    assert_eq!(CaseOptions::default().attrs().unwrap(), json!({}));
    assert_eq!(
        CaseOptions {
            clear_assertions: true,
            ..Default::default()
        }
        .attrs()
        .unwrap(),
        json!({"assertion_config":{}})
    );
}

#[test]
fn resource_ids_cannot_change_paths_or_queries() {
    for id in [
        "",
        "..",
        "../cases",
        "case?org=other",
        "case#fragment",
        "%2e%2e",
        "x/y",
        "x\\y",
    ] {
        assert!(validate_id(id).is_err(), "accepted {id}");
    }
    assert!(validate_id("e97d88bf-c4d0-4f30-8041-a43577f7c8da").is_ok());
}

#[test]
fn wait_settings_are_bounded_and_zero_is_explicit() {
    let wait = |timeout, timeout_ms, interval, interval_ms| WaitArgs {
        timeout,
        timeout_ms,
        interval,
        interval_ms,
    };
    assert_eq!(
        wait_settings(wait(None, None, None, None)).unwrap(),
        (Duration::from_secs(30), Duration::from_millis(250))
    );
    assert_eq!(
        wait_settings(wait(Some(Duration::ZERO), None, None, None))
            .unwrap()
            .0,
        Duration::ZERO
    );
    assert!(wait_settings(wait(Some(Duration::from_secs(7_200)), None, None, None)).is_err());
    assert!(wait_settings(wait(None, Some(3_600_001), None, None)).is_err());
    assert_eq!(
        wait_settings(wait(None, Some(1_500), None, Some(100))).unwrap(),
        (Duration::from_millis(1_500), Duration::from_millis(100))
    );
    assert_eq!(
        wait_settings(wait(None, None, Some(Duration::from_secs(2)), None))
            .unwrap()
            .1,
        Duration::from_secs(2)
    );
}

#[test]
fn destinations_reject_credentials_and_non_http_schemes_without_echoing_them() {
    for value in [
        "ftp://example.com",
        "https://user:secret@example.com",
        "https://example.com/#secret",
        "not a URL",
    ] {
        let error = validate_destination_url(value).unwrap_err().to_string();
        assert!(!error.contains("secret"));
    }
    assert!(validate_destination_url("https://example.com/webhooks?test=1").is_ok());
}

#[test]
fn parser_rejects_conflicting_or_invalid_case_flags() {
    for args in [
        vec![
            "cases",
            "update",
            "case-1",
            "--clear-assertions",
            "--expect-status",
            "200",
        ],
        vec!["cases", "save", "ep-1", "req-1", "--expect-status", "600"],
        vec![
            "cases",
            "run",
            "ep-1",
            "--target",
            "cli",
            "--dry-run",
            "--wait",
        ],
        vec![
            "cases",
            "replay",
            "case-1",
            "--target",
            "cli",
            "--target-url",
            "https://example.com",
        ],
        vec!["cases", "runs", "list", "ep-1", "--page-size", "101"],
        vec!["cases", "runs", "wait", "run-1", "--interval-ms", "0"],
        vec!["cases", "runs", "wait", "run-1", "--interval", "50ms"],
        vec!["cases", "runs", "wait", "run-1", "--interval", "31s"],
        vec![
            "cases",
            "runs",
            "wait",
            "run-1",
            "--timeout",
            "5 fortnights",
        ],
        vec![
            "cases",
            "runs",
            "wait",
            "run-1",
            "--interval",
            "1s",
            "--interval-ms",
            "1000",
        ],
        vec![
            "cases", "replay", "case-1", "--target", "cli", "--method", "TRACE",
        ],
        vec![
            "cases",
            "runs",
            "wait",
            "run-1",
            "--timeout",
            "1s",
            "--timeout-ms",
            "1000",
        ],
    ] {
        assert!(
            crate::cli::Cli::try_parse_from(std::iter::once("hooklistener").chain(args)).is_err()
        );
    }
}

#[test]
fn suite_alias_and_org_override_parse() {
    let cli = crate::cli::Cli::try_parse_from([
        "hooklistener",
        "cases",
        "run",
        "ep-1",
        "--target",
        "cli",
        "--suite",
        "suite-1",
        "--org",
        "other-org",
        "--json",
    ])
    .unwrap();
    let Some(crate::cli::Commands::Cases {
        action: CasesAction::Run {
            case_suite_id, org, ..
        },
    }) = cli.command
    else {
        panic!("expected case run");
    };
    assert_eq!(case_suite_id.as_deref(), Some("suite-1"));
    assert_eq!(org.as_deref(), Some("other-org"));
}

#[test]
fn run_failures_are_not_hidden_by_a_success_label() {
    let result: CaseRunResult = serde_json::from_value(json!({"status":"completed","result_status":"passed","async":false,"endpoint_id":"ep-1","target":{},"assertion_error_count":1})).unwrap();
    assert!(crate::commands::cases::case_run_failed(&result));
}
