# Tunnel Phase 1 conformance

The cross-platform gate runs the public binary as a subprocess on Linux,
macOS, and Windows. Each runner uses an isolated authenticated configuration
and a local HTTP contract fixture, then exercises `tunnel prepare` and
cloud-authoritative `tunnel list` in both human and `--json` modes.

The job first runs the bounded tunnel transport tests, so a platform receipt is
uploaded only after framing, queue, cancellation, and lifecycle tests pass.
The service release gate accepts exactly one receipt per supported platform and
requires `authenticated=true` plus passing human and JSON flows.

Run the same check locally with:

```sh
cargo test --locked tunnel
HOOKLISTENER_CONFORMANCE_OUTPUT=tmp/phase1-linux.json \
HOOKLISTENER_CONFORMANCE_PLATFORM=linux \
cargo test --locked --test tunnel_phase1_conformance -- --nocapture
```

Fixtures never contain production credentials or payloads, and both stdout and
stderr are checked for token disclosure before a receipt is written.
