//! Saved endpoint case commands. Local fixtures and tunnel investigation cases are separate workflows.

use anyhow::{Result, anyhow, bail};
use clap::{Args, Subcommand};
use serde_json::{Map, Value, json};
use std::time::Duration;
use tokio::time::{Instant, sleep, timeout_at};

use crate::api;
use crate::api::{
    ApiClient, CaseRunParams, CaseRunResult,
    cases::{CaseSuite, SavedCase, validate_action_key, validate_id},
};
use crate::cli::{HttpMethod, duration_millis, resolve_millis_flag};
use crate::config;
use crate::credentials::{ensure_valid_token, require_organization};
use crate::output::Stylize;
use crate::render::{
    OutputStatus, TerminalTextLayout, new_table, print_field, print_section, print_status,
    sanitize_terminal, sanitize_terminal_display, value_or_dash, yes_no,
};

#[derive(Subcommand)]
pub enum CasesAction {
    /// List saved cases for an endpoint
    List(EndpointArgs),
    /// Save a captured request as a case
    Save {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Captured request ID
        request_id: String,
        #[command(flatten)]
        options: Box<CaseOptions>,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Show a saved case and its assertions
    Show(CaseArgs),
    /// Update a saved case (omitted fields remain unchanged)
    Update {
        /// Saved case ID
        case_id: String,
        #[command(flatten)]
        options: Box<CaseOptions>,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Queue one saved case for delivery by the service or an active CLI listener
    Replay {
        /// Saved case ID
        case_id: String,
        #[command(flatten)]
        destination: DestinationArgs,
        /// Override the HTTP method for this delivery only
        #[arg(long, value_name = "METHOD", ignore_case = true)]
        method: Option<HttpMethod>,
        /// JSON object of request header overrides for this delivery only
        #[arg(long, value_name = "JSON")]
        headers: Option<String>,
        /// Request body override for this delivery only
        #[arg(long, value_name = "BODY")]
        body: Option<String>,
        /// Delivery key; reuse with identical inputs to recover a lost receipt [default: new UUID]
        #[arg(long, value_name = "KEY", conflicts_with = "dry_run")]
        idempotency_key: Option<String>,
        /// Server preview of scope and target policy; queues no delivery
        #[arg(long)]
        dry_run: bool,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Run saved cases for an endpoint
    Run {
        /// Debug endpoint ID
        endpoint_id: String,
        #[command(flatten)]
        destination: DestinationArgs,
        /// Display name recorded for the target
        #[arg(long, value_name = "NAME")]
        target_name: Option<String>,
        /// Run only this named suite (see cases suites list)
        #[arg(long, alias = "suite", value_name = "SUITE_ID")]
        case_suite_id: Option<String>,
        /// Wait for completion using read-only polling; timeout does not cancel delivery
        #[arg(long, conflicts_with = "dry_run")]
        wait: bool,
        #[command(flatten)]
        wait_options: WaitArgs,
        /// Delivery key; reuse with identical inputs to recover a lost receipt [default: new UUID]
        #[arg(long, value_name = "KEY", conflicts_with = "dry_run")]
        idempotency_key: Option<String>,
        /// Server preview of scope and target policy; queues no delivery
        #[arg(long)]
        dry_run: bool,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
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
    /// Debug endpoint ID
    pub endpoint_id: String,
    /// Organization ID (overrides the configured default)
    #[arg(short = 'o', long, value_name = "ORG_ID")]
    pub org: Option<String>,
}

#[derive(Args)]
pub struct CaseArgs {
    /// Saved case ID
    #[arg(value_name = "CASE_ID")]
    pub id: String,
    /// Organization ID (overrides the configured default)
    #[arg(short = 'o', long, value_name = "ORG_ID")]
    pub org: Option<String>,
}

#[derive(Args)]
pub struct SuiteArgs {
    /// Case suite ID
    #[arg(value_name = "SUITE_ID")]
    pub id: String,
    /// Organization ID (overrides the configured default)
    #[arg(short = 'o', long, value_name = "ORG_ID")]
    pub org: Option<String>,
}

#[derive(Args)]
pub struct RunArgs {
    /// Case run ID
    #[arg(value_name = "RUN_ID")]
    pub id: String,
    /// Organization ID (overrides the configured default)
    #[arg(short = 'o', long, value_name = "ORG_ID")]
    pub org: Option<String>,
}

#[derive(Args, Default)]
pub struct CaseOptions {
    /// Case display name
    #[arg(long, value_name = "NAME")]
    name: Option<String>,
    /// Free-form notes stored with the case
    #[arg(long, value_name = "TEXT")]
    notes: Option<String>,
    /// Default service-side replay destination (not a local forwarding URL)
    #[arg(long, value_name = "URL")]
    default_target_url: Option<String>,
    /// Default HTTP method for replays
    #[arg(long, value_name = "METHOD", ignore_case = true)]
    method: Option<HttpMethod>,
    /// JSON object of default request header overrides
    #[arg(long, value_name = "JSON")]
    headers: Option<String>,
    /// Default request body override
    #[arg(long, value_name = "BODY")]
    body: Option<String>,
    /// Expected response status code
    #[arg(long, value_name = "CODE", value_parser = clap::value_parser!(u16).range(100..=599), conflicts_with = "clear_assertions")]
    expect_status: Option<u16>,
    /// Nonempty JSON object expected as a subset of the response body
    #[arg(long, value_name = "JSON", conflicts_with = "clear_assertions")]
    expect_json: Option<String>,
    /// Remove all assertions; delivery completion will no longer prove a test passed
    #[arg(long)]
    clear_assertions: bool,
}

#[derive(Args)]
pub struct DestinationArgs {
    /// Target: a service-side URL, a saved target ID, or cli for an active listen session
    #[arg(long, value_name = "TARGET", conflicts_with_all = ["target_url", "target_id"])]
    pub(crate) target: Option<String>,
    #[arg(long, hide = true, value_name = "URL", conflicts_with_all = ["target", "target_id"])]
    pub(crate) target_url: Option<String>,
    #[arg(long, hide = true, value_name = "TARGET_ID", conflicts_with_all = ["target", "target_url"])]
    pub(crate) target_id: Option<String>,
}

#[derive(Args)]
pub struct WaitArgs {
    /// Maximum wait, such as 60, 2m, or 1h (up to 1h) [default: 30s]
    #[arg(long, value_name = "DURATION", value_parser = crate::cli::parse_duration)]
    pub(crate) timeout: Option<Duration>,
    #[arg(long, hide = true, value_name = "MS", conflicts_with = "timeout")]
    pub(crate) timeout_ms: Option<u64>,
    /// Read-only polling interval, such as 500ms or 2s (100ms to 30s) [default: 250ms]
    #[arg(long, value_name = "DURATION", value_parser = crate::cli::parse_duration_within(MIN_POLL_INTERVAL, MAX_POLL_INTERVAL))]
    pub(crate) interval: Option<Duration>,
    #[arg(long, hide = true, value_name = "MS", conflicts_with = "interval", value_parser = clap::value_parser!(u64).range(100..=30_000))]
    pub(crate) interval_ms: Option<u64>,
}

impl WaitArgs {
    fn is_set(&self) -> bool {
        self.timeout.is_some()
            || self.timeout_ms.is_some()
            || self.interval.is_some()
            || self.interval_ms.is_some()
    }
}

const MIN_POLL_INTERVAL: Duration = Duration::from_millis(100);
const MAX_POLL_INTERVAL: Duration = Duration::from_secs(30);

#[derive(Subcommand)]
pub enum SuiteAction {
    /// List named suites for an endpoint
    List(EndpointArgs),
    /// Show a suite and its member cases
    Show(SuiteArgs),
}

#[derive(Subcommand)]
pub enum RunAction {
    /// List case runs for an endpoint
    List {
        /// Debug endpoint ID
        endpoint_id: String,
        /// Page number
        #[arg(long, value_name = "N", default_value_t = 1, value_parser = clap::value_parser!(u32).range(1..))]
        page: u32,
        /// Results per page
        #[arg(long, value_name = "N", default_value_t = 20, value_parser = clap::value_parser!(u32).range(1..=100))]
        page_size: u32,
        /// Only runs of this named suite
        #[arg(long, alias = "suite", value_name = "SUITE_ID")]
        case_suite_id: Option<String>,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
        org: Option<String>,
    },
    /// Show a case run and its results
    Show(RunArgs),
    /// Wait for an existing run; never queues another delivery
    Wait {
        /// Case run ID
        #[arg(value_name = "RUN_ID")]
        id: String,
        #[command(flatten)]
        wait_options: WaitArgs,
        /// Organization ID (overrides the configured default)
        #[arg(short = 'o', long, value_name = "ORG_ID")]
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
            Self::Show(args) => args.org.clone(),
            Self::Suites {
                action: SuiteAction::Show(args),
            } => args.org.clone(),
            Self::Runs {
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
        if let Some(method) = self.method {
            attrs.insert("default_method".into(), json!(method.as_uppercase()));
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

fn wait_settings(options: WaitArgs) -> Result<(Duration, Duration)> {
    let ms = resolve_millis_flag(options.timeout, options.timeout_ms, "timeout-ms", "timeout")
        .unwrap_or(30_000);
    if ms > 3_600_000 {
        bail!("Maximum case wait is 1h. Resume with cases runs wait <run-id>.");
    }
    let interval = resolve_millis_flag(
        options.interval,
        options.interval_ms,
        "interval-ms",
        "interval",
    )
    .unwrap_or(250);
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
                interval: None,
                interval_ms: None,
            })?;
            validate_target(&params)?;
            let mut body_params = serde_json::to_value(&params)?;
            if let Some(method) = method {
                body_params["method"] = json!(method.as_uppercase());
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
                        crate::render::print_status(
                            crate::render::OutputStatus::Info,
                            "CASE REPLAY",
                        );
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
                            &format!("hooklistener endpoint show-forward {}", replay.forward_id),
                        );
                    },
                )?;
                return Ok(replay.status == "failed"
                    || matches!(replay.assertion_status.as_deref(), Some("failed" | "error")));
            }
        }
        CasesAction::Run {
            endpoint_id,
            destination,
            target_name,
            case_suite_id,
            wait,
            wait_options,
            idempotency_key,
            dry_run,
            ..
        } => {
            if !wait && wait_options.is_set() {
                bail!("Wait options require --wait.");
            }
            let settings = wait_settings(wait_options)?;
            let mut params = build_case_run_params(CaseRunInput {
                target: destination.target,
                target_url: destination.target_url,
                target_id: destination.target_id,
                target_name,
                wait: false,
                timeout: None,
                timeout_ms: None,
                interval: None,
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
                        let mut table = crate::render::new_table(&["Position", "Case ID", "Name"]);
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
                            crate::render::new_table(&["Run ID", "Result", "Passed", "Unasserted"]);
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
                        crate::render::print_pagination(&history.pagination);
                    },
                )?;
            }
            RunAction::Show(args) => {
                return emit_run(&client.get_case_run(&args.id).await?, org, json_output);
            }
            RunAction::Wait {
                id, wait_options, ..
            } => {
                let (duration, interval) = wait_settings(wait_options)?;
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
        crate::render::print_status(crate::render::OutputStatus::Info, "CASE PREVIEW");
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
        crate::render::print_context("Organization:", org);
        print_case_run_result(result);
        if let Some(receipt) = &result.idempotency {
            field("IDEMPOTENCY KEY", &receipt.key);
            field("DISPOSITION", &receipt.disposition);
        }
    })?;
    Ok(case_run_failed(result))
}

fn emit<T: serde::Serialize>(value: &T, json_output: bool, human: impl FnOnce()) -> Result<()> {
    if json_output {
        crate::render::print_json(value)?;
    } else {
        human();
    }
    Ok(())
}

fn safe(value: &str) -> String {
    crate::render::sanitize_terminal_display(value)
}
fn field(label: &str, value: &str) {
    crate::render::print_field(label, safe(value));
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
    let mut table = crate::render::new_table(&["Case ID", "Name", "Method", "Path"]);
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

pub struct CaseRunInput {
    pub target: Option<String>,
    pub target_url: Option<String>,
    pub target_id: Option<String>,
    pub target_name: Option<String>,
    pub wait: bool,
    pub timeout: Option<Duration>,
    pub timeout_ms: Option<u64>,
    pub interval: Option<Duration>,
    pub interval_ms: Option<u64>,
}

pub fn build_case_run_params(input: CaseRunInput) -> Result<api::CaseRunParams> {
    let CaseRunInput {
        target,
        target_url,
        target_id,
        target_name,
        wait,
        timeout,
        timeout_ms,
        interval,
        interval_ms,
    } = input;

    let explicit_targets = target_url.iter().count() + target_id.iter().count();
    if target.is_some() && explicit_targets > 0 {
        return Err(anyhow!(
            "Use --target by itself, or use one of --target-url/--target-id."
        ));
    }
    if explicit_targets > 1 {
        return Err(anyhow!("Use only one of --target-url or --target-id."));
    }

    let mut params = api::CaseRunParams {
        case_suite_id: None,
        target_url: None,
        target_id: None,
        target: None,
        target_name,
        wait: wait.then_some(true),
        timeout_ms: timeout.map(duration_millis).or(timeout_ms),
        interval_ms: interval.map(duration_millis).or(interval_ms),
    };

    if let Some(target_url) = target_url {
        params.target_url = Some(target_url);
    } else if let Some(target_id) = target_id {
        params.target_id = Some(target_id);
    } else if let Some(target) = target {
        let normalized = target.trim();
        if normalized.eq_ignore_ascii_case("cli") {
            params.target = Some("cli".to_string());
        } else if normalized.starts_with("http://") || normalized.starts_with("https://") {
            params.target_url = Some(target);
        } else {
            params.target_id = Some(target);
        }
    } else {
        return Err(anyhow!(
            "Target is required. Use --target <URL|TARGET_ID|cli>."
        ));
    }

    Ok(params)
}

pub fn case_run_failed(result: &api::CaseRunResult) -> bool {
    !matches!(
        result.result_status.as_str(),
        "pending" | "completed" | "passed"
    ) || result.timed_out == Some(true)
        || result.failed_count > 0
        || result.queue_failed_count > 0
        || result.delivery_failed_count > 0
        || result.assertion_failed_count > 0
        || result.assertion_error_count > 0
}

pub fn case_run_target_label(target: &api::CaseRunTarget) -> String {
    if target.r#type.as_deref() == Some("cli") {
        return "CLI listener".to_string();
    }

    target
        .name
        .as_deref()
        .or(target.url.as_deref())
        .or(target.id.as_deref())
        .unwrap_or("-")
        .to_string()
}

pub fn case_run_forward_target(forward: &api::CaseRunForward) -> String {
    forward.target_url.as_deref().unwrap_or("CLI").to_string()
}

pub fn has_case_run_error(error: Option<&serde_json::Value>) -> bool {
    match error {
        Some(serde_json::Value::Null) | None => false,
        Some(serde_json::Value::String(value)) => !value.is_empty(),
        Some(serde_json::Value::Object(value)) => !value.is_empty(),
        Some(_) => true,
    }
}

pub fn case_run_value_message(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::String(message) => message.clone(),
        serde_json::Value::Object(object) => {
            if let Some(detail) = object.get("detail").and_then(|value| value.as_str()) {
                detail.to_string()
            } else if let Some(error_type) = object.get("type").and_then(|value| value.as_str()) {
                error_type.replace('_', " ")
            } else {
                value.to_string()
            }
        }
        _ => value.to_string(),
    }
}

pub fn case_run_failure_reason(failure: &api::CaseRunFailure) -> String {
    case_run_value_message(&failure.reason)
}

pub fn print_case_run_result(result: &api::CaseRunResult) {
    let status = if case_run_failed(result) {
        OutputStatus::Err
    } else if result.not_configured_count > 0 {
        OutputStatus::Warn
    } else if matches!(result.result_status.as_str(), "pending" | "completed") {
        OutputStatus::Info
    } else {
        OutputStatus::Ok
    };

    print_status(status, "CASE RUN");
    println!();
    if let Some(run_id) = result.case_suite_run_id.as_deref().or(result.id.as_deref()) {
        print_field(
            "RUN ID",
            sanitize_terminal(run_id, TerminalTextLayout::Inline),
        );
    }
    if let Some(suite_id) = result.case_suite_id.as_deref() {
        print_field("SUITE ID", sanitize_terminal_display(suite_id));
    }
    if let Some(report_url) = result.case_suite_run_url.as_deref() {
        print_field(
            "REPORT",
            sanitize_terminal(report_url, TerminalTextLayout::Inline),
        );
    }
    print_field(
        "RESULT",
        sanitize_terminal(&result.result_status, TerminalTextLayout::Inline).bold(),
    );
    print_field(
        "STATUS",
        sanitize_terminal(&result.status, TerminalTextLayout::Inline),
    );
    print_field(
        "ENDPOINT",
        sanitize_terminal(&result.endpoint_id, TerminalTextLayout::Inline),
    );
    print_field(
        "TARGET",
        sanitize_terminal(
            &case_run_target_label(&result.target),
            TerminalTextLayout::Inline,
        ),
    );
    if let Some(source) = result.source.as_deref() {
        print_field(
            "SOURCE",
            sanitize_terminal(&source.to_uppercase(), TerminalTextLayout::Inline),
        );
    }
    print_field("ASYNC", yes_no(result.async_run));
    if let Some(waited) = result.waited {
        print_field("WAITED", yes_no(waited));
    }
    if result.timed_out == Some(true) {
        print_field("TIMED OUT", "yes".red());
    }
    print_field(
        "COUNTS",
        format!(
            "total={} queued={} failed={}",
            result.total_count, result.queued_count, result.failed_count
        ),
    );

    if result.waited == Some(true) || result.completed_count > 0 {
        print_field(
            "RESULTS",
            format!(
                "completed={} waiting={}",
                result.completed_count, result.waiting_count
            ),
        );
        print_field(
            "ASSERTIONS",
            format!(
                "passed={} failed={} error={} unconfigured={}",
                result.passed_count,
                result.assertion_failed_count,
                result.assertion_error_count,
                result.not_configured_count
            ),
        );
        print_field(
            "FAILURES",
            format!(
                "queue={} delivery={}",
                result.queue_failed_count, result.delivery_failed_count
            ),
        );
    }

    let problem_forwards = result
        .forwards
        .iter()
        .filter(|forward| {
            has_case_run_error(forward.error_message.as_ref())
                || forward.status_code.is_some_and(|code| code >= 400)
                || matches!(
                    forward.assertion_status.as_deref(),
                    Some("failed" | "error" | "timeout")
                )
        })
        .collect::<Vec<_>>();

    if !problem_forwards.is_empty() {
        println!();
        print_section("Failed Forwards");
        let mut table = new_table(&[
            "ID",
            "Request",
            "Case",
            "Target",
            "HTTP",
            "Assertion",
            "Error",
        ]);
        for forward in problem_forwards {
            let status = forward
                .status_code
                .map(|code| code.to_string())
                .unwrap_or_else(|| "-".to_string());
            let assertion = forward
                .assertion_status
                .as_deref()
                .unwrap_or("-")
                .to_string();
            let error = forward
                .error_message
                .as_ref()
                .map(case_run_value_message)
                .or_else(|| forward.poll_url.clone())
                .unwrap_or_else(|| "-".to_string());
            table.add_row(vec![
                sanitize_terminal_display(&forward.id),
                sanitize_terminal_display(&forward.debug_request_id),
                sanitize_terminal_display(value_or_dash(forward.debug_request_case_id.as_deref())),
                sanitize_terminal_display(case_run_forward_target(forward)),
                status,
                sanitize_terminal_display(assertion),
                sanitize_terminal_display(error),
            ]);
        }
        println!("{table}");
    }

    if !result.failures.is_empty() {
        println!();
        print_section("Queue Failures");
        let mut table = new_table(&["Case", "Reason"]);
        for failure in &result.failures {
            table.add_row(vec![
                sanitize_terminal_display(&failure.case_id),
                sanitize_terminal_display(case_run_failure_reason(failure)),
            ]);
        }
        println!("{table}");
    }
}

#[cfg(test)]
mod tests;

fn print_suites(suites: &[CaseSuite]) {
    if suites.is_empty() {
        println!("No named suites. Create a suite in Hooklistener or through MCP.");
        return;
    }
    let mut table = crate::render::new_table(&["Suite ID", "Name", "Cases"]);
    for suite in suites {
        table.add_row([
            safe(&suite.id),
            safe(&suite.name),
            suite.case_count.to_string(),
        ]);
    }
    println!("{table}");
}
