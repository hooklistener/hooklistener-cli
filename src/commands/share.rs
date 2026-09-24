//! `share` commands: shared request links.

use anyhow::Result;
use clap::Subcommand;
use std::ops::ControlFlow;
use std::time::Duration;

use crate::api::ApiClient;
use crate::cli::{duration_hours, parse_whole_hours, warn_deprecated_flag};
use crate::commands::confirm_destructive_action;
use crate::credentials::{ensure_valid_token, require_organization};
use crate::output::Stylize;
use crate::render::{
    OutputStatus, TerminalTextLayout, new_table, output_field, print_body_section, print_context,
    print_empty_state, print_field, print_json, print_section, print_status, print_status_block,
    sanitize_terminal, style_status_code, value_or_dash, yes_no,
};
use crate::{api, config};

#[derive(Subcommand)]
pub enum ShareAction {
    /// Create a public link for a captured request
    Create {
        /// Captured request ID (from `endpoint list-requests`)
        request_id: String,
        /// Link lifetime in whole hours, such as 24h, 86400, or 7d
        #[arg(long, value_name = "DURATION", value_parser = parse_whole_hours)]
        expires_in: Option<Duration>,
        #[arg(long, hide = true, value_name = "HOURS", conflicts_with = "expires_in")]
        expires_in_hours: Option<u64>,
        /// Password required to open the link
        #[arg(long)]
        password: Option<String>,
        /// Include forwards in the shared view
        #[arg(long)]
        include_forwards: bool,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// List links for a captured request
    List {
        /// Captured request ID (from `endpoint list-requests`)
        request_id: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Show a shared request by token (no login required)
    Show {
        /// Share token (from `share create`)
        share_token: String,
    },
    /// Revoke a public link
    Revoke {
        /// Share token (from `share create`)
        share_token: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
}

pub async fn execute(action: ShareAction, json: bool, yes: bool) -> Result<ControlFlow<()>> {
    match action {
        ShareAction::Create {
            request_id,
            expires_in,
            expires_in_hours,
            password,
            include_forwards,
            org,
        } => {
            if let Some(hours) = expires_in_hours {
                warn_deprecated_flag("expires-in-hours", "expires-in", &format!("{hours}h"));
            }
            let expires_in_hours = expires_in_hours.or(expires_in.map(duration_hours));
            let mut config = config::Config::load()?;
            let organization_id = require_organization(org, &config)?;
            let token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
            let shared = client
                .create_shared_request(
                    &request_id,
                    expires_in_hours,
                    password.as_deref(),
                    include_forwards,
                )
                .await?;
            if json {
                print_json(&serde_json::json!({
                    "organization_id": organization_id,
                    "shared_request": shared
                }))?;
            } else {
                print_status(OutputStatus::Ok, "SHARE LINK CREATED");
                println!();
                print_field("SHARE TOKEN", shared.share_token);
                if let Some(url) = shared.share_url.as_deref() {
                    print_field("SHARE URL", url.underlined());
                }
                print_field("REQUEST ID", shared.debug_request_id);
                print_field("ORGANIZATION", &organization_id);
                if shared.password_protected {
                    print_field("PASSWORD", "protected".yellow());
                }
                if let Some(expires) = shared.expires_at.as_deref() {
                    print_field("EXPIRES AT", expires.dim());
                }
                print_field("INCLUDE FWDS", shared.include_forwards);
            }
        }
        ShareAction::List { request_id, org } => {
            let mut config = config::Config::load()?;
            let organization_id = require_organization(org, &config)?;
            let token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
            let shares = client.list_shared_requests(&request_id).await?;
            if json {
                print_json(&serde_json::json!({
                    "organization_id": organization_id,
                    "debug_request_id": request_id,
                    "shares": shares
                }))?;
            } else {
                print_context("Organization:", &organization_id);
                print_context("Request:", &request_id);
                print_shared_requests(&shares);
            }
        }
        ShareAction::Show { share_token } => {
            let client = ApiClient::unauthenticated()?;
            let data = client.get_shared_request(&share_token).await?;
            if json {
                print_json(&data)?;
            } else {
                // Check if it's a protected share
                if data.get("protected").and_then(|v| v.as_bool()) == Some(true) {
                    print_status(OutputStatus::Info, "PROTECTED SHARE");
                    println!();
                    if let Some(expires) = data.get("expires_at").and_then(|v| v.as_str()) {
                        print_field("EXPIRES AT", expires.dim());
                    }
                    print_field(
                        "ACTION",
                        "Use the web UI or API to authenticate with the password.".dim(),
                    );
                } else {
                    print_shared_request_full(&data);
                }
            }
        }
        ShareAction::Revoke { share_token, org } => {
            let mut config = config::Config::load()?;
            let organization_id = require_organization(org, &config)?;
            if !confirm_destructive_action(
                "REVOKE SHARED LINK?",
                &format!("share {share_token}"),
                &organization_id,
                yes,
                json,
            )? {
                return Ok(ControlFlow::Break(()));
            }
            let access_token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(access_token, Some(organization_id.clone()))?;
            client.revoke_shared_request(&share_token).await?;
            if json {
                print_json(&serde_json::json!({
                    "status": "revoked",
                    "organization_id": organization_id,
                    "share_token": share_token
                }))?;
            } else {
                print_status_block(
                    OutputStatus::Ok,
                    "SHARE REVOKED",
                    &[
                        output_field("SHARE TOKEN", share_token.bold()),
                        output_field("ORGANIZATION", organization_id.dim()),
                    ],
                );
            }
        }
    }
    Ok(ControlFlow::Continue(()))
}

pub fn print_shared_requests(shares: &[api::SharedRequestSummary]) {
    if shares.is_empty() {
        print_empty_state(
            "NO SHARES FOUND",
            "Run `hooklistener share create <request-id>` to create one.",
        );
        return;
    }

    let mut table = new_table(&["ID", "Token", "Fwds", "Views", "Protected", "Expires At"]);
    for share in shares {
        table.add_row(vec![
            sanitize_terminal(&share.id, TerminalTextLayout::Inline).into_owned(),
            sanitize_terminal(&share.share_token, TerminalTextLayout::Inline).into_owned(),
            yes_no(share.include_forwards).to_string(),
            share.view_count.to_string(),
            yes_no(share.password_protected).to_string(),
            sanitize_terminal(
                value_or_dash(share.expires_at.as_deref()),
                TerminalTextLayout::Inline,
            )
            .into_owned(),
        ]);
    }
    println!("{table}");
}

pub fn print_shared_request_full(data: &serde_json::Value) {
    if let Some(token) = data.get("share_token").and_then(|v| v.as_str()) {
        print_field(
            "SHARE TOKEN",
            sanitize_terminal(token, TerminalTextLayout::Inline),
        );
    }
    if let Some(expires) = data.get("expires_at").and_then(|v| v.as_str()) {
        print_field(
            "EXPIRES AT",
            sanitize_terminal(expires, TerminalTextLayout::Inline).dim(),
        );
    }
    if let Some(views) = data.get("view_count").and_then(|v| v.as_u64()) {
        print_field("VIEWS", views);
    }

    if let Some(request) = data.get("debug_request") {
        println!();
        print_section("DEBUG REQUEST");
        if let Some(id) = request.get("id").and_then(|v| v.as_str()) {
            print_field("ID", sanitize_terminal(id, TerminalTextLayout::Inline));
        }
        if let Some(method) = request.get("method").and_then(|v| v.as_str()) {
            print_field(
                "METHOD",
                sanitize_terminal(method, TerminalTextLayout::Inline).bold(),
            );
        }
        if let Some(url) = request.get("url").and_then(|v| v.as_str()) {
            print_field("URL", sanitize_terminal(url, TerminalTextLayout::Inline));
        }
        if let Some(remote) = request.get("remote_addr").and_then(|v| v.as_str()) {
            print_field(
                "REMOTE",
                sanitize_terminal(remote, TerminalTextLayout::Inline).dim(),
            );
        }
        if let Some(created) = request.get("created_at").and_then(|v| v.as_str()) {
            print_field(
                "CREATED AT",
                sanitize_terminal(created, TerminalTextLayout::Inline).dim(),
            );
        }

        if let Some(headers) = request.get("headers").and_then(|v| v.as_object())
            && !headers.is_empty()
        {
            println!();
            print_section("HEADERS");
            for (key, value) in headers {
                let value = value.to_string();
                println!(
                    "  {}: {}",
                    sanitize_terminal(key, TerminalTextLayout::Inline).dim(),
                    sanitize_terminal(&value, TerminalTextLayout::Inline)
                );
            }
        }

        let body = request
            .get("body")
            .and_then(|v| v.as_str())
            .or_else(|| request.get("body_preview").and_then(|v| v.as_str()));
        if let Some(body) = body
            && !body.is_empty()
        {
            println!();
            print_body_section("BODY", Some(body));
        }
    }

    if let Some(forwards) = data.get("forwards").and_then(|v| v.as_array())
        && !forwards.is_empty()
    {
        println!();
        print_section("FORWARDS");
        for fwd in forwards {
            let target = fwd
                .get("target_url")
                .and_then(|v| v.as_str())
                .unwrap_or("-");
            let method = fwd.get("method").and_then(|v| v.as_str()).unwrap_or("-");
            let status = fwd
                .get("status_code")
                .and_then(|v| v.as_u64())
                .and_then(|c| u16::try_from(c).ok())
                .map(style_status_code)
                .unwrap_or_else(|| "-".to_string());
            let duration = fwd
                .get("duration_ms")
                .and_then(|v| v.as_u64())
                .map(|ms| format!("{}ms", ms))
                .unwrap_or_else(|| "-".to_string());
            println!(
                "  {} {} → {} ({})",
                sanitize_terminal(method, TerminalTextLayout::Inline).bold(),
                status,
                sanitize_terminal(target, TerminalTextLayout::Inline),
                duration
            );
        }
    }
}
