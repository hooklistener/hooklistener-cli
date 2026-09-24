//! Command implementations, one module per top-level subcommand, plus helpers they share.

pub mod anon;
pub mod cases;
pub mod completions;
pub mod config;
pub mod endpoint;
pub mod listen;
pub mod login;
pub mod monitor;
pub mod org;
pub mod share;
pub mod static_tunnel;
pub mod tunnel;

use anyhow::{Result, anyhow};
use std::future::Future;
use std::io::{self, IsTerminal, Write};
use tokio::task::JoinHandle;

use crate::render::{OutputStatus, eprint_field, eprint_status};

#[derive(Debug, PartialEq, Eq)]
pub enum WorkerCompletion<T> {
    Stream(T),
    Shutdown,
}

pub async fn supervise_json_worker<T, S: Future<Output = T>, Q: Future<Output = Result<()>>>(
    worker: JoinHandle<()>,
    stream: S,
    shutdown: Q,
) -> Result<WorkerCompletion<T>> {
    tokio::pin!(stream);
    tokio::pin!(shutdown);
    let result = tokio::select! {
        value = &mut stream => WorkerCompletion::Stream(value),
        signal = &mut shutdown => { signal?; WorkerCompletion::Shutdown }
    };
    worker.abort();
    if let Err(error) = worker.await
        && !error.is_cancelled()
    {
        return Err(error.into());
    }
    Ok(result)
}

pub fn confirmation_is_yes(input: &str) -> bool {
    input.trim().eq_ignore_ascii_case("yes")
}

pub fn confirm_destructive_action(
    title: &str,
    resource: &str,
    organization_id: &str,
    confirmed: bool,
    json: bool,
) -> Result<bool> {
    if confirmed {
        return Ok(true);
    }

    if json || !io::stdin().is_terminal() || !io::stderr().is_terminal() {
        return Err(anyhow!(
            "Confirmation required for {resource}. Re-run with --yes after verifying the resource and organization."
        ));
    }

    eprint_status(OutputStatus::Warn, title);
    eprintln!();
    eprint_field("RESOURCE", resource);
    eprint_field("ORGANIZATION", organization_id);
    eprint_field("CONSEQUENCE", "This action cannot be undone.");
    eprint!("TYPE YES      Type `yes` to continue: ");
    io::stderr().flush()?;

    let mut response = String::new();
    io::stdin().read_line(&mut response)?;
    if confirmation_is_yes(&response) {
        return Ok(true);
    }

    eprintln!();
    eprint_status(OutputStatus::Info, "COMMAND CANCELED");
    eprint_field("ACTION", "No changes were made.");
    Ok(false)
}
