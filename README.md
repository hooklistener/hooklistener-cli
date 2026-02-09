# Hooklistener CLI

[![CI](https://github.com/hooklistener/hooklistener-cli/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/hooklistener/hooklistener-cli/actions/workflows/ci.yml)
[![CodeQL](https://github.com/hooklistener/hooklistener-cli/actions/workflows/codeql.yml/badge.svg?branch=main)](https://github.com/hooklistener/hooklistener-cli/actions/workflows/codeql.yml)
[![Release](https://img.shields.io/github/v/release/hooklistener/hooklistener-cli?sort=semver)](https://github.com/hooklistener/hooklistener-cli/releases)
[![crates.io](https://img.shields.io/crates/v/hooklistener-cli.svg)](https://crates.io/crates/hooklistener-cli)
[![npm](https://img.shields.io/npm/v/hooklistener.svg)](https://www.npmjs.com/package/hooklistener)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

A fast, terminal-based CLI for browsing webhooks, forwarding events, and exposing local servers using [Hooklistener](https://hooklistener.com). Built with Rust for a responsive interactive terminal experience.

![Hooklistener CLI Demo](docs/images/hooklistener-cli.gif)

## Features

- 🚀 **Fast & Lightweight** - Built in Rust for maximum performance
- 🖥️ **Interactive Terminal Experience** - Browse requests with keyboard shortcuts
- 🔄 **Real-time Forwarding** - Stream webhook requests from existing endpoints to your local server
- 🚇 **HTTP Tunneling** - Expose your local server to the internet with a public URL (like ngrok)
- 🔍 **Search & Filter** - Quickly find specific requests
- 📋 **Request Details** - View headers, body, and metadata
- 🔐 **Secure OAuth** - Device code authentication flow (no API keys needed)
- 📊 **Professional Logging** - Structured logging with file rotation and diagnostics

## Installation

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

Install with a single command:

#### macOS / Linux
```bash
curl -fsSL https://raw.githubusercontent.com/hooklistener/hooklistener-cli/main/scripts/install.sh | sh
```

#### Windows (PowerShell)
```powershell
irm https://raw.githubusercontent.com/hooklistener/hooklistener-cli/main/scripts/install.ps1 | iex
```

### Manual Installation

Download and extract the binary for your platform from the [Releases page](https://github.com/hooklistener/hooklistener-cli/releases).

### Install System-Wide (Optional)

After downloading, move the binary to your PATH:

```bash
# macOS/Linux
sudo mv hooklistener /usr/local/bin/hooklistener
```

Now you can run `hooklistener` from anywhere.

## Quick Start

1. **Install** (see above).
2. **Authenticate**:
   ```bash
   hooklistener login
   ```
   Follow the on-screen instructions to authorize the device.

3. **Choose your mode**:
   - **Forward & Inspect Webhooks**: Run `hooklistener listen <endpoint-slug>` to forward events from an existing endpoint.
   - **Expose Local Server**: Run `hooklistener tunnel` to get a public URL for your local app.

## Usage

### Authentication
Authenticate securely via the device flow. This is required for all operations.

```bash
# Authenticate
hooklistener login

# Force re-authentication
hooklistener login --force

# Log out
hooklistener logout
```

### Organization Selection
Most API-backed commands use a selected organization. Set it once and reuse it.

```bash
# List organizations available to your account
hooklistener org list

# Set default organization for commands that require one
hooklistener org use <organization-id>

# Clear default organization
hooklistener org clear
```

### Forwarding Webhooks (`listen`)
Use this when you have an **existing Hooklistener Endpoint** and want to debug webhooks locally. It forwards requests sent to that endpoint to your localhost and shows live request details in the terminal.

```bash
# Forward webhooks from 'my-endpoint' to http://localhost:3000 (default)
hooklistener listen my-endpoint

# Forward to a custom local URL
hooklistener listen my-endpoint --target http://localhost:8080
```

### Endpoint Discovery (`endpoint`)
Manage endpoints and captured requests from the CLI.

```bash
# Create/list/show/delete endpoints
hooklistener endpoint create "Billing Webhooks" --slug billing-webhooks
hooklistener endpoint list
hooklistener endpoint show <endpoint-id>
hooklistener endpoint delete <endpoint-id>

# Override organization for a single command
hooklistener endpoint list --org <organization-id>

# List captured requests for an endpoint
hooklistener endpoint requests <endpoint-id> --page 1 --page-size 50

# Show/delete a single request
hooklistener endpoint request <endpoint-id> <request-id>
hooklistener endpoint delete-request <endpoint-id> <request-id>

# Replay and inspect replay attempts
hooklistener endpoint forward-request <endpoint-id> <request-id> http://localhost:3000/webhook
hooklistener endpoint forwards <endpoint-id> <request-id>
hooklistener endpoint forward <forward-id>
```

### Exposing Local Server (`tunnel`)
Use this to create a **public URL** that tunnels traffic to your local machine. Great for receiving webhooks from third-party services directly to your dev environment.

```bash
# Start a tunnel to port 3000 (default)
hooklistener tunnel

# Tunnel to a specific port
hooklistener tunnel --port 8080

# Tunnel to a specific host and port
hooklistener tunnel --host 127.0.0.1 --port 5000

# Use a persistent subdomain (Paid plans)
hooklistener tunnel --slug my-cool-app
```

### Static Tunnel Slugs (`static-tunnel`)
Manage reserved static slugs used by `hooklistener tunnel --slug`.

```bash
# List static tunnel slugs
hooklistener static-tunnel list

# Create a static slug
hooklistener static-tunnel create my-cool-app --name "Local App"

# Delete a static slug by ID
hooklistener static-tunnel delete <slug-id>
```

### Maintenance & Diagnostics

```bash
# Generate a diagnostic bundle for support/debugging
hooklistener diagnostics --output ./debug-bundle

# Clean up old log files
hooklistener clean-logs --keep 5

# Show help
hooklistener --help
```

### Automation & Completions

```bash
# Machine-readable output for non-interactive commands
hooklistener --json org list
hooklistener --json endpoint list

# Generate shell completions
hooklistener completions bash > ~/.local/share/bash-completion/completions/hooklistener
hooklistener completions zsh > ~/.zfunc/_hooklistener
```

## Configuration

Configuration is stored in `~/.config/hooklistener/config.json`. The CLI handles token management automatically.

### Environment Variables

For advanced usage or self-hosting:

- `HOOKLISTENER_API_URL`: Base URL for API requests.
- `HOOKLISTENER_WS_URL`: Base URL for WebSocket connections.
- `HOOKLISTENER_DEVICE_PORTAL_URL`: URL for device activation.

## Development

### Prerequisites
- Rust 1.75+
- Cargo

### Building

```bash
git clone https://github.com/hooklistener/hooklistener-cli.git
cd hooklistener-cli

# Run locally
cargo run -- listen my-endpoint

# Build release
cargo build --release
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## License

MIT License - see [LICENSE](LICENSE).
