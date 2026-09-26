//! Duration parsing and duration-valued flags.

use super::*;

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
