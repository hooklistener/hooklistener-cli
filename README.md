Hooklistener CLI
================

[![CI](https://github.com/hooklistener/hooklistener-cli/actions/workflows/ci.yml/badge.svg?branch=main)](https://github.com/hooklistener/hooklistener-cli/actions/workflows/ci.yml)
[![CodeQL](https://github.com/hooklistener/hooklistener-cli/actions/workflows/codeql.yml/badge.svg?branch=main)](https://github.com/hooklistener/hooklistener-cli/actions/workflows/codeql.yml)
[![Release](https://img.shields.io/github/v/release/hooklistener/hooklistener-cli?sort=semver)](https://github.com/hooklistener/hooklistener-cli/releases)

A fast, TUI-based CLI to browse and forward Hooklistener requests from your terminal.

Install
- Downloads: grab prebuilt binaries from the latest Release.
- With cargo (from Git): `cargo install --git https://github.com/hooklistener/hooklistener-cli`
- Coming soon: cargo-dist installers (shell/powershell), Homebrew tap.

Development
- Format: `cargo fmt --all`
- Lint: `cargo clippy --all-targets --all-features -- -D warnings`
- Test: `cargo test --all-targets --all-features --locked`

Releasing
- Tag a version like `v0.1.0` and push it; CI builds and publishes artifacts for Linux, macOS (Intel + Apple Silicon), and Windows. Checksums are attached to the GitHub Release.

