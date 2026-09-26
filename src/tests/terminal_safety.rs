//! Terminal sanitizing of remote text in command output.

use super::*;

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
