//! `monitor` commands: uptime monitors and their checks.

use anyhow::{Result, anyhow};
use clap::{ArgAction, Subcommand, ValueEnum};
use std::ops::ControlFlow;

use crate::api::ApiClient;
use crate::commands::confirm_destructive_action;
use crate::credentials::{ensure_valid_token, require_organization};
use crate::output::Stylize;
use crate::render::{
    OutputStatus, TerminalTextLayout, new_table, output_field, print_context, print_empty_state,
    print_field, print_json, print_pagination, print_status, print_status_block, sanitize_terminal,
    sanitize_terminal_display, status_code_label, truncate_terminal_inline, value_or_dash,
};
use crate::{api, config};

#[derive(Subcommand)]
pub enum MonitorAction {
    /// Create an uptime monitor
    Create {
        /// Monitor display name
        name: String,
        /// URL to check (http:// or https://)
        url: String,
        /// HTTP method
        #[arg(long, value_enum, ignore_case = true, default_value_t = MonitorMethod::Get)]
        method: MonitorMethod,
        /// Expected HTTP status code
        #[arg(long, value_name = "CODE", default_value_t = 200, value_parser = clap::value_parser!(u16).range(100..=599))]
        expected_status: u16,
        /// Check interval
        #[arg(long, value_enum, default_value = "5m", ignore_case = true)]
        interval: MonitorInterval,
        /// Text the response body must contain
        #[arg(long, value_name = "TEXT")]
        body_contains: Option<String>,
        /// Request body to send with POST, PUT, or PATCH
        #[arg(long)]
        body: Option<String>,
        /// Consecutive failures before alerting
        #[arg(long, value_name = "N", default_value_t = 2, value_parser = clap::value_parser!(u32).range(1..))]
        failure_threshold: u32,
        /// Disable email notifications for this monitor
        #[arg(long, conflicts_with = "email")]
        no_email: bool,
        #[arg(
            long,
            hide = true,
            default_value_t = true,
            action = ArgAction::Set,
            num_args = 0..=1,
            default_missing_value = "true",
            conflicts_with = "no_email"
        )]
        email: bool,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// List uptime monitors
    List {
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Show an uptime monitor
    Show {
        /// Monitor ID (from `monitor list`)
        monitor_id: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Update an uptime monitor
    Update {
        /// Monitor ID (from `monitor list`)
        monitor_id: String,
        /// New name
        #[arg(long)]
        name: Option<String>,
        /// New URL
        #[arg(long)]
        url: Option<String>,
        /// HTTP method
        #[arg(long, value_enum, ignore_case = true)]
        method: Option<MonitorMethod>,
        /// Expected HTTP status code
        #[arg(long, value_name = "CODE", value_parser = clap::value_parser!(u16).range(100..=599))]
        expected_status: Option<u16>,
        /// Check interval
        #[arg(long, value_enum, ignore_case = true)]
        interval: Option<MonitorInterval>,
        /// Resume checks for this monitor
        #[arg(long, conflicts_with_all = ["disable", "enabled"])]
        enable: bool,
        /// Pause checks for this monitor
        #[arg(long, conflicts_with_all = ["enable", "enabled"])]
        disable: bool,
        #[arg(long, hide = true, value_name = "BOOL")]
        enabled: Option<bool>,
        /// Consecutive failures before alerting
        #[arg(long, value_name = "N", value_parser = clap::value_parser!(u32).range(1..))]
        failure_threshold: Option<u32>,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Delete an uptime monitor
    Delete {
        /// Monitor ID (from `monitor list`)
        monitor_id: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// List recent checks for a monitor
    Checks {
        /// Monitor ID (from `monitor list`)
        monitor_id: String,
        /// Page number
        #[arg(long, value_name = "N", default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
        page: u32,
        /// Results per page
        #[arg(long, value_name = "N", default_value_t = 50, value_parser = clap::value_parser!(u32).range(1..))]
        page_size: u32,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
}

/// HTTP method for uptime monitors (sent lowercase on the wire; no OPTIONS).
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum MonitorMethod {
    #[value(name = "GET")]
    Get,
    #[value(name = "POST")]
    Post,
    #[value(name = "PUT")]
    Put,
    #[value(name = "PATCH")]
    Patch,
    #[value(name = "DELETE")]
    Delete,
    #[value(name = "HEAD")]
    Head,
}

impl MonitorMethod {
    pub fn as_lowercase(self) -> &'static str {
        match self {
            Self::Get => "get",
            Self::Post => "post",
            Self::Put => "put",
            Self::Patch => "patch",
            Self::Delete => "delete",
            Self::Head => "head",
        }
    }
}

// Required by `default_value_t`; renders the help default as `[default: GET]`.
impl std::fmt::Display for MonitorMethod {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.to_possible_value().unwrap().get_name())
    }
}

/// Check interval accepted by the monitor API, in minutes.
#[derive(Clone, Copy, Debug, PartialEq, Eq, ValueEnum)]
pub enum MonitorInterval {
    #[value(name = "1m", alias = "1")]
    M1,
    #[value(name = "5m", alias = "5")]
    M5,
    #[value(name = "10m", alias = "10")]
    M10,
    #[value(name = "30m", alias = "30")]
    M30,
    #[value(name = "60m", alias = "60", alias = "1h")]
    M60,
}

impl MonitorInterval {
    /// Wire value for `check_interval`.
    pub fn minutes(self) -> u32 {
        match self {
            MonitorInterval::M1 => 1,
            MonitorInterval::M5 => 5,
            MonitorInterval::M10 => 10,
            MonitorInterval::M30 => 30,
            MonitorInterval::M60 => 60,
        }
    }
}

pub async fn execute(action: MonitorAction, json: bool, yes: bool) -> Result<ControlFlow<()>> {
    match action {
        MonitorAction::Create {
            name,
            url,
            method,
            expected_status,
            interval,
            body_contains,
            body,
            failure_threshold,
            no_email,
            email,
            org,
        } => {
            let mut config = config::Config::load()?;
            let organization_id = require_organization(org, &config)?;
            let token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;

            let email_enabled = monitor_email_enabled(email, no_email);
            let mut params = serde_json::json!({
                "name": name,
                "url": url,
                "method": method.as_lowercase(),
                "expected_status_code": expected_status,
                "check_interval": interval.minutes(),
                "failure_threshold": failure_threshold,
                "email_enabled": email_enabled,
            });
            if let Some(bc) = body_contains {
                params["body_contains"] = serde_json::Value::String(bc);
            }
            if let Some(b) = body {
                params["body"] = serde_json::Value::String(b);
            }

            let monitor = client.create_uptime_monitor(&params).await?;
            if json {
                print_json(&serde_json::json!({
                    "organization_id": organization_id,
                    "monitor": monitor
                }))?;
            } else {
                print_status(OutputStatus::Ok, "UPTIME MONITOR CREATED");
                println!();
                print_monitor_detail(&monitor);
                print_field("ORGANIZATION", &organization_id);
            }
        }
        MonitorAction::List { org } => {
            let mut config = config::Config::load()?;
            let organization_id = require_organization(org, &config)?;
            let token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
            let monitors = client.list_uptime_monitors().await?;
            if json {
                print_json(&serde_json::json!({
                    "organization_id": organization_id,
                    "monitors": monitors
                }))?;
            } else {
                print_context("Organization:", &organization_id);
                print_monitors(&monitors);
            }
        }
        MonitorAction::Show { monitor_id, org } => {
            let mut config = config::Config::load()?;
            let organization_id = require_organization(org, &config)?;
            let token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
            let monitor = client.get_uptime_monitor(&monitor_id).await?;
            if json {
                print_json(&serde_json::json!({
                    "organization_id": organization_id,
                    "monitor": monitor
                }))?;
            } else {
                print_context("Organization:", &organization_id);
                print_monitor_detail(&monitor);
            }
        }
        MonitorAction::Update {
            monitor_id,
            name,
            url,
            method,
            expected_status,
            interval,
            enable,
            disable,
            enabled,
            failure_threshold,
            org,
        } => {
            let enabled = monitor_enabled_update(enable, disable, enabled);
            let mut config = config::Config::load()?;
            let organization_id = require_organization(org, &config)?;
            let token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;

            let mut params = serde_json::Map::new();
            if let Some(v) = name {
                params.insert("name".into(), serde_json::Value::String(v));
            }
            if let Some(v) = url {
                params.insert("url".into(), serde_json::Value::String(v));
            }
            if let Some(v) = method {
                params.insert(
                    "method".into(),
                    serde_json::Value::String(v.as_lowercase().to_string()),
                );
            }
            if let Some(v) = expected_status {
                params.insert("expected_status_code".into(), v.into());
            }
            if let Some(v) = interval {
                params.insert("check_interval".into(), v.minutes().into());
            }
            if let Some(v) = enabled {
                params.insert("enabled".into(), serde_json::Value::Bool(v));
            }
            if let Some(v) = failure_threshold {
                params.insert("failure_threshold".into(), v.into());
            }

            if params.is_empty() {
                return Err(anyhow!(
                    "No fields to update. Use --name, --url, --method, --expected-status, --interval, --enable, --disable, or --failure-threshold."
                ));
            }

            let monitor = client
                .update_uptime_monitor(&monitor_id, &serde_json::Value::Object(params))
                .await?;
            if json {
                print_json(&serde_json::json!({
                    "organization_id": organization_id,
                    "monitor": monitor
                }))?;
            } else {
                print_status(OutputStatus::Ok, "MONITOR UPDATED");
                println!();
                print_monitor_detail(&monitor);
                print_field("ORGANIZATION", &organization_id);
            }
        }
        MonitorAction::Delete { monitor_id, org } => {
            let mut config = config::Config::load()?;
            let organization_id = require_organization(org, &config)?;
            if !confirm_destructive_action(
                "DELETE MONITOR?",
                &format!("monitor {monitor_id}"),
                &organization_id,
                yes,
                json,
            )? {
                return Ok(ControlFlow::Break(()));
            }
            let token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
            client.delete_uptime_monitor(&monitor_id).await?;
            if json {
                print_json(&serde_json::json!({
                    "status": "deleted",
                    "organization_id": organization_id,
                    "monitor_id": monitor_id
                }))?;
            } else {
                print_status_block(
                    OutputStatus::Ok,
                    "MONITOR DELETED",
                    &[
                        output_field("MONITOR", monitor_id.bold()),
                        output_field("ORGANIZATION", organization_id.dim()),
                    ],
                );
            }
        }
        MonitorAction::Checks {
            monitor_id,
            page,
            page_size,
            org,
        } => {
            let mut config = config::Config::load()?;
            let organization_id = require_organization(org, &config)?;
            let token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
            let response = client
                .list_uptime_checks(&monitor_id, page, page_size)
                .await?;
            if json {
                print_json(&serde_json::json!({
                    "organization_id": organization_id,
                    "monitor_id": monitor_id,
                    "checks": response
                }))?;
            } else {
                print_context("Organization:", &organization_id);
                print_context("Monitor:", &monitor_id);
                print_uptime_checks(&response);
            }
        }
    }
    Ok(ControlFlow::Continue(()))
}

/// Effective email setting for `monitor create`: `--no-email` wins over the
/// hidden `--email <BOOL>` shape (clap already rejects supplying both).
pub fn monitor_email_enabled(email: bool, no_email: bool) -> bool {
    email && !no_email
}

/// Effective `enabled` update for `monitor update`: `--enable` / `--disable`
/// take precedence over the hidden `--enabled <BOOL>` shape.
pub fn monitor_enabled_update(enable: bool, disable: bool, enabled: Option<bool>) -> Option<bool> {
    if enable {
        Some(true)
    } else if disable {
        Some(false)
    } else {
        enabled
    }
}

pub fn monitor_status_label(status: Option<&str>) -> &str {
    status.unwrap_or("pending")
}

pub fn style_monitor_status(status: Option<&str>) -> String {
    match monitor_status_label(status) {
        "up" => "up".green().to_string(),
        "down" => "down".red().to_string(),
        status => sanitize_terminal(status, TerminalTextLayout::Inline)
            .yellow()
            .to_string(),
    }
}

pub fn print_monitors(monitors: &[api::UptimeMonitor]) {
    if monitors.is_empty() {
        print_empty_state(
            "NO UPTIME MONITORS FOUND",
            "Run `hooklistener monitor create <name> <url>` to create one.",
        );
        return;
    }

    let mut table = new_table(&["ID", "Status", "Method", "Int", "URL", "Name"]);
    for m in monitors {
        let status = monitor_status_label(m.current_status.as_deref());
        let interval = m
            .check_interval
            .map(|i| format!("{}m", i))
            .unwrap_or_default();
        let url_display = truncate_terminal_inline(&m.url, 24);
        let sanitized_name = sanitize_terminal(&m.name, TerminalTextLayout::Inline);
        let name = if m.enabled {
            sanitized_name.into_owned()
        } else {
            format!("{sanitized_name} (disabled)")
        };
        table.add_row(vec![
            sanitize_terminal_display(&m.id),
            sanitize_terminal_display(status),
            sanitize_terminal_display(m.method.to_uppercase()),
            interval,
            url_display,
            name,
        ]);
    }
    println!("{table}");
}

pub fn print_monitor_detail(m: &api::UptimeMonitor) {
    print_field("ID", sanitize_terminal(&m.id, TerminalTextLayout::Inline));
    print_field(
        "NAME",
        sanitize_terminal(&m.name, TerminalTextLayout::Inline),
    );
    print_field(
        "URL",
        sanitize_terminal(&m.url, TerminalTextLayout::Inline).underlined(),
    );
    print_field(
        "METHOD",
        sanitize_terminal(&m.method.to_uppercase(), TerminalTextLayout::Inline).bold(),
    );

    let status = style_monitor_status(m.current_status.as_deref());
    let enabled = if m.enabled {
        "yes".green().to_string()
    } else {
        "no (paused)".yellow().to_string()
    };
    print_field("STATUS", status);
    print_field("ENABLED", enabled);

    if let Some(code) = m.expected_status_code {
        print_field("EXPECTED", code);
    }
    if let Some(ref bc) = m.body_contains {
        print_field(
            "BODY MATCH",
            sanitize_terminal(bc, TerminalTextLayout::Inline),
        );
    }
    if let Some(interval) = m.check_interval {
        print_field("INTERVAL", format!("{interval}m"));
    }
    if let Some(threshold) = m.failure_threshold {
        print_field("THRESHOLD", threshold);
    }
    if let Some(failures) = m.consecutive_failures {
        print_field("FAILURES", failures);
    }

    print_field(
        "NOTIFY",
        format!(
            "email={}, slack={}",
            if m.email_enabled { "on" } else { "off" },
            if m.slack_enabled { "on" } else { "off" }
        ),
    );

    if let Some(ref checked) = m.last_checked_at {
        print_field(
            "LAST CHECK",
            sanitize_terminal(checked, TerminalTextLayout::Inline).dim(),
        );
    }
    if let Some(ref changed) = m.last_status_change_at {
        print_field(
            "STATUS CHANGE",
            sanitize_terminal(changed, TerminalTextLayout::Inline).dim(),
        );
    }
    if let Some(ref created) = m.created_at {
        print_field(
            "CREATED AT",
            sanitize_terminal(created, TerminalTextLayout::Inline).dim(),
        );
    }
}

pub fn print_uptime_checks(response: &api::UptimeChecksResponse) {
    if let Some(ref stats) = response.stats {
        let uptime = stats
            .uptime_percentage
            .map(|p| {
                let s = format!("{:.2}%", p);
                if p >= 99.9 {
                    s.green().to_string()
                } else if p >= 95.0 {
                    s.yellow().to_string()
                } else {
                    s.red().to_string()
                }
            })
            .unwrap_or_else(|| "-".to_string());
        let avg_rt = stats
            .avg_response_time_ms
            .map(|ms| format!("{:.0}ms", ms))
            .unwrap_or_else(|| "-".to_string());
        println!(
            "{} {}  {} {}",
            "UPTIME".bold(),
            uptime,
            "AVG RESPONSE".bold(),
            avg_rt
        );
        println!();
    }

    if response.data.is_empty() {
        print_empty_state(
            "NO CHECKS RECORDED",
            "Wait for the first interval, then run `hooklistener monitor checks <monitor-id>`.",
        );
    } else {
        let mut table = new_table(&["ID", "Status", "Code", "Response", "Checked At", "Error"]);
        for check in &response.data {
            let code = check
                .status_code
                .map(status_code_label)
                .unwrap_or_else(|| "-".to_string());
            let rt = check
                .response_time_ms
                .map(|ms| format!("{}ms", ms))
                .unwrap_or_else(|| "-".to_string());
            let checked = value_or_dash(check.checked_at.as_deref());
            let error = value_or_dash(check.error_message.as_deref());

            table.add_row(vec![
                sanitize_terminal_display(&check.id),
                sanitize_terminal_display(&check.status),
                code,
                rt,
                sanitize_terminal_display(checked),
                sanitize_terminal_display(error),
            ]);
        }
        println!("{table}");
    }

    print_pagination(&response.pagination);
}
