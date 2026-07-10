use ratatui::style::Color;

// ANSI roles are resolved by the user's terminal theme, unlike fixed RGB values.
pub const PRIMARY: Color = Color::Yellow;
pub const SECONDARY: Color = Color::Blue;
pub const SUCCESS: Color = Color::Green;
pub const ERROR: Color = Color::Red;
pub const WARNING: Color = Color::Yellow;
pub const INFO: Color = Color::Blue;
pub const MUTED: Color = Color::Reset;
pub const TEXT: Color = Color::Reset;
pub const ACCENT: Color = Color::Yellow;

pub const STRUCTURE: Color = PRIMARY;
pub const KEY: Color = SECONDARY;
pub const STRING: Color = SUCCESS;
pub const NUMBER: Color = PRIMARY;
pub const KEYWORD: Color = SECONDARY;
pub const PUNCTUATION: Color = MUTED;

pub fn for_http_status(status: u16) -> Color {
    if (200..300).contains(&status) {
        SUCCESS
    } else if (400..500).contains(&status) {
        WARNING
    } else if status >= 500 {
        ERROR
    } else {
        INFO
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn semantic_palette_uses_terminal_controlled_colors() {
        let colors = [
            PRIMARY, SECONDARY, SUCCESS, ERROR, WARNING, INFO, MUTED, TEXT, ACCENT,
        ];

        assert!(
            colors
                .into_iter()
                .all(|color| !matches!(color, Color::Rgb(..) | Color::Indexed(..)))
        );
    }

    #[test]
    fn neutral_text_inherits_terminal_foreground() {
        assert_eq!(TEXT, Color::Reset);
    }
}
