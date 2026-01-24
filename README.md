# Hooklistener CLI

[![CI](https://github.com/hooklistener/hooklistener-cli/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/hooklistener/hooklistener-cli/actions/workflows/ci.yml)
[![CodeQL](https://github.com/hooklistener/hooklistener-cli/actions/workflows/codeql.yml/badge.svg?branch=main)](https://github.com/hooklistener/hooklistener-cli/actions/workflows/codeql.yml)
[![Release](https://img.shields.io/github/v/release/hooklistener/hooklistener-cli?sort=semver)](https://github.com/hooklistener/hooklistener-cli/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

A fast, terminal-based CLI for browsing webhooks, forwarding events, and exposing local servers using [Hooklistener](https://hooklistener.com). Built with Rust and Ratatui for a smooth, responsive TUI experience.

![Hooklistener CLI Demo](docs/images/hooklistener-cli.gif)

## Features

- 🚀 **Fast & Lightweight** - Built in Rust for maximum performance
- 🖥️ **Terminal UI** - Browse requests with a keyboard-driven interface
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
sudo mv hooklistener-cli /usr/local/bin/hooklistener
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
   - **Browse Webhooks**: Run `hooklistener` (or `hooklistener tui`) to view requests.
   - **Forward Webhooks**: Run `hooklistener listen <endpoint-slug>` to forward events from an existing endpoint.
   - **Expose Local Server**: Run `hooklistener tunnel` to get a public URL for your local app.

## Usage

### Authentication
Authenticate securely via the device flow. This is required for all operations.

```bash
# Authenticate (opens browser if needed)
hooklistener login

# Force re-authentication
hooklistener login --force
```

### Browsing Webhooks (TUI)
Launch the interactive Terminal User Interface to browse, search, and inspect webhook requests.

```bash
# Launch TUI (default command)
hooklistener tui
# or simply
hooklistener
```

**Keyboard Shortcuts:**
| Key | Action |
|-----|--------|
| `↑`/`k` | Move up |
| `↓`/`j` | Move down |
| `Enter` | View request details |
| `/` | Search requests |
| `f` | Toggle filters |
| `r` | Refresh |
| `q` | Quit |
| `?` | Show help |

### Forwarding Webhooks (`listen`)
Use this when you have an **existing Hooklistener Endpoint** and want to debug webhooks locally. It forwards requests sent to that endpoint to your localhost.

```bash
# Forward webhooks from 'my-endpoint' to http://localhost:3000 (default)
hooklistener listen my-endpoint

# Forward to a custom local URL
hooklistener listen my-endpoint --target http://localhost:8080
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

### Maintenance & Diagnostics

```bash
# Generate a diagnostic bundle for support/debugging
hooklistener diagnostics --output ./debug-bundle

# Clean up old log files
hooklistener clean-logs --keep 5

# Show help
hooklistener --help
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
cargo run -- tui

# Build release
cargo build --release
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## License

MIT License - see [LICENSE](LICENSE).