//! HTTP adapters for saved endpoint cases. These routes are distinct from MCP tools.

use super::{ApiClient, CaseRunResult, DataResponse, Pagination};
use anyhow::{Context, Result, anyhow, bail};
use reqwest::Method;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use serde_json::Value;

#[derive(Debug, Deserialize, Serialize)]
pub struct SavedCase {
    pub id: String,
    pub endpoint_id: String,
    pub name: String,
    pub notes: Option<String>,
    pub method: Option<String>,
    pub path: Option<String>,
    pub default_target_url: Option<String>,
    pub default_method: Option<String>,
    pub default_request_headers: Option<Value>,
    pub default_request_body: Option<String>,
    pub assertion_config: Value,
    pub source_debug_request_id: String,
    pub baseline_forward_id: Option<String>,
    pub organization_id: Option<String>,
    pub url: Option<String>,
    pub source_request: Option<Value>,
    #[serde(default)]
    pub case_suites: Vec<Value>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CaseSuite {
    pub id: String,
    pub endpoint_id: String,
    pub name: String,
    pub description: Option<String>,
    pub case_count: u64,
    pub cases: Vec<SuiteCase>,
    pub organization_id: Option<String>,
    pub created_at: Option<String>,
    pub updated_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct SuiteCase {
    pub id: Option<String>,
    pub position: u64,
    pub case: SavedCase,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct IdempotencyReceipt {
    pub key: String,
    pub tool_name: String,
    pub disposition: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CaseReplay {
    pub idempotency: IdempotencyReceipt,
    pub forward_id: String,
    pub debug_request_case_id: String,
    pub status: String,
    pub assertion_status: Option<String>,
    pub target: super::CaseRunTarget,
    pub poll_url: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CaseRunHistory {
    pub data: Vec<CaseRunResult>,
    pub pagination: Pagination,
}

// Reject path delimiters rather than letting an ID change the resource being addressed.
pub fn validate_id(id: &str) -> Result<()> {
    if id.is_empty()
        || !id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
    {
        bail!("Resource IDs must contain only letters, digits, hyphens, or underscores.");
    }
    Ok(())
}

pub fn validate_action_key(key: &str) -> Result<()> {
    if !(8..=200).contains(&key.len()) || !key.bytes().all(|byte| (33..=126).contains(&byte)) {
        bail!("Idempotency keys must be 8–200 printable ASCII bytes without spaces.");
    }
    Ok(())
}

impl ApiClient {
    pub(super) async fn case_action_request<T: DeserializeOwned>(
        &self,
        path: &str,
        body: &Value,
        key: Option<&str>,
    ) -> Result<T> {
        let mut request = self.client.post(self.api_url(path)?).json(body);
        if let Some(key) = key {
            validate_action_key(key)?;
            request = request.header("idempotency-key", key);
        }
        // Dedicated routes prevent an older server from ignoring safety flags.
        // Never retry or fall back to the legacy delivery endpoints.
        let response = request
            .send()
            .await
            .map_err(|error| error.without_url())
            .context(
                "Case action request failed; recover deliveries using the SAME key and inputs",
            )?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| error.without_url())
            .context(
                "Lost case action response; recover deliveries using the SAME key and inputs",
            )?;
        let parsed: Option<Value> = serde_json::from_str(&text).ok();
        let data = parsed.as_ref().and_then(|value| value.get("data"));
        let failed_receipt = key.is_some()
            && status == reqwest::StatusCode::UNPROCESSABLE_ENTITY
            && data.is_some_and(|data| {
                data["result_status"] == "failed" && data["case_suite_run_id"].as_str().is_some()
            });
        if !status.is_success() && !failed_receipt {
            let error = super::server_response_error("Case action failed", status, &text);
            return Err(if status == reqwest::StatusCode::NOT_FOUND {
                error.context("Check resource scope and server support for case actions v1. No legacy endpoint was attempted")
            } else {
                error
            });
        }
        let data = data
            .ok_or_else(|| anyhow!("Missing case action receipt; do not retry with a new key"))?;
        let expected_schema = if key.is_some() {
            "hooklistener.cases.action/1"
        } else {
            "hooklistener.cases.preview/1"
        };
        if data["$schema"] != expected_schema || data["schema_version"] != 1 {
            bail!(
                "Unsupported case action contract; check server version. Recover deliveries using the SAME key and inputs."
            );
        }
        if let Some(key) = key {
            if data["idempotency"]["key"] != key
                || !matches!(
                    data["idempotency"]["disposition"].as_str(),
                    Some("executed" | "replayed")
                )
            {
                bail!("Invalid idempotency receipt; recover using the SAME key and inputs.");
            }
        } else if data["dry_run"] != true || data["status"] != "preview" {
            bail!("Invalid server preview response.");
        }
        serde_json::from_value(data.clone()).map_err(|_| {
            anyhow!(
                "Invalid case action response; recover deliveries using the SAME key and inputs"
            )
        })
    }

    async fn case_request<T: DeserializeOwned>(
        &self,
        method: Method,
        path: &str,
        body: Option<&Value>,
    ) -> Result<T> {
        let mut request = self.client.request(method, self.api_url(path)?);
        if let Some(body) = body {
            request = request.json(body);
        }
        // Case authoring routes are not idempotent; never retry a mutation.
        let response = request
            .send()
            .await
            .map_err(|error| error.without_url())
            .context("Case API request failed; delivery POSTs are not retried automatically")?;
        let status = response.status();
        let text = response
            .text()
            .await
            .map_err(|error| error.without_url())
            .context("Failed to read case response; inspect history before retrying a delivery")?;
        if !status.is_success() {
            return Err(super::server_response_error(
                "Case API request failed",
                status,
                &text,
            ));
        }
        // Typed deserializer errors may quote response values containing request secrets.
        serde_json::from_str(&text).map_err(|_| {
            anyhow!("Invalid case API response; inspect history before retrying a delivery.")
        })
    }

    pub async fn list_cases(&self, endpoint_id: &str) -> Result<Vec<SavedCase>> {
        validate_id(endpoint_id)?;
        let response: DataResponse<Vec<SavedCase>> = self
            .case_request(
                Method::GET,
                &format!("/api/v1/endpoints/{endpoint_id}/cases"),
                None,
            )
            .await?;
        Ok(response.data)
    }

    pub async fn get_case(&self, id: &str) -> Result<SavedCase> {
        validate_id(id)?;
        let response: DataResponse<SavedCase> = self
            .case_request(Method::GET, &format!("/api/v1/cases/{id}"), None)
            .await?;
        Ok(response.data)
    }

    pub async fn save_case(
        &self,
        endpoint_id: &str,
        request_id: &str,
        attrs: &Value,
    ) -> Result<SavedCase> {
        validate_id(endpoint_id)?;
        validate_id(request_id)?;
        let response: DataResponse<SavedCase> = self
            .case_request(
                Method::POST,
                &format!("/api/v1/endpoints/{endpoint_id}/requests/{request_id}/cases"),
                Some(attrs),
            )
            .await?;
        Ok(response.data)
    }

    pub async fn update_case(&self, id: &str, attrs: &Value) -> Result<SavedCase> {
        validate_id(id)?;
        let response: DataResponse<SavedCase> = self
            .case_request(Method::PATCH, &format!("/api/v1/cases/{id}"), Some(attrs))
            .await?;
        Ok(response.data)
    }

    pub async fn replay_case(&self, id: &str, params: &Value, key: &str) -> Result<CaseReplay> {
        validate_id(id)?;
        self.case_action_request(
            &format!("/api/v1/cases/{id}/replay/execute"),
            params,
            Some(key),
        )
        .await
    }

    pub async fn preview_case_replay(&self, id: &str, params: &Value) -> Result<Value> {
        validate_id(id)?;
        self.case_action_request(&format!("/api/v1/cases/{id}/replay/preview"), params, None)
            .await
    }

    pub async fn preview_case_run(&self, id: &str, params: &super::CaseRunParams) -> Result<Value> {
        validate_id(id)?;
        self.case_action_request(
            &format!("/api/v1/endpoints/{id}/cases/run/preview"),
            &serde_json::to_value(params)?,
            None,
        )
        .await
    }

    pub async fn list_case_suites(&self, endpoint_id: &str) -> Result<Vec<CaseSuite>> {
        validate_id(endpoint_id)?;
        let response: DataResponse<Vec<CaseSuite>> = self
            .case_request(
                Method::GET,
                &format!("/api/v1/endpoints/{endpoint_id}/case-suites"),
                None,
            )
            .await?;
        Ok(response.data)
    }

    pub async fn get_case_suite(&self, id: &str) -> Result<CaseSuite> {
        validate_id(id)?;
        let response: DataResponse<CaseSuite> = self
            .case_request(Method::GET, &format!("/api/v1/case-suites/{id}"), None)
            .await?;
        Ok(response.data)
    }

    pub async fn get_case_run(&self, id: &str) -> Result<CaseRunResult> {
        validate_id(id)?;
        let response: DataResponse<CaseRunResult> = self
            .case_request(Method::GET, &format!("/api/v1/case-runs/{id}"), None)
            .await?;
        Ok(response.data)
    }

    pub async fn list_case_runs(
        &self,
        endpoint_id: &str,
        page: u32,
        page_size: u32,
        suite_id: Option<&str>,
    ) -> Result<CaseRunHistory> {
        validate_id(endpoint_id)?;
        let mut query = reqwest::Url::parse("https://unused.invalid")?;
        query
            .query_pairs_mut()
            .append_pair("page", &page.to_string())
            .append_pair("page_size", &page_size.to_string());
        if let Some(id) = suite_id {
            validate_id(id)?;
            query.query_pairs_mut().append_pair("case_suite_id", id);
        }
        let path = format!(
            "/api/v1/endpoints/{endpoint_id}/case-runs?{}",
            query.query().unwrap_or_default()
        );
        self.case_request(Method::GET, &path, None).await
    }
}
