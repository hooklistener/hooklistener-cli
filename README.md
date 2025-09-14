# Hooklistener CLI

[![CI](https://github.com/hooklistener/hooklistener-cli/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/hooklistener/hooklistener-cli/actions/workflows/ci.yml)
[![CodeQL](https://github.com/hooklistener/hooklistener-cli/actions/workflows/codeql.yml/badge.svg?branch=main)](https://github.com/hooklistener/hooklistener-cli/actions/workflows/codeql.yml)
[![Release](https://img.shields.io/github/v/release/hooklistener/hooklistener-cli?sort=semver)](https://github.com/hooklistener/hooklistener-cli/releases)
[![License: MIT](https://img.shields.io/badge/License-MIT-yellow.svg)](https://opensource.org/licenses/MIT)

A fast, terminal-based CLI for browsing and forwarding webhook requests from [Hooklistener](https://hooklistener.com) directly in your terminal. Built with Rust and Ratatui for a smooth, responsive TUI experience.

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

### From Binary (Recommended)

Download the latest prebuilt binary for your platform from the [Releases](https://github.com/hooklistener/hooklistener-cli/releases) page.

```bash
# Linux/macOS
curl -L https://github.com/hooklistener/hooklistener-cli/releases/latest/download/hooklistener-$(uname -s)-$(uname -m) -o hooklistener
chmod +x hooklistener
sudo mv hooklistener /usr/local/bin/
```

### From Source

```bash
# Install from GitHub
cargo install --git https://github.com/hooklistener/hooklistener-cli

# Or clone and build locally
git clone https://github.com/hooklistener/hooklistener-cli.git
cd hooklistener-cli
cargo build --release
```

### Coming Soon

- 🍺 Homebrew tap
- 📦 cargo-dist installers (shell/powershell)
- 🐳 Docker image

## Quick Start

1. **Start the CLI**:
   ```bash
   hooklistener
   ```

2. **Authenticate** using the device code flow:
   - The CLI will display a verification code and URL
   - Visit the URL in your browser and enter the code
   - Once authenticated, you can browse your webhook requests

3. **Browse your webhooks** through the terminal UI interface

## Usage

### Basic Commands

```bash
# Start the TUI
hooklistener

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
cargo run
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

