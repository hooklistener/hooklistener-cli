mod api;
mod app;
mod auth;
mod cli;
mod commands;
mod config;
mod credentials;
mod errors;
mod logger;
mod logo;
mod models;
mod output;
mod receipts;
mod render;
mod syntax;
mod target_policy;
mod theme;
mod tui;
mod tunnel;
mod tunnel_v3;
mod ui;
mod updater;

use anyhow::{Result, anyhow};
use clap::{CommandFactory, Parser};
use std::io;
use std::time::Duration;

use cli::{Cli, Commands};
use commands::completions::print_completions;
use commands::listen::effective_listen_ws_url;
use commands::login::run_login_flow;
use commands::tunnel::run_tunnel_lifecycle_command;
use errors::{command_exit_code, display_error};
use logger::{LogConfig, Logger};
use output::Stylize;
use render::{OutputStatus, output_field, print_json, print_status_block};

#[tokio::main]
async fn main() {
    let cli = Cli::parse();
    api::configure_server_url_security(cli.allow_insecure_dev_server);
    output::configure(cli.color, cli.json);
    let json = cli.json;

    if let Err(err) = run(cli).await {
        display_error(&err, json);
        std::process::exit(command_exit_code(&err));
    }
}

async fn run(cli: Cli) -> Result<()> {
    let Cli {
        command,
        json,
        color: _,
        yes,
        log_level,
        log_dir,
        log_stdout,
        allow_insecure_dev_server: _,
    } = cli;

    let Some(command) = command else {
        Cli::command().print_help()?;
        println!();
        return Ok(());
    };

    if json
        && matches!(
            &command,
            Commands::Login { .. } | Commands::Completions { .. }
        )
    {
        return Err(anyhow!(
            "This command does not support --json. Run it without --json."
        ));
    }

    // Spawn a background version check for eligible commands.
    let mut update_handle =
        if !json && !matches!(command, Commands::Update | Commands::Completions { .. }) {
            config::Config::load()
                .ok()
                .and_then(|cfg| updater::spawn_version_check(&cfg))
        } else {
            None
        };

    match command {
        Commands::Login { force } => {
            let log_config = LogConfig {
                level: log_level.as_str().to_string(),
                output_to_stdout: log_stdout,
                directory: log_dir
                    .clone()
                    .unwrap_or_else(|| LogConfig::default().directory),
                ..Default::default()
            };
            let _logger = Logger::new(log_config)?;
            run_login_flow(force).await?;
        }
        Commands::Listen {
            endpoint_slug,
            target,
            ws_url,
            allow_non_loopback,
            insecure_tls,
        } => {
            let ws_url = effective_listen_ws_url(ws_url.as_deref())?;
            // Initialize logging for tunnel
            let log_config = LogConfig {
                level: log_level.as_str().to_string(),
                output_to_stdout: false, // Disable stdout logging for TUI
                directory: log_dir
                    .clone()
                    .unwrap_or_else(|| LogConfig::default().directory),
                ..Default::default()
            };
            let _logger = Logger::new(log_config)?;

            commands::listen::execute(
                endpoint_slug,
                target,
                ws_url,
                allow_non_loopback,
                insecure_tls,
                json,
                &mut update_handle,
            )
            .await?;
        }
        Commands::Diagnostics { output } => {
            // Initialize minimal logging for diagnostics
            let log_config = LogConfig {
                level: log_level.as_str().to_string(),
                // Keep machine-readable output clean when --json is enabled.
                output_to_stdout: !json,
                directory: log_dir
                    .clone()
                    .unwrap_or_else(|| LogConfig::default().directory),
                ..Default::default()
            };
            let logger = Logger::new(log_config)?;
            logger.create_diagnostic_bundle(&output)?;
            if json {
                print_json(&serde_json::json!({
                    "status": "ok",
                    "output": output.display().to_string()
                }))?;
            } else {
                print_status_block(
                    OutputStatus::Ok,
                    "DIAGNOSTIC BUNDLE CREATED",
                    &[output_field("OUTPUT", output.display().to_string().bold())],
                );
            }
        }
        Commands::CleanLogs { keep } => {
            let directory = log_dir
                .clone()
                .unwrap_or_else(|| LogConfig::default().directory);
            std::fs::create_dir_all(&directory)?;
            let removed = Logger::cleanup_old_logs(&directory, keep)?;
            if json {
                print_json(&serde_json::json!({
                    "removed": removed,
                    "directory": directory.display().to_string(),
                    "keep": keep
                }))?;
            } else {
                print_status_block(
                    OutputStatus::Ok,
                    "LOGS CLEANED",
                    &[
                        output_field("REMOVED", removed.to_string().bold()),
                        output_field("DIRECTORY", directory.display().to_string().dim()),
                        output_field("KEEP", keep.to_string().bold()),
                    ],
                );
            }
        }
        Commands::Config { action } => commands::config::execute(action, json).await?,
        Commands::Logout => commands::login::logout(json).await?,
        Commands::Org { action } => commands::org::execute(action, json).await?,
        Commands::Endpoint { action } => {
            if commands::endpoint::execute(action, json, yes)
                .await?
                .is_break()
            {
                // A declined confirmation ends the command before the update notice.
                return Ok(());
            }
        }
        Commands::Cases { action } => {
            if commands::cases::execute(action, json).await? {
                std::process::exit(1);
            }
        }
        Commands::StaticTunnel { action } => {
            if commands::static_tunnel::execute(action, json, yes)
                .await?
                .is_break()
            {
                // A declined confirmation ends the command before the update notice.
                return Ok(());
            }
        }
        Commands::Anon { action } => {
            commands::anon::execute(action, json, &mut update_handle).await?;
        }
        Commands::Share { action } => {
            if commands::share::execute(action, json, yes)
                .await?
                .is_break()
            {
                // A declined confirmation ends the command before the update notice.
                return Ok(());
            }
        }
        Commands::Monitor { action } => {
            if commands::monitor::execute(action, json, yes)
                .await?
                .is_break()
            {
                // A declined confirmation ends the command before the update notice.
                return Ok(());
            }
        }
        Commands::Completions { shell } => {
            let mut stdout = io::stdout().lock();
            print_completions(shell, &mut stdout)?;
        }
        Commands::Update => {
            updater::run_self_update(json).await?;
        }
        Commands::Tunnel { action, target } => {
            // Initialize logging for tunnel
            let log_config = LogConfig {
                level: log_level.as_str().to_string(),
                output_to_stdout: false, // Disable stdout logging for TUI
                directory: log_dir
                    .clone()
                    .unwrap_or_else(|| LogConfig::default().directory),
                ..Default::default()
            };
            let _logger = Logger::new(log_config)?;
            run_tunnel_lifecycle_command(action, target, json, &mut update_handle).await?;
        }
    }

    // Await background version check and show notification if update is available
    if let Some(handle) = update_handle
        && let Ok(Ok(Some(new_version))) =
            tokio::time::timeout(Duration::from_millis(500), handle).await
    {
        updater::persist_check_result(Some(&new_version));
        updater::print_update_notification(&new_version);
    }

    Ok(())
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod audit_findings;
