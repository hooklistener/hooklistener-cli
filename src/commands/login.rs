//! `login` and `logout` commands: the device-code sign-in flow.

use anyhow::{Result, anyhow};
use chrono::{Duration as ChronoDuration, Utc};
use crossterm::{
    cursor::MoveToColumn,
    execute,
    terminal::{Clear, ClearType},
};
use std::io::{self, Write};
use std::time::Duration;
use tokio::time::sleep;

use crate::credentials::{save_login_tokens, token_expiry_from_now};
use crate::output::Stylize;
use crate::render::{OutputStatus, output_label, print_field, print_json, print_status};
use crate::{api, auth, config};

pub async fn logout(json: bool) -> Result<()> {
    let mut config = config::Config::load()?;
    if config.access_token.is_none() && config.token_expires_at.is_none() {
        if json {
            print_json(&serde_json::json!({
                "status": "already_logged_out"
            }))?;
        } else {
            print_status(OutputStatus::Info, "ALREADY LOGGED OUT");
        }
    } else {
        // Best-effort revoke refresh token server-side
        if let Some(ref refresh_token) = config.refresh_token {
            let _ = api::revoke_refresh_token(refresh_token).await;
        }
        config.clear_token();
        config.save()?;
        if json {
            print_json(&serde_json::json!({
                "status": "logged_out"
            }))?;
        } else {
            print_status(OutputStatus::Ok, "LOGGED OUT");
        }
    }
    Ok(())
}

pub const SESSION_TOKEN_VALIDITY_DAYS: i64 = 60;

pub async fn run_login_flow(force_reauth: bool) -> Result<()> {
    let mut config = config::Config::load()?;

    if config.is_token_valid() && !force_reauth {
        println!();
        print_status(OutputStatus::Ok, "AUTHENTICATED");
        println!();
        print_field(
            "ACTION",
            format!(
                "Run {} to start forwarding webhooks.",
                "hooklistener listen <endpoint-slug>".bold()
            ),
        );
        print_field(
            "ACTION",
            format!(
                "Use {} to re-authenticate.",
                "hooklistener login --force".bold()
            ),
        );
        println!();
        return Ok(());
    }

    if force_reauth {
        config.clear_token();
        config.save()?;
    }

    let mut device_flow = auth::DeviceCodeFlow::new(api::default_base_url()?)?;

    let user_code = device_flow.initiate_device_flow().await?;
    let display_code = device_flow
        .format_user_code()
        .unwrap_or_else(|| user_code.clone());
    let portal_url = device_portal_url();

    let clipboard_ok = arboard::Clipboard::new()
        .and_then(|mut cb| cb.set_text(&display_code))
        .is_ok();

    println!("\n{}\n", "HOOKLISTENER LOGIN".bold());
    print_field("OPEN", portal_url.as_str().underlined());
    print!("{} {}", output_label("CODE").bold(), display_code.bold());
    if clipboard_ok {
        print!(" {}", "(copied to clipboard)".dim());
    }
    println!("\n");

    let spinner_chars = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
    let mut spinner_idx: usize = 0;
    let mut stdout = io::stdout();

    loop {
        // Poll the API
        match device_flow.poll_for_authorization().await {
            Ok(Some(token_response)) => {
                execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
                let access_expires_at = match token_response.expires_in {
                    Some(secs) => token_expiry_from_now(secs)?,
                    None => Utc::now() + ChronoDuration::days(SESSION_TOKEN_VALIDITY_DAYS),
                };
                let refresh_expires_at = token_response
                    .refresh_expires_in
                    .map(token_expiry_from_now)
                    .transpose()?;
                save_login_tokens(
                    &config,
                    token_response.access_token,
                    access_expires_at,
                    token_response.refresh_token,
                    refresh_expires_at,
                    None,
                )?;
                print_status(OutputStatus::Ok, "AUTHENTICATION COMPLETE");
                println!();
                print_field(
                    "ACTION",
                    format!(
                        "Run {} to forward webhooks.",
                        "hooklistener listen <endpoint-slug>".bold()
                    ),
                );
                println!();
                return Ok(());
            }
            Ok(None) => {
                if let Some(remaining) = device_flow.time_remaining()
                    && remaining == ChronoDuration::zero()
                {
                    execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
                    return Err(anyhow!(
                        "Device code expired before authorization completed. Please run `hooklistener login` again."
                    ));
                }
            }
            Err(err) => {
                execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
                return Err(anyhow!("Authentication failed: {}", err));
            }
        }

        let poll_delay = device_flow.time_until_next_poll();
        if poll_delay.is_zero() {
            continue;
        }
        let next_poll = tokio::time::sleep(poll_delay);
        tokio::pin!(next_poll);

        // Animate spinner until the server-authorized next poll.
        loop {
            if device_flow
                .time_remaining()
                .is_some_and(|remaining| remaining == ChronoDuration::zero())
            {
                execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
                return Err(anyhow!(
                    "Device code expired before authorization completed. Please run `hooklistener login` again."
                ));
            }

            let spinner = spinner_chars[spinner_idx % spinner_chars.len()];
            spinner_idx = (spinner_idx + 1) % spinner_chars.len();

            let status = if let Some(remaining) = device_flow.time_remaining() {
                let minutes = remaining.num_minutes();
                let seconds = remaining.num_seconds() % 60;
                let timer = if minutes > 0 {
                    format!("{minutes}m {seconds:02}s")
                } else {
                    format!("{seconds}s")
                };
                format!("  {spinner} Waiting for approval... {}", timer.dim())
            } else {
                format!("  {spinner} Waiting for approval...")
            };

            execute!(stdout, MoveToColumn(0), Clear(ClearType::CurrentLine))?;
            print!("{status}");
            stdout.flush()?;

            tokio::select! {
                _ = sleep(Duration::from_millis(80)) => continue,
                _ = &mut next_poll => break,
            }
        }
    }
}

pub fn device_portal_url() -> String {
    std::env::var("HOOKLISTENER_DEVICE_PORTAL_URL")
        .unwrap_or_else(|_| "https://app.hooklistener.com/device-codes".to_string())
}
