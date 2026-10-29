//! `org` commands: list and select organizations.

use anyhow::{Result, anyhow};
use clap::Subcommand;

use crate::api::ApiClient;
use crate::credentials::ensure_valid_token;
use crate::output::Stylize;
use crate::render::{
    OutputField, OutputStatus, TerminalTextLayout, new_table, output_field, print_empty_state,
    print_json, print_status, print_status_block, sanitize_terminal,
};
use crate::{api, config};

#[derive(Subcommand)]
pub enum OrgAction {
    /// List organizations available to your account
    List,
    /// Set the default organization
    Use {
        /// Organization ID (from `org list`)
        org_id: String,
    },
    /// Clear the default organization
    Clear,
}

pub async fn execute(action: OrgAction, json: bool) -> Result<()> {
    match action {
        OrgAction::List => {
            let mut config = config::Config::load()?;
            let token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(token, None)?;
            let organizations = client.list_organizations().await?;
            if json {
                print_json(&serde_json::json!({
                    "selected_organization_id": config.selected_organization_id,
                    "organizations": organizations
                }))?;
            } else {
                print_organizations(&organizations, config.selected_organization_id.as_deref());
            }
        }
        OrgAction::Use { org_id } => {
            let mut config = config::Config::load()?;
            let token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(token, None)?;
            let organizations = client.list_organizations().await?;
            let organization_name = organizations
                .iter()
                .find(|org| org.id == org_id)
                .map(|org| org.name.clone())
                .ok_or_else(|| {
                    anyhow!(
                        "Organization not found or not accessible with id: {}",
                        org_id
                    )
                })?;

            config.selected_organization_id = Some(org_id.clone());
            config.save()?;
            if json {
                print_json(&serde_json::json!({
                    "status": "ok",
                    "selected_organization_id": org_id,
                    "organization_name": organization_name
                }))?;
            } else {
                print_status_block(
                    OutputStatus::Ok,
                    "ORGANIZATION SELECTED",
                    &organization_selected_fields(&organization_name, &org_id),
                );
            }
        }
        OrgAction::Clear => {
            let mut config = config::Config::load()?;
            config.selected_organization_id = None;
            config.save()?;
            if json {
                print_json(&serde_json::json!({
                    "status": "ok",
                    "selected_organization_id": null
                }))?;
            } else {
                print_status(OutputStatus::Ok, "ORGANIZATION CLEARED");
            }
        }
    }
    Ok(())
}

/// The `org use` confirmation. The name comes from the API, so it is
/// neutralized before styling.
pub(crate) fn organization_selected_fields(
    organization_name: &str,
    org_id: &str,
) -> Vec<OutputField> {
    vec![
        output_field(
            "NAME",
            sanitize_terminal(organization_name, TerminalTextLayout::Inline).bold(),
        ),
        output_field(
            "ORGANIZATION",
            sanitize_terminal(org_id, TerminalTextLayout::Inline).dim(),
        ),
    ]
}

pub fn print_organizations(organizations: &[api::Organization], selected_org: Option<&str>) {
    if organizations.is_empty() {
        print_empty_state(
            "NO ORGANIZATIONS FOUND",
            "Run `hooklistener login --force` to refresh account access.",
        );
        return;
    }

    println!(
        "{}",
        render_organizations_table(organizations, selected_org)
    );
}

pub(crate) fn render_organizations_table(
    organizations: &[api::Organization],
    selected_org: Option<&str>,
) -> String {
    let mut table = new_table(&["", "ID", "Name"]);
    for org in organizations {
        let marker = if selected_org.is_some_and(|id| id == org.id) {
            "*"
        } else {
            ""
        };
        table.add_row(vec![
            marker.to_string(),
            sanitize_terminal(&org.id, TerminalTextLayout::Inline).into_owned(),
            sanitize_terminal(&org.name, TerminalTextLayout::Inline).into_owned(),
        ]);
    }
    table.to_string()
}
