# Saved-case CI conformance

Saved-case commands are qualified by the existing
`.github/workflows/tunnel-phase1-conformance.yml` matrix on Linux, macOS and
Windows. Its required check names remain `Authenticated lifecycle (linux)`,
`Authenticated lifecycle (macos)` and `Authenticated lifecycle (windows)`.
The release workflow already depends on that reusable workflow, so failures
block publication without adding a new branch-protection context.

## What runs

- The normal CI test job runs all Rust unit/integration tests and the Python
  tests for the conformance gate.
- Each platform builds a release binary from the checked-out source SHA.
- `scripts/verify_cases_release_tests.py` compares the **compiled** `cases::`
  integration-test names against `fixtures/cases_release_test_inventory.txt`.
  Missing, renamed, unexpectedly added or ignored contracts fail the gate. An
  empty inventory or accidentally disconnected test module cannot pass.
- After checking the inventory, the gate runs the whole
  `tunnel_phase1_conformance` target once with `--release --locked`. The existing
  tunnel lifecycle tests and their evidence remain intact.
- A saved-case receipt is written only after all tests pass. Failed reruns
  remove stale receipts; missing artifacts fail the workflow.

Coverage includes case authoring, scoped suites/history, server previews,
fail-closed older-server behavior, idempotency key propagation and conflicts,
422 run receipts, bounded observation, secret-safe failures, and exact empty
or whitespace-only replay overrides.

These tests launch real CLI subprocesses against **local mock HTTP APIs** with
isolated homes and fixture credentials. They do not read developer credentials,
contact the deployed service, or queue production deliveries. Backend deployment
and real-server transaction/worker correctness must be validated separately in
the backend's disposable integration environment.

## Evidence

Each platform uploads:

- The existing `phase1-<platform>-<run>-<attempt>` lifecycle artifact.
- `cases-<platform>-<run>-<attempt>`, containing
  `$schema: "hooklistener.cases.cli-platform-evidence/1"`.

The saved-case receipt includes the required test names/count, checked-out Git
SHA, a dirty-source indicator, tested executable SHA-256, platform, run ID and
attempt. It explicitly labels the backend `local_mock_http` and sets
`live_backend_verified: false`; it is not proof of a deployed backend smoke test.
The gate also refuses evidence if the executable changes during execution.

## Local checks

The Rust toolchain must be available to child processes. In the repository's
mise environment:

```sh
mise exec -- make check
mise exec -- make check-cases
```

`make check` runs workflow/gate self-tests, the full Rust test suite, rustfmt and
Clippy. `make check-cases` checks the inventory and runs release-profile platform
conformance, without requiring a running backend or secrets.

To mirror the CI receipt generation on macOS/Linux:

```sh
mise exec -- cargo build --release --locked
HOOKLISTENER_CONFORMANCE_BINARY="$PWD/target/release/hooklistener" \
HOOKLISTENER_CASES_CONFORMANCE_OUTPUT="/tmp/cases-conformance.json" \
  mise exec -- python3 scripts/verify_cases_release_tests.py
```

On Windows, select `target/release/hooklistener.exe`. Local uncommitted changes
are recorded as `source_dirty: true`; a local receipt is not clean-commit release
evidence.

When changing case tests, update the sorted inventory deliberately and run both
commands. Keep the safety-critical regressions asserted by
`scripts/release_workflow_contract_test.py`.
