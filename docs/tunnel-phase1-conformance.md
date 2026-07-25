# Tunnel Phase 1 conformance

The cross-platform gate runs the release-profile public binary as a subprocess
on Linux, macOS, and Windows. Each runner uses an isolated authenticated
configuration and a local HTTP contract fixture, then exercises `tunnel
prepare` and cloud-authoritative `tunnel list` in both human and `--json`
modes.

The job first runs the protocol v3 streaming transport tests in Cargo's release
profile, so a platform receipt is uploaded only after framing, queue,
cancellation, backpressure, and lifecycle tests pass. The authenticated
service release gate independently repeats these flows, emits the same receipt
schema with both the service and CLI commits, and accepts exactly one receipt
per supported platform with `authenticated=true` plus passing human and JSON
flows. Standalone CLI receipts intentionally record only the CLI commit and
cannot replace that cross-repository evidence.

The stable release workflow calls this gate with the immutable tag commit and
cannot create or resume a GitHub release until every platform passes. The
reusable workflow checks out that exact commit, verifies that an input tag
resolves to it, receives no publisher secrets, and records the CLI commit in
each runner-attempt-scoped receipt. Publication depends on every matrix result,
not on aggregating the uploaded receipts. If only a failed matrix leg is
rerun, receipts can therefore span run attempts without weakening the
publication gate. Pull requests, pushes to `main`, and manual runs keep
exercising the same matrix independently.

Run the same check locally with:

```sh
SOURCE_SHA="$(git rev-parse HEAD)"
python3 scripts/verify_tunnel_v3_release_tests.py
cargo build --release --locked
HOOKLISTENER_CONFORMANCE_OUTPUT=tmp/phase1-linux.json \
HOOKLISTENER_CONFORMANCE_PLATFORM=linux \
HOOKLISTENER_CONFORMANCE_BINARY=target/release/hooklistener \
HOOKLISTENER_CLI_GIT_SHA="${SOURCE_SHA}" \
cargo test --release --locked --test tunnel_phase1_conformance -- --nocapture
```

Fixtures never contain production credentials or payloads, and both stdout and
stderr are checked for token disclosure before a receipt is written.
