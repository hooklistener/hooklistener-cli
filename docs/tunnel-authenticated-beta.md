# Authenticated tunnel beta

The beta exposes two explicit relay modes to signed-in users whose organization
is in the server-managed cohort:

- `hooklistener tunnel start --port 3000` gives the public caller the local
  handler's response.
- `hooklistener listen <endpoint> --target http://localhost:3000` forwards an
  already captured endpoint request. The endpoint's configured response, not
  the local handler's response, remains public.

Run `hooklistener login`, select the intended organization with
`hooklistener org use <organization-id>`, and use `hooklistener tunnel prepare
--port 3000` to verify authentication, schema compatibility, target resolution,
and the activation plan before starting a direct-response relay.

## Lifecycle and diagnostics

Cloud-authoritative state remains available when the original CLI process is
gone:

```text
hooklistener tunnel list
hooklistener tunnel status <session-id>
hooklistener tunnel events --cursor <cursor> --follow
hooklistener tunnel capture <capture-id>
hooklistener tunnel attempt <attempt-id>
hooklistener tunnel detach <session-id> --reason "switching machines"
hooklistener tunnel stop <session-id> --reason "deployment complete"
```

Use `--json` for noninteractive NDJSON receipts and events. Persist opaque event
cursors; if a cursor expires, rehydrate the resources named by the error before
continuing. Support requests should include the safe session, capture, attempt,
outcome, and event identifiers, plus `hooklistener --version` and the operating
system. Do not include account credentials, relay tickets, resume tokens, raw
headers, queries, bodies, or local target details.

## Relay security

Each WebSocket connection obtains a new short-lived, single-use relay ticket.
The ticket is bound to the activation mode, organization and route, and a target
plan containing the resolved addresses. The CLI pins those addresses, ignores
proxy environment variables, and does not follow redirects.

Targets are loopback-only unless `--allow-non-loopback` is supplied for an
explicitly trusted host. `listen` also requires a visible `--insecure-tls` flag
to disable HTTPS certificate verification. Target credentials, query strings,
fragments, invalid schemes, and forbidden addresses are rejected before relay
activation.

## Phase 1 boundaries

Direct-response transport uses protocol version 2, bounded 64 KiB frames,
per-frame acknowledgement, bounded work and response queues, advertised body
and header limits, and one end-to-end deadline. Capture-forward reads only an
already durable capture through a scoped, audited grant.

Anonymous tunnels use the separate bounded workflow documented in
`docs/tunnel-anonymous-routes.md`. The beta does not promise WebSocket upgrades,
HTTP trailers, public sharing, stable public API compatibility, MCP exposure,
or unattended agent autonomy. A server rollback can reject activation or stop
dispatch to an existing authenticated client; retain the safe typed error and
retry only after the cohort is restored. There is no legacy relay fallback.
