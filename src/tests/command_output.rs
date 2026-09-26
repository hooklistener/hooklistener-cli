//! Human-readable command output snapshots.

use super::*;

fn assert_no_ansi_escape(output: &str) {
    assert!(
        !output.contains("\u{1b}["),
        "output contains ANSI escape sequences: {output:?}"
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
