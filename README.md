# Hooklistener CLI

[![CI](https://github.com/hooklistener/hooklistener-cli/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/hooklistener/hooklistener-cli/actions/workflows/ci.yml)
[![CodeQL](https://github.com/hooklistener/hooklistener-cli/actions/workflows/codeql.yml/badge.svg?branch=main)](https://github.com/hooklistener/hooklistener-cli/actions/workflows/codeql.yml)
[![Release](https://img.shields.io/github/v/release/hooklistener/hooklistener-cli?sort=semver)](https://github.com/hooklistener/hooklistener-cli/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

A fast, terminal-based CLI for browsing and forwarding webhook requests from [Hooklistener](https://hooklistener.com) directly in your terminal. Built with Rust and Ratatui for a smooth, responsive TUI experience.

![Hooklistener CLI Demo](docs/images/hooklistener-cli.gif)

## Features

- 🚀 **Fast & Lightweight** - Built in Rust for maximum performance
- 🖥️ **Terminal UI** - Browse requests with a keyboard-driven interface
- 🔄 **Real-time Updates** - Stream webhook requests as they arrive
- 🔍 **Search & Filter** - Quickly find specific requests
- 📋 **Request Details** - View headers, body, and metadata
- 🔐 **Secure OAuth** - Device code authentication flow (no API keys needed)
- 📊 **Professional Logging** - Structured logging with file rotation and diagnostics
- 🛠️ **Debug Tools** - Built-in diagnostic bundle generation for troubleshooting
- 🎨 **Customizable** - Configure display preferences and shortcuts

## Installation

### Quick Install (Recommended)

Download and extract the binary for your platform:

#### macOS (Apple Silicon M1/M2/M3)
```bash
curl -L https://github.com/hooklistener/hooklistener-cli/releases/latest/download/hooklistener-cli-aarch64-apple-darwin.tar.gz -o hooklistener.tar.gz
tar -xzf hooklistener.tar.gz
./hooklistener-cli tui
```

#### macOS (Intel)
```bash
curl -L https://github.com/hooklistener/hooklistener-cli/releases/latest/download/hooklistener-cli-x86_64-apple-darwin.tar.gz -o hooklistener.tar.gz
tar -xzf hooklistener.tar.gz
./hooklistener-cli tui
```

#### Linux (x86_64)
```bash
curl -L https://github.com/hooklistener/hooklistener-cli/releases/latest/download/hooklistener-cli-x86_64-unknown-linux-gnu.tar.gz -o hooklistener.tar.gz
tar -xzf hooklistener.tar.gz
./hooklistener-cli tui
```

#### Windows
```bash
# Download and extract the ZIP file
curl -L https://github.com/hooklistener/hooklistener-cli/releases/latest/download/hooklistener-cli.exe-x86_64-pc-windows-msvc.zip -o hooklistener.zip
# Extract using Windows Explorer or PowerShell:
Expand-Archive -Path hooklistener.zip -DestinationPath .
.\hooklistener-cli.exe tui
```

### Install System-Wide (Optional)

After downloading and extracting, you can move the binary to make it available from anywhere:

```bash
# macOS/Linux
sudo mv hooklistener-cli /usr/local/bin/
# Rename for convenience (optional)
sudo mv /usr/local/bin/hooklistener-cli /usr/local/bin/hooklistener

# Now you can run from anywhere:
hooklistener tui
# Run `hooklistener` with no command to view the help/command list
```

### Alternative Installation Methods

#### Using Cargo
```bash
# Install from GitHub (requires Rust toolchain)
cargo install --git https://github.com/hooklistener/hooklistener-cli
```

#### Build from Source
```bash
git clone https://github.com/hooklistener/hooklistener-cli.git
cd hooklistener-cli
cargo build --release
./target/release/hooklistener-cli tui
```

### Coming Soon

- 🍺 Homebrew tap
- 📦 cargo-dist installers (shell/powershell)
- 🐳 Docker image

## Quick Start

1. **Download and run** (takes less than 30 seconds):
   ```bash
   # Example for macOS Apple Silicon
   curl -L https://github.com/hooklistener/hooklistener-cli/releases/latest/download/hooklistener-cli-aarch64-apple-darwin.tar.gz -o hooklistener.tar.gz
   tar -xzf hooklistener.tar.gz
   ./hooklistener-cli tui
   ```

2. **Authenticate** (one-time setup):
   - The CLI will display a verification code and URL
   - Visit the URL in your browser and enter the code
   - Once authenticated, you're ready to go!
   - You can also run `hooklistener login` any time to refresh your session without opening the TUI

3. **Start receiving webhooks** - The terminal UI will show incoming requests in real-time

## Usage

### Basic Commands

```bash
# Show global help (also runs when no command is provided)
hooklistener

# Launch the TUI to browse requests
hooklistener tui

# Authenticate via the device flow without opening the TUI
hooklistener login

# Force re-authentication if you need to switch accounts
hooklistener login --force

# Listen to a specific endpoint and forward traffic to your local server
hooklistener listen my-endpoint --target http://localhost:3000

# Generate diagnostic bundle for troubleshooting
hooklistener diagnostics --output ./diagnostics

# Clean up old log files
hooklistener clean-logs --keep 5

# Show help
hooklistener --help

# Advanced logging options
hooklistener --log-level debug --log-stdout
```

### Keyboard Shortcuts

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

## Configuration

Configuration is automatically managed and stored in `~/.config/hooklistener/config.json`. The config file contains:

```json
{
  "access_token": "your_oauth_token_here",
  "token_expires_at": "2024-12-31T23:59:59Z",
  "selected_organization_id": "org_123456789"
}
```

- **access_token**: OAuth access token obtained through device code flow
- **token_expires_at**: Token expiration timestamp
- **selected_organization_id**: Last selected organization (for faster startup)

The CLI automatically handles token refresh and manages this configuration for you.

### Environment Variables

For local development or testing against a custom backend, you can override the default API and WebSocket URLs using environment variables:

- `HOOKLISTENER_API_URL`: Sets the base URL for API requests.
- `HOOKLISTENER_WS_URL`: Sets the base URL for WebSocket connections.
- `HOOKLISTENER_DEVICE_PORTAL_URL`: Overrides the browser URL shown during `hooklistener login` (defaults to `https://app.hooklistener.com/device-codes`).

**Example:**

```bash
HOOKLISTENER_API_URL="http://localhost:4000" HOOKLISTENER_WS_URL="ws://localhost:4000" hooklistener tui
```

## Development

### Prerequisites

- Rust 1.75 or later
- Cargo

### Building

```bash
# Clone the repository
git clone https://github.com/hooklistener/hooklistener-cli.git
cd hooklistener-cli

# Build debug version
cargo build

# Build release version
cargo build --release

# Run directly
cargo run -- tui
```

### Testing & Quality

```bash
# Run tests
cargo test

# Check code without building
cargo check

# Format code
cargo fmt

# Run linter
cargo clippy

# Check for security vulnerabilities (requires cargo-audit)
cargo audit
```

## Release Process

Releases are automated via GitHub Actions:

1. Tag a version: `git tag v0.1.0`
2. Push the tag: `git push origin v0.1.0`
3. CI builds and publishes binaries for:
   - Linux (x86_64, aarch64)
   - macOS (Intel, Apple Silicon)
   - Windows (x86_64)

## Troubleshooting

### Common Issues

#### "Permission denied" on macOS/Linux
```bash
# Make the binary executable
chmod +x hooklistener-cli
```

#### "Cannot be opened because it is from an unidentified developer" on macOS
```bash
# Remove quarantine attribute
xattr -d com.apple.quarantine hooklistener-cli

# Or allow in System Settings > Privacy & Security
```

#### "command not found" after installation
```bash
# Check if the binary is in your PATH
which hooklistener-cli

# If not, add to PATH or use full path:
./hooklistener-cli tui

# Or move to a directory in PATH:
sudo mv hooklistener-cli /usr/local/bin/
```

#### "Device not configured" error
This usually means the terminal is not properly configured. Try:
- Running in a different terminal emulator
- Ensuring your terminal supports TUI applications
- Checking that your `TERM` environment variable is set correctly

## Contributing

We welcome contributions! Please see [CONTRIBUTING.md](CONTRIBUTING.md) for guidelines.

## Security

For security issues, please see [SECURITY.md](SECURITY.md).

## License

MIT License - see [LICENSE](LICENSE) for details.

## Support

- 📖 [Documentation](https://github.com/hooklistener/hooklistener-cli/wiki)
- 🐛 [Issue Tracker](https://github.com/hooklistener/hooklistener-cli/issues)
- 💬 [Discussions](https://github.com/hooklistener/hooklistener-cli/discussions)

## Acknowledgments

Built with:
- [Ratatui](https://github.com/ratatui-org/ratatui) - Terminal UI framework
- [Tokio](https://tokio.rs) - Async runtime
- [Serde](https://serde.rs) - Serialization framework
- [Reqwest](https://github.com/seanmonstar/reqwest) - HTTP client
- [Tracing](https://github.com/tokio-rs/tracing) - Structured logging
- [Clap](https://github.com/clap-rs/clap) - Command line argument parsing
- [Crossterm](https://github.com/crossterm-rs/crossterm) - Cross-platform terminal manipulation
