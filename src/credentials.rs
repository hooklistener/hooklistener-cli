//! Access-token validation, refresh, and organization selection for commands.

use anyhow::{Result, anyhow};
use chrono::{Duration as ChronoDuration, Utc};
use std::io;
use std::time::Duration;
use tokio::{sync::watch, time::sleep};
use tracing::{error, warn};

use crate::{api, auth, config};

/// Environment variable holding an access token for noninteractive use (CI).
/// When set, it takes precedence over the saved login and is never refreshed.
pub const ACCESS_TOKEN_ENV: &str = "HOOKLISTENER_TOKEN";

/// Environment variable naming the organization when `--org` is not passed.
pub const ORGANIZATION_ENV: &str = "HOOKLISTENER_ORG";

/// Reads a trimmed, non-empty value from the environment. CI secret stores
/// often append a trailing newline, which would otherwise corrupt the header.
fn env_value(name: &str) -> Option<String> {
    non_empty_trimmed(std::env::var(name).ok())
}

fn non_empty_trimmed(value: Option<String>) -> Option<String> {
    value
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub fn env_access_token() -> Option<String> {
    env_value(ACCESS_TOKEN_ENV)
}

pub fn resolve_tunnel_org(cli_org: Option<String>, config: &config::Config) -> Option<String> {
    resolve_org_from(cli_org, env_value(ORGANIZATION_ENV), config)
}

/// Organization precedence: `--org`, then `HOOKLISTENER_ORG`, then the saved default.
pub fn resolve_org_from(
    cli_org: Option<String>,
    env_org: Option<String>,
    config: &config::Config,
) -> Option<String> {
    cli_org
        .or(env_org)
        .or_else(|| config.selected_organization_id.clone())
}

pub const ACCESS_TOKEN_REFRESH_SKEW_SECONDS: i64 = 60;

pub const ACCESS_TOKEN_REFRESH_RETRY_SECONDS: u64 = 30;

pub fn refreshed_access_token_rx(
    access_token: String,
    config: config::Config,
) -> watch::Receiver<String> {
    let (token_tx, token_rx) = watch::channel(access_token);

    // An environment token belongs to a different credential than the saved
    // login; refreshing the saved one would silently swap identities.
    if env_access_token().is_none() && config.is_refresh_token_valid() {
        tokio::spawn(refresh_access_token_loop(config, token_tx));
    }

    token_rx
}

pub async fn refresh_access_token_loop(config: config::Config, token_tx: watch::Sender<String>) {
    let base_url = match api::default_base_url() {
        Ok(base_url) => base_url,
        Err(error) => {
            error!(%error, "Access-token refresh disabled by invalid API URL");
            return;
        }
    };
    refresh_access_token_loop_with(config, token_tx, &base_url).await;
}

pub async fn refresh_access_token_loop_with(
    mut config: config::Config,
    token_tx: watch::Sender<String>,
    base_url: &str,
) {
    loop {
        if token_tx.is_closed()
            || config.refresh_token.is_none()
            || !config.is_refresh_token_valid()
        {
            return;
        }

        if sleep_or_token_receiver_closed(&token_tx, access_token_refresh_delay(&config)).await {
            return;
        }

        let refresh_result = tokio::select! {
            _ = token_tx.closed() => return,
            result = refresh_access_token_from_config_with(&mut config, base_url, None) => result,
        };
        match refresh_result {
            Ok(access_token) => {
                if token_tx.send(access_token).is_err() {
                    return;
                }
            }
            Err(err) => {
                error!(error = %err, "Failed to refresh CLI access token");
                if sleep_or_token_receiver_closed(
                    &token_tx,
                    Duration::from_secs(ACCESS_TOKEN_REFRESH_RETRY_SECONDS),
                )
                .await
                {
                    return;
                }
            }
        }
    }
}

pub async fn sleep_or_token_receiver_closed(
    token_tx: &watch::Sender<String>,
    duration: Duration,
) -> bool {
    tokio::select! {
        _ = token_tx.closed() => true,
        _ = sleep(duration) => false,
    }
}

pub fn access_token_refresh_delay(config: &config::Config) -> Duration {
    let Some(expires_at) = config.token_expires_at.as_ref() else {
        return Duration::from_secs(0);
    };

    let duration_until_refresh = expires_at.signed_duration_since(Utc::now())
        - ChronoDuration::seconds(ACCESS_TOKEN_REFRESH_SKEW_SECONDS);

    duration_until_refresh
        .to_std()
        .unwrap_or_else(|_| Duration::from_secs(0))
}

pub async fn ensure_valid_token(config: &mut config::Config) -> Result<String> {
    ensure_valid_token_with(config, env_access_token()).await
}

pub async fn ensure_valid_token_with(
    config: &mut config::Config,
    env_token: Option<String>,
) -> Result<String> {
    // 0. An environment token wins over the saved login and is used as-is
    if let Some(token) = env_token {
        return Ok(token);
    }

    // 1. If access token is still valid, return it
    if config.is_token_valid() {
        return config
            .access_token
            .clone()
            .ok_or_else(|| anyhow!("No access token found. Please run `hooklistener login`."));
    }

    // 2. If refresh token is valid, try refreshing
    if config.refresh_token.is_some() && config.is_refresh_token_valid() {
        return refresh_access_token_from_config(config)
            .await
            .map_err(|err| {
                anyhow!(
                    "Session expired. Please run `hooklistener login` to re-authenticate. ({err})"
                )
            });
    }

    // 3. No valid tokens
    Err(anyhow!(
        "Session expired. Please run `hooklistener login` to re-authenticate."
    ))
}

pub async fn refresh_access_token_from_config(config: &mut config::Config) -> Result<String> {
    let base_url = api::default_base_url()?;
    refresh_access_token_from_config_with(config, &base_url, None).await
}

pub async fn refresh_access_token_from_config_with(
    config: &mut config::Config,
    base_url: &str,
    save_path: Option<&std::path::Path>,
) -> Result<String> {
    let refresh_token = config
        .refresh_token
        .clone()
        .ok_or_else(|| anyhow!("No refresh token found"))?;

    if !config.is_refresh_token_valid() {
        return Err(anyhow!("Refresh token expired"));
    }

    let response = api::refresh_access_token(&refresh_token, base_url).await?;
    let expires_at = token_expiry_from_now(response.expires_in)?;

    // Other processes (`org use`, `config set`, `login --force`, `logout`) may
    // have saved the config since this copy was loaded. Persist only the fields
    // this refresh owns on top of what is currently on disk, so their changes
    // (including a rotated refresh token) survive.
    let mut updated = match load_saved_config(save_path) {
        Ok(Some(on_disk)) => on_disk,
        Ok(None) => in_memory_config_copy(config),
        Err(err) => {
            warn!(
                error = %err,
                "Failed to reload config before saving refreshed token; saving in-memory copy"
            );
            in_memory_config_copy(config)
        }
    };

    if updated.refresh_token.is_none() {
        // The user logged out in another process. Do not resurrect the session
        // on disk: the fresh access token is handed back for this process to
        // finish its current work, and `config` mirrors the logged-out state so
        // the background refresh loop stops on its next iteration.
        *config = updated;
        return Ok(response.access_token);
    }

    updated.access_token = Some(response.access_token.clone());
    updated.token_expires_at = Some(expires_at);
    if let Some(path) = save_path {
        updated.save_to(path)?;
    } else {
        updated.save()?;
    }
    *config = updated;

    Ok(response.access_token)
}

/// Persists freshly issued login tokens on top of the config currently on disk.
///
/// The device-authorization wait can last minutes, so `loaded` (the copy taken
/// when `login` started) may be stale: `org use`, `config set`, or an update
/// check in another process may have saved since. Only the token fields belong
/// to the login, so everything else is taken from disk and `loaded` is used
/// solely as a fallback when no config file exists any more.
pub fn save_login_tokens(
    loaded: &config::Config,
    access_token: String,
    access_expires_at: chrono::DateTime<Utc>,
    refresh_token: Option<String>,
    refresh_expires_at: Option<chrono::DateTime<Utc>>,
    save_path: Option<&std::path::Path>,
) -> Result<()> {
    let mut updated = match load_saved_config(save_path) {
        Ok(Some(on_disk)) => on_disk,
        Ok(None) => in_memory_config_copy(loaded),
        Err(err) => {
            warn!(
                error = %err,
                "Failed to reload config before saving login tokens; saving in-memory copy"
            );
            in_memory_config_copy(loaded)
        }
    };
    updated.set_tokens(
        access_token,
        access_expires_at,
        refresh_token,
        refresh_expires_at,
    );
    match save_path {
        Some(path) => updated.save_to(path),
        None => updated.save(),
    }
}

/// Loads the config currently on disk, or `None` when no config file exists.
pub fn load_saved_config(path: Option<&std::path::Path>) -> Result<Option<config::Config>> {
    let path = match path {
        Some(path) => path.to_path_buf(),
        None => config::Config::config_path()?,
    };
    match std::fs::symlink_metadata(&path) {
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(err) => Err(err.into()),
        Ok(_) => config::Config::load_from(&path).map(Some),
    }
}

pub fn in_memory_config_copy(config: &config::Config) -> config::Config {
    config::Config {
        access_token: config.access_token.clone(),
        token_expires_at: config.token_expires_at,
        refresh_token: config.refresh_token.clone(),
        refresh_token_expires_at: config.refresh_token_expires_at,
        selected_organization_id: config.selected_organization_id.clone(),
        last_update_check: config.last_update_check,
        latest_known_version: config.latest_known_version.clone(),
    }
}

/// Converts a server-supplied token lifetime into an absolute expiry, failing
/// cleanly instead of panicking on absurd values.
pub fn token_expiry_from_now(seconds: u64) -> Result<chrono::DateTime<Utc>> {
    auth::expiry_from_now(seconds)
        .map_err(|err| anyhow!("Authorization server returned an invalid token lifetime ({err})"))
}

pub fn require_organization(cli_org: Option<String>, config: &config::Config) -> Result<String> {
    resolve_tunnel_org(cli_org, config).ok_or_else(|| {
        anyhow!(
            "No organization selected. Use `hooklistener org use <organization-id>`, pass --org, or set HOOKLISTENER_ORG."
        )
    })
}

#[cfg(test)]
mod env_tests {
    use super::non_empty_trimmed;

    #[test]
    fn environment_values_are_trimmed_and_blank_values_ignored() {
        assert_eq!(
            non_empty_trimmed(Some(" token\n".to_string())).as_deref(),
            Some("token")
        );
        assert_eq!(non_empty_trimmed(Some("  \n".to_string())), None);
        assert_eq!(non_empty_trimmed(None), None);
    }
}
