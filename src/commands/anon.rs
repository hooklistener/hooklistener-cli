//! `anon` commands: anonymous endpoints, their events, and anonymous tunnels.

use anyhow::Result;
use clap::Subcommand;
use std::time::Duration;
use tokio::task::JoinHandle;

use crate::api::ApiClient;
use crate::cli::{parse_duration, parse_duration_within};
use crate::commands::tunnel::run_anonymous_tunnel_activation;
use crate::credentials::{ensure_valid_token, require_organization};
use crate::output::Stylize;
use crate::render::{
    OutputStatus, TerminalTextLayout, new_table, print_body_section, print_context,
    print_empty_state, print_field, print_json, print_key_value_map, print_pagination,
    print_status, sanitize_terminal, value_or_dash,
};
use crate::{api, config};

#[derive(Subcommand)]
pub enum AnonAction {
    /// Create a temporary anonymous endpoint
    Create {
        /// Endpoint lifetime, such as 3600, 1h, or 24h
        #[arg(long, value_name = "DURATION", default_value = "24h", value_parser = parse_duration)]
        ttl: Duration,
    },
    /// Show an anonymous endpoint
    Show {
        /// Anonymous endpoint ID (from `anon create`)
        endpoint_id: String,
    },
    /// List captured events for an anonymous endpoint
    #[command(name = "list-events", visible_alias = "events")]
    ListEvents {
        /// Anonymous endpoint ID (from `anon create`)
        endpoint_id: String,
        /// Viewer token (from `anon create`)
        #[arg(long, value_name = "VIEWER_TOKEN")]
        token: String,
        /// Page number
        #[arg(long, value_name = "N", default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
        page: u32,
        /// Results per page
        #[arg(long, value_name = "N", default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..))]
        page_size: u32,
    },
    /// Show a captured event
    #[command(name = "show-event", visible_alias = "event")]
    ShowEvent {
        /// Anonymous endpoint ID (from `anon create`)
        endpoint_id: String,
        /// Event ID (from `anon events`)
        event_id: String,
        /// Viewer token (from `anon create`)
        #[arg(long, value_name = "VIEWER_TOKEN")]
        token: String,
    },
    /// Expose a local HTTP server without signing in
    Tunnel {
        /// Local port to forward requests to
        #[arg(short, long, default_value = "3000")]
        port: u16,
        /// Local host to forward to
        #[arg(long, default_value = "localhost")]
        host: String,
        /// Stable public route name
        #[arg(long)]
        name: Option<String>,
        /// Route lifetime, such as 300, 10m, or 30m
        #[arg(
            long,
            value_name = "DURATION",
            default_value = "15m",
            value_parser = parse_duration_within(Duration::from_secs(60), Duration::from_secs(1800))
        )]
        ttl: Duration,
        /// Allow forwarding to a host that resolves outside loopback
        #[arg(long)]
        allow_non_loopback: bool,
    },
    /// Claim an anonymous route into an organization
    Claim {
        /// Anonymous route ID (from `anon tunnel`)
        #[arg(value_name = "ROUTE_ID")]
        route_id: String,
        /// Claim token (from `anon tunnel`)
        #[arg(long, value_name = "CLAIM_TOKEN")]
        token: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
}

pub async fn execute(
    action: AnonAction,
    json: bool,
    update_handle: &mut Option<JoinHandle<Option<String>>>,
) -> Result<()> {
    match action {
        AnonAction::Create { ttl } => {
            let client = ApiClient::unauthenticated()?;
            let endpoint = client.create_anon_endpoint(Some(ttl.as_secs())).await?;
            if json {
                print_json(&endpoint)?;
            } else {
                print_status(OutputStatus::Ok, "ANONYMOUS ENDPOINT CREATED");
                println!();
                print_field("ID", endpoint.id);
                print_field("WEBHOOK URL", endpoint.webhook_url.as_str().underlined());
                print_field("EXPIRES AT", endpoint.expires_at.dim());
                println!();
                print_field("VIEWER TOKEN", endpoint.viewer_token.as_str().yellow());
                print_field(
                    "ACTION",
                    "Save this token. It is required to list captured events.".dim(),
                );
            }
        }
        AnonAction::Show { endpoint_id } => {
            let client = ApiClient::unauthenticated()?;
            let status = client.get_anon_endpoint(&endpoint_id).await?;
            if json {
                print_json(&status)?;
            } else {
                print_status(OutputStatus::Info, "ANONYMOUS ENDPOINT");
                println!();
                print_field("ID", status.id);
                let active_str = if status.active {
                    "active".green().to_string()
                } else {
                    "expired".red().to_string()
                };
                print_field("STATUS", active_str);
                if let Some(expires) = status.expires_at.as_deref() {
                    print_field("EXPIRES AT", expires.dim());
                }
                if let Some(url) = status.webhook_url.as_deref() {
                    print_field("WEBHOOK URL", url.underlined());
                }
                if let Some(error) = status.error.as_deref() {
                    print_field("ERROR", error);
                }
            }
        }
        AnonAction::ListEvents {
            endpoint_id,
            token,
            page,
            page_size,
        } => {
            let client = ApiClient::with_organization(token, None)?;
            let response = client
                .list_anon_events(&endpoint_id, page, page_size)
                .await?;
            if json {
                print_json(&serde_json::json!({
                    "endpoint_id": endpoint_id,
                    "events": response
                }))?;
            } else {
                print_context("Endpoint:", &endpoint_id);
                print_anon_events(&response);
            }
        }
        AnonAction::ShowEvent {
            endpoint_id,
            event_id,
            token,
        } => {
            let client = ApiClient::with_organization(token, None)?;
            let event = client.get_anon_event(&endpoint_id, &event_id).await?;
            if json {
                print_json(&serde_json::json!({
                    "endpoint_id": endpoint_id,
                    "event": event
                }))?;
            } else {
                print_context("Endpoint:", &endpoint_id);
                print_anon_event_detail(&event);
            }
        }
        AnonAction::Tunnel {
            port,
            host,
            name,
            ttl,
            allow_non_loopback,
        } => {
            run_anonymous_tunnel_activation(
                host,
                port,
                name,
                ttl.as_secs(),
                allow_non_loopback,
                json,
                update_handle,
            )
            .await?;
        }
        AnonAction::Claim {
            route_id,
            token,
            org,
        } => {
            let mut config = config::Config::load()?;
            let organization_id = require_organization(org, &config)?;
            let access_token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(access_token, Some(organization_id.clone()))?;
            let claimed = client
                .claim_anonymous_tunnel_route(&route_id, &token)
                .await?;

            if json {
                print_json(&serde_json::json!({
                    "operation": "claim_anonymous_tunnel",
                    "status": "succeeded",
                    "organization_id": organization_id,
                    "route": claimed,
                }))?;
            } else {
                print_status(OutputStatus::Ok, "ANONYMOUS ROUTE CLAIMED");
                println!();
                print_field("ROUTE", claimed.id);
                print_field(
                    "PUBLIC URL",
                    format!("https://{}.hook.events", claimed.slug),
                );
                print_field("ORGANIZATION", organization_id.dim());
                print_field("CAPTURES TRANSFERRED", "0");
                print_field(
                    "PRIVACY",
                    "Pre-claim captures were permanently discarded.".dim(),
                );
            }
        }
    }
    Ok(())
}

pub fn print_anon_events(response: &api::AnonEventsResponse) {
    if response.data.is_empty() {
        print_empty_state(
            "NO EVENTS CAPTURED",
            "Send a webhook, then run `hooklistener anon list-events <endpoint-id> --token <viewer-token>`.",
        );
    } else {
        let mut table = new_table(&["ID", "Method", "Received At"]);
        for event in &response.data {
            table.add_row(vec![
                sanitize_terminal(&event.id, TerminalTextLayout::Inline).into_owned(),
                sanitize_terminal(&event.method, TerminalTextLayout::Inline).into_owned(),
                sanitize_terminal(
                    value_or_dash(event.inserted_at.as_deref()),
                    TerminalTextLayout::Inline,
                )
                .into_owned(),
            ]);
        }
        println!("{table}");
    }
    print_pagination(&response.pagination);
}

pub fn print_anon_event_detail(event: &api::AnonEvent) {
    print_field(
        "EVENT ID",
        sanitize_terminal(&event.id, TerminalTextLayout::Inline),
    );
    print_field(
        "ENDPOINT ID",
        sanitize_terminal(&event.endpoint_id, TerminalTextLayout::Inline),
    );
    print_field(
        "METHOD",
        sanitize_terminal(&event.method, TerminalTextLayout::Inline).bold(),
    );
    if let Some(status) = event.status.as_deref() {
        print_field(
            "STATUS",
            sanitize_terminal(status, TerminalTextLayout::Inline),
        );
    }
    if let Some(inserted_at) = event.inserted_at.as_deref() {
        print_field(
            "RECEIVED AT",
            sanitize_terminal(inserted_at, TerminalTextLayout::Inline).dim(),
        );
    }

    println!();
    print_key_value_map("Headers:", &event.headers, ": ");

    println!();
    print_body_section("Body:", event.body.as_deref());
}
