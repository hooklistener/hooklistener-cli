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
- 🔐 **Secure** - API key stored safely in your config directory
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

1. **Get your API key** from [Hooklistener](https://hooklistener.com)

2. **Configure the CLI**:
   ```bash
   hooklistener config --api-key YOUR_API_KEY
   ```

3. **Start browsing webhooks**:
   ```bash
   hooklistener
   ```

## Usage

### Basic Commands

```bash
# Start the TUI
hooklistener

# Configure API key
hooklistener config --api-key YOUR_API_KEY

# Show help
hooklistener --help
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

Configuration is stored in `~/.config/hooklistener/config.json`:

```json
{
  "api_key": "your_api_key_here",
  "theme": "dark",
  "auto_refresh": true,
  "refresh_interval": 5
}
```

See [example.config.json](example.config.json) for all available options.

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
cargo test --all-targets --all-features --locked

# Format code
cargo fmt --all

# Run linter
cargo clippy --all-targets --all-features -- -D warnings

# Check for security vulnerabilities
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

