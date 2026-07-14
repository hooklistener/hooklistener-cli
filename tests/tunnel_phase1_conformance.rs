use mockito::{Matcher, Server};
use serde_json::{Value, json};
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use tempfile::TempDir;

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
        let mut command = Command::new(env!("CARGO_BIN_EXE_hooklistener"));
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
    let evidence_id = std::env::var("GITHUB_RUN_ID")
        .map(|run| format!("{run}-{platform}"))
        .unwrap_or_else(|_| format!("local-{platform}"));
    let receipt = json!({
        "$schema": "hooklistener.tunnel.cli-platform-evidence/1",
        "platform": platform,
        "status": "passed",
        "authenticated": true,
        "flows": {"human": "passed", "json": "passed"},
        "commands": ["tunnel prepare", "tunnel list"],
        "contract_schema_major": 1,
        "evidence_id": evidence_id
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
