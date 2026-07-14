use reqwest::{Client, Url, redirect::Policy};
use serde::Serialize;
use std::collections::BTreeSet;
use std::fmt;
use std::net::{IpAddr, SocketAddr};
use std::time::Duration;

#[derive(Clone, Debug)]
pub struct TargetPolicy {
    url: Url,
    host: String,
    port: u16,
    pinned_addresses: Vec<SocketAddr>,
    allow_non_loopback: bool,
    insecure_tls: bool,
}

#[derive(Clone, Debug, Serialize)]
pub struct TargetPlan {
    pub scheme: String,
    pub host: String,
    pub port: u16,
    pub path: String,
    pub pinned_addresses: Vec<String>,
    pub allow_non_loopback: bool,
    pub insecure_tls: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TargetPolicyError {
    code: &'static str,
    message: String,
}

impl TargetPolicyError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    pub fn code(&self) -> &'static str {
        self.code
    }
}

impl fmt::Display for TargetPolicyError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}: {}", self.code(), self.message)
    }
}

impl std::error::Error for TargetPolicyError {}

impl TargetPolicy {
    pub async fn resolve(
        raw: &str,
        allow_non_loopback: bool,
        insecure_tls: bool,
    ) -> Result<Self, TargetPolicyError> {
        let url = Url::parse(raw).map_err(|error| {
            TargetPolicyError::new("target_url_invalid", format!("Invalid target URL: {error}"))
        })?;

        if !matches!(url.scheme(), "http" | "https") {
            return Err(TargetPolicyError::new(
                "target_scheme_invalid",
                "Target scheme must be http or https",
            ));
        }
        if !url.username().is_empty() || url.password().is_some() {
            return Err(TargetPolicyError::new(
                "target_credentials_forbidden",
                "Target URLs must not contain credentials",
            ));
        }
        if url.query().is_some() {
            return Err(TargetPolicyError::new(
                "target_query_forbidden",
                "Target URLs must not contain a query string",
            ));
        }
        if url.fragment().is_some() {
            return Err(TargetPolicyError::new(
                "target_fragment_forbidden",
                "Target URLs must not contain a fragment",
            ));
        }
        if insecure_tls && url.scheme() != "https" {
            return Err(TargetPolicyError::new(
                "target_insecure_tls_invalid",
                "--insecure-tls applies only to HTTPS targets",
            ));
        }

        let host = url
            .host_str()
            .ok_or_else(|| {
                TargetPolicyError::new("target_authority_invalid", "Target host is required")
            })?
            .to_ascii_lowercase();
        let port = url.port_or_known_default().ok_or_else(|| {
            TargetPolicyError::new("target_port_invalid", "Target port is required")
        })?;

        let resolved = tokio::net::lookup_host((host.as_str(), port))
            .await
            .map_err(|error| {
                TargetPolicyError::new(
                    "target_dns_failed",
                    format!("Target host could not be resolved: {error}"),
                )
            })?;

        let pinned_addresses: Vec<SocketAddr> = resolved
            .map(|address| SocketAddr::new(address.ip(), port))
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();

        if pinned_addresses.is_empty() {
            return Err(TargetPolicyError::new(
                "target_dns_empty",
                "Target host resolved to no addresses",
            ));
        }

        if !allow_non_loopback
            && pinned_addresses
                .iter()
                .any(|address| !address.ip().is_loopback())
        {
            return Err(TargetPolicyError::new(
                "target_non_loopback_requires_scope",
                "Non-loopback targets require --allow-non-loopback",
            ));
        }

        if pinned_addresses
            .iter()
            .any(|address| forbidden_address(address.ip()))
        {
            return Err(TargetPolicyError::new(
                "target_address_forbidden",
                "Target resolved to an unspecified, multicast, or broadcast address",
            ));
        }

        Ok(Self {
            url,
            host,
            port,
            pinned_addresses,
            allow_non_loopback,
            insecure_tls,
        })
    }

    pub fn plan(&self) -> TargetPlan {
        TargetPlan {
            scheme: self.url.scheme().to_string(),
            host: self.host.clone(),
            port: self.port,
            path: normalized_path(self.url.path()),
            pinned_addresses: self
                .pinned_addresses
                .iter()
                .map(|address| address.ip().to_string())
                .collect(),
            allow_non_loopback: self.allow_non_loopback,
            insecure_tls: self.insecure_tls,
        }
    }

    pub fn display_url(&self) -> String {
        self.url.to_string().trim_end_matches('/').to_string()
    }

    pub fn request_url(
        &self,
        request_path: &str,
        query: Option<&str>,
    ) -> Result<Url, TargetPolicyError> {
        let mut target = self.url.clone();
        let base_path = self.url.path().trim_end_matches('/');
        let request_path = request_path.trim_start_matches('/');
        let combined = if request_path.is_empty() {
            normalized_path(base_path)
        } else if base_path.is_empty() || base_path == "/" {
            format!("/{request_path}")
        } else {
            format!("{base_path}/{request_path}")
        };

        target.set_path(&combined);
        let scoped_base_path = normalized_path(base_path);
        if !path_is_within_base(target.path(), &scoped_base_path) {
            return Err(TargetPolicyError::new(
                "target_path_escape",
                "Forwarded request path must remain within the configured target path",
            ));
        }
        target.set_query(query.filter(|value| !value.is_empty()));
        target.set_fragment(None);
        Ok(target)
    }

    pub fn http_client(&self) -> Result<Client, TargetPolicyError> {
        self.http_client_with_timeout(Duration::from_secs(30))
    }

    pub fn http_client_with_timeout(&self, timeout: Duration) -> Result<Client, TargetPolicyError> {
        Client::builder()
            .timeout(timeout)
            .redirect(Policy::none())
            .no_proxy()
            .danger_accept_invalid_certs(self.insecure_tls)
            .resolve_to_addrs(&self.host, &self.pinned_addresses)
            .build()
            .map_err(|error| {
                TargetPolicyError::new(
                    "target_client_failed",
                    format!("Failed to build pinned target client: {error}"),
                )
            })
    }
}

fn normalized_path(path: &str) -> String {
    if path.is_empty() {
        "/".to_string()
    } else {
        path.to_string()
    }
}

fn path_is_within_base(path: &str, base_path: &str) -> bool {
    base_path == "/"
        || path == base_path
        || path
            .strip_prefix(base_path)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn forbidden_address(address: IpAddr) -> bool {
    address.is_unspecified()
        || address.is_multicast()
        || address == IpAddr::from([255, 255, 255, 255])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn rejects_query_fragment_credentials_and_non_http_schemes() {
        for (target, code) in [
            (
                "http://localhost:3000/hooks?admin=true",
                "target_query_forbidden",
            ),
            (
                "http://localhost:3000/hooks#secret",
                "target_fragment_forbidden",
            ),
            (
                "http://user:pass@localhost:3000",
                "target_credentials_forbidden",
            ),
            ("ftp://localhost:3000", "target_scheme_invalid"),
        ] {
            let error = TargetPolicy::resolve(target, false, false)
                .await
                .unwrap_err();
            assert_eq!(error.code(), code);
        }
    }

    #[tokio::test]
    async fn requires_explicit_non_loopback_scope() {
        let error = TargetPolicy::resolve("http://192.0.2.10:3000", false, false)
            .await
            .unwrap_err();
        assert_eq!(error.code(), "target_non_loopback_requires_scope");

        let target = TargetPolicy::resolve("http://192.0.2.10:3000", true, false)
            .await
            .unwrap();
        assert!(target.plan().allow_non_loopback);
    }

    #[tokio::test]
    async fn pins_loopback_and_builds_paths_without_changing_authority() {
        let target = TargetPolicy::resolve("http://localhost:3000/base", false, false)
            .await
            .unwrap();
        let plan = target.plan();
        assert!(plan.pinned_addresses.iter().all(|address| {
            address
                .parse::<IpAddr>()
                .map(|address| address.is_loopback())
                .unwrap_or(false)
        }));

        let request = target
            .request_url("//evil.example/admin", Some("a=1"))
            .unwrap();
        assert_eq!(request.host_str(), Some("localhost"));
        assert_eq!(request.port(), Some(3000));
        assert_eq!(request.path(), "/base/evil.example/admin");
        assert_eq!(request.query(), Some("a=1"));
    }

    #[tokio::test]
    async fn rejects_request_paths_that_escape_the_configured_base_path() {
        let target = TargetPolicy::resolve("http://localhost:3000/base", false, false)
            .await
            .unwrap();

        for path in ["../admin", "%2e%2e/admin", ".%2e/admin", "%2e./admin"] {
            let error = target.request_url(path, None).unwrap_err();
            assert_eq!(error.code(), "target_path_escape", "path: {path}");
        }

        assert_eq!(
            target
                .request_url("nested/../allowed", None)
                .unwrap()
                .path(),
            "/base/allowed"
        );
    }

    #[tokio::test]
    async fn insecure_tls_is_explicit_and_https_only() {
        let error = TargetPolicy::resolve("http://localhost:3000", false, true)
            .await
            .unwrap_err();
        assert_eq!(error.code(), "target_insecure_tls_invalid");

        let target = TargetPolicy::resolve("https://localhost:3443", false, true)
            .await
            .unwrap();
        assert!(target.plan().insecure_tls);
    }

    #[tokio::test]
    async fn pinned_client_does_not_follow_redirects() {
        let mut server = mockito::Server::new_async().await;
        let redirect = server
            .mock("GET", "/start")
            .with_status(302)
            .with_header("location", "/unexpected")
            .create_async()
            .await;

        let target = TargetPolicy::resolve(&server.url(), false, false)
            .await
            .unwrap();
        let response = target
            .http_client()
            .unwrap()
            .get(target.request_url("/start", None).unwrap())
            .send()
            .await
            .unwrap();

        assert_eq!(response.status(), reqwest::StatusCode::FOUND);
        redirect.assert_async().await;
    }
}
