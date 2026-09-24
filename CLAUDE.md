# CLAUDE.md

Guidance for Claude Code when working in this repository.

## Project Overview

`hooklistener-cli` is a Rust CLI (binary name `hooklistener`) for browsing
webhooks captured by Hooklistener, forwarding them to local servers, exposing
local servers through tunnels, and monitoring endpoints. It has a Ratatui TUI
mode (`listen`, `tunnel`) and a scriptable command mode with `--json` output.
See README.md for user-facing docs and PRODUCT.md / DESIGN.md for product and
visual direction.

## Toolchain

- Rust is pinned to 1.92.0 in `mise.toml`. Run `mise install` once.
- `cargo` is not on the default PATH in this environment. Prefix commands
  with `mise exec --` (for example `mise exec -- cargo test`). This also
  applies to `scripts/verify_tunnel_v3_release_tests.py`, which shells out to
  cargo and fails with "No such file or directory: 'cargo'" otherwise.

## Build and Development Commands

```bash
make check                  # workflow/gate tests + Rust tests, fmt, clippy
make check-cases            # release-profile saved-case + lifecycle conformance
cargo build                 # debug build
cargo build --release
cargo run -- <args>         # e.g. cargo run -- listen
cargo test                  # unit + integration tests
cargo fmt --all
cargo clippy --all-targets --all-features -- -D warnings
cargo machete               # unused dependencies (CI runs this too)
```

CI (`.github/workflows/ci.yml`) fails on any rustfmt diff, any clippy or
compiler warning, any failing test, any `cargo audit` advisory, and any unused
dependency reported by `cargo machete`. Keep the tree warning-free.

## Project Structure

Single binary crate; all modules are declared in `src/main.rs`.

- `src/main.rs` - clap CLI definition, command dispatch, non-TUI command
  output, error-to-exit-code mapping (`error_hint`, `error_code`).
- `src/api.rs` - HTTP client for the Hooklistener API and its response types.
- `src/auth.rs` - device-code login flow and token refresh.
- `src/config.rs` - `~/.config/hooklistener/config.json` persistence.
- `src/app.rs` - TUI application state and key handling.
- `src/ui.rs` - Ratatui rendering for every TUI screen.
- `src/tunnel.rs` - WebSocket tunnel forwarder (Phoenix channel protocol,
  request/response framing, reconnect).
- `src/tunnel_v3.rs` - protocol v3 streaming transport and bounded body
  primitives; selected automatically from the relay ticket.
- `src/target_policy.rs` - validation of local forwarding targets.
- `src/updater.rs` - self-update via GitHub releases (`self_update` crate).
- `src/logger.rs` - tracing setup, per-session log files, log cleanup.
- `src/output.rs`, `src/syntax.rs`, `src/theme.rs`, `src/logo.rs` - terminal
  styling, JSON highlighting, status colors, startup animation.
- `src/errors.rs` - typed errors that carry user hints (`TunnelLifecycleError`,
  `UpdateError`).
- `src/models.rs` - shared request/response models.

## Tests

- Unit tests live inline in each module under `#[cfg(test)]`.
- `insta` snapshot tests render TUI screens and command output; snapshots are
  in `src/snapshots/`. Review new `.snap.new` files with `cargo insta review`
  or by inspecting the diff before committing.
- `mockito` mocks the HTTP API in `src/api.rs` and `src/updater.rs` tests.
- `tests/tunnel_phase1_conformance.rs` is the integration suite for the tunnel
  protocol; fixtures are in `fixtures/`.
- `fixtures/tunnel_v3_release_test_inventory.txt` lists tunnel v3 tests that
  must exist. `scripts/verify_tunnel_v3_release_tests.py` checks it; update
  the inventory when adding or renaming those tests.
- Saved-case subprocess tests are in `tests/support/cases.rs`, included by the
  same integration target. `fixtures/cases_release_test_inventory.txt` locks
  their release-profile inventory; missing or ignored tests fail CI on all
  three platforms. Update it with test changes and run `make check-cases`.
  See `docs/cases-conformance.md` for receipt generation and local checks.

## Conventions

- Edition 2024. Match the existing style; `cargo fmt` is authoritative.
- Do not add `#[allow(dead_code)]`. If something is unused, delete it. The
  codebase currently has zero such suppressions.
- Derive only `Deserialize` on API response structs. Add `Serialize` only
  where a value is actually emitted (for example `--json` output or a request
  body). A `Serialize` derive counts as a read for every field and hides
  unused fields from the compiler's dead-code lint.
- Prefer removing fields that are parsed but never consumed; serde ignores
  unknown JSON keys, so trimming a response struct is safe.
- `#[allow(clippy::too_many_arguments)]` is accepted on a few tunnel and
  command functions; do not add new ones without reason.

## Release

Releases are driven by `.github/workflows/release.yml` and `release.toml`
(cargo-dist). Packaging for npm and Homebrew lives in `npm/` and
`homebrew-tap/`. `scripts/release_workflow_contract_test.py` validates the
workflow contracts and runs in CI.

Publishing credentials (`CARGO_REGISTRY_TOKEN`, `NPM_TOKEN`, and
`HOMEBREW_TAP_TOKEN`) are intentionally repository-level Actions secrets. The
owner-approved policy permits solo-maintainer releases; an empty `release`
environment-secret list alone is not a release blocker. Follow the current
controls in `CONTRIBUTING.md`, including required CI checks, tag-only release
deployments, and immutable release tags. Do not change credentials or protections
to reconcile stale documentation.
