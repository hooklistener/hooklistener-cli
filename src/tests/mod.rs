use super::*;
use crate::cli::*;
use crate::commands::anon::*;
use crate::commands::cases::*;
use crate::commands::completions::*;
use crate::commands::config::*;
use crate::commands::endpoint::*;
use crate::commands::monitor::*;
use crate::commands::org::*;
use crate::commands::share::*;
use crate::commands::static_tunnel::*;
use crate::commands::tunnel::*;
use crate::commands::{WorkerCompletion, confirmation_is_yes, supervise_json_worker};
use crate::credentials::*;
use crate::errors::*;
use crate::receipts::*;
use crate::render::*;
use crate::tui::*;
use crate::tunnel::TunnelEvent;
use chrono::{Duration as ChronoDuration, Utc};
use clap::ValueEnum;
use std::path::PathBuf;
use tokio::sync::watch;

mod cases_run;
mod command_output;
mod command_parsing;
mod credentials;
mod durations;
mod flag_values;
mod help;
mod lifecycle;
mod receipts;
mod runtime;
mod terminal_safety;

fn assert_no_emoji(output: &str) {
    assert!(
        !output.chars().any(|ch| {
            let code = ch as u32;
            (0x1F300..=0x1FAFF).contains(&code) || (0x2600..=0x27BF).contains(&code)
        }),
        "output contains emoji-like glyphs: {output}"
    );
}

fn parsed_tunnel_target(args: &[&str]) -> TunnelTarget {
    let cli = Cli::try_parse_from(args).expect("tunnel command");
    match cli.command.expect("parsed command") {
        Commands::Tunnel {
            action: None,
            target,
        } => target.resolve(),
        Commands::Tunnel {
            action: Some(TunnelAction::Start(action_target) | TunnelAction::Prepare(action_target)),
            target,
        } => target.merge(action_target).resolve(),
        _ => panic!("expected a tunnel target command"),
    }
}

fn parse_error<I, T>(args: I) -> clap::Error
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    match Cli::try_parse_from(args) {
        Ok(_) => panic!("expected argument parsing to fail"),
        Err(err) => err,
    }
}

fn parse_error_kind<I, T>(args: I) -> clap::error::ErrorKind
where
    I: IntoIterator<Item = T>,
    T: Into<std::ffi::OsString> + Clone,
{
    parse_error(args).kind()
}
