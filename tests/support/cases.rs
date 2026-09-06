//! Subprocess coverage using the same isolated home and authenticated mock API as lifecycle tests.
use super::{ACCESS_TOKEN, ORGANIZATION_ID, TestHome, assert_json_error, assert_success};
use mockito::{Matcher, Server};
use serde_json::{Value, json};

fn saved_case() -> Value {
    json!({
        "id":"case-1", "endpoint_id":"ep-1", "name":"Payment succeeded",
        "method":"POST", "path":"/webhooks", "source_debug_request_id":"req-1",
        "assertion_config":{"expected_status_code":200,"expected_json_body_subset":{"accepted":true}}
    })
}

fn suite() -> Value {
    json!({"id":"suite-1","endpoint_id":"ep-1","name":"Billing","case_count":1,
        "cases":[{"position":0,"case":saved_case()}]})
}

fn run(result: &str) -> Value {
    json!({"id":"run-1","case_suite_run_id":"run-1","endpoint_id":"ep-1",
        "case_suite_id":"suite-1","case_suite_name":"Billing",
        "status":if result == "pending" {"queued"} else {"completed"},
        "result_status":result,"async":result == "pending", "target":{"type":"cli"},
        "total_count":1,"queued_count":1,
        "completed_count":if result == "pending" {0} else {1},
        "not_configured_count":if result == "completed" {1} else {0},
        "passed_count":if result == "passed" {1} else {0}})
}

fn mock(server: &mut Server, method: &str, path: &str, value: Value) -> mockito::Mock {
    let action = path.ends_with("/execute");
    let operation = if path.contains("/cases/run/") {
        "http.case.run/1"
    } else {
        "http.case.replay/1"
    };
    server
        .mock(method, path)
        .match_header("authorization", format!("Bearer {ACCESS_TOKEN}").as_str())
        .match_header("x-organization-id", ORGANIZATION_ID)
        .with_status(200)
        .with_header("content-type", "application/json")
        .with_body_from_request(move |request| {
            let mut data = value.clone();
            if action {
                let headers = request.header("idempotency-key");
                let key = headers
                    .first()
                    .expect("delivery must have a key")
                    .to_str()
                    .unwrap();
                data["$schema"] = json!("hooklistener.cases.action/1");
                data["schema_version"] = json!(1);
                data["idempotency"] =
                    json!({"key":key,"tool_name":operation,"disposition":"executed"});
            }
            serde_json::to_vec(&json!({"data":data})).unwrap()
        })
}

#[tokio::test(flavor = "multi_thread")]
async fn all_queue_failures_keep_the_durable_run_receipt_from_http_422() {
    let mut server = Server::new_async().await;
    let mut report = run("failed");
    report["queued_count"] = json!(0);
    report["completed_count"] = json!(0);
    report["failed_count"] = json!(1);
    report["queue_failed_count"] = json!(1);
    let post = mock(
        &mut server,
        "POST",
        "/api/v1/endpoints/ep-1/cases/run/execute",
        report,
    )
    .with_status(422)
    .expect(1)
    .create_async()
    .await;
    let output = TestHome::new().command(
        &server.url(),
        &["cases", "run", "ep-1", "--target", "cli", "--json"],
    );
    assert_eq!(output.status.code(), Some(1));
    let data: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(data["case_suite_run_id"], "run-1");
    assert_eq!(data["queue_failed_count"], 1);
    assert!(
        String::from_utf8_lossy(&output.stderr)
            .contains(data["idempotency"]["key"].as_str().unwrap())
    );
    post.assert_async().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn malformed_case_response_does_not_quote_sensitive_values() {
    let mut server = Server::new_async().await;
    let mut malformed = suite();
    malformed["case_count"] = json!("SECRET-RESPONSE-VALUE");
    let get = mock(&mut server, "GET", "/api/v1/case-suites/suite-1", malformed)
        .create_async()
        .await;
    let output = TestHome::new().command(
        &server.url(),
        &["cases", "suites", "show", "suite-1", "--json"],
    );
    assert_json_error(&output, 1, "command_failed");
    assert!(!String::from_utf8_lossy(&output.stdout).contains("SECRET-RESPONSE-VALUE"));
    get.assert_async().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn wait_resumes_observing_a_server_timed_out_run_with_pending_deliveries() {
    let mut server = Server::new_async().await;
    let mut report = run("timeout");
    report["timed_out"] = json!(true);
    report["waiting_count"] = json!(1);
    report["completed_count"] = json!(0);
    let initial = mock(&mut server, "GET", "/api/v1/case-runs/run-1", report)
        .expect(1)
        .create_async()
        .await;
    let completed = mock(&mut server, "GET", "/api/v1/case-runs/run-1", run("passed"))
        .expect(1)
        .create_async()
        .await;
    let no_posts = server
        .mock("POST", Matcher::Any)
        .expect(0)
        .create_async()
        .await;
    let output = TestHome::new().command(
        &server.url(),
        &[
            "cases",
            "runs",
            "wait",
            "run-1",
            "--timeout",
            "3s",
            "--interval-ms",
            "100",
            "--json",
        ],
    );
    assert_eq!(response(&output)["result_status"], "passed");
    initial.assert_async().await;
    completed.assert_async().await;
    no_posts.assert_async().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn leaf_org_override_scopes_case_reads() {
    let mut server = Server::new_async().await;
    let get = server
        .mock("GET", "/api/v1/cases/case-1")
        .match_header("authorization", format!("Bearer {ACCESS_TOKEN}").as_str())
        .match_header("x-organization-id", "other-org")
        .with_body(json!({"data":saved_case()}).to_string())
        .create_async()
        .await;
    let output = TestHome::new().command(
        &server.url(),
        &["cases", "show", "case-1", "--org", "other-org", "--json"],
    );
    assert_eq!(response(&output)["organization_id"], "other-org");
    get.assert_async().await;
}

fn response(output: &std::process::Output) -> Value {
    let stdout = assert_success(output);
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.is_empty()
            || (stderr.starts_with("Idempotency key: ") && stderr.lines().count() == 1),
        "{output:?}"
    );
    serde_json::from_str(&stdout).expect("single JSON document")
}

#[tokio::test(flavor = "multi_thread")]
async fn case_list_and_show_preserve_assertions_and_scope() {
    let mut server = Server::new_async().await;
    let list = mock(
        &mut server,
        "GET",
        "/api/v1/endpoints/ep-1/cases",
        json!([saved_case()]),
    )
    .create_async()
    .await;
    let get = mock(&mut server, "GET", "/api/v1/cases/case-1", saved_case())
        .create_async()
        .await;
    let home = TestHome::new();
    let data = response(&home.command(&server.url(), &["cases", "list", "ep-1", "--json"]));
    assert_eq!(data["cases"][0]["id"], "case-1");
    let data = response(&home.command(&server.url(), &["cases", "show", "case-1", "--json"]));
    assert_eq!(
        data["case"]["assertion_config"]["expected_status_code"],
        200
    );
    list.assert_async().await;
    get.assert_async().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn case_save_maps_cli_options_to_http_case_attributes() {
    let mut server = Server::new_async().await;
    let save = mock(&mut server, "POST", "/api/v1/endpoints/ep-1/requests/req-1/cases", saved_case())
        .match_body(Matcher::Json(json!({"name":"Payment succeeded", "default_method":"POST",
            "default_request_headers":{"content-type":"application/json"},
            "default_request_body":"{\"event\":1}",
            "assertion_config":{"expected_status_code":200,"expected_json_body_subset":{"accepted":true}}})))
        .expect(1).create_async().await;
    let output = TestHome::new().command(
        &server.url(),
        &[
            "cases",
            "save",
            "ep-1",
            "req-1",
            "--name",
            "Payment succeeded",
            "--method",
            "post",
            "--headers",
            "{\"content-type\":\"application/json\"}",
            "--body",
            "{\"event\":1}",
            "--expect-status",
            "200",
            "--expect-json",
            "{\"accepted\":true}",
            "--json",
        ],
    );
    assert_eq!(response(&output)["case"]["id"], "case-1");
    save.assert_async().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn case_update_preserves_other_assertions_and_clear_is_explicit() {
    let mut server = Server::new_async().await;
    let get = mock(&mut server, "GET", "/api/v1/cases/case-1", saved_case())
        .expect(1)
        .create_async()
        .await;
    let patch = mock(&mut server, "PATCH", "/api/v1/cases/case-1", saved_case())
        .match_body(Matcher::Json(json!({"assertion_config":{"expected_status_code":201,"expected_json_body_subset":{"accepted":true}}})))
        .expect(1).create_async().await;
    let clear = mock(&mut server, "PATCH", "/api/v1/cases/case-1", saved_case())
        .match_body(Matcher::Json(json!({"assertion_config":{}})))
        .expect(1)
        .create_async()
        .await;
    let home = TestHome::new();
    response(&home.command(
        &server.url(),
        &[
            "cases",
            "update",
            "case-1",
            "--expect-status",
            "201",
            "--json",
        ],
    ));
    response(&home.command(
        &server.url(),
        &["cases", "update", "case-1", "--clear-assertions", "--json"],
    ));
    get.assert_async().await;
    patch.assert_async().await;
    clear.assert_async().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn suite_discovery_and_selection_queue_once_then_poll_existing_run() {
    let mut server = Server::new_async().await;
    let list = mock(
        &mut server,
        "GET",
        "/api/v1/endpoints/ep-1/case-suites",
        json!([suite()]),
    )
    .create_async()
    .await;
    let show = mock(&mut server, "GET", "/api/v1/case-suites/suite-1", suite())
        .create_async()
        .await;
    let post = mock(
        &mut server,
        "POST",
        "/api/v1/endpoints/ep-1/cases/run/execute",
        run("pending"),
    )
    .match_body(Matcher::Json(
        json!({"target":"cli","case_suite_id":"suite-1"}),
    ))
    .expect(1)
    .create_async()
    .await;
    let get = mock(&mut server, "GET", "/api/v1/case-runs/run-1", run("passed"))
        .expect(1)
        .create_async()
        .await;
    let home = TestHome::new();
    assert_eq!(
        response(&home.command(
            &server.url(),
            &["cases", "suites", "list", "ep-1", "--json"]
        ))["suites"][0]["name"],
        "Billing"
    );
    assert_eq!(
        response(&home.command(
            &server.url(),
            &["cases", "suites", "show", "suite-1", "--json"]
        ))["suite"]["cases"][0]["case"]["id"],
        "case-1"
    );
    let output = home.command(
        &server.url(),
        &[
            "cases",
            "run",
            "ep-1",
            "--target",
            "cli",
            "--case-suite-id",
            "suite-1",
            "--wait",
            "--timeout",
            "3s",
            "--interval-ms",
            "100",
            "--json",
        ],
    );
    let result = response(&output);
    assert_eq!(result["result_status"], "passed");
    assert_eq!(result["waited"], true);
    assert_eq!(result["idempotency"]["disposition"], "executed");
    assert!(uuid::Uuid::parse_str(result["idempotency"]["key"].as_str().unwrap()).is_ok());
    for mock in [list, show, post, get] {
        mock.assert_async().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn dry_run_uses_only_authoritative_preview_routes() {
    let mut server = Server::new_async().await;
    let no_delivery = server
        .mock("POST", Matcher::Regex("/(execute|run|replay)$".into()))
        .expect(0)
        .create_async()
        .await;
    for (operation, id, path) in [
        ("run", "ep-1", "/api/v1/endpoints/ep-1/cases/run/preview"),
        ("replay", "case-1", "/api/v1/cases/case-1/replay/preview"),
    ] {
        let data = json!({"$schema":"hooklistener.cases.preview/1","schema_version":1,
            "status":"preview","dry_run":true,"operation":operation,"case_ids":["case-1"],
            "checks":{"scope":true,"target_resolution":true,"target_policy":true,"delivery":false},
            "active_listener_count":0,"target":{"type":"cli"}});
        let preview = mock(&mut server, "POST", path, data.clone())
            .match_header("idempotency-key", Matcher::Missing)
            .match_body(Matcher::Json(json!({"target":"cli"})))
            .expect(1)
            .create_async()
            .await;
        let output = TestHome::new().command(
            &server.url(),
            &[
                "cases",
                operation,
                id,
                "--target",
                "cli",
                "--dry-run",
                "--json",
            ],
        );
        assert_eq!(response(&output), data);
        assert!(output.stderr.is_empty());
        preview.assert_async().await;
    }
    no_delivery.assert_async().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn dry_run_rejects_a_suite_from_another_endpoint() {
    let mut server = Server::new_async().await;
    let get = server
        .mock("POST", "/api/v1/endpoints/ep-1/cases/run/preview")
        .match_body(Matcher::Json(
            json!({"target":"cli","case_suite_id":"suite-1"}),
        ))
        .with_status(404)
        .with_body(
            json!({"errors":{"code":"case_resource_not_found","detail":"Suite outside scope"}})
                .to_string(),
        )
        .create_async()
        .await;
    let no_posts = server
        .mock("POST", Matcher::Regex("/(execute|run|replay)$".into()))
        .expect(0)
        .create_async()
        .await;
    let output = TestHome::new().command(
        &server.url(),
        &[
            "cases",
            "run",
            "ep-1",
            "--suite",
            "suite-1",
            "--target",
            "cli",
            "--dry-run",
            "--json",
        ],
    );
    assert_json_error(&output, 1, "command_failed");
    get.assert_async().await;
    no_posts.assert_async().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_queues_request_overrides_and_returns_forward_id() {
    let mut server = Server::new_async().await;
    let post = mock(&mut server, "POST", "/api/v1/cases/case-1/replay/execute", json!({"forward_id":"forward-1","debug_request_case_id":"case-1","status":"queued","assertion_status":"pending","target":{"type":"cli"},"poll_url":"/api/v1/forwards/forward-1"}))
        .match_body(Matcher::Json(json!({"target":"cli","method":"PATCH","request_headers":{"x-test":"yes"},"request_body":"new body"})))
        .expect(1).create_async().await;
    let output = TestHome::new().command(
        &server.url(),
        &[
            "cases",
            "replay",
            "case-1",
            "--target",
            "cli",
            "--method",
            "patch",
            "--headers",
            "{\"x-test\":\"yes\"}",
            "--body",
            "new body",
            "--json",
        ],
    );
    assert_eq!(response(&output)["replay"]["forward_id"], "forward-1");
    post.assert_async().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_preserves_empty_and_whitespace_body_overrides() {
    let mut server = Server::new_async().await;
    for body in ["", " \t\n"] {
        let post = mock(
            &mut server,
            "POST",
            "/api/v1/cases/case-1/replay/execute",
            json!({"forward_id":"forward-1","debug_request_case_id":"case-1","status":"queued",
                "target":{"type":"cli"},"poll_url":"/api/v1/forwards/forward-1"}),
        )
        .match_body(Matcher::Json(json!({"target":"cli","request_body":body})))
        .expect(1)
        .create_async()
        .await;
        let output = TestHome::new().command(
            &server.url(),
            &[
                "cases", "replay", "case-1", "--target", "cli", "--body", body, "--json",
            ],
        );
        assert_eq!(response(&output)["replay"]["forward_id"], "forward-1");
        post.assert_async().await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn run_history_has_pagination_and_suite_filter() {
    let mut server = Server::new_async().await;
    let get = server.mock("GET", "/api/v1/endpoints/ep-1/case-runs")
        .match_query(Matcher::AllOf(vec![Matcher::UrlEncoded("page".into(),"2".into()),Matcher::UrlEncoded("page_size".into(),"5".into()),Matcher::UrlEncoded("case_suite_id".into(),"suite-1".into())]))
        .match_header("x-organization-id", ORGANIZATION_ID)
        .with_body(json!({"data":[run("completed")],"pagination":{"page":2,"page_size":5,"total_count":6,"total_pages":2}}).to_string())
        .create_async().await;
    let result = response(&TestHome::new().command(
        &server.url(),
        &[
            "cases",
            "runs",
            "list",
            "ep-1",
            "--page",
            "2",
            "--page-size",
            "5",
            "--suite",
            "suite-1",
            "--json",
        ],
    ));
    assert_eq!(result["pagination"]["page"], 2);
    assert_eq!(result["runs"][0]["result_status"], "completed");
    get.assert_async().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn bounded_wait_reports_timeout_without_requeueing() {
    let mut server = Server::new_async().await;
    let get = mock(
        &mut server,
        "GET",
        "/api/v1/case-runs/run-1",
        run("pending"),
    )
    .expect(1)
    .create_async()
    .await;
    let no_posts = server
        .mock("POST", Matcher::Any)
        .expect(0)
        .create_async()
        .await;
    let output = TestHome::new().command(
        &server.url(),
        &["cases", "runs", "wait", "run-1", "--timeout", "0", "--json"],
    );
    assert_eq!(output.status.code(), Some(1));
    let result: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(result["result_status"], "timeout");
    assert_eq!(result["case_suite_run_id"], "run-1");
    assert!(output.stderr.is_empty());
    get.assert_async().await;
    no_posts.assert_async().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn run_results_distinguish_assertion_failure_unasserted_and_unknown_status() {
    let mut server = Server::new_async().await;
    let home = TestHome::new();
    for (result, expected_exit) in [("failed", 1), ("completed", 0), ("future-unknown", 1)] {
        let get = mock(&mut server, "GET", "/api/v1/case-runs/run-1", run(result))
            .expect(1)
            .create_async()
            .await;
        let output = home.command(&server.url(), &["cases", "runs", "show", "run-1", "--json"]);
        assert_eq!(output.status.code(), Some(expected_exit));
        let data: Value = serde_json::from_slice(&output.stdout).unwrap();
        assert_eq!(data["result_status"], result);
        assert_eq!(data["passed_count"], 0);
        get.assert_async().await;
    }
}

fn delivery_error(output: &std::process::Output) -> Value {
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.starts_with("Idempotency key: "), "{output:?}");
    assert_eq!(stderr.lines().count(), 1);
    assert!(!stderr.contains(ACCESS_TOKEN));
    // The pre-submission recovery notice is intentional; stdout retains the error contract.
    let mut without_notice = output.clone();
    without_notice.stderr.clear();
    assert_json_error(&without_notice, 1, "command_failed")
}

#[tokio::test(flavor = "multi_thread")]
async fn delivery_transport_failure_is_not_retried_and_body_is_not_disclosed() {
    let mut server = Server::new_async().await;
    let post = server
        .mock("POST", "/api/v1/endpoints/ep-1/cases/run/execute")
        .with_status(503)
        .with_body("private-payload-DO-NOT-PRINT")
        .expect(1)
        .create_async()
        .await;
    let output = TestHome::new().command(
        &server.url(),
        &["cases", "run", "ep-1", "--target", "cli", "--json"],
    );
    delivery_error(&output);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("private-payload"));
    post.assert_async().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn explicit_keys_are_reused_and_conflicts_are_not_retried() {
    let mut server = Server::new_async().await;
    let post = mock(
        &mut server,
        "POST",
        "/api/v1/endpoints/ep-1/cases/run/execute",
        run("pending"),
    )
    .match_header("idempotency-key", "intentional-run-123")
    .match_body(Matcher::Json(json!({"target":"cli"})))
    .expect(2)
    .create_async()
    .await;
    let home = TestHome::new();
    for _ in 0..2 {
        let output = home.command(
            &server.url(),
            &[
                "cases",
                "run",
                "ep-1",
                "--target",
                "cli",
                "--idempotency-key",
                "intentional-run-123",
                "--json",
            ],
        );
        assert_eq!(
            response(&output)["idempotency"]["key"],
            "intentional-run-123"
        );
    }
    post.assert_async().await;
    let conflict = server.mock("POST", "/api/v1/endpoints/ep-1/cases/run/execute")
        .match_header("idempotency-key", "intentional-run-123")
        .match_body(Matcher::Json(json!({"target_url":"https://example.com/changed"})))
        .with_status(409).with_body(json!({"errors":{"code":"idempotency_key_conflict","detail":"Key already used with different inputs"}}).to_string())
        .expect(1).create_async().await;
    let output = home.command(
        &server.url(),
        &[
            "cases",
            "run",
            "ep-1",
            "--target",
            "https://example.com/changed",
            "--idempotency-key",
            "intentional-run-123",
            "--json",
        ],
    );
    delivery_error(&output);
    assert!(String::from_utf8_lossy(&output.stdout).contains("different inputs"));
    conflict.assert_async().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn older_servers_fail_closed_for_both_preview_and_execution() {
    let mut server = Server::new_async().await;
    let legacy = server
        .mock("POST", Matcher::Regex("/(run|replay)$".into()))
        .expect(0)
        .create_async()
        .await;
    for (operation, id, prefix) in [
        ("run", "ep-1", "/api/v1/endpoints/ep-1/cases/run"),
        ("replay", "case-1", "/api/v1/cases/case-1/replay"),
    ] {
        for action in ["preview", "execute"] {
            let unsupported = server
                .mock("POST", format!("{prefix}/{action}").as_str())
                .with_status(404)
                .expect(1)
                .create_async()
                .await;
            let mut args = vec!["cases", operation, id, "--target", "cli", "--json"];
            if action == "preview" {
                args.push("--dry-run");
            }
            let output = TestHome::new().command(&server.url(), &args);
            if action == "preview" {
                assert_json_error(&output, 1, "command_failed");
            } else {
                delivery_error(&output);
            }
            assert!(String::from_utf8_lossy(&output.stdout).contains("No legacy endpoint"));
            unsupported.assert_async().await;
        }
    }
    legacy.assert_async().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn incompatible_preview_and_mismatched_receipts_are_not_accepted() {
    let mut server = Server::new_async().await;
    let preview = mock(
        &mut server,
        "POST",
        "/api/v1/endpoints/ep-1/cases/run/preview",
        json!({"status":"client_preview","dry_run":true}),
    )
    .expect(1)
    .create_async()
    .await;
    let output = TestHome::new().command(
        &server.url(),
        &[
            "cases",
            "run",
            "ep-1",
            "--target",
            "cli",
            "--dry-run",
            "--json",
        ],
    );
    assert_json_error(&output, 1, "command_failed");
    preview.assert_async().await;
    let mut data = run("pending");
    data["$schema"] = json!("hooklistener.cases.action/1");
    data["schema_version"] = json!(1);
    data["idempotency"] =
        json!({"key":"DO-NOT-PRINT","disposition":"executed","tool_name":"http.case.run/1"});
    let execute = server
        .mock("POST", "/api/v1/endpoints/ep-1/cases/run/execute")
        .with_status(202)
        .with_body(json!({"data":data}).to_string())
        .expect(1)
        .create_async()
        .await;
    let output = TestHome::new().command(
        &server.url(),
        &["cases", "run", "ep-1", "--target", "cli", "--json"],
    );
    delivery_error(&output);
    assert!(!String::from_utf8_lossy(&output.stdout).contains("DO-NOT-PRINT"));
    execute.assert_async().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn replay_preview_includes_delivery_overrides_without_a_key() {
    let mut server = Server::new_async().await;
    let preview = mock(&mut server, "POST", "/api/v1/cases/case-1/replay/preview", json!({"$schema":"hooklistener.cases.preview/1","schema_version":1,"status":"preview","dry_run":true}))
        .match_header("idempotency-key", Matcher::Missing)
        .match_body(Matcher::Json(json!({"target":"cli","method":"PATCH","request_body":"preview body","request_headers":{"x-test":"yes"}})))
        .expect(1).create_async().await;
    let output = TestHome::new().command(
        &server.url(),
        &[
            "cases",
            "replay",
            "case-1",
            "--target",
            "cli",
            "--method",
            "patch",
            "--body",
            "preview body",
            "--headers",
            "{\"x-test\":\"yes\"}",
            "--dry-run",
            "--json",
        ],
    );
    assert_eq!(response(&output)["status"], "preview");
    preview.assert_async().await;
}

#[tokio::test(flavor = "multi_thread")]
async fn human_case_output_is_control_neutral_and_fits_80_columns() {
    let mut server = Server::new_async().await;
    let mut case = saved_case();
    case["name"] = json!("bad\u{1b}[31m\nstatus");
    let get = mock(
        &mut server,
        "GET",
        "/api/v1/endpoints/ep-1/cases",
        json!([case]),
    )
    .create_async()
    .await;
    let output = TestHome::new().command(&server.url(), &["cases", "list", "ep-1"]);
    let stdout = assert_success(&output);
    assert!(!stdout.contains('\u{1b}'));
    assert!(stdout.lines().all(|line| line.chars().count() <= 80));
    get.assert_async().await;
}
