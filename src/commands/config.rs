//! `config` commands: show and edit the local configuration file.

use anyhow::Result;
use clap::{Subcommand, ValueEnum};

use crate::output::Stylize;
use crate::render::{OutputStatus, output_field, print_field, print_json, print_status_block};
use crate::{config, credentials};

#[derive(Subcommand)]
pub enum ConfigAction {
    /// Show the current configuration
    Show,
    /// Set a configuration value
    Set {
        /// Configuration key
        #[arg(value_enum)]
        key: ConfigKey,
        /// New value
        value: String,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum ConfigKey {
    #[value(name = "selected_organization_id")]
    SelectedOrganizationId,
}

pub async fn execute(action: ConfigAction, json: bool) -> Result<()> {
    match action {
        ConfigAction::Show => {
            let config = config::Config::load()?;
            let config_path = config::Config::config_path()?;
            let env_token = credentials::env_access_token().is_some();
            let organization_id = credentials::resolve_tunnel_org(None, &config);
            if json {
                let token_status = if env_token {
                    "environment"
                } else if config.access_token.is_none() {
                    "none"
                } else if config.is_token_valid() {
                    "valid"
                } else {
                    "expired"
                };

                print_json(&serde_json::json!({
                    "config_path": config_path.display().to_string(),
                    "token": {
                        "present": env_token || config.access_token.is_some(),
                        "status": token_status
                    },
                    "organization_id": organization_id
                }))?;
            } else {
                print_field("CONFIG FILE", config_path.display());
                println!();
                if env_token {
                    print_field(
                        "TOKEN",
                        format!("(from {})", credentials::ACCESS_TOKEN_ENV).green(),
                    );
                } else {
                    match &config.access_token {
                        Some(_) => {
                            if config.is_token_valid() {
                                print_field("TOKEN", "(present, valid)".green());
                            } else {
                                print_field("TOKEN", "(present, expired)".red());
                            }
                        }
                        None => print_field("TOKEN", "(none)".dim()),
                    }
                }
                match &organization_id {
                    Some(org_id) => print_field("ORGANIZATION", org_id),
                    None => print_field("ORGANIZATION", "(none)".dim()),
                }
            }
        }
        ConfigAction::Set { key, value } => match key {
            ConfigKey::SelectedOrganizationId => {
                let mut config = config::Config::load()?;
                if value == "none" {
                    config.selected_organization_id = None;
                    config.save()?;
                    if json {
                        print_json(&serde_json::json!({
                            "status": "ok",
                            "key": "selected_organization_id",
                            "value": null
                        }))?;
                    } else {
                        print_status_block(
                            OutputStatus::Ok,
                            "CONFIG CLEARED",
                            &[
                                output_field("KEY", "selected_organization_id"),
                                output_field("VALUE", "(none)".dim()),
                            ],
                        );
                    }
                } else {
                    config.selected_organization_id = Some(value);
                    config.save()?;
                    if json {
                        print_json(&serde_json::json!({
                            "status": "ok",
                            "key": "selected_organization_id",
                            "value": config.selected_organization_id
                        }))?;
                    } else {
                        print_status_block(
                            OutputStatus::Ok,
                            "CONFIG SET",
                            &[
                                output_field("KEY", "selected_organization_id"),
                                output_field(
                                    "VALUE",
                                    config
                                        .selected_organization_id
                                        .as_deref()
                                        .unwrap_or_default()
                                        .bold(),
                                ),
                            ],
                        );
                    }
                }
            }
        },
    }
    Ok(())
}
