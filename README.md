# Hooklistener CLI

[![CI](https://github.com/hooklistener/hooklistener-cli/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/hooklistener/hooklistener-cli/actions/workflows/ci.yml)
[![CodeQL](https://github.com/hooklistener/hooklistener-cli/actions/workflows/codeql.yml/badge.svg?branch=main)](https://github.com/hooklistener/hooklistener-cli/actions/workflows/codeql.yml)
[![Release](https://img.shields.io/github/v/release/hooklistener/hooklistener-cli?sort=semver)](https://github.com/hooklistener/hooklistener-cli/releases)
[![crates.io](https://img.shields.io/crates/v/hooklistener-cli.svg)](https://crates.io/crates/hooklistener-cli)
[![npm](https://img.shields.io/npm/v/hooklistener.svg)](https://www.npmjs.com/package/hooklistener)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

Debug, forward, share, and monitor webhooks from your terminal.

Hooklistener CLI is a Rust-based command-line tool for developers who need to inspect inbound webhooks, replay requests to local services, expose localhost with public URLs, and manage Hooklistener resources without leaving the terminal. It combines an interactive terminal UI with automation-friendly commands for endpoints, tunnels, request sharing, uptime monitoring, diagnostics, and shell integration.

![Hooklistener CLI Demo](docs/images/hooklistener-cli.gif)

## Why Hooklistener CLI

- Inspect webhook traffic in a fast, keyboard-first terminal interface.
- Forward requests from existing Hooklistener endpoints straight to your local app.
- Expose a local service with a public tunnel when an external provider needs a callback URL.
- Replay captured requests, inspect forward attempts, and share payloads with teammates.
- Create temporary anonymous endpoints when you need quick testing without login.
- Manage organizations, endpoints, tunnels, shares, and monitors with scriptable commands and JSON output.

## Use Cases

| Use case | Command | Best when |
| --- | --- | --- |
| Forward webhook traffic to localhost | `hooklistener listen` | You already have a Hooklistener endpoint receiving real events |
| Expose a local app publicly | `hooklistener tunnel` | A third-party service needs to call your machine directly |
| Manage webhook endpoints and captured requests | `hooklistener endpoint` | You want to create endpoints, inspect payloads, and replay traffic |
| Reserve a stable tunnel subdomain | `hooklistener static-tunnel` | You need a persistent public URL for a development workflow |
| Create a temporary endpoint without login | `hooklistener anon` | You want a short-lived, low-friction test endpoint |
| Share a request with teammates | `hooklistener share` | You need to send a captured payload or replay history for review |
| Monitor endpoint uptime | `hooklistener monitor` | You want recurring checks and failure visibility for webhook URLs |

## Installation

Choose the install method that fits your environment.

### Homebrew (macOS / Linux)

```bash
brew tap hooklistener/tap
brew install hooklistener
```

### npm

```bash
npm install -g hooklistener
```

### Cargo

```bash
cargo install hooklistener-cli
```

### Quick Install Script

macOS / Linux:

```bash
curl -fsSL https://raw.githubusercontent.com/hooklistener/hooklistener-cli/main/scripts/install.sh | sh
```

Windows PowerShell:

```powershell
irm https://raw.githubusercontent.com/hooklistener/hooklistener-cli/main/scripts/install.ps1 | iex
```

### Manual Binary Install

Download the appropriate archive from the [Releases page](https://github.com/hooklistener/hooklistener-cli/releases), extract it, and place the `hooklistener` binary somewhere on your `PATH`.

Example on macOS or Linux:

```bash
sudo mv hooklistener /usr/local/bin/hooklistener
```

Verify the installation:

```bash
hooklistener --version
```

## Quick Start

### Authenticated workflow

1. Sign in:

   ```bash
   hooklistener login
   ```

2. Pick the default organization for API-backed commands:

   ```bash
   hooklistener org list
   hooklistener org use <organization-id>
   ```

3. Create a debug endpoint and start forwarding traffic to your local app:

   ```bash
   hooklistener endpoint create "Stripe Sandbox" --slug stripe-sandbox
   hooklistener listen stripe-sandbox --target http://localhost:3000/webhooks/stripe
   ```

4. Inspect, replay, or list captured traffic:

   ```bash
   hooklistener endpoint requests <endpoint-id>
   hooklistener endpoint request <endpoint-id> <request-id>
   hooklistener endpoint forward-request <endpoint-id> <request-id> http://localhost:3000/webhooks/stripe
   ```

### No-login workflow

If you want to evaluate Hooklistener quickly or create a short-lived endpoint for temporary sharing, use anonymous endpoints:

```bash
hooklistener anon create --ttl 3600
```

The command returns the temporary endpoint details. Use the returned endpoint ID and viewer token with `hooklistener anon show`, `hooklistener anon events`, and `hooklistener anon event`.

## Core Workflows

### Authenticate and manage session state

Most commands that interact with your Hooklistener account require login. The CLI uses a secure device flow and stores session state locally.

```bash
hooklistener login
hooklistener login --force
hooklistener logout
```

### Select an organization

Most account-backed commands operate against a selected organization. Set it once, or override it per command with `--org`.

```bash
hooklistener org list
hooklistener org use <organization-id>
hooklistener org clear
```

You can inspect or set the same value directly through config:

```bash
hooklistener config show
hooklistener config set selected_organization_id <organization-id>
```

### Forward webhooks from an existing endpoint

Use `listen` when the external service is already posting to a Hooklistener endpoint and you want those requests forwarded into your local environment.

```bash
# Forward to the default local target
hooklistener listen my-endpoint

# Forward to a specific local route
hooklistener listen my-endpoint --target http://localhost:8080/webhooks

# Override the WebSocket endpoint for advanced or self-hosted setups
hooklistener listen my-endpoint --ws-url wss://your-instance.example.com/socket/websocket
```

This workflow is ideal when you want real inbound traffic plus an interactive terminal experience for inspecting headers, bodies, metadata, and replay results.

### Create and manage debug endpoints

Use `endpoint` to manage endpoints and the requests captured by them.

```bash
# Create and inspect endpoints
hooklistener endpoint create "Billing Webhooks" --slug billing-webhooks
hooklistener endpoint list
hooklistener endpoint show <endpoint-id>

# Browse captured requests
hooklistener endpoint requests <endpoint-id> --page 1 --page-size 50
hooklistener endpoint request <endpoint-id> <request-id>

# Replay a captured request to a target URL
hooklistener endpoint forward-request <endpoint-id> <request-id> http://localhost:3000/webhooks

# Review replay attempts
hooklistener endpoint forwards <endpoint-id> <request-id>
hooklistener endpoint forward <forward-id>

# Delete captured traffic or the endpoint itself
hooklistener endpoint delete-request <endpoint-id> <request-id>
hooklistener endpoint delete <endpoint-id>
```

### Expose a local server with a public tunnel

Use `tunnel` when a provider needs to reach your machine directly. Hooklistener creates a public URL and forwards traffic to your chosen host and port.

```bash
# Default: localhost:3000
hooklistener tunnel

# Forward to a different local port
hooklistener tunnel --port 8080

# Forward to a specific host and port
hooklistener tunnel --host 127.0.0.1 --port 5000

# Request a persistent slug for a reserved static tunnel
hooklistener tunnel --slug my-cool-app
```

`--slug` is intended for reserved static tunnel slugs and may depend on your Hooklistener plan.

### Reserve and manage static tunnel slugs

Static tunnel slugs let you request a stable public subdomain with `hooklistener tunnel --slug`.

```bash
hooklistener static-tunnel list
hooklistener static-tunnel create my-cool-app --name "Local App"
hooklistener static-tunnel delete <slug-id>
```

Static tunnel slugs are plan-gated. Document them in team workflows when you need a consistent callback URL for local development or demos.

### Create temporary anonymous endpoints

Anonymous endpoints are useful when you need a short-lived endpoint without authenticating first.

```bash
# Create a temporary endpoint with a one-hour TTL
hooklistener anon create --ttl 3600

# Check endpoint status
hooklistener anon show <endpoint-id>

# List or inspect captured events using the viewer token returned at creation time
hooklistener anon events <endpoint-id> --token <viewer-token>
hooklistener anon event <endpoint-id> <event-id> --token <viewer-token>
```

This is the fastest way to test a webhook payload, share a temporary endpoint, or validate a sender integration without setting up an account-backed workflow.

### Share a captured request

Use `share` to generate a public link for a captured request. Shares can have expirations, optional passwords, and optional replay history attached.

```bash
# Create a share link
hooklistener share create <debug-request-id> --expires-in-hours 24 --include-forwards

# Add password protection when needed
hooklistener share create <debug-request-id> --password my-secret

# List, view, and revoke shares
hooklistener share list <debug-request-id>
hooklistener share show <share-token>
hooklistener share revoke <share-token>
```

### Monitor an endpoint or webhook URL

Use `monitor` when you want recurring checks against an HTTP endpoint and a simple operational view from the CLI.

```bash
# Create a monitor
hooklistener monitor create "Production Webhook" https://example.com/webhook --interval 5 --expected-status 200

# Require a string in the response body
hooklistener monitor create "Healthcheck" https://example.com/health --body-contains ok

# Review and manage monitors
hooklistener monitor list
hooklistener monitor show <monitor-id>
hooklistener monitor checks <monitor-id>
hooklistener monitor update <monitor-id> --interval 10 --failure-threshold 3
hooklistener monitor delete <monitor-id>
```

## Automation and Shell Integration

Most non-interactive commands support `--json`, which makes the CLI useful in scripts, internal tooling, and CI jobs.

```bash
hooklistener --json org list
hooklistener --json endpoint list
hooklistener --json endpoint request <endpoint-id> <request-id>
hooklistener --json share list <debug-request-id>
hooklistener --json monitor list
```

Generate shell completions for your shell:

```bash
hooklistener completions bash > ~/.local/share/bash-completion/completions/hooklistener
hooklistener completions zsh > ~/.zfunc/_hooklistener
hooklistener completions fish > ~/.config/fish/completions/hooklistener.fish
```

Available completion targets are `bash`, `zsh`, `fish`, `power-shell`, and `elvish`.

## Configuration

The CLI stores configuration under your operating system's standard config directory.
On Linux, that is typically:

```text
~/.config/hooklistener/config.json
```

By default, logs are written under the same config root.
On Linux, that is typically:

```text
~/.config/hooklistener/logs
```

The config file stores items such as:

- Access and refresh token metadata
- Selected default organization
- Cached update-check information

The CLI manages tokens automatically. You generally only need to care about configuration when selecting an organization, overriding runtime settings, or debugging local issues.

### Environment variables

Use these variables for advanced setups, testing, or self-hosting:

- `HOOKLISTENER_API_URL`: Override the base HTTP API URL.
- `HOOKLISTENER_WS_URL`: Override the WebSocket base URL used by tunnels and listeners.
- `HOOKLISTENER_DEVICE_PORTAL_URL`: Override the device authentication portal URL.

### Logging options

All commands support these global logging flags:

- `--log-level <trace|debug|info|warn|error>`
- `--log-dir <path>`
- `--log-stdout`

## Diagnostics and Updates

Generate a diagnostic bundle for support or debugging:

```bash
hooklistener diagnostics --output ./debug-bundle
```

Clean up older log files:

```bash
hooklistener clean-logs --keep 5
```

Update behavior depends on how you installed the CLI:

```bash
# Direct binary installs
hooklistener update

# Homebrew installs
brew upgrade hooklistener

# npm installs
npm update -g hooklistener

# Cargo installs
cargo install hooklistener-cli
```

## FAQ

### Do I need to log in to use the CLI?

No. Most account-backed workflows require login, but `hooklistener anon ...` is designed for temporary anonymous endpoints and does not require authentication.

### What is the difference between `listen` and `tunnel`?

Use `listen` when Hooklistener is already receiving webhook traffic on one of your endpoints and you want those captured requests forwarded to your local app. Use `tunnel` when an external system needs a public URL that points directly at your machine.

### How do I choose the organization a command uses?

Set a default once with `hooklistener org use <organization-id>`, or pass `--org <organization-id>` to supported commands when you want to override it for a single invocation.

### Can I use Hooklistener CLI in scripts or CI?

Yes. Use `--json` with non-interactive commands to get machine-readable output. Interactive workflows such as live terminal views are better suited to local development sessions.

### Where are configuration and logs stored?

They are stored under your operating system's config directory. On Linux, that is typically `~/.config/hooklistener/config.json` for config and `~/.config/hooklistener/logs` for logs.

### Are static tunnel slugs available on every plan?

Not necessarily. Static tunnel slugs are plan-gated. If `hooklistener tunnel --slug ...` is part of your workflow, confirm that your Hooklistener plan includes reserved static tunnels.

### How can I share a webhook payload safely?

Use `hooklistener share create` with an expiration and, when needed, `--password`. Revoke it later with `hooklistener share revoke <share-token>`.

### How does `hooklistener update` work?

For direct binary installs, `hooklistener update` performs the self-update. If you installed through Homebrew, npm, or Cargo, use the corresponding package manager command instead.

## Development

### Prerequisites

- Rust 1.75+
- Cargo

### Build and run

```bash
git clone https://github.com/hooklistener/hooklistener-cli.git
cd hooklistener-cli

# Run locally
cargo run -- listen my-endpoint

# Build a release binary
cargo build --release
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for contribution guidelines.

## License

MIT License. See [LICENSE](LICENSE).
