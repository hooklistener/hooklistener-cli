//! Human-readable terminal output: status blocks, fields, tables, and sanitizing.

use anyhow::Result;
use comfy_table::{ContentArrangement, Table, presets::UTF8_FULL_CONDENSED};
use std::io::{self, Write};

use crate::output::Stylize;
use crate::{api, output};

pub(crate) fn print_json<T: serde::Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string_pretty(value)?);
    Ok(())
}

pub fn print_json_line<T: serde::Serialize>(value: &T) -> Result<()> {
    println!("{}", serde_json::to_string(value)?);
    io::stdout().flush()?;
    Ok(())
}

pub const FIELD_LABEL_WIDTH: usize = 14;

#[derive(Clone, Copy)]
pub(crate) enum OutputStatus {
    Ok,
    Err,
    Warn,
    Info,
}

impl OutputStatus {
    fn token(self) -> &'static str {
        match self {
            Self::Ok => "[OK]",
            Self::Err => "[ERR]",
            Self::Warn => "[WARN]",
            Self::Info => "[INFO]",
        }
    }
}

pub(crate) struct OutputField {
    pub label: &'static str,
    pub value: String,
}

pub(crate) fn output_field(label: &'static str, value: impl std::fmt::Display) -> OutputField {
    OutputField {
        label,
        value: value.to_string(),
    }
}

pub fn output_label(label: &str) -> String {
    let normalized = label.trim().trim_end_matches(':').to_ascii_uppercase();
    format!("{normalized:<width$}", width = FIELD_LABEL_WIDTH)
}

pub fn output_title(title: &str) -> String {
    title.trim().trim_end_matches(':').to_ascii_uppercase()
}

pub(crate) fn format_status_line(status: OutputStatus, title: &str) -> String {
    format!("{} {}", status.token(), output_title(title))
}

pub(crate) fn format_field_line(label: &str, value: impl std::fmt::Display) -> String {
    format!("{} {}", output_label(label), value)
}

pub fn wrap_plain_text(value: &str, width: usize) -> Vec<String> {
    let width = width.max(1);
    let mut lines = Vec::new();

    for paragraph in value.lines() {
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            if current.is_empty() {
                current.push_str(word);
            } else if current.chars().count() + 1 + word.chars().count() <= width {
                current.push(' ');
                current.push_str(word);
            } else {
                lines.push(current);
                current = word.to_string();
            }
        }
        lines.push(current);
    }

    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

pub fn format_wrapped_field_lines(label: &str, value: &str, width: u16) -> String {
    let label = output_label(label);
    let prefix_width = label.chars().count() + 1;
    let value_width = usize::from(width).saturating_sub(prefix_width).max(1);
    let continuation = " ".repeat(prefix_width);

    wrap_plain_text(value, value_width)
        .into_iter()
        .enumerate()
        .map(|(index, line)| {
            if index == 0 {
                format!("{label} {line}")
            } else {
                format!("{continuation}{line}")
            }
        })
        .collect::<Vec<_>>()
        .join("\n")
}

pub fn styled_status_line(status: OutputStatus, title: &str) -> String {
    let line = format_status_line(status, title);
    match status {
        OutputStatus::Ok => line.green().bold().to_string(),
        OutputStatus::Err => line.red().bold().to_string(),
        OutputStatus::Warn => line.yellow().bold().to_string(),
        OutputStatus::Info => line.blue().bold().to_string(),
    }
}

pub fn format_pagination_line(p: &api::Pagination) -> String {
    format!(
        "PAGE {}/{}  PAGE SIZE {}  TOTAL {}",
        p.page, p.total_pages, p.page_size, p.total_count
    )
}

pub fn value_or_dash(value: Option<&str>) -> &str {
    value.unwrap_or("-")
}

pub fn yes_no(value: bool) -> &'static str {
    if value { "yes" } else { "no" }
}

#[cfg(test)]
pub(crate) fn render_status_block(
    status: OutputStatus,
    title: &str,
    fields: &[OutputField],
) -> String {
    let mut output = format_status_line(status, title);
    if !fields.is_empty() {
        output.push('\n');
        output.push('\n');
    }
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            output.push('\n');
        }
        output.push_str(&format_field_line(field.label, &field.value));
    }
    output.push('\n');
    output
}

pub fn print_status(status: OutputStatus, title: &str) {
    println!("{}", styled_status_line(status, title));
}

pub fn print_status_block(status: OutputStatus, title: &str, fields: &[OutputField]) {
    print_status(status, title);
    if !fields.is_empty() {
        println!();
    }
    for field in fields {
        print_field(field.label, &field.value);
    }
}

pub fn print_empty_state(title: &str, action: &str) {
    print_status(OutputStatus::Info, title);
    println!();
    println!(
        "{}",
        format_wrapped_field_lines("ACTION", action, output::terminal_width())
    );
}

pub fn eprint_field(label: &str, value: impl std::fmt::Display) {
    eprintln!("{} {}", output_label(label).bold(), value);
}

pub fn eprint_status(status: OutputStatus, title: &str) {
    eprintln!("{}", styled_status_line(status, title));
}

pub fn print_field(label: &str, value: impl std::fmt::Display) {
    println!("{} {}", output_label(label).bold(), value);
}

pub fn print_section(label: &str) {
    println!("{}", output_title(label).bold());
}

/// Print a dim context line like "ORGANIZATION abc123".
pub fn print_context(label: &str, value: &str) {
    println!(
        "{} {}",
        output_label(label).dim(),
        sanitize_terminal(value, TerminalTextLayout::Inline).dim()
    );
}

/// Print a pagination footer.
pub fn print_pagination(p: &api::Pagination) {
    println!("{}", format_pagination_line(p).dim());
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TerminalTextLayout {
    Inline,
    Block,
}

/// Escape terminal control characters while retaining readable, inert text.
///
/// Inline values escape every control character so untrusted input cannot move
/// the cursor or forge additional output. Block values additionally retain LF
/// and tab for payload readability; all other C0, DEL, and C1 controls remain
/// escaped, including ESC, BEL, CR, and the 8-bit OSC/CSI introducers.
pub fn sanitize_terminal(value: &str, layout: TerminalTextLayout) -> std::borrow::Cow<'_, str> {
    let should_escape = |character: char| {
        character.is_control()
            && !(layout == TerminalTextLayout::Block && matches!(character, '\n' | '\t'))
    };

    if !value.chars().any(should_escape) {
        return std::borrow::Cow::Borrowed(value);
    }

    let mut sanitized = String::with_capacity(value.len());
    for character in value.chars() {
        if should_escape(character) {
            sanitized.extend(character.escape_default());
        } else {
            sanitized.push(character);
        }
    }
    std::borrow::Cow::Owned(sanitized)
}

pub fn sanitize_terminal_display(value: impl std::fmt::Display) -> String {
    let value = value.to_string();
    sanitize_terminal(&value, TerminalTextLayout::Inline).into_owned()
}

pub fn truncate_terminal_inline(value: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }

    let sanitized = sanitize_terminal(value, TerminalTextLayout::Inline);
    if sanitized.chars().count() <= max_chars {
        return sanitized.into_owned();
    }

    let prefix = sanitized
        .chars()
        .take(max_chars.saturating_sub(1))
        .collect::<String>();
    format!("{prefix}…")
}

pub fn format_terminal_body(body: &str) -> String {
    sanitize_terminal(body, TerminalTextLayout::Block)
        .split('\n')
        .map(|line| format!("│ {line}"))
        .collect::<Vec<_>>()
        .join("\n")
}

/// Print a key-value map (headers, query params) with a bold section label.
pub fn print_key_value_map(
    label: &str,
    map: &std::collections::HashMap<String, serde_json::Value>,
    separator: &str,
) {
    if map.is_empty() {
        print_field(label, "(none)".dim());
    } else {
        print_section(label);
        for (key, value) in map {
            let value = value.to_string();
            println!(
                "  {}{}{}",
                sanitize_terminal(key, TerminalTextLayout::Inline).dim(),
                separator,
                sanitize_terminal(&value, TerminalTextLayout::Inline)
            );
        }
    }
}

/// Print a body section, showing "(empty)" when the body is absent or blank.
pub fn print_body_section(label: &str, body: Option<&str>) {
    match body {
        Some(body) if !body.is_empty() => {
            print_section(label);
            println!("{}", format_terminal_body(body));
        }
        _ => print_field(label, "(empty)".dim()),
    }
}

/// Create a pre-configured table with the standard preset and dynamic content arrangement.
pub fn new_table(headers: &[&str]) -> Table {
    let headers = headers
        .iter()
        .map(|header| output_title(header))
        .collect::<Vec<_>>();
    let mut table = Table::new();
    table
        .load_preset(UTF8_FULL_CONDENSED)
        .set_width(output::terminal_width())
        .set_content_arrangement(ContentArrangement::Dynamic)
        .set_header(headers);
    table
}

pub fn status_code_label(code: u16) -> String {
    code.to_string()
}

pub fn style_status_code(code: u16) -> String {
    let s = status_code_label(code);
    match code {
        200..=299 => s.green().to_string(),
        300..=399 => s.yellow().to_string(),
        400..=599 => s.red().to_string(),
        _ => s,
    }
}
