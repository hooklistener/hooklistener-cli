use thiserror::Error;

use crate::receipts::COMMAND_ERROR_SCHEMA;
use crate::render::{OutputStatus, eprint_field, eprint_status, sanitize_terminal_display};

#[derive(Debug, Error)]
pub enum TunnelLifecycleError {
    #[error("Tunnel lifecycle schema {actual} is incompatible with supported major {supported}")]
    IncompatibleSchema { supported: u64, actual: u64 },

    #[error("Tunnel event cursor expired")]
    CursorExpired {
        earliest_cursor: Option<String>,
        resync: serde_json::Value,
    },

    #[error("Tunnel API request failed ({code}, HTTP {status}): {message}")]
    Api {
        status: u16,
        code: String,
        message: String,
    },
}

impl TunnelLifecycleError {
    pub fn code(&self) -> &str {
        match self {
            Self::IncompatibleSchema { .. } => "incompatible_schema",
            Self::CursorExpired { .. } => "cursor_expired",
            Self::Api { code, .. } => code,
        }
    }

    pub fn hint(&self) -> Option<&str> {
        match self {
            Self::IncompatibleSchema { .. } => {
                Some("Upgrade Hooklistener CLI before activating this tunnel.")
            }
            Self::CursorExpired {
                earliest_cursor: Some(_),
                ..
            } => Some(
                "Rehydrate sessions, captures, attempts, and outcomes, then resume from the earliest cursor.",
            ),
            Self::CursorExpired {
                earliest_cursor: None,
                ..
            } => Some(
                "Resync sessions, captures, attempts, and outcomes, then request a fresh event cursor.",
            ),
            Self::Api { .. } => None,
        }
    }
}

/// A request the server refused because of the organization's plan: a limit
/// reached, or a feature the plan does not include. It keeps the billing link
/// the server sent with the error, so the user learns where to lift the limit
/// instead of hitting a dead end.
#[derive(Debug, Error)]
#[error("{message}")]
pub struct PlanLimitError {
    message: String,
    reason: String,
    upgrade_url: String,
    trial_available: bool,
    hint: String,
}

impl PlanLimitError {
    pub fn new(
        message: String,
        reason: String,
        upgrade_url: String,
        trial_available: bool,
    ) -> Self {
        let hint = if trial_available {
            format!("Start a free Pro trial to lift this limit: {upgrade_url}")
        } else {
            format!("Upgrade your plan to lift this limit: {upgrade_url}")
        };

        Self {
            message,
            reason,
            upgrade_url,
            trial_available,
            hint,
        }
    }

    /// `plan_limit_reached` or `plan_feature_unavailable`.
    pub fn code(&self) -> &str {
        &self.reason
    }

    pub fn hint(&self) -> &str {
        &self.hint
    }

    pub fn upgrade_url(&self) -> &str {
        &self.upgrade_url
    }
}

/// `endpoint wait` ran out of time before a matching request arrived.
#[derive(Debug, Error)]
#[error("No matching request reached endpoint {endpoint_id} within {timeout}.")]
pub struct RequestWaitTimeout {
    pub endpoint_id: String,
    pub timeout: String,
}

#[derive(Debug, Error)]
pub enum UpdateError {
    #[error("Failed to check for updates: {0}")]
    CheckFailed(String),

    #[error("Failed to update: {0}")]
    UpdateFailed(String),
}

impl UpdateError {
    pub fn hint(&self) -> Option<&str> {
        match self {
            UpdateError::CheckFailed(_) => Some("Check your internet connection and try again."),
            UpdateError::UpdateFailed(_) => {
                Some("Try updating manually or check file permissions.")
            }
        }
    }
}

pub fn error_hint(err: &anyhow::Error) -> Option<&str> {
    if let Some(e) = err.downcast_ref::<TunnelLifecycleError>() {
        return e.hint();
    }
    if let Some(e) = err.downcast_ref::<PlanLimitError>() {
        return Some(e.hint());
    }
    if let Some(e) = err.downcast_ref::<UpdateError>() {
        return e.hint();
    }
    if err.downcast_ref::<RequestWaitTimeout>().is_some() {
        return Some(
            "Check the filters and --since: without --since, only requests captured after the command started match.",
        );
    }

    let message = err.to_string();
    if message.contains("Session expired") || message.contains("No access token") {
        return Some(
            "Run `hooklistener login` to re-authenticate, or set HOOKLISTENER_TOKEN for noninteractive use.",
        );
    }
    if message.contains("No organization selected") {
        return Some(
            "Run `hooklistener org use <organization-id>`, pass --org, or set HOOKLISTENER_ORG.",
        );
    }
    if message.contains("Confirmation required") {
        return Some("Verify the resource and organization, then re-run with --yes.");
    }
    None
}

pub fn error_code(err: &anyhow::Error) -> String {
    if let Some(error) = err.downcast_ref::<TunnelLifecycleError>() {
        error.code().to_string()
    } else if let Some(error) = err.downcast_ref::<PlanLimitError>() {
        error.code().to_string()
    } else if err.downcast_ref::<UpdateError>().is_some() {
        "update_error".to_string()
    } else if err.downcast_ref::<RequestWaitTimeout>().is_some() {
        "wait_timeout".to_string()
    } else {
        let message = err.to_string();
        if message.contains("Confirmation required") {
            "confirmation_required".to_string()
        } else if message.contains("does not support --json") {
            "unsupported_output_mode".to_string()
        } else if message.contains("Session expired") || message.contains("No access token") {
            "authentication_required".to_string()
        } else if message.contains("No organization selected") {
            "organization_required".to_string()
        } else {
            "command_failed".to_string()
        }
    }
}

pub fn command_exit_code(err: &anyhow::Error) -> i32 {
    match err.downcast_ref::<TunnelLifecycleError>() {
        Some(TunnelLifecycleError::IncompatibleSchema { .. }) => 3,
        Some(TunnelLifecycleError::CursorExpired { .. }) => 4,
        _ => 1,
    }
}

pub fn json_error_receipt(err: &anyhow::Error) -> serde_json::Value {
    let causes = err
        .chain()
        .skip(1)
        .map(ToString::to_string)
        .collect::<Vec<_>>();

    let details = match err.downcast_ref::<TunnelLifecycleError>() {
        Some(TunnelLifecycleError::CursorExpired {
            earliest_cursor,
            resync,
        }) => serde_json::json!({
            "earliest_cursor": earliest_cursor,
            "resync": resync,
        }),
        Some(TunnelLifecycleError::IncompatibleSchema { supported, actual }) => {
            serde_json::json!({"supported_major": supported, "actual_major": actual})
        }
        _ => match err.downcast_ref::<PlanLimitError>() {
            Some(error) => serde_json::json!({
                "upgrade_url": error.upgrade_url(),
                "trial_available": error.trial_available,
            }),
            None => serde_json::Value::Null,
        },
    };

    serde_json::json!({
        "$schema": COMMAND_ERROR_SCHEMA,
        "schema_version": 1,
        "type": "error",
        "ok": false,
        "error": {
            "code": error_code(err),
            "message": err.to_string(),
            "hint": error_hint(err),
            "causes": causes,
            "details": details,
        }
    })
}

pub fn display_error(err: &anyhow::Error, json: bool) {
    if json {
        match serde_json::to_string(&json_error_receipt(err)) {
            Ok(receipt) => println!("{receipt}"),
            Err(_) => println!(
                r#"{{"$schema":"hooklistener.cli.error/1","schema_version":1,"type":"error","ok":false,"error":{{"code":"serialization_error","message":"Failed to serialize the command error."}}}}"#
            ),
        }
        return;
    }

    eprint_status(OutputStatus::Err, "COMMAND FAILED");
    eprintln!();
    eprint_field("MESSAGE", sanitize_terminal_display(err));
    if let Some(hint) = error_hint(err) {
        eprint_field("HINT", sanitize_terminal_display(hint));
    }
    for cause in err.chain().skip(1) {
        eprint_field("CAUSE", sanitize_terminal_display(cause));
    }
}
