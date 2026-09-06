use mockito::{Matcher, Server};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

#[path = "support/cases.rs"]
mod cases;

const ACCESS_TOKEN: &str = "phase1-conformance-access-token";
const ORGANIZATION_ID: &str = "phase1-conformance-organization";

struct TestHome {
    _temp: TempDir,
    root: PathBuf,
    config_root: PathBuf,
}

impl TestHome {
    fn new() -> Self {
        let temp = TempDir::new().expect("create isolated CLI home");
        let root = temp.path().to_path_buf();

        #[cfg(target_os = "windows")]
        let config_root = root.join("AppData").join("Roaming");
        #[cfg(target_os = "macos")]
        let config_root = root.join("Library").join("Application Support");
        #[cfg(not(any(target_os = "windows", target_os = "macos")))]
        let config_root = root.join("config");

        fs::create_dir_all(config_root.join("hooklistener")).expect("create config directory");
        fs::write(
            config_root.join("hooklistener").join("config.json"),
            serde_json::to_vec_pretty(&json!({
                "access_token": ACCESS_TOKEN,
                "token_expires_at": "2099-01-01T00:00:00Z",
                "refresh_token": null,
                "refresh_token_expires_at": null,
                "selected_organization_id": ORGANIZATION_ID,
                "last_update_check": "2099-01-01T00:00:00Z",
                "latest_known_version": null
            }))
            .expect("serialize config"),
        )
        .expect("write config");

        Self {
            _temp: temp,
            root,
            config_root,
        }
    }

    fn command(&self, api_url: &str, args: &[&str]) -> Output {
        let binary = std::env::var_os("HOOKLISTENER_CONFORMANCE_BINARY")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_BIN_EXE_hooklistener")));
        let mut command = Command::new(binary);
        command
            .args(["--color", "never", "--log-dir"])
            .arg(self.root.join("logs"))
            .args(args)
            .env("HOOKLISTENER_API_URL", api_url)
            .env("HOME", &self.root)
            .env("USERPROFILE", &self.root)
            .env("APPDATA", &self.config_root)
            .env("LOCALAPPDATA", &self.config_root)
            .env("XDG_CONFIG_HOME", &self.config_root)
            .env("NO_COLOR", "1");

        command.output().expect("run Hooklistener CLI")
    }
}

fn assert_success(output: &Output) -> String {
    let stdout = String::from_utf8_lossy(&output.stdout).into_owned();
    let stderr = String::from_utf8_lossy(&output.stderr).into_owned();
    assert!(
        output.status.success(),
        "CLI failed with {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout,
        stderr
    );
    assert!(!stdout.contains(ACCESS_TOKEN));
    assert!(!stderr.contains(ACCESS_TOKEN));
    stdout
}

fn assert_json_error(output: &Output, exit: i32, code: &str) -> Value {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert_eq!(
        output.status.code(),
        Some(exit),
        "stdout={stdout}\nstderr={stderr}"
    );
    assert!(!stdout.contains(ACCESS_TOKEN));
    assert!(!stderr.contains(ACCESS_TOKEN));
    assert!(stderr.is_empty());
    let value: Value = serde_json::from_str(stdout.trim()).expect("JSON error");
    assert_eq!(value["$schema"], "hooklistener.cli.error/1");
    assert_eq!(value["schema_version"], 1);
    assert_eq!(value["type"], "error");
    assert_eq!(value["ok"], false);
    assert_eq!(value["error"]["code"], code);
    value
}

fn assert_json_receipt(stdout: &str, operation: &str) -> Value {
    let receipt: Value = serde_json::from_str(stdout.trim()).expect("one valid NDJSON receipt");
    assert_eq!(receipt["$schema"], "hooklistener.tunnel.receipt/1");
    assert_eq!(receipt["schema_version"], 1);
    assert_eq!(receipt["type"], "receipt");
    assert_eq!(receipt["operation"], operation);
    assert_eq!(receipt["organization_id"], ORGANIZATION_ID);
    receipt
}

fn write_platform_receipt(path: &Path) {
    let platform = std::env::var("HOOKLISTENER_CONFORMANCE_PLATFORM")
        .unwrap_or_else(|_| std::env::consts::OS.to_string());
    let service_git_sha = std::env::var("HOOKLISTENER_SERVICE_GIT_SHA").ok();
    let cli_git_sha = std::env::var("HOOKLISTENER_CLI_GIT_SHA").ok();
    let run_attempt = std::env::var("GITHUB_RUN_ATTEMPT").unwrap_or_else(|_| "1".to_string());
    let evidence_id = std::env::var("GITHUB_RUN_ID")
        .map(|run| format!("{run}-{run_attempt}-{platform}"))
        .unwrap_or_else(|_| format!("local-{platform}"));
    let receipt = json!({
        "$schema": "hooklistener.tunnel.cli-platform-evidence/1",
        "platform": platform,
        "status": "passed",
        "authenticated": true,
        "flows": {"human": "passed", "json": "passed"},
        "commands": ["tunnel prepare", "tunnel list"],
        "contract_schema_major": 1,
        "evidence_id": evidence_id,
        "provenance": {
            "service_git_sha": service_git_sha,
            "cli_git_sha": cli_git_sha
        }
    });

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create receipt directory");
    }
    fs::write(
        path,
        serde_json::to_vec_pretty(&receipt).expect("serialize receipt"),
    )
    .expect("write platform receipt");
}

#[tokio::test(flavor = "multi_thread")]
async fn authenticated_human_and_json_lifecycle_flows_emit_platform_evidence() {
    let mut server = Server::new_async().await;
    let contract = server
        .mock("GET", "/api/v1/tunnel/contract")
        .match_header("authorization", format!("Bearer {ACCESS_TOKEN}").as_str())
        .match_header("x-organization-id", ORGANIZATION_ID)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(
            r#"{"data":{"id":"hooklistener.tunnel.lifecycle","version":"1.0.0","schema":{"major":1,"minor":0},"receipts":{},"events":{},"resources":{},"lifecycle":{},"exit_codes":{}}}"#,
        )
        .expect(4)
        .create_async()
        .await;
    let sessions = server
        .mock("GET", "/api/v1/tunnel/sessions")
        .match_query(Matcher::Any)
        .match_header("authorization", format!("Bearer {ACCESS_TOKEN}").as_str())
        .match_header("x-organization-id", ORGANIZATION_ID)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body(r#"{"data":[],"meta":{"count":0}}"#)
        .expect(2)
        .create_async()
        .await;
    let home = TestHome::new();

    let human_prepare = assert_success(&home.command(
        &server.url(),
        &["tunnel", "prepare", "--port", "3000", "--host", "127.0.0.1"],
    ));
    assert!(human_prepare.contains("TUNNEL PREPARED"));
    assert!(human_prepare.contains("1.0.0"));

    let json_prepare = assert_success(&home.command(
        &server.url(),
        &[
            "--json",
            "tunnel",
            "prepare",
            "--port",
            "3000",
            "--host",
            "127.0.0.1",
        ],
    ));
    assert_eq!(
        assert_json_receipt(&json_prepare, "prepare")["status"],
        "prepared"
    );

    let human_list =
        assert_success(&home.command(&server.url(), &["tunnel", "list", "--limit", "50"]));
    assert!(human_list.contains("NO TUNNEL SESSIONS"));

    let json_list = assert_success(&home.command(
        &server.url(),
        &["--json", "tunnel", "list", "--limit", "50"],
    ));
    let list_receipt = assert_json_receipt(&json_list, "list");
    assert_eq!(list_receipt["status"], "succeeded");
    assert_eq!(list_receipt["data"]["data"], json!([]));

    contract.assert_async().await;
    sessions.assert_async().await;

    if let Some(path) = std::env::var_os("HOOKLISTENER_CONFORMANCE_OUTPUT") {
        write_platform_receipt(Path::new(&path));
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn subprocess_error_exit_contracts_are_stable_and_redacted() {
    let mut server = Server::new_async().await;
    let api = server
        .mock("GET", "/api/v1/endpoints")
        .match_header("authorization", format!("Bearer {ACCESS_TOKEN}").as_str())
        .match_header("x-organization-id", ORGANIZATION_ID)
        .with_status(503)
        .with_body("temporary failure")
        .expect(1)
        .create_async()
        .await;
    let home = TestHome::new();
    let error = assert_json_error(
        &home.command(&server.url(), &["--json", "endpoint", "list"]),
        1,
        "command_failed",
    );
    assert!(
        error["error"]["message"]
            .as_str()
            .unwrap()
            .contains("HTTP 503")
    );
    api.assert_async().await;

    let incompatible = server.mock("GET", "/api/v1/tunnel/contract")
        .with_status(200).with_header("content-type", "application/json")
        .with_body(r#"{"data":{"id":"lifecycle","version":"2.0.0","schema":{"major":2,"minor":0},"receipts":{},"events":{},"resources":{},"lifecycle":{},"exit_codes":{}}}"#)
        .expect(1).create_async().await;
    let error = assert_json_error(
        &home.command(&server.url(), &["--json", "tunnel", "list"]),
        3,
        "incompatible_schema",
    );
    assert_eq!(error["error"]["details"]["actual_major"], 2);
    incompatible.assert_async().await;

    assert_json_error(
        &home.command("http://unused.invalid", &["--json", "login"]),
        1,
        "unsupported_output_mode",
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn expired_events_cursor_exits_four_with_resync_details() {
    let mut server = Server::new_async().await;
    let contract = server.mock("GET", "/api/v1/tunnel/contract")
        .with_status(200).with_header("content-type", "application/json")
        .with_body(r#"{"data":{"id":"lifecycle","version":"1.0.0","schema":{"major":1,"minor":0},"receipts":{},"events":{},"resources":{},"lifecycle":{},"exit_codes":{}}}"#)
        .expect(1).create_async().await;
    let events = server.mock("GET", "/api/v1/tunnel/events")
        .match_query(Matcher::AllOf(vec![Matcher::UrlEncoded("cursor".into(), "expired".into()), Matcher::UrlEncoded("limit".into(), "50".into())]))
        .with_status(410).with_header("content-type", "application/json")
        .with_body(r#"{"error":{"code":"cursor_expired","message":"expired","earliest_cursor":"earliest","resync":{"sessions":"/api/v1/tunnel/sessions"}}}"#)
        .expect(1).create_async().await;
    let error = assert_json_error(
        &TestHome::new().command(
            &server.url(),
            &["--json", "tunnel", "events", "--cursor", "expired"],
        ),
        4,
        "cursor_expired",
    );
    assert_eq!(error["error"]["details"]["earliest_cursor"], "earliest");
    contract.assert_async().await;
    events.assert_async().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn destructive_endpoint_dispatches_no_request_until_yes_then_exactly_one() {
    let mut server = Server::new_async().await;
    let delete = server
        .mock("DELETE", "/api/v1/endpoints/ep_123")
        .match_header("authorization", format!("Bearer {ACCESS_TOKEN}").as_str())
        .match_header("x-organization-id", ORGANIZATION_ID)
        .with_status(204)
        .expect(1)
        .create_async()
        .await;
    let home = TestHome::new();
    assert_json_error(
        &home.command(&server.url(), &["--json", "endpoint", "delete", "ep_123"]),
        1,
        "confirmation_required",
    );
    let stdout = assert_success(&home.command(
        &server.url(),
        &["--json", "--yes", "endpoint", "delete", "ep_123"],
    ));
    let result: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(result["status"], "deleted");
    assert_eq!(result["organization_id"], ORGANIZATION_ID);
    delete.assert_async().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn endpoint_create_dispatches_scoped_body_with_human_and_json_output() {
    let mut server = Server::new_async().await;
    let create = server.mock("POST", "/api/v1/endpoints")
        .match_header("authorization", format!("Bearer {ACCESS_TOKEN}").as_str())
        .match_header("x-organization-id", ORGANIZATION_ID)
        .match_body(Matcher::Json(json!({"debug_endpoint":{"name":"Orders","slug":"orders"}})))
        .with_status(200).with_header("content-type", "application/json")
        .with_body(r#"{"data":{"id":"ep_123","name":"Orders","slug":"orders","status":"active","webhook_url":"https://example.test/orders"}}"#)
        .expect(2).create_async().await;
    let home = TestHome::new();
    let human = assert_success(&home.command(
        &server.url(),
        &["endpoint", "create", "Orders", "--slug", "orders"],
    ));
    assert!(human.contains("ENDPOINT CREATED") && human.contains("ep_123"));
    let stdout = assert_success(&home.command(
        &server.url(),
        &["--json", "endpoint", "create", "Orders", "--slug", "orders"],
    ));
    let result: Value = serde_json::from_str(stdout.trim()).unwrap();
    assert_eq!(result["organization_id"], ORGANIZATION_ID);
    assert_eq!(result["endpoint"]["id"], "ep_123");
    create.assert_async().await;
}

#[test]
fn every_top_level_command_exposes_help_without_loading_runtime_state() {
    let home = TestHome::new();
    for command in [
        "login",
        "logout",
        "listen",
        "tunnel",
        "endpoint",
        "cases",
        "static-tunnel",
        "anon",
        "share",
        "monitor",
        "org",
        "config",
        "diagnostics",
        "clean-logs",
        "completions",
        "update",
    ] {
        let output = home.command("http://unused.invalid", &[command, "--help"]);
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            output.status.success(),
            "{command} --help failed\nstdout={stdout}\nstderr={stderr}"
        );
        assert!(stdout.contains("Usage:"), "missing usage for {command}");
        assert!(!stdout.contains(ACCESS_TOKEN));
        assert!(!stderr.contains(ACCESS_TOKEN));
    }
}
