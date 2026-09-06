//! Saved endpoint case commands. Local fixtures and tunnel investigation cases are separate workflows.

use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use clap::{Args, Subcommand};
use serde_json::{Map, Value, json};
use tokio::time::{Instant, sleep, timeout_at};

use crate::api::{
    ApiClient, CaseRunParams, CaseRunResult,
    cases::{CaseSuite, SavedCase, validate_action_key, validate_id},
};
use crate::{
    CaseRunInput, build_case_run_params, config, ensure_valid_token, require_organization,
};

#[derive(Subcommand)]
pub enum CasesAction {
    /// List saved cases for an endpoint
    List(EndpointArgs),
    /// Save a captured request as a case
    Save {
        endpoint_id: String,
        request_id: String,
        #[command(flatten)]
        options: Box<CaseOptions>,
        #[arg(long)]
        org: Option<String>,
    },
    /// Show a saved case and its assertions
    Show(ResourceArgs),
    /// Update a saved case (omitted fields remain unchanged)
    Update {
        case_id: String,
        #[command(flatten)]
        options: Box<CaseOptions>,
        #[arg(long)]
        org: Option<String>,
    },
    /// Queue one saved case for delivery by the service or an active CLI listener
    Replay {
        case_id: String,
        #[command(flatten)]
        destination: DestinationArgs,
        /// Override the HTTP method for this delivery only
        #[arg(long)]
        method: Option<String>,
        /// JSON object of request header overrides for this delivery only
        #[arg(long)]
        headers: Option<String>,
        /// Request body override for this delivery only
        #[arg(long)]
        body: Option<String>,
        /// Delivery key; reuse with identical inputs to recover a lost receipt (default: UUID)
        #[arg(long, conflicts_with = "dry_run")]
        idempotency_key: Option<String>,
        /// Server preview of scope and target policy; queues no delivery
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        org: Option<String>,
    },
    /// Run saved cases for an endpoint
    Run {
        endpoint_id: String,
        /// Target URL (service-side), saved target ID, or "cli" (local listener)
        #[arg(long, conflicts_with_all = ["target_url", "target_id"])]
        target: Option<String>,
        /// Explicit service-side destination URL; use --target cli for local delivery
        #[arg(long, conflicts_with_all = ["target", "target_id"])]
        target_url: Option<String>,
        #[arg(long, conflicts_with_all = ["target", "target_url"])]
        target_id: Option<String>,
        /// Optional display name to save for the target
        #[arg(long)]
        target_name: Option<String>,
        /// Run only this named suite ID (see cases suites list)
        #[arg(long, alias = "suite")]
        case_suite_id: Option<String>,
        /// Wait for completion using read-only polling; timeout does not cancel delivery
        #[arg(long, conflicts_with = "dry_run")]
        wait: bool,
        /// Maximum wait, such as 60s or 2m (default 30s, maximum 1h)
        #[arg(long, conflicts_with = "timeout_ms")]
        timeout: Option<String>,
        #[arg(long, conflicts_with = "timeout")]
        timeout_ms: Option<u64>,
        /// Read-only polling interval (100 to 30000 milliseconds)
        #[arg(long, value_parser = clap::value_parser!(u64).range(100..=30_000))]
        interval_ms: Option<u64>,
        /// Delivery key; reuse with identical inputs to recover a lost receipt (default: UUID)
        #[arg(long, conflicts_with = "dry_run")]
        idempotency_key: Option<String>,
        /// Server preview of scope and target policy; queues no delivery
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        org: Option<String>,
    },
    /// Discover named suites and their members
    Suites {
        #[command(subcommand)]
        action: SuiteAction,
    },
    /// Inspect run history and wait for existing runs without requeueing
    Runs {
        #[command(subcommand)]
        action: RunAction,
    },
}

#[derive(Args)]
pub struct EndpointArgs {
    pub endpoint_id: String,
    #[arg(long)]
    pub org: Option<String>,
}

#[derive(Args)]
pub struct ResourceArgs {
    pub id: String,
    #[arg(long)]
    pub org: Option<String>,
}

#[derive(Args, Default)]
pub struct CaseOptions {
    #[arg(long)]
    name: Option<String>,
    #[arg(long)]
    notes: Option<String>,
    /// Default service-side replay destination (not a local forwarding URL)
    #[arg(long)]
    default_target_url: Option<String>,
    #[arg(long)]
    method: Option<String>,
    /// JSON object of default request header overrides
    #[arg(long)]
    headers: Option<String>,
    /// Default request body override
    #[arg(long)]
    body: Option<String>,
    /// Expected response status, 100 to 599
    #[arg(long, value_parser = clap::value_parser!(u16).range(100..=599), conflicts_with = "clear_assertions")]
    expect_status: Option<u16>,
    /// Nonempty JSON object expected as a subset of the response body
    #[arg(long, conflicts_with = "clear_assertions")]
    expect_json: Option<String>,
    /// Remove all assertions; delivery completion will no longer prove a test passed
    #[arg(long)]
    clear_assertions: bool,
}

#[derive(Args)]
pub struct DestinationArgs {
    /// Service-side URL, saved target ID, or "cli" for an active local listener
    #[arg(long, conflicts_with_all = ["target_url", "target_id"])]
    target: Option<String>,
    #[arg(long, conflicts_with_all = ["target", "target_id"])]
    target_url: Option<String>,
    #[arg(long, conflicts_with_all = ["target", "target_url"])]
    target_id: Option<String>,
}

#[derive(Subcommand)]
pub enum SuiteAction {
    List(EndpointArgs),
    Show(ResourceArgs),
}

#[derive(Subcommand)]
pub enum RunAction {
    List {
        endpoint_id: String,
        #[arg(long, default_value = "1", value_parser = clap::value_parser!(u32).range(1..))]
        page: u32,
        #[arg(long, default_value = "20", value_parser = clap::value_parser!(u32).range(1..=100))]
        page_size: u32,
        #[arg(long, alias = "suite")]
        case_suite_id: Option<String>,
        #[arg(long)]
        org: Option<String>,
    },
    Show(ResourceArgs),
    /// Wait for an existing run; never queues another delivery
    Wait {
        id: String,
        #[arg(long, conflicts_with = "timeout_ms")]
        timeout: Option<String>,
        #[arg(long, conflicts_with = "timeout")]
        timeout_ms: Option<u64>,
        #[arg(long, value_parser = clap::value_parser!(u64).range(100..=30_000))]
        interval_ms: Option<u64>,
        #[arg(long)]
        org: Option<String>,
    },
}

impl CasesAction {
    fn organization(&self) -> Option<String> {
        match self {
            Self::List(args)
            | Self::Suites {
                action: SuiteAction::List(args),
            } => args.org.clone(),
            Self::Show(args)
            | Self::Suites {
                action: SuiteAction::Show(args),
            }
            | Self::Runs {
                action: RunAction::Show(args),
            } => args.org.clone(),
            Self::Save { org, .. }
            | Self::Update { org, .. }
            | Self::Replay { org, .. }
            | Self::Run { org, .. }
            | Self::Runs {
                action: RunAction::List { org, .. },
            }
            | Self::Runs {
                action: RunAction::Wait { org, .. },
            } => org.clone(),
        }
    }
}

impl CaseOptions {
    fn attrs(&self) -> Result<Value> {
        let mut attrs = Map::new();
        for (key, value) in [
            ("name", &self.name),
            ("notes", &self.notes),
            ("default_target_url", &self.default_target_url),
            ("default_request_body", &self.body),
        ] {
            if let Some(value) = value {
                attrs.insert(key.into(), json!(value));
            }
        }
        if let Some(url) = &self.default_target_url {
            validate_destination_url(url)?;
        }
        if let Some(method) = crate::normalize_http_method(self.method.clone())? {
            attrs.insert("default_method".into(), json!(method));
        }
        if let Some(headers) = &self.headers {
            attrs.insert("default_request_headers".into(), header_object(headers)?);
        }
        let mut assertions = Map::new();
        if let Some(status) = self.expect_status {
            assertions.insert("expected_status_code".into(), json!(status));
        }
        if let Some(raw) = &self.expect_json {
            let subset = json_object(raw, "--expect-json")?;
            if subset.as_object().is_none_or(|map| map.is_empty()) {
                bail!("--expect-json requires a nonempty JSON object.");
            }
            assertions.insert("expected_json_body_subset".into(), subset);
        }
        if self.clear_assertions || !assertions.is_empty() {
            attrs.insert("assertion_config".into(), Value::Object(assertions));
        }
        Ok(Value::Object(attrs))
    }
}

fn json_object(raw: &str, flag: &str) -> Result<Value> {
    // Do not include parser errors or the supplied value: header/body arguments may contain secrets.
    let value: Value =
        serde_json::from_str(raw).map_err(|_| anyhow!("{flag} requires valid JSON."))?;
    if !value.is_object() {
        bail!("{flag} requires a JSON object.");
    }
    Ok(value)
}

fn header_object(raw: &str) -> Result<Value> {
    let value = json_object(raw, "--headers")?;
    if let Some(headers) = value.as_object() {
        for (name, value) in headers {
            reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| anyhow!("Invalid request header name."))?;
            let value = value
                .as_str()
                .ok_or_else(|| anyhow!("Header values must be strings."))?;
            reqwest::header::HeaderValue::from_str(value)
                .map_err(|_| anyhow!("Invalid request header value."))?;
        }
    }
    Ok(value)
}

fn validate_destination_url(raw: &str) -> Result<()> {
    let url = reqwest::Url::parse(raw).map_err(|_| anyhow!("Invalid destination URL."))?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        bail!("Destination must be an HTTP(S) URL without credentials or a fragment.");
    }
    Ok(())
}

fn validate_target(params: &CaseRunParams) -> Result<()> {
    if let Some(url) = &params.target_url {
        validate_destination_url(url)?;
    }
    if let Some(id) = &params.target_id {
        validate_id(id)?;
    }
    Ok(())
}

fn wait_settings(
    timeout: Option<String>,
    timeout_ms: Option<u64>,
    interval_ms: Option<u64>,
) -> Result<(Duration, Duration)> {
    let ms = crate::parse_timeout_ms(timeout, timeout_ms)?.unwrap_or(30_000);
    if ms > 3_600_000 {
        bail!("Maximum case wait is 1h. Resume with cases runs wait <run-id>.");
    }
    let interval = interval_ms.unwrap_or(250);
    if !(100..=30_000).contains(&interval) {
        bail!("Polling interval must be 100 to 30000 milliseconds.");
    }
    Ok((Duration::from_millis(ms), Duration::from_millis(interval)))
}

/// Poll only GETs after the initial delivery receipt. Timing out never requeues or cancels work.
async fn wait_for_run(
    client: &ApiClient,
    mut result: CaseRunResult,
    duration: Duration,
    interval: Duration,
) -> Result<CaseRunResult> {
    let id = result
        .case_suite_run_id
        .as_deref()
        .or(result.id.as_deref())
        .ok_or_else(|| anyhow!("Run response has no ID; do not rerun the delivery blindly."))?
        .to_owned();
    validate_id(&id)?;
    let deadline = Instant::now() + duration;
    while result.result_status == "pending"
        || (result.result_status == "timeout"
            && (result.waiting_count > 0 || result.queued_count > result.completed_count))
    {
        let refresh = async {
            sleep(interval).await;
            client.get_case_run(&id).await
        };
        match timeout_at(deadline, refresh).await {
            Ok(Ok(updated)) => result = updated,
            Ok(Err(error)) => {
                return Err(error.context(format!(
                    "Run {id} remains inspectable with cases runs show; delivery was not retried"
                )));
            }
            Err(_) => {
                result.result_status = "timeout".into();
                result.timed_out = Some(true);
                result.waited = Some(true);
                return Ok(result);
            }
        }
    }
    result.waited = Some(true);
    Ok(result)
}

pub async fn execute(action: CasesAction, json_output: bool) -> Result<bool> {
    let mut config = config::Config::load()?;
    let org = require_organization(action.organization(), &config)?;
    let token = ensure_valid_token(&mut config).await?;
    let client = ApiClient::with_organization(token, Some(org.clone()))?;
    execute_with_client(action, &client, &org, json_output).await
}

async fn execute_with_client(
    action: CasesAction,
    client: &ApiClient,
    org: &str,
    json_output: bool,
) -> Result<bool> {
    match action {
        CasesAction::List(args) => {
            let cases = client.list_cases(&args.endpoint_id).await?;
            emit(
                &json!({"organization_id":org,"cases":cases}),
                json_output,
                || print_cases(&cases),
            )?;
        }
        CasesAction::Show(args) => {
            let case = client.get_case(&args.id).await?;
            emit(
                &json!({"organization_id":org,"case":case}),
                json_output,
                || print_case(&case),
            )?;
        }
        CasesAction::Save {
            endpoint_id,
            request_id,
            options,
            ..
        } => {
            let attrs = options.attrs()?;
            let case = client.save_case(&endpoint_id, &request_id, &attrs).await?;
            emit(
                &json!({"organization_id":org,"case":case}),
                json_output,
                || print_case(&case),
            )?;
        }
        CasesAction::Update {
            case_id, options, ..
        } => {
            let mut attrs = options.attrs()?;
            if attrs.as_object().is_none_or(|map| map.is_empty()) {
                bail!("Provide at least one case field to update.");
            }
            // Backend assertion_config is a replacement, not a merge. Preserve other assertions.
            if !options.clear_assertions
                && let Some(new) = attrs.get("assertion_config")
            {
                let case = client.get_case(&case_id).await?;
                let mut config = case
                    .assertion_config
                    .as_object()
                    .cloned()
                    .unwrap_or_default();
                if let Some(new) = new.as_object() {
                    config.extend(new.clone());
                }
                attrs["assertion_config"] = json!(config);
            }
            let case = client.update_case(&case_id, &attrs).await?;
            emit(
                &json!({"organization_id":org,"case":case}),
                json_output,
                || print_case(&case),
            )?;
        }
        CasesAction::Replay {
            case_id,
            destination,
            method,
            headers,
            body,
            idempotency_key,
            dry_run,
            ..
        } => {
            let params = build_case_run_params(CaseRunInput {
                target: destination.target,
                target_url: destination.target_url,
                target_id: destination.target_id,
                target_name: None,
                wait: false,
                timeout: None,
                timeout_ms: None,
                interval_ms: None,
            })?;
            validate_target(&params)?;
            let mut body_params = serde_json::to_value(&params)?;
            if let Some(method) = crate::normalize_http_method(method)? {
                body_params["method"] = json!(method);
            }
            if let Some(headers) = headers {
                body_params["request_headers"] = header_object(&headers)?;
            }
            if let Some(body) = body {
                body_params["request_body"] = json!(body);
            }
            if dry_run {
                let preview = client.preview_case_replay(&case_id, &body_params).await?;
                print_preview(&preview, json_output)?;
            } else {
                let key = delivery_key(idempotency_key)?;
                let replay = client.replay_case(&case_id, &body_params, &key).await?;
                emit(
                    &json!({"organization_id":org,"replay":replay}),
                    json_output,
                    || {
                        crate::print_status(crate::OutputStatus::Info, "CASE REPLAY");
                        field("FORWARD ID", &replay.forward_id);
                        field("IDEMPOTENCY KEY", &replay.idempotency.key);
                        field("DISPOSITION", &replay.idempotency.disposition);
                        field("STATUS", &replay.status);
                        field(
                            "ASSERTION",
                            replay
                                .assertion_status
                                .as_deref()
                                .unwrap_or("not_configured"),
                        );
                        field(
                            "INSPECT",
                            &format!("hooklistener endpoint forward {}", replay.forward_id),
                        );
                    },
                )?;
                return Ok(replay.status == "failed"
                    || matches!(replay.assertion_status.as_deref(), Some("failed" | "error")));
            }
        }
        CasesAction::Run {
            endpoint_id,
            target,
            target_url,
            target_id,
            target_name,
            case_suite_id,
            wait,
            timeout,
            timeout_ms,
            interval_ms,
            idempotency_key,
            dry_run,
            ..
        } => {
            let settings = wait_settings(timeout.clone(), timeout_ms, interval_ms)?;
            if !wait && (timeout.is_some() || timeout_ms.is_some() || interval_ms.is_some()) {
                bail!("Wait options require --wait.");
            }
            let mut params = build_case_run_params(CaseRunInput {
                target,
                target_url,
                target_id,
                target_name,
                wait: false,
                timeout: None,
                timeout_ms: None,
                interval_ms: None,
            })?;
            params.case_suite_id = case_suite_id;
            validate_target(&params)?;
            validate_id(&endpoint_id)?;
            if dry_run {
                let preview = client.preview_case_run(&endpoint_id, &params).await?;
                print_preview(&preview, json_output)?;
            } else {
                let key = delivery_key(idempotency_key)?;
                let mut result = client
                    .run_endpoint_cases(&endpoint_id, &params, &key)
                    .await?;
                let receipt = result.idempotency.take();
                if wait {
                    result = wait_for_run(client, result, settings.0, settings.1).await?;
                }
                result.idempotency = receipt;
                return emit_run(&result, org, json_output);
            }
        }
        CasesAction::Suites { action } => match action {
            SuiteAction::List(args) => {
                let suites = client.list_case_suites(&args.endpoint_id).await?;
                emit(
                    &json!({"organization_id":org,"suites":suites}),
                    json_output,
                    || print_suites(&suites),
                )?;
            }
            SuiteAction::Show(args) => {
                let suite = client.get_case_suite(&args.id).await?;
                emit(
                    &json!({"organization_id":org,"suite":suite}),
                    json_output,
                    || {
                        field("SUITE ID", &suite.id);
                        field("NAME", &suite.name);
                        field("ENDPOINT", &suite.endpoint_id);
                        let mut table = crate::new_table(&["Position", "Case ID", "Name"]);
                        for member in &suite.cases {
                            table.add_row([
                                member.position.to_string(),
                                safe(&member.case.id),
                                safe(&member.case.name),
                            ]);
                        }
                        println!("{table}");
                    },
                )?;
            }
        },
        CasesAction::Runs { action } => match action {
            RunAction::List {
                endpoint_id,
                page,
                page_size,
                case_suite_id,
                ..
            } => {
                let history = client
                    .list_case_runs(&endpoint_id, page, page_size, case_suite_id.as_deref())
                    .await?;
                emit(
                    &json!({"organization_id":org,"runs":history.data,"pagination":history.pagination}),
                    json_output,
                    || {
                        if history.data.is_empty() {
                            println!("No case runs found.");
                            return;
                        }
                        let mut table =
                            crate::new_table(&["Run ID", "Result", "Passed", "Unasserted"]);
                        for run in &history.data {
                            table.add_row([
                                safe(
                                    run.case_suite_run_id
                                        .as_deref()
                                        .or(run.id.as_deref())
                                        .unwrap_or("-"),
                                ),
                                safe(&run.result_status),
                                run.passed_count.to_string(),
                                run.not_configured_count.to_string(),
                            ]);
                        }
                        println!("{table}");
                        crate::print_pagination(&history.pagination);
                    },
                )?;
            }
            RunAction::Show(args) => {
                return emit_run(&client.get_case_run(&args.id).await?, org, json_output);
            }
            RunAction::Wait {
                id,
                timeout,
                timeout_ms,
                interval_ms,
                ..
            } => {
                let (duration, interval) = wait_settings(timeout, timeout_ms, interval_ms)?;
                let started = Instant::now();
                // Initial GET also consumes the wait budget; an API timeout is not a test result.
                let fetch_budget = if duration.is_zero() {
                    Duration::from_secs(10)
                } else {
                    duration
                };
                let initial = tokio::time::timeout(fetch_budget, client.get_case_run(&id)).await
                    .map_err(|_| anyhow!("Timed out reading the run; use cases runs show to inspect it. No delivery was queued."))??;
                let result = wait_for_run(
                    client,
                    initial,
                    duration.saturating_sub(started.elapsed()),
                    interval,
                )
                .await?;
                return emit_run(&result, org, json_output);
            }
        },
    }
    Ok(false)
}

fn delivery_key(key: Option<String>) -> Result<String> {
    let key = key.unwrap_or_else(|| uuid::Uuid::new_v4().to_string());
    validate_action_key(&key)?;
    // Print before sending: this remains recoverable even if no response arrives.
    // Stderr preserves the single structured JSON object on stdout.
    eprintln!("Idempotency key: {key}. Recover with the SAME key and delivery inputs.");
    Ok(key)
}

fn print_preview(preview: &Value, json_output: bool) -> Result<()> {
    emit(preview, json_output, || {
        crate::print_status(crate::OutputStatus::Info, "CASE PREVIEW");
        field("OPERATION", preview["operation"].as_str().unwrap_or("-"));
        field(
            "CASES",
            &preview["case_ids"]
                .as_array()
                .map_or(0, Vec::len)
                .to_string(),
        );
        field(
            "TARGET",
            preview["target"]["url"]
                .as_str()
                .or(preview["target"]["label"].as_str())
                .or(preview["target"]["type"].as_str())
                .unwrap_or("-"),
        );
        println!("No delivery queued. Preview does not reserve a target or prove delivery.");
    })
}

fn emit_run(result: &CaseRunResult, org: &str, json_output: bool) -> Result<bool> {
    // Preserve the original cases run JSON shape.
    emit(result, json_output, || {
        crate::print_context("Organization:", org);
        crate::print_case_run_result(result);
        if let Some(receipt) = &result.idempotency {
            field("IDEMPOTENCY KEY", &receipt.key);
            field("DISPOSITION", &receipt.disposition);
        }
    })?;
    Ok(crate::case_run_failed(result))
}

fn emit<T: serde::Serialize>(value: &T, json_output: bool, human: impl FnOnce()) -> Result<()> {
    if json_output {
        crate::print_json(value)?;
    } else {
        human();
    }
    Ok(())
}

fn safe(value: &str) -> String {
    crate::sanitize_terminal_display(value)
}
fn field(label: &str, value: &str) {
    crate::print_field(label, safe(value));
}

fn print_case(case: &SavedCase) {
    field("CASE ID", &case.id);
    field("NAME", &case.name);
    field("ENDPOINT", &case.endpoint_id);
    field("REQUEST", &case.source_debug_request_id);
    field("METHOD", case.method.as_deref().unwrap_or("-"));
    field("PATH", case.path.as_deref().unwrap_or("-"));
    field(
        "TARGET",
        case.default_target_url
            .as_deref()
            .unwrap_or("not configured"),
    );
    if let Some(notes) = &case.notes {
        field("NOTES", notes);
    }
    field("ASSERTIONS", &case.assertion_config.to_string());
    println!("Use --json for request overrides and full case metadata.");
}

fn print_cases(cases: &[SavedCase]) {
    if cases.is_empty() {
        println!("No saved cases. Use hooklistener cases save <endpoint-id> <request-id>.");
        return;
    }
    let mut table = crate::new_table(&["Case ID", "Name", "Method", "Path"]);
    for case in cases {
        table.add_row([
            safe(&case.id),
            safe(&case.name),
            safe(case.method.as_deref().unwrap_or("-")),
            safe(case.path.as_deref().unwrap_or("-")),
        ]);
    }
    println!("{table}");
}

#[cfg(test)]
mod tests;

fn print_suites(suites: &[CaseSuite]) {
    if suites.is_empty() {
        println!("No named suites. Create a suite in Hooklistener or through MCP.");
        return;
    }
    let mut table = crate::new_table(&["Suite ID", "Name", "Cases"]);
    for suite in suites {
        table.add_row([
            safe(&suite.id),
            safe(&suite.name),
            suite.case_count.to_string(),
        ]);
    }
    println!("{table}");
}
