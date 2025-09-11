use ratatui::{
    prelude::*,
    style::{Color, Style},
    text::{Line, Span},
};

pub struct JsonHighlighter;

impl JsonHighlighter {
    /// Highlights JSON content and returns formatted Lines
    pub fn highlight_json(json_str: &str) -> Vec<Line<'_>> {
        if json_str.trim().is_empty() {
            return vec![Line::from("")];
        }

        // Try to detect if this looks like JSON
        let trimmed = json_str.trim();
        if !trimmed.starts_with('{') && !trimmed.starts_with('[') {
            // Not JSON, return as plain text
            return Self::plain_text_lines(json_str);
        }

        let mut lines = Vec::new();
        let mut current_line = Vec::new();
        let chars: Vec<char> = json_str.chars().collect();
        let mut i = 0;

        while i < chars.len() {
            let ch = chars[i];

            match ch {
                // Structural characters
                '{' | '}' | '[' | ']' => {
                    current_line.push(Span::styled(
                        ch.to_string(),
                        Style::default()
                            .fg(Color::Yellow)
                            .add_modifier(Modifier::BOLD),
                    ));
                    i += 1;
                }
                // Colon
                ':' => {
                    current_line.push(Span::styled(
                        ch.to_string(),
                        Style::default().fg(Color::White),
                    ));
                    i += 1;
                }
                // Comma
                ',' => {
                    current_line.push(Span::styled(
                        ch.to_string(),
                        Style::default().fg(Color::Gray),
                    ));
                    i += 1;
                }
                // Strings (including keys and values)
                '"' => {
                    let (string_span, new_i) = Self::parse_string(&chars, i);
                    let is_key = Self::is_likely_key(&chars, new_i);

                    let style = if is_key {
                        Style::default()
                            .fg(Color::Blue)
                            .add_modifier(Modifier::BOLD)
                    } else {
                        Style::default().fg(Color::Green)
                    };

                    current_line.push(Span::styled(string_span, style));
                    i = new_i;
                }
                // Numbers
                c if c.is_ascii_digit() || c == '-' => {
                    let (number_span, new_i) = Self::parse_number(&chars, i);
                    current_line.push(Span::styled(
                        number_span,
                        Style::default().fg(Color::Magenta),
                    ));
                    i = new_i;
                }
                // Keywords (true, false, null)
                't' | 'f' | 'n' => {
                    if let Some((keyword, new_i)) = Self::parse_keyword(&chars, i) {
                        current_line.push(Span::styled(
                            keyword,
                            Style::default()
                                .fg(Color::Cyan)
                                .add_modifier(Modifier::BOLD),
                        ));
                        i = new_i;
                    } else {
                        current_line.push(Span::styled(
                            ch.to_string(),
                            Style::default().fg(Color::White),
                        ));
                        i += 1;
                    }
                }
                // Newlines
                '\n' => {
                    lines.push(Line::from(current_line.clone()));
                    current_line.clear();
                    i += 1;
                }
                // Whitespace and other characters
                _ => {
                    current_line.push(Span::styled(
                        ch.to_string(),
                        Style::default().fg(Color::White),
                    ));
                    i += 1;
                }
            }
        }

        // Add the last line if it's not empty
        if !current_line.is_empty() {
            lines.push(Line::from(current_line));
        }

        lines
    }

    /// Parse a JSON string starting from a quote
    fn parse_string(chars: &[char], start: usize) -> (String, usize) {
        let mut result = String::new();
        let mut i = start;

        if i < chars.len() && chars[i] == '"' {
            result.push(chars[i]);
            i += 1;

            while i < chars.len() {
                let ch = chars[i];
                result.push(ch);

                if ch == '"' && (i == start + 1 || chars[i - 1] != '\\') {
                    i += 1;
                    break;
                }
                i += 1;
            }
        }

        (result, i)
    }

    /// Parse a number starting from the current position
    fn parse_number(chars: &[char], start: usize) -> (String, usize) {
        let mut result = String::new();
        let mut i = start;

        while i < chars.len() {
            let ch = chars[i];
            if ch.is_ascii_digit() || ch == '.' || ch == '-' || ch == '+' || ch == 'e' || ch == 'E'
            {
                result.push(ch);
                i += 1;
            } else {
                break;
            }
        }

        (result, i)
    }

    /// Parse keywords like true, false, null
    fn parse_keyword(chars: &[char], start: usize) -> Option<(String, usize)> {
        let keywords = ["true", "false", "null"];

        for keyword in &keywords {
            if Self::matches_keyword(chars, start, keyword) {
                return Some((keyword.to_string(), start + keyword.len()));
            }
        }

        None
    }

    /// Check if a keyword matches at the given position
    fn matches_keyword(chars: &[char], start: usize, keyword: &str) -> bool {
        let keyword_chars: Vec<char> = keyword.chars().collect();

        if start + keyword_chars.len() > chars.len() {
            return false;
        }

        for (i, &expected) in keyword_chars.iter().enumerate() {
            if chars[start + i] != expected {
                return false;
            }
        }

        // Check that the keyword is not part of a longer identifier
        if start + keyword_chars.len() < chars.len() {
            let next_char = chars[start + keyword_chars.len()];
            if next_char.is_alphanumeric() || next_char == '_' {
                return false;
            }
        }

        true
    }

    /// Check if a string is likely a JSON key (followed by a colon)
    fn is_likely_key(chars: &[char], string_end: usize) -> bool {
        let mut i = string_end;

        // Skip whitespace
        while i < chars.len() && chars[i].is_whitespace() && chars[i] != '\n' {
            i += 1;
        }

        // Check if followed by a colon
        i < chars.len() && chars[i] == ':'
    }

    /// Convert plain text to lines without syntax highlighting
    fn plain_text_lines(text: &str) -> Vec<Line<'_>> {
        text.lines()
            .map(|line| {
                Line::from(Span::styled(
                    line.to_string(),
                    Style::default().fg(Color::White),
                ))
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_simple_json_highlighting() {
        let json = r#"{"name": "test", "value": 42}"#;
        let lines = JsonHighlighter::highlight_json(json);
        assert!(!lines.is_empty());
    }

    #[test]
    fn test_plain_text() {
        let text = "This is not JSON";
        let lines = JsonHighlighter::highlight_json(text);
        assert_eq!(lines.len(), 1);
    }

    #[test]
    fn test_complex_json_highlighting() {
        let json = r#"{
  "string_value": "hello world",
  "number_value": 123,
  "boolean_true": true,
  "boolean_false": false,
  "null_value": null,
  "nested_object": {
    "inner_key": "inner_value"
  },
  "array_value": [1, 2, "three", null]
}"#;
        let lines = JsonHighlighter::highlight_json(json);
        assert!(lines.len() > 5); // Should have multiple lines

        // Verify it doesn't crash with complex JSON
        assert!(!lines.is_empty());
    }

    #[test]
    fn test_malformed_json_fallback() {
        let malformed = r#"{"incomplete": json"#;
        let lines = JsonHighlighter::highlight_json(malformed);
        assert!(!lines.is_empty()); // Should still render something
    }

    #[test]
    fn test_empty_string() {
        let empty = "";
        let lines = JsonHighlighter::highlight_json(empty);
        assert_eq!(lines.len(), 1); // Should return one empty line
        assert!(lines[0].spans.is_empty() || lines[0].spans.len() == 1); // May be empty or contain empty span
    }

    #[test]
    fn test_array_json() {
        let json_array = r#"[{"id": 1}, {"id": 2}, {"id": 3}]"#;
        let lines = JsonHighlighter::highlight_json(json_array);
        assert!(!lines.is_empty());
    }
}
