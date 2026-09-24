//! `endpoint` commands: debug endpoints, captured requests, and forwards.

use anyhow::{Result, anyhow};
use clap::Subcommand;
use reqwest::Url;
use std::ops::ControlFlow;

use crate::api::ApiClient;
use crate::cli::HttpMethod;
use crate::commands::confirm_destructive_action;
use crate::credentials::{ensure_valid_token, require_organization};
use crate::output::Stylize;
use crate::receipts::{
    forward_poll_command, forward_poll_path, forward_resource_uri, request_forwards_resource_uri,
    request_resource_uri,
};
use crate::render::{
    OutputStatus, TerminalTextLayout, new_table, output_field, print_body_section, print_context,
    print_empty_state, print_field, print_json, print_key_value_map, print_pagination,
    print_status, print_status_block, sanitize_terminal, style_status_code,
};
use crate::{api, config};

#[derive(Subcommand)]
pub enum EndpointAction {
    /// Create a debug endpoint
    Create {
        /// Endpoint display name
        name: String,
        /// Custom slug
        #[arg(long)]
        slug: Option<String>,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// List debug endpoints for an organization
    List {
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Show a single debug endpoint by ID
    Show {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Delete a debug endpoint by ID
    Delete {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// List captured requests for an endpoint
    #[command(name = "list-requests", visible_alias = "requests")]
    ListRequests {
        /// Debug endpoint ID
        endpoint_id: String,
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
    /// Show a captured request
    #[command(name = "show-request", visible_alias = "request")]
    ShowRequest {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Captured request ID
        request_id: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Delete a captured request
    DeleteRequest {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Captured request ID
        request_id: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Replay a captured request to a target URL
    ForwardRequest {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Captured request ID
        request_id: String,
        /// URL to replay the request to
        #[arg(value_name = "URL")]
        target_url: String,
        /// HTTP method override
        #[arg(long, value_enum, ignore_case = true)]
        method: Option<HttpMethod>,
        /// Validate scope and print the forward plan without queueing delivery
        #[arg(long)]
        dry_run: bool,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// List forwards of a captured request
    #[command(name = "list-forwards", visible_alias = "forwards")]
    ListForwards {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Captured request ID
        request_id: String,
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
    /// Show a forward by ID
    #[command(name = "show-forward", visible_alias = "forward")]
    ShowForward {
        /// Forward ID
        forward_id: String,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
}

pub async fn execute(action: EndpointAction, json: bool, yes: bool) -> Result<ControlFlow<()>> {
    match action {
        EndpointAction::Create { name, slug, org } => {
            let mut config = config::Config::load()?;
            let organization_id = require_organization(org, &config)?;
            let token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
            let endpoint = client.create_endpoint(&name, slug.as_deref()).await?;
            if json {
                print_json(&serde_json::json!({
                    "organization_id": organization_id,
                    "endpoint": endpoint
                }))?;
            } else {
                print_status(OutputStatus::Ok, "ENDPOINT CREATED");
                println!();
                print_endpoint_detail(&endpoint);
                print_field("ORGANIZATION", &organization_id);
                print_field(
                    "ACTION",
                    format!("Run `hooklistener listen {}`", endpoint.slug).dim(),
                );
            }
        }
        EndpointAction::List { org } => {
            let mut config = config::Config::load()?;
            let organization_id = require_organization(org, &config)?;
            let token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
            let endpoints = client.list_endpoints().await?;
            if json {
                print_json(&serde_json::json!({
                    "organization_id": organization_id,
                    "endpoints": endpoints
                }))?;
            } else {
                print_context("Organization:", &organization_id);
                print_endpoints(&endpoints);
            }
        }
        EndpointAction::Show { endpoint_id, org } => {
            let mut config = config::Config::load()?;
            let organization_id = require_organization(org, &config)?;
            let token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
            let endpoint = client.get_endpoint(&endpoint_id).await?;
            if json {
                print_json(&serde_json::json!({
                    "organization_id": organization_id,
                    "endpoint": endpoint
                }))?;
            } else {
                print_endpoint_detail(&endpoint);
                print_field("ORGANIZATION", &organization_id);
            }
        }
        EndpointAction::Delete { endpoint_id, org } => {
            let mut config = config::Config::load()?;
            let organization_id = require_organization(org, &config)?;
            if !confirm_destructive_action(
                "DELETE ENDPOINT?",
                &format!("endpoint {endpoint_id}"),
                &organization_id,
                yes,
                json,
            )? {
                return Ok(ControlFlow::Break(()));
            }
            let token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
            client.delete_endpoint(&endpoint_id).await?;
            if json {
                print_json(&serde_json::json!({
                    "status": "deleted",
                    "organization_id": organization_id,
                    "endpoint_id": endpoint_id
                }))?;
            } else {
                print_status_block(
                    OutputStatus::Ok,
                    "ENDPOINT DELETED",
                    &[
                        output_field("ENDPOINT", endpoint_id.bold()),
                        output_field("ORGANIZATION", organization_id.dim()),
                    ],
                );
            }
        }
        EndpointAction::ListRequests {
            endpoint_id,
            page,
            page_size,
            org,
        } => {
            let mut config = config::Config::load()?;
            let organization_id = require_organization(org, &config)?;
            let token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
            let requests = client
                .list_endpoint_requests(&endpoint_id, page, page_size)
                .await?;
            if json {
                print_json(&serde_json::json!({
                    "organization_id": organization_id,
                    "endpoint_id": endpoint_id,
                    "requests": requests
                }))?;
            } else {
                print_context("Organization:", &organization_id);
                print_context("Endpoint:", &endpoint_id);
                print_endpoint_requests(&requests);
            }
        }
        EndpointAction::ShowRequest {
            endpoint_id,
            request_id,
            org,
        } => {
            let mut config = config::Config::load()?;
            let organization_id = require_organization(org, &config)?;
            let token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
            let request = client
                .get_endpoint_request(&endpoint_id, &request_id)
                .await?;
            if json {
                print_json(&serde_json::json!({
                    "organization_id": organization_id,
                    "endpoint_id": endpoint_id,
                    "request": request
                }))?;
            } else {
                print_context("Organization:", &organization_id);
                print_context("Endpoint:", &endpoint_id);
                print_endpoint_request_detail(&request);
            }
        }
        EndpointAction::DeleteRequest {
            endpoint_id,
            request_id,
            org,
        } => {
            let mut config = config::Config::load()?;
            let organization_id = require_organization(org, &config)?;
            if !confirm_destructive_action(
                "DELETE CAPTURED REQUEST?",
                &format!("request {request_id} from endpoint {endpoint_id}"),
                &organization_id,
                yes,
                json,
            )? {
                return Ok(ControlFlow::Break(()));
            }
            let token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
            client
                .delete_endpoint_request(&endpoint_id, &request_id)
                .await?;
            if json {
                print_json(&serde_json::json!({
                    "status": "deleted",
                    "organization_id": organization_id,
                    "endpoint_id": endpoint_id,
                    "request_id": request_id
                }))?;
            } else {
                print_status_block(
                    OutputStatus::Ok,
                    "REQUEST DELETED",
                    &[
                        output_field("REQUEST", request_id.bold()),
                        output_field("ENDPOINT", endpoint_id.dim()),
                        output_field("ORGANIZATION", organization_id.dim()),
                    ],
                );
            }
        }
        EndpointAction::ForwardRequest {
            endpoint_id,
            request_id,
            target_url,
            method,
            dry_run,
            org,
        } => {
            run_endpoint_forward_request(
                endpoint_id,
                request_id,
                target_url,
                method,
                dry_run,
                org,
                json,
            )
            .await?;
        }
        EndpointAction::ListForwards {
            endpoint_id,
            request_id,
            page,
            page_size,
            org,
        } => {
            let mut config = config::Config::load()?;
            let organization_id = require_organization(org, &config)?;
            let token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
            let forwards = client
                .list_endpoint_request_forwards(&endpoint_id, &request_id, page, page_size)
                .await?;
            if json {
                print_json(&serde_json::json!({
                    "organization_id": organization_id,
                    "endpoint_id": endpoint_id,
                    "request_id": request_id,
                    "forwards": forwards
                }))?;
            } else {
                print_context("Organization:", &organization_id);
                print_context("Endpoint:", &endpoint_id);
                print_context("Request:", &request_id);
                print_endpoint_request_forwards(&forwards);
            }
        }
        EndpointAction::ShowForward { forward_id, org } => {
            let mut config = config::Config::load()?;
            let organization_id = require_organization(org, &config)?;
            let token = ensure_valid_token(&mut config).await?;
            let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;
            let forward = client.get_forward(&forward_id).await?;
            if json {
                print_json(&serde_json::json!({
                    "organization_id": organization_id,
                    "forward": forward
                }))?;
            } else {
                print_context("Organization:", &organization_id);
                print_forward_detail(&forward);
            }
        }
    }
    Ok(ControlFlow::Continue(()))
}

pub fn validate_forward_target_url(target_url: &str) -> Result<()> {
    let parsed = Url::parse(target_url)
        .map_err(|err| anyhow!("Invalid target URL '{}': {}", target_url, err))?;

    match parsed.scheme() {
        "http" | "https" => Ok(()),
        scheme => Err(anyhow!(
            "Invalid target URL scheme '{}'. Use http or https.",
            scheme
        )),
    }
}

pub fn forward_method(method: Option<&str>, request: &api::DebugRequestDetail) -> String {
    method
        .map(str::to_string)
        .unwrap_or_else(|| request.method.clone())
}

pub fn forward_request_preview_receipt(
    organization_id: &str,
    endpoint_id: &str,
    request_id: &str,
    target_url: &str,
    method: Option<&str>,
    request: &api::DebugRequestDetail,
) -> serde_json::Value {
    let method = forward_method(method, request);
    let request_resource_uri = request_resource_uri(request_id);
    let next_action = format!(
        "Run `hooklistener endpoint forward-request {endpoint_id} {request_id} {target_url}` without --dry-run to queue the forward."
    );

    serde_json::json!({
        "dry_run": true,
        "status": "preview",
        "command": "endpoint forward-request",
        "operation": "forward_request",
        "would_create": "debug_request_forward",
        "risk_level": "external_side_effect",
        "required_confirmation": true,
        "organization_id": organization_id,
        "endpoint_id": endpoint_id,
        "request_id": request_id,
        "target_url": target_url,
        "method": method,
        "source_request": {
            "id": &request.id,
            "method": &request.method,
            "path": &request.path,
            "url": &request.url,
            "resource_uri": &request_resource_uri
        },
        "request_resource_uri": request_resource_uri,
        "receipt_resource_uri_template": "hooklistener://forwards/{forward_id}",
        "next_action": next_action
    })
}

pub fn forward_request_receipt(
    organization_id: &str,
    endpoint_id: &str,
    request_id: &str,
    response: &api::EndpointRequestForwardResponse,
) -> serde_json::Value {
    let forward_resource_uri = forward_resource_uri(&response.forward_id);
    let request_resource_uri = request_resource_uri(request_id);
    let request_forwards_resource_uri = request_forwards_resource_uri(request_id);
    let poll_url = forward_poll_path(&response.forward_id);
    let poll_command = forward_poll_command(&response.forward_id);
    let forwards_command =
        format!("hooklistener endpoint list-forwards {endpoint_id} {request_id}");

    serde_json::json!({
        "status": &response.status,
        "delivery_status": "queued",
        "command": "endpoint forward-request",
        "operation": "forward_request",
        "organization_id": organization_id,
        "endpoint_id": endpoint_id,
        "request_id": request_id,
        "forward_id": &response.forward_id,
        "resource_uri": &forward_resource_uri,
        "forward_resource_uri": &forward_resource_uri,
        "request_resource_uri": &request_resource_uri,
        "poll_url": poll_url,
        "resources": {
            "self": forward_resource_uri,
            "request": request_resource_uri,
            "request_forwards": request_forwards_resource_uri
        },
        "next_actions": [
            poll_command,
            forwards_command
        ],
        "forward": response
    })
}

pub fn print_forward_request_preview(
    organization_id: &str,
    endpoint_id: &str,
    request_id: &str,
    target_url: &str,
    method: Option<&str>,
    request: &api::DebugRequestDetail,
) {
    let method = forward_method(method, request);
    let request_resource_uri = request_resource_uri(request_id);
    let endpoint_id = sanitize_terminal(endpoint_id, TerminalTextLayout::Inline);
    let organization_id = sanitize_terminal(organization_id, TerminalTextLayout::Inline);
    let request_id = sanitize_terminal(request_id, TerminalTextLayout::Inline);

    print_status_block(
        OutputStatus::Info,
        "FORWARD PREVIEW",
        &[
            output_field("DRY RUN", "true"),
            output_field("WOULD CREATE", "debug_request_forward"),
            output_field(
                "TARGET URL",
                sanitize_terminal(target_url, TerminalTextLayout::Inline).underlined(),
            ),
            output_field(
                "METHOD",
                sanitize_terminal(&method, TerminalTextLayout::Inline).bold(),
            ),
            output_field("REQUEST", request_id.dim()),
            output_field(
                "RESOURCE",
                sanitize_terminal(&request_resource_uri, TerminalTextLayout::Inline).dim(),
            ),
            output_field("ENDPOINT", endpoint_id.dim()),
            output_field("ORGANIZATION", organization_id.dim()),
            output_field(
                "NEXT",
                "Run without --dry-run to queue the forward; receipt will be hooklistener://forwards/<forward_id>.",
            ),
        ],
    );
}

pub fn print_forward_request_accepted(
    organization_id: &str,
    endpoint_id: &str,
    request_id: &str,
    response: &api::EndpointRequestForwardResponse,
) {
    let forward_resource = forward_resource_uri(&response.forward_id);
    let poll_command = forward_poll_command(&response.forward_id);

    print_status_block(
        OutputStatus::Ok,
        "FORWARD ACCEPTED",
        &[
            output_field(
                "FORWARD ID",
                sanitize_terminal(&response.forward_id, TerminalTextLayout::Inline).bold(),
            ),
            output_field(
                "STATUS",
                sanitize_terminal(&response.status, TerminalTextLayout::Inline).bold(),
            ),
            output_field(
                "TARGET URL",
                sanitize_terminal(&response.target_url, TerminalTextLayout::Inline).underlined(),
            ),
            output_field(
                "RESOURCE",
                sanitize_terminal(&forward_resource, TerminalTextLayout::Inline).dim(),
            ),
            output_field(
                "POLL",
                sanitize_terminal(&poll_command, TerminalTextLayout::Inline).dim(),
            ),
            output_field(
                "REQUEST",
                sanitize_terminal(request_id, TerminalTextLayout::Inline).dim(),
            ),
            output_field(
                "ENDPOINT",
                sanitize_terminal(endpoint_id, TerminalTextLayout::Inline).dim(),
            ),
            output_field(
                "ORGANIZATION",
                sanitize_terminal(organization_id, TerminalTextLayout::Inline).dim(),
            ),
        ],
    );
}

pub async fn run_endpoint_forward_request(
    endpoint_id: String,
    request_id: String,
    target_url: String,
    method: Option<HttpMethod>,
    dry_run: bool,
    org: Option<String>,
    json: bool,
) -> Result<()> {
    let mut config = config::Config::load()?;
    let organization_id = require_organization(org, &config)?;
    let token = ensure_valid_token(&mut config).await?;
    let method = method.map(HttpMethod::as_uppercase);
    validate_forward_target_url(&target_url)?;
    let client = ApiClient::with_organization(token, Some(organization_id.clone()))?;

    if dry_run {
        let request = client
            .get_endpoint_request(&endpoint_id, &request_id)
            .await?;

        if json {
            print_json(&forward_request_preview_receipt(
                &organization_id,
                &endpoint_id,
                &request_id,
                &target_url,
                method,
                &request,
            ))?;
        } else {
            print_forward_request_preview(
                &organization_id,
                &endpoint_id,
                &request_id,
                &target_url,
                method,
                &request,
            );
        }

        return Ok(());
    }

    let response = client
        .forward_endpoint_request(&endpoint_id, &request_id, &target_url, method)
        .await?;

    if json {
        print_json(&forward_request_receipt(
            &organization_id,
            &endpoint_id,
            &request_id,
            &response,
        ))?;
    } else {
        print_forward_request_accepted(&organization_id, &endpoint_id, &request_id, &response);
    }

    Ok(())
}

pub fn print_endpoints(endpoints: &[api::DebugEndpointSummary]) {
    if endpoints.is_empty() {
        print_empty_state(
            "NO DEBUG ENDPOINTS FOUND",
            "Run `hooklistener endpoint create <name>` to create one.",
        );
        return;
    }

    let mut table = new_table(&["ID", "Slug", "Status", "Webhook URL", "Name"]);
    for endpoint in endpoints {
        table.add_row(vec![
            sanitize_terminal(&endpoint.id, TerminalTextLayout::Inline).into_owned(),
            sanitize_terminal(&endpoint.slug, TerminalTextLayout::Inline).into_owned(),
            sanitize_terminal(&endpoint.status, TerminalTextLayout::Inline).into_owned(),
            sanitize_terminal(&endpoint.webhook_url, TerminalTextLayout::Inline).into_owned(),
            sanitize_terminal(&endpoint.name, TerminalTextLayout::Inline).into_owned(),
        ]);
    }
    println!("{table}");
}

pub fn print_endpoint_detail(endpoint: &api::DebugEndpointSummary) {
    print_field(
        "ID",
        sanitize_terminal(&endpoint.id, TerminalTextLayout::Inline),
    );
    print_field(
        "SLUG",
        sanitize_terminal(&endpoint.slug, TerminalTextLayout::Inline),
    );
    print_field(
        "STATUS",
        sanitize_terminal(&endpoint.status, TerminalTextLayout::Inline),
    );
    print_field(
        "WEBHOOK URL",
        sanitize_terminal(&endpoint.webhook_url, TerminalTextLayout::Inline).underlined(),
    );
    print_field(
        "NAME",
        sanitize_terminal(&endpoint.name, TerminalTextLayout::Inline),
    );
    if let Some(created_at) = endpoint.created_at.as_deref() {
        print_field(
            "CREATED AT",
            sanitize_terminal(created_at, TerminalTextLayout::Inline).dim(),
        );
    }
}

pub fn print_endpoint_requests(response: &api::EndpointRequestsResponse) {
    if response.data.is_empty() {
        print_empty_state(
            "NO REQUESTS FOUND",
            "Send a webhook, then run `hooklistener endpoint list-requests <endpoint-id>` again.",
        );
        return;
    }

    let mut table = new_table(&["ID", "Method", "URL", "Remote"]);
    for request in &response.data {
        table.add_row(vec![
            sanitize_terminal(&request.id, TerminalTextLayout::Inline).into_owned(),
            sanitize_terminal(&request.method, TerminalTextLayout::Inline).into_owned(),
            sanitize_terminal(&request.url, TerminalTextLayout::Inline).into_owned(),
            sanitize_terminal(&request.remote_addr, TerminalTextLayout::Inline).into_owned(),
        ]);
    }
    println!("{table}");
    print_pagination(&response.pagination);
}

pub fn print_endpoint_request_detail(request: &api::DebugRequestDetail) {
    print_field(
        "REQUEST ID",
        sanitize_terminal(&request.id, TerminalTextLayout::Inline),
    );
    print_field(
        "METHOD",
        sanitize_terminal(&request.method, TerminalTextLayout::Inline).bold(),
    );
    if let Some(path) = request.path.as_deref() {
        print_field("PATH", sanitize_terminal(path, TerminalTextLayout::Inline));
    }
    print_field(
        "URL",
        sanitize_terminal(&request.url, TerminalTextLayout::Inline),
    );

    if let Some(status_remote) = request.remote_addr.as_deref() {
        print_field(
            "REMOTE",
            sanitize_terminal(status_remote, TerminalTextLayout::Inline),
        );
    }
    if let Some(content_length) = request.content_length {
        print_field("CONTENT LEN", content_length);
    }
    if let Some(created_at) = request.created_at.as_deref() {
        print_field(
            "CREATED AT",
            sanitize_terminal(created_at, TerminalTextLayout::Inline).dim(),
        );
    }

    println!();
    print_key_value_map("Headers:", &request.headers, ": ");

    println!();
    print_key_value_map("Query Params:", &request.query_params, "=");

    println!();
    print_body_section(
        "Body:",
        request.body.as_deref().or(request.body_preview.as_deref()),
    );
}

pub fn print_endpoint_request_forwards(response: &api::EndpointRequestForwardsResponse) {
    if response.data.is_empty() {
        print_empty_state(
            "NO FORWARDS FOUND",
            "Run `hooklistener endpoint forward-request <endpoint-id> <request-id> <url>`.",
        );
        return;
    }

    let mut table = new_table(&["ID", "Method", "Status", "Duration", "Target"]);
    for forward in &response.data {
        let status = forward
            .status_code
            .map(|c| c.to_string())
            .unwrap_or_else(|| "-".into());
        let duration = forward
            .duration_ms
            .map(|ms| format!("{ms}ms"))
            .unwrap_or_else(|| "-".into());
        let target = match forward.error_message.as_deref() {
            Some(err) => format!(
                "{}\n  [ERR] {}",
                sanitize_terminal(&forward.target_url, TerminalTextLayout::Inline),
                sanitize_terminal(err, TerminalTextLayout::Inline)
            ),
            None => sanitize_terminal(&forward.target_url, TerminalTextLayout::Inline).into_owned(),
        };
        table.add_row(vec![
            sanitize_terminal(&forward.id, TerminalTextLayout::Inline).into_owned(),
            sanitize_terminal(&forward.method, TerminalTextLayout::Inline).into_owned(),
            status,
            duration,
            target,
        ]);
    }
    println!("{table}");
    print_pagination(&response.pagination);
}

pub fn print_forward_detail(forward: &api::DebugRequestForwardDetail) {
    print_field(
        "FORWARD ID",
        sanitize_terminal(&forward.id, TerminalTextLayout::Inline),
    );
    print_field(
        "REQUEST ID",
        sanitize_terminal(&forward.debug_request_id, TerminalTextLayout::Inline),
    );
    print_field(
        "TARGET URL",
        sanitize_terminal(&forward.target_url, TerminalTextLayout::Inline),
    );
    print_field(
        "METHOD",
        sanitize_terminal(&forward.method, TerminalTextLayout::Inline).bold(),
    );
    if let Some(status_code) = forward.status_code {
        print_field("STATUS", style_status_code(status_code));
    } else {
        print_field("STATUS", "(pending)".yellow());
    }
    if let Some(duration_ms) = forward.duration_ms {
        print_field("DURATION", format!("{duration_ms}ms"));
    }
    if let Some(attempted_at) = forward.attempted_at.as_deref() {
        print_field(
            "ATTEMPTED AT",
            sanitize_terminal(attempted_at, TerminalTextLayout::Inline).dim(),
        );
    }
    if let Some(error_message) = forward.error_message.as_deref() {
        print_field(
            "ERROR",
            sanitize_terminal(error_message, TerminalTextLayout::Inline),
        );
    }

    println!();
    print_key_value_map("Request Headers:", &forward.request_headers, ": ");

    if forward
        .request_body
        .as_deref()
        .is_some_and(|b| !b.is_empty())
    {
        println!();
        print_body_section("Request Body:", forward.request_body.as_deref());
    }

    println!();
    print_key_value_map("Response Headers:", &forward.response_headers, ": ");

    if forward
        .response_body
        .as_deref()
        .is_some_and(|b| !b.is_empty())
    {
        println!();
        print_body_section("Response Body:", forward.response_body.as_deref());
    }
}
