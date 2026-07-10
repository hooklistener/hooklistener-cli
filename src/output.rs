use std::fmt::{self, Display};
use std::io::{self, IsTerminal};
use std::sync::atomic::{AtomicBool, Ordering};

use clap::ValueEnum;
use crossterm::style::{Attribute, Color, ContentStyle};

static STYLES_ENABLED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, ValueEnum)]
pub enum ColorMode {
    #[default]
    Auto,
    Always,
    Never,
}

pub fn configure(mode: ColorMode, json: bool) {
    let no_color = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
    let enabled = resolve_styles_enabled(
        mode,
        json,
        no_color,
        io::stdout().is_terminal(),
        io::stderr().is_terminal(),
    );
    STYLES_ENABLED.store(enabled, Ordering::Relaxed);
}

pub fn styles_enabled() -> bool {
    STYLES_ENABLED.load(Ordering::Relaxed)
}

fn resolve_styles_enabled(
    mode: ColorMode,
    json: bool,
    no_color: bool,
    stdout_is_terminal: bool,
    stderr_is_terminal: bool,
) -> bool {
    if json {
        return false;
    }

    match mode {
        ColorMode::Always => true,
        ColorMode::Never => false,
        ColorMode::Auto => !no_color && stdout_is_terminal && stderr_is_terminal,
    }
}

pub fn terminal_width() -> u16 {
    std::env::var("COLUMNS")
        .ok()
        .and_then(|value| value.parse::<u16>().ok())
        .filter(|width| *width > 0)
        .or_else(|| {
            io::stdout()
                .is_terminal()
                .then(crossterm::terminal::size)
                .transpose()
                .ok()
                .flatten()
                .map(|(width, _)| width)
        })
        .unwrap_or(80)
}

pub struct StyledValue<D> {
    content: D,
    style: ContentStyle,
}

impl<D: Display> Display for StyledValue<D> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        if styles_enabled() {
            write!(formatter, "{}", self.style.apply(&self.content))
        } else {
            self.content.fmt(formatter)
        }
    }
}

pub trait Stylize: Display + Sized {
    fn styled(self, style: ContentStyle) -> StyledValue<Self> {
        StyledValue {
            content: self,
            style,
        }
    }

    fn bold(self) -> StyledValue<Self> {
        let mut style = ContentStyle::new();
        style.attributes.set(Attribute::Bold);
        self.styled(style)
    }

    fn dim(self) -> StyledValue<Self> {
        let mut style = ContentStyle::new();
        style.attributes.set(Attribute::Dim);
        self.styled(style)
    }

    fn underlined(self) -> StyledValue<Self> {
        let mut style = ContentStyle::new();
        style.attributes.set(Attribute::Underlined);
        self.styled(style)
    }

    fn green(self) -> StyledValue<Self> {
        self.styled(ContentStyle {
            foreground_color: Some(Color::Green),
            ..ContentStyle::new()
        })
    }

    fn red(self) -> StyledValue<Self> {
        self.styled(ContentStyle {
            foreground_color: Some(Color::Red),
            ..ContentStyle::new()
        })
    }

    fn yellow(self) -> StyledValue<Self> {
        self.styled(ContentStyle {
            foreground_color: Some(Color::Yellow),
            ..ContentStyle::new()
        })
    }

    fn blue(self) -> StyledValue<Self> {
        self.styled(ContentStyle {
            foreground_color: Some(Color::Blue),
            ..ContentStyle::new()
        })
    }
}

impl<D: Display> Stylize for D {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_disables_styles_for_non_terminal_output() {
        assert!(!resolve_styles_enabled(
            ColorMode::Auto,
            false,
            false,
            false,
            false
        ));
    }

    #[test]
    fn auto_respects_no_color() {
        assert!(!resolve_styles_enabled(
            ColorMode::Auto,
            false,
            true,
            true,
            true
        ));
    }

    #[test]
    fn json_always_disables_styles() {
        assert!(!resolve_styles_enabled(
            ColorMode::Always,
            true,
            false,
            true,
            true
        ));
    }

    #[test]
    fn disabled_styles_render_plain_text() {
        STYLES_ENABLED.store(false, Ordering::Relaxed);

        assert_eq!("COMMAND FAILED".red().bold().to_string(), "COMMAND FAILED");
    }
}
