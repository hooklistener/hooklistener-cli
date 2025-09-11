# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project Overview

This is a Rust CLI application called `hooklistener-cli` that uses the Ratatui library for terminal UI functionality. The project is in early development stage.

## Build and Development Commands

```bash
# Build the project
cargo build

# Build release version
cargo build --release

# Run the application
cargo run

# Run tests
cargo test

# Check code without building
cargo check

# Format code
cargo fmt

# Run linter
cargo clippy
```

## Project Structure

- `src/main.rs` - Main entry point for the CLI application
- `Cargo.toml` - Project dependencies and configuration

## Dependencies

- **ratatui** (^0.29.0) - Terminal UI library for building rich command-line interfaces

## Development Notes

- The project uses Rust edition 2024
- This appears to be a terminal-based hook listener CLI tool, likely for monitoring or processing webhooks or Git hooks