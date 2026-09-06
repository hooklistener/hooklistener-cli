# Hooklistener CLI

[![CI](https://github.com/hooklistener/hooklistener-cli/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/hooklistener/hooklistener-cli/actions/workflows/ci.yml)
[![Release](https://img.shields.io/github/v/release/hooklistener/hooklistener-cli?sort=semver)](https://github.com/hooklistener/hooklistener-cli/releases)
[![crates.io](https://img.shields.io/crates/v/hooklistener-cli.svg)](https://crates.io/crates/hooklistener-cli)
[![npm](https://img.shields.io/npm/v/hooklistener.svg)](https://www.npmjs.com/package/hooklistener)

Inspect webhooks, replay failures, and expose localhost from your terminal.

Hooklistener CLI combines live terminal views with scriptable commands for forwarding webhook traffic, managing captures, sharing requests, and monitoring endpoints.

## Installation

| Method | Command |
| --- | --- |
| Homebrew (macOS or Linux) | `brew install hooklistener/tap/hooklistener` |
| npm | `npm install -g hooklistener` |
| Cargo | `cargo install hooklistener-cli` |

Prebuilt binaries are available on the [Releases page](https://github.com/hooklistener/hooklistener-cli/releases). You can also use the install scripts for [macOS or Linux](https://raw.githubusercontent.com/hooklistener/hooklistener-cli/main/scripts/install.sh) and [Windows PowerShell](https://raw.githubusercontent.com/hooklistener/hooklistener-cli/main/scripts/install.ps1).

The Linux x86_64 prebuilt binary requires glibc 2.35 or newer (for example,
Ubuntu 22.04 or Debian 12). On an older distribution, build from source with
`cargo install hooklistener-cli`.

Every prebuilt-binary installer and the self-updater require the archive to
match its exact entry in the release's `SHA256SUMS.txt`. The release workflow
also creates a GitHub SLSA provenance attestation that binds each archive
digest to the release workflow, exact source commit, and tag. To authenticate a
download independently of the co-hosted checksum file, install the GitHub CLI
and verify all four constraints:

```bash
gh attestation verify hooklistener-x86_64-unknown-linux-gnu.tar.gz \
  --repo hooklistener/hooklistener-cli \
  --signer-workflow hooklistener/hooklistener-cli/.github/workflows/release.yml \
  --source-digest <full-release-commit-sha> \
  --source-ref refs/tags/v1.8.0
```

Verify the installation:

```bash
hooklistener --version
```

## Quick start

Sign in, then expose your local server with a public HTTPS URL:

```bash
hooklistener login
hooklistener tunnel --port 3000
```

Use the public URL printed by the CLI as the callback URL in Stripe, GitHub, Shopify, or any other webhook provider. Incoming requests and responses appear in the live terminal view.

If the provider already sends events to a Hooklistener debug endpoint, forward those events to your local app instead:

```bash
hooklistener listen stripe-sandbox \
  --target http://localhost:3000/webhooks/stripe
```

Listener targets are loopback-only by default. Hooklistener resolves and pins the target address,
does not follow redirects, ignores proxy environment variables, and rejects target URLs containing
credentials, query strings, or fragments. Use `--allow-non-loopback` only for an explicitly trusted
remote target. HTTPS certificate verification can be disabled only with the visible
`--insecure-tls` opt-in.

### Try it without logging in

Expose localhost directly with a bounded 15-minute anonymous route:

```bash
hooklistener anon tunnel --port 3000 --name stable-demo
```

Save the route and claim tokens printed before the live view starts. After
signing in, move the stable name into your organization without transferring
any pre-claim captures:

```bash
hooklistener anon claim <route-id> --token <claim-token> --org <organization-id>
```

To create a capture-only temporary endpoint instead:

```bash
hooklistener anon create --ttl 3600
```

The result includes the endpoint URL, endpoint ID, and viewer token needed to inspect captured events.

## Choose the right workflow

| Goal | Command | Use it when |
| --- | --- | --- |
| Forward an existing Hooklistener endpoint | `hooklistener listen` | Events already arrive at Hooklistener and should be forwarded to your local app |
| Expose a local server | `hooklistener tunnel` | A provider needs a public URL that points directly to localhost |
| Expose localhost without signing in | `hooklistener anon tunnel` | You need a bounded temporary relay and can accept reduced limits |
| Inspect and replay captured requests | `hooklistener endpoint` | You need stored payloads, headers, and forwarding history |
| Run saved endpoint cases | `hooklistener cases` | You want to replay a repeatable test suite against a URL or saved target |
| Create a temporary endpoint | `hooklistener anon` | You need a short-lived capture URL without an account |
| Share a captured request | `hooklistener share` | A teammate needs access to a payload and its forwarding history |
| Check an HTTP endpoint | `hooklistener monitor` | You need recurring uptime checks and failure visibility |

Reserved static tunnel slugs depend on your Hooklistener plan.

Tunnel requests are forwarded concurrently, so a slow local response does not block later
requests. If the WebSocket disconnects, in-flight work is cancelled and never replayed; static
tunnels reacquire their slug on reconnect, while requests already dispatched to localhost are
reported by the service as having an unknown outcome.

Machine-readable tunnel receipts redact authorization, cookies, API keys, tokens, and secret
header values. Debug logs omit the access-token query parameter from WebSocket endpoints.

### Operate tunnel lifecycle state

Tunnel sessions, captures, delivery attempts, and events are cloud-authoritative. They remain inspectable and stoppable after the CLI process that started them exits:

```bash
hooklistener tunnel prepare --port 3000
hooklistener tunnel start --port 3000
hooklistener tunnel list
hooklistener tunnel status <session-id>
hooklistener tunnel events --cursor <cursor> --follow
hooklistener tunnel capture <capture-id>
hooklistener tunnel attempt <attempt-id>
hooklistener tunnel detach <session-id> --reason "switching machines"
hooklistener tunnel stop <session-id> --reason "deployment complete"
```

`hooklistener tunnel --port 3000` remains an alias for `tunnel start`. Every lifecycle command negotiates the authenticated tunnel contract first; an incompatible schema major fails before relay activation.

`tunnel` and `listen` are authenticated beta relay modes and require an organization enabled by Hooklistener. Every connection exchanges the account credential for a short-lived, single-use ticket scoped to its mode, route, organization, and pinned target. `anon tunnel` uses a separate public bootstrap with a 1 MiB body limit, per-route request limits, short expiry, and no authenticated capture access. See [the authenticated beta guide](docs/tunnel-authenticated-beta.md) and [the anonymous route guide](docs/tunnel-anonymous-routes.md).

## Work with captured requests

Select an organization once for account-backed commands:

```bash
hooklistener org list
hooklistener org use <organization-id>
```

Create an endpoint, inspect its traffic, and replay a request:

```bash
hooklistener endpoint create "Billing Webhooks" --slug billing-webhooks
hooklistener endpoint requests <endpoint-id>
hooklistener endpoint request <endpoint-id> <request-id>
hooklistener endpoint forward-request \
  <endpoint-id> <request-id> http://localhost:3000/webhooks
```

Save a captured request with response assertions, then run it against an active local listener:

```bash
# Keep this running in another terminal; use the endpoint's slug here.
hooklistener listen <endpoint-slug> --target http://localhost:3000/webhooks

hooklistener cases save <endpoint-id> <request-id> \
  --name "Payment accepted" --expect-status 200 --expect-json '{"accepted":true}'
hooklistener cases run <endpoint-id> --target cli --wait --timeout 60s
```

`--target cli` delivers through active CLI listeners for that endpoint. A URL passed
with `--target` or `--target-url` is delivered **by the service**, not by this CLI
process. Use `--target cli` for localhost. Multiple active listeners may receive
replays; stop other listeners if you need a single local delivery.

### Manage saved cases and inspect results

```bash
hooklistener cases list <endpoint-id>
hooklistener cases show <case-id> --json
hooklistener cases update <case-id> --expect-status 202
hooklistener cases replay <case-id> --target cli --method POST --body '{"test":true}'

hooklistener cases suites list <endpoint-id>
hooklistener cases suites show <suite-id>
hooklistener cases run <endpoint-id> --suite <suite-id> --target cli --wait

hooklistener cases runs list <endpoint-id> --suite <suite-id> --page 1 --page-size 20
hooklistener cases runs show <run-id> --json
hooklistener cases runs wait <run-id> --timeout 60s --json
```

Save and update accept `--name`, `--notes`, `--default-target-url`, `--method`,
`--headers` (a JSON object of string values), `--body`, `--expect-status`, and
`--expect-json` (a nonempty JSON object). Updates preserve omitted fields and
merge supplied assertions with the current configuration; `--clear-assertions`
explicitly removes all assertions. Assertion updates use read-then-write, not an
atomic server merge: avoid simultaneous edits to the same case.

Replay requires an explicit target. Its `--method`, `--headers`, and `--body`
options affect only that delivery, not the saved case. `--body ''` sends an empty
body, and whitespace-only overrides are preserved exactly. Omitting `--body`
inherits the saved/default request body. Replay returns a forward ID; inspect it with `hooklistener endpoint forward <forward-id>`. Named suite
creation and membership management remain available through the service/MCP.
All case commands support `--json` and an `--org` override on the leaf command.
Case metadata can contain sensitive request overrides; review it before sharing
output, and avoid putting real secrets in shell arguments or history.

### Preview and wait safely

```bash
hooklistener cases run <endpoint-id> --target cli --dry-run --json
hooklistener cases replay <case-id> --target cli --dry-run --json
```

These call dedicated **server preview** routes. The server resolves case/suite
scope and saved targets, applies public-URL policy, and reports the current CLI
listener count. Previews create no run, forward, job, or idempotency receipt.
JSON uses `status: "preview"` and `$schema: "hooklistener.cases.preview/1"`.
A preview is a snapshot, not a reservation or delivery test: DNS, listener
availability, saved configuration, and delivery outcomes can change afterward.

Run and replay use server-enforced idempotency. Supply a stable key when an agent
or job may need to recover a submission:

```bash
hooklistener cases run <endpoint-id> --target cli --idempotency-key build-482-smoke --wait --json
hooklistener cases replay <case-id> --target cli --idempotency-key incident-732-replay --json
```

Without `--idempotency-key`, the CLI generates a UUID. It prints the key to
**stderr before submission**, including in JSON mode, and includes the receipt's
`idempotency` metadata in successful/failed-run output. Stdout remains one JSON
document. Keys must be 8–200 printable ASCII bytes without spaces; do not use
secrets as keys. Preview mode does not accept a key.

The CLI never automatically retries delivery POSTs. If a response is lost,
repeat the command with the **same key, organization, resource, and delivery
inputs** to recover its original receipt without queueing it again. Wait options
are client-side and may change. Different delivery inputs with an existing key
return HTTP 409; use a new key only for an intentional new delivery. Keys are
scoped by organization and operation, separately from MCP and legacy HTTP calls.
Idempotency protects submission, not exactly-once delivery or worker retries.
Even an all-queue-failed suite run retains its receipt; repeating that key returns
the failed run, not a new attempt. Inspect current outcomes with `cases runs show`
or `cases runs wait`, since a recovered submission receipt is not live run state.

These commands require backend **case actions v1** (`.../run/preview`,
`.../run/execute`, `.../replay/preview`, `.../replay/execute`). Deploy the backend
first. An older server fails closed: the CLI never falls back to legacy delivery
routes or a weaker client preview. Released CLIs can still use the legacy routes.

`cases run --wait` queues once, then polls the run ID. `cases runs wait` only
observes an existing run. Waits default to 30 seconds, accept up to 1 hour, and
support `--interval-ms` from 100 to 30000 (default 250). The wait budget starts
after the submission receipt; the submission has its own HTTP timeout. A zero
wait performs no further polling (waiting on an existing ID still performs one
bounded GET). Timeout or interruption stops observation, **not delivery**; resume
with `cases runs wait <run-id>` rather than starting another run.

Run output keeps delivery and assertion outcomes separate. `pending` means
accepted, not passed; `completed` may have no assertions; `not_configured_count`
counts unasserted deliveries. Failed assertions, execution failures, unknown
result statuses, and wait timeouts exit with status 1. Completed unasserted runs
exit 0 but are not labeled as passing tests. Run JSON is emitted even for failed
results; command/transport errors use the standard CLI error envelope.

Use `hooklistener <command> --help` for every option and subcommand.

## Command overview

| Command | Purpose |
| --- | --- |
| `login`, `logout` | Manage the authenticated session |
| `org`, `config` | Select an organization and inspect local configuration |
| `listen` | Stream an existing endpoint and forward events to a local URL |
| `tunnel`, `static-tunnel` | Expose localhost and manage reserved tunnel slugs |
| `endpoint`, `cases` | Manage captures, requests, forwards, and saved replay cases |
| `anon` | Create and inspect temporary anonymous endpoints |
| `share` | Create, inspect, and revoke public request links |
| `monitor` | Manage uptime monitors and their checks |
| `diagnostics`, `clean-logs` | Collect support information and remove old logs |
| `completions`, `update` | Generate shell completions and update direct binary installs |

Run `hooklistener --help` for the complete command list.

## Automation

Most non-interactive commands support `--json` for scripts, agents, and CI:

```bash
hooklistener --json org list
hooklistener --json endpoint request <endpoint-id> <request-id>
hooklistener --json endpoint forward-request \
  <endpoint-id> <request-id> http://localhost:3000/webhooks --dry-run
```

Long-running `listen --json` and `tunnel --json` commands emit newline-delimited JSON. Each line is a receipt or event, so consumers can process connection state, captured request URIs, and forwarding outcomes as they happen.

```bash
hooklistener --json listen <endpoint-slug>
hooklistener --json tunnel --port 3000
hooklistener --json anon tunnel --port 3000 --ttl 900
hooklistener --json tunnel events --cursor <cursor> --follow
```

Tunnel delivery is independent from terminal rendering. If a slow or stalled consumer fills the presentation queue, the CLI continues reading requests and returning local responses, then emits a recoverable `stream_gap` event with the number of presentation events omitted. Treat the displayed request history as incomplete after a gap; delivery itself is unaffected.

The relay runtime admits at most 8 local requests and 64 MiB of retained request bodies at once. Inbound streams, buffered local responses, and the serialized WebSocket writer have separate count and byte ceilings. Requests rejected before the local connection begins report `known_not_executed`; cancellations or failures after forwarding begins report `outcome_unknown`.

Tunnel lifecycle receipts use `hooklistener.tunnel.receipt/1`; durable events use `hooklistener.tunnel.event/1` and include an event ID, journal position, sequence, opaque resume cursor, and canonical resource links. `--json` is noninteractive and writes only NDJSON to stdout. Persist event cursors and, after a `cursor_expired` error, rehydrate from the supplied resource links before resuming at the earliest cursor. If no earliest cursor is supplied, resync the resources and request a fresh cursor.

Runtime errors use a consistent envelope:

```json
{"$schema":"hooklistener.cli.error/1","schema_version":1,"type":"error","error":{"causes":[],"code":"command_failed","hint":null,"message":"..."},"ok":false}
```

Exit status `0` means success, `1` means a runtime failure, `2` means command-line parsing failed, `3` means the tunnel schema major is incompatible, and `4` means an event cursor expired. Destructive commands in scripts require `--yes`. `login` and `completions` do not support JSON output.

## Terminal behavior

Human output adapts to the terminal. Styling is disabled when output is redirected, when `NO_COLOR` is set, or when you pass `--color never`.

```bash
hooklistener --color never endpoint list
NO_COLOR=1 hooklistener monitor list
```

Live views are keyboard operated and show the available shortcuts in their status bar. Generate completions with `hooklistener completions <shell>`; accepted shell names are `bash`, `zsh`, `fish`, `power-shell`, and `elvish`.

## Configuration

Hooklistener stores its configuration and logs in the standard configuration directory for your operating system. On Linux, the default locations are:

```text
~/.config/hooklistener/config.json
~/.config/hooklistener/logs
```

Inspect the active configuration with:

```bash
hooklistener config show
```

Advanced and self-hosted setups can override the service URLs with `HOOKLISTENER_API_URL`, `HOOKLISTENER_WS_URL`, and `HOOKLISTENER_DEVICE_PORTAL_URL`. Hooklistener API and relay overrides require `https://` and `wss://` respectively. Cleartext `http://` and `ws://` are accepted automatically only for loopback development servers such as `localhost` or `127.0.0.1`.

For an isolated development environment that cannot use TLS on a non-loopback host, opt in explicitly with `--allow-insecure-dev-server` or `HOOKLISTENER_ALLOW_INSECURE_DEV_SERVER=1`. The CLI prints a warning because this exposes credentials and relay tickets to interception; never use this opt-in for production services.

## Documentation

- [Hooklistener documentation](https://docs.hooklistener.com)
- [CLI releases](https://github.com/hooklistener/hooklistener-cli/releases)
- [Issue tracker](https://github.com/hooklistener/hooklistener-cli/issues)

## Development

Official release, CI, and cross-repository qualification builds are pinned to
Rust 1.92.0. Install the repository toolchain with `mise install`, or select
Rust 1.92.0 explicitly before building with Cargo.

```bash
git clone https://github.com/hooklistener/hooklistener-cli.git
cd hooklistener-cli
cargo build
cargo test
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the complete development workflow and contribution guidelines.

## License

MIT License. See [LICENSE](LICENSE).
