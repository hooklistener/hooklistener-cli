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
hooklistener anon create --ttl 1h
```

Duration flags accept a bare number of seconds (`--ttl 3600`) or a unit suffix (`500ms`, `30s`, `10m`, `1h`, `7d`). The result includes the endpoint URL, endpoint ID, and viewer token needed to inspect captured events.

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

`hooklistener tunnel --port 3000` remains an alias for `tunnel start`. Target flags given before a subcommand set defaults; the same flag on the subcommand overrides them. Every lifecycle command negotiates the authenticated tunnel contract first; an incompatible schema major fails before relay activation.

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
hooklistener endpoint list-requests <endpoint-id>
hooklistener endpoint show-request <endpoint-id> <request-id>
hooklistener endpoint forward-request \
  <endpoint-id> <request-id> http://localhost:3000/webhooks
```

Run every saved case for an endpoint and wait for the result:

```bash
hooklistener cases run <endpoint-id> \
  --target http://localhost:3000/webhooks \
  --wait --timeout 60s
```

Use `hooklistener <command> --help` for every option and subcommand.

## Command overview

| Command | Purpose |
| --- | --- |
| `listen` | Forward events from a debug endpoint to a local URL |
| `tunnel`, `static-tunnel` | Expose a local HTTP server on a public URL and reserve static tunnel slugs |
| `endpoint`, `cases` | Manage debug endpoints and captured requests, and run saved replay cases |
| `anon` | Create temporary endpoints and tunnels without signing in |
| `share` | Create, inspect, and revoke public links to captured requests |
| `monitor` | Create and inspect uptime monitors |
| `login`, `logout` | Sign in with the device flow and sign out |
| `org`, `config` | Set the default organization and show or set CLI configuration |
| `diagnostics`, `clean-logs` | Write a diagnostic bundle for support and delete old log files |
| `completions`, `update` | Print a shell completion script and update the binary |

Run `hooklistener --help` for the complete command list.

## Automation

Most non-interactive commands support `--json` for scripts, agents, and CI:

```bash
hooklistener --json org list
hooklistener --json endpoint show-request <endpoint-id> <request-id>
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

`--json`, `--color`, `--yes`, `--log-level`, `--log-dir`, and `--log-stdout` are global flags and are accepted before or after any subcommand, so `hooklistener tunnel --log-level debug prepare` and `hooklistener --log-level debug tunnel prepare` are equivalent.

### Migration

The following spellings still parse and behave as before. The `endpoint` and `anon` list/show names remain listed as aliases in `--help`; every other entry is hidden from `--help`. Scripts should move to the replacement; the `-ms` and `-hours` flags print a one-line deprecation warning on stderr and leave stdout untouched.

| Hidden spelling | Replacement |
| --- | --- |
| `endpoint requests` | `endpoint list-requests` |
| `endpoint request` | `endpoint show-request` |
| `endpoint forwards` | `endpoint list-forwards` |
| `endpoint forward` | `endpoint show-forward` |
| `anon events` | `anon list-events` |
| `anon event` | `anon show-event` |
| `tunnel activate` | `tunnel start` |
| `completions power-shell` | `completions powershell` |
| `cases run --target-url <url>` | `cases run --target <url>` |
| `cases run --target-id <target-id>` | `cases run --target <target-id>` |
| `cases run --timeout-ms <ms>` | `cases run --timeout <duration>`, for example `--timeout 1500ms` |
| `cases run --interval-ms <ms>` | `cases run --interval <duration>`, for example `--interval 500ms` |
| `tunnel events --interval-ms <ms>` | `tunnel events --interval <duration>`, for example `--interval 250ms` |
| `share create --expires-in-hours <hours>` | `share create --expires-in <duration>`, for example `--expires-in 24h` |
| `monitor create --email <true\|false>` | `monitor create --no-email` to disable notifications; omit the flag to keep them enabled |
| `monitor update --enabled <true\|false>` | `monitor update --enable` or `monitor update --disable` |

## Terminal behavior

Human output adapts to the terminal. Styling is disabled when output is redirected, when `NO_COLOR` is set, or when you pass `--color never`.

```bash
hooklistener --color never endpoint list
NO_COLOR=1 hooklistener monitor list
```

Live views are keyboard operated and show the available shortcuts in their status bar. Generate completions with `hooklistener completions <shell>`; accepted shell names are `bash`, `zsh`, `fish`, `powershell`, and `elvish`.

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
