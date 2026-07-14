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

### Try it without logging in

Create a temporary anonymous endpoint:

```bash
hooklistener anon create --ttl 3600
```

The result includes the endpoint URL, endpoint ID, and viewer token needed to inspect captured events.

## Choose the right workflow

| Goal | Command | Use it when |
| --- | --- | --- |
| Forward an existing Hooklistener endpoint | `hooklistener listen` | Events already arrive at Hooklistener and should be forwarded to your local app |
| Expose a local server | `hooklistener tunnel` | A provider needs a public URL that points directly to localhost |
| Inspect and replay captured requests | `hooklistener endpoint` | You need stored payloads, headers, and forwarding history |
| Run saved endpoint cases | `hooklistener cases` | You want to replay a repeatable test suite against a URL or saved target |
| Create a temporary endpoint | `hooklistener anon` | You need a short-lived capture URL without an account |
| Share a captured request | `hooklistener share` | A teammate needs access to a payload and its forwarding history |
| Check an HTTP endpoint | `hooklistener monitor` | You need recurring uptime checks and failure visibility |

Reserved static tunnel slugs depend on your Hooklistener plan.

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
```

Tunnel delivery is independent from terminal rendering. If a slow or stalled consumer fills the presentation queue, the CLI continues reading requests and returning local responses, then emits a recoverable `stream_gap` event with the number of presentation events omitted. Treat the displayed request history as incomplete after a gap; delivery itself is unaffected.

The relay runtime admits at most 8 local requests and 64 MiB of retained request bodies at once. Inbound streams, buffered local responses, and the serialized WebSocket writer have separate count and byte ceilings. Requests rejected before the local connection begins report `known_not_executed`; cancellations or failures after forwarding begins report `outcome_unknown`.

Runtime errors use a consistent envelope:

```json
{"error":{"causes":[],"code":"command_failed","hint":null,"message":"..."},"ok":false}
```

Exit status `0` means success, `1` means a runtime failure, and `2` means command-line parsing failed. Destructive commands in scripts require `--yes`. `login` and `completions` do not support JSON output.

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

Advanced and self-hosted setups can override the service URLs with `HOOKLISTENER_API_URL`, `HOOKLISTENER_WS_URL`, and `HOOKLISTENER_DEVICE_PORTAL_URL`.

## Documentation

- [Hooklistener documentation](https://docs.hooklistener.com)
- [CLI releases](https://github.com/hooklistener/hooklistener-cli/releases)
- [Issue tracker](https://github.com/hooklistener/hooklistener-cli/issues)

## Development

Building from source requires Rust 1.85 or later and Cargo.

```bash
git clone https://github.com/hooklistener/hooklistener-cli.git
cd hooklistener-cli
cargo build
cargo test
```

See [CONTRIBUTING.md](CONTRIBUTING.md) for the complete development workflow and contribution guidelines.

## License

MIT License. See [LICENSE](LICENSE).
