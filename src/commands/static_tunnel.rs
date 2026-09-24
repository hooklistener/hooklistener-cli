//! `static-tunnel` commands: reserved tunnel subdomains.

use anyhow::Result;
use clap::Subcommand;
use std::ops::ControlFlow;

use crate::api::ApiClient;
use crate::commands::confirm_destructive_action;
use crate::credentials::{ensure_valid_token, require_organization};
use crate::output::Stylize;
use crate::render::{
    OutputStatus, new_table, print_context, print_empty_state, print_field, print_json,
    print_status, sanitize_terminal_display,
};
use crate::{api, config};

#[derive(Subcommand)]
pub enum StaticTunnelAction {
    /// List reserved static tunnel slugs
    List {
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Reserve a static tunnel slug
    Create {
        /// Slug to reserve
        slug: String,
        /// Display name
        #[arg(long)]
        name: Option<String>,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Release a static tunnel slug
    Delete {
        /// Static tunnel ID (from `static-tunnel list`)
        static_tunnel_id: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
}

pub async fn execute(action: StaticTunnelAction, json: bool, yes: bool) -> Result<ControlFlow<()>> {
    match action {
        StaticTunnelAction::List { org } => {
            let mut config = config::Config::load()?;
            let organization_id = require_organization(org, &config)?;
            let token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
            let tunnels = client.list_static_tunnels(&organization_id).await?;
            if json {
                print_json(&serde_json::json!({
                    "organization_id": organization_id,
                    "tunnels": tunnels
                }))?;
            } else {
                print_context("Organization:", &organization_id);
                print_static_tunnels(&tunnels);
            }
        }
        StaticTunnelAction::Create { slug, name, org } => {
            let mut config = config::Config::load()?;
            let organization_id = require_organization(org, &config)?;
            let token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
            let created = client
                .create_static_tunnel(&organization_id, &slug, name.as_deref())
                .await?;
            if json {
                print_json(&serde_json::json!({
                    "organization_id": organization_id,
                    "result": created
                }))?;
            } else {
                print_status(OutputStatus::Ok, "STATIC TUNNEL CREATED");
                println!();
                print_field("ID", &created.static_tunnel.id);
                print_field("SLUG", created.static_tunnel.slug.as_str().bold());
                if let Some(name) = created.static_tunnel.name.as_deref() {
                    print_field("NAME", name);
                }
                print_field("ORGANIZATION", organization_id.dim());
                if let Some(message) = created.message {
                    print_field("MESSAGE", message.dim());
                }
            }
        }
        StaticTunnelAction::Delete {
            static_tunnel_id,
            org,
        } => {
            let mut config = config::Config::load()?;
            let organization_id = require_organization(org, &config)?;
            if !confirm_destructive_action(
                "DELETE STATIC TUNNEL?",
                &format!("static tunnel {static_tunnel_id}"),
                &organization_id,
                yes,
                json,
            )? {
                return Ok(ControlFlow::Break(()));
            }
            let token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
            let response = client
                .delete_static_tunnel(&organization_id, &static_tunnel_id)
                .await?;
            if json {
                print_json(&serde_json::json!({
                    "organization_id": organization_id,
                    "slug_id": static_tunnel_id,
                    "status": "deleted",
                    "message": response.message
                }))?;
            } else {
                print_status(OutputStatus::Ok, "STATIC TUNNEL DELETED");
                println!();
                print_field("SLUG/ID", static_tunnel_id);
                print_field("ORGANIZATION", organization_id.dim());
                if let Some(message) = response.message {
                    print_field("MESSAGE", message.dim());
                }
            }
        }
    }
    Ok(ControlFlow::Continue(()))
}

pub fn print_static_tunnels(response: &api::StaticTunnelsResponse) {
    if response.static_tunnels.is_empty() {
        print_empty_state(
            "NO STATIC TUNNELS FOUND",
            "Run `hooklistener static-tunnel create <slug>` to reserve one.",
        );
    } else {
        let mut table = new_table(&["ID", "Slug", "Name"]);
        for tunnel in &response.static_tunnels {
            let name = tunnel.name.as_deref().unwrap_or("");
            table.add_row(vec![
                sanitize_terminal_display(&tunnel.id),
                sanitize_terminal_display(&tunnel.slug),
                sanitize_terminal_display(name),
            ]);
        }
        println!("{table}");
    }

    print_field(
        "USED",
        format!("{}/{}", response.used, response.limit).dim(),
    );
}
