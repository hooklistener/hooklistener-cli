# Build footprint

The first size/build-time pass keeps the complete CLI: no commands, terminal
screens, authentication flows, or tunnel functionality are removed. An
agent-focused build without the human interface is a separate follow-up.

## Changes

- Disable Arboard's default image support. Login and request-copy actions use
  `set_text`; clipboard text remains available on Linux, macOS, and Windows.
- Keep Ratatui's Crossterm backend, underline colors, and layout cache, but
  remove its unused convenience macros. Its top-level `all-widgets` request
  is also removed, although Ratatui 0.30's transitive `ratatui-widgets` defaults
  still enable calendar support. This pass does not eliminate that code.
- Replace Tokio's `full` feature with the I/O, macros, networking, multithreaded
  runtime, signals, synchronization, and timers used by the application/tests.
  Keep `parking_lot` to preserve its internal synchronization implementation;
  Reqwest's streaming support still enables `fs` transitively.
- Select Reqwest's existing native TLS backend explicitly, preserving its
  `charset`, `http2`, and `system-proxy` defaults plus JSON and streaming.
- Strip release symbols. Keep the normal optimization level, codegen units,
  unwinding, and development profile; do not introduce LTO to trade build time
  for size without measurements.

### Why the TLS change preserves the current backend

In the previous feature graph, Reqwest 0.13.3's defaults enabled Rustls/AWS-LC,
while `self_update/native-tls` also enabled Reqwest's native TLS feature.
Reqwest 0.13.3's `TlsBackend::default` selects **native TLS** when both are
compiled and HTTP/3 is not enabled. No application call selects Rustls or
provides a custom TLS backend. The WebSocket client already uses native TLS.

This change removes an unused compiled backend, rather than switching the
application's TLS implementation. Certificate verification, platform trust
stores, HTTP/2/ALPN, proxy behavior, and the explicit local-target
`--insecure-tls` opt-in are preserved. Recheck this assumption when upgrading
Reqwest. This does not make the Linux binary static or remove its OpenSSL
runtime dependency.

The lockfile drops 40 packages without adding packages or changing retained
versions. A lockfile includes optional and foreign-target packages; this is
not a claim that 40 fewer crates are compiled on every platform. Rustls-related
entries can remain in the lockfile even when absent from the active host graph.

## Initial measurements

Before: commit `f7561fb642278a3403e34c83da531269d06a53a7` (v1.8.6).
After: this dependency/profile change, with otherwise identical Rust sources.
Both release and development runs used matching source snapshots per variant.

Host: AMD Ryzen 7 9800X3D (16 logical CPUs), Linux x86_64/glibc 2.44,
Rust/Cargo 1.92.0, eight Cargo jobs, empty isolated target directories, cached
registry sources, no build-environment overrides. Builds ran sequentially.
These are **one run per variant**, not a statistically established speedup or
a release-platform guarantee. The local binaries are measurement artifacts;
production Linux releases still build on Ubuntu 22.04 for the glibc 2.35 floor.

| Measurement | Before | After |
| --- | ---: | ---: |
| Release executable | 21,188,168 bytes (20.21 MiB) | 13,370,552 bytes (12.75 MiB) |
| Release binary-only tar/gzip | 7,867,088 bytes (7.50 MiB) | 5,503,902 bytes (5.25 MiB) |
| Clean release build | 43.867 s | 27.744 s |
| No-op release build | 0.098 s | 0.136 s |
| Warm release source rebuild | 7.804 s | 8.193 s |
| Clean development build | 23.240 s | 15.224 s |
| No-op development build | 0.095 s | 0.161 s |
| Incremental development comment-edit probe | 1.029 s | 0.900 s |
| `Compiling` entries in each clean build log | 278 | 257 |

The release executable is **36.9% smaller**, its compressed archive is
**30.0% smaller**, and the observed clean release build took **36.8% less time**.
The warm release rebuild did not improve; no-op differences are tens of
milliseconds. Do not interpret the comment-only development probe as a general
incremental-build speedup.

Cargo's baseline timings identify the removed AWS-LC native build-script run
at 30.03 seconds, image compilation at 7.27 seconds, and its `moxcms` dependency
at 9.77 seconds. These overlap with other compilation work and must not be
summed as elapsed-time savings. The source-rebuild result suggests that
root-crate compile time is still a separate concern from dependency trimming.

Raw reports and per-stage logs from this run are under
`target/size-audit/{baseline,trimmed}-{release,dev}/`. Baseline and trimmed
executables also produced identical `--version`, root `--help`, and `--help`
output for all 16 top-level command groups.

## Reproduce measurements

Use the pinned Rust toolchain and Python 3. Run on the same machine, with the
same job count and no other builds running. Fetch dependencies **before** the
timed build (substitute the host triple printed by `rustc -vV`):

```sh
mise exec -- cargo fetch --locked --target x86_64-unknown-linux-gnu
mise exec -- python3 scripts/measure_build.py \
  --output target/measurements/before-release --profile release --jobs 8
```

After applying a change, use a **new** output directory:

```sh
mise exec -- python3 scripts/measure_build.py \
  --output target/measurements/after-release --profile release --jobs 8
```

Repeat with `--profile dev` in new directories to measure incremental developer
builds separately from distribution builds. The tool copies the current
`Cargo.toml`, `Cargo.lock`, `src/`, and `fixtures/` (plus `.cargo/` if present),
including uncommitted edits, to a private source directory. It never runs
`cargo clean`, changes the checkout's sources, or reuses an existing output
directory. It is specific to this single-binary crate; update its input list if
build scripts, workspace packages, or other build inputs are introduced.

Each run performs:

1. **Clean:** an offline, locked build with an empty target directory. Registry
   downloads are excluded; OS filesystem caches are not flushed. Disable
   compiler wrappers/caches when comparing compilation cost.
2. **No-op:** the identical build with no changes.
3. **Source rebuild:** append a comment to the private copy of `src/main.rs`
   and rebuild with warm dependencies. Release defaults do **not** enable
   incremental compilation; label this a warm root-crate rebuild, not an
   incremental release build. With `--profile dev`, it exercises incremental
   invalidation, but a comment-only edit is a lower-bound probe, not a
   representative implementation change.

Outputs include per-stage logs, Cargo HTML timings under the private target
directory, and `report.json` with source/manifest/lockfile hashes, toolchain,
platform, job count, selected build environment overrides, wall-clock seconds,
executable bytes, and binary-only tar/gzip bytes. The tar/gzip uses fixed
metadata and level 6 for comparison; it is not a signed release artifact and
Windows releases use ZIP instead. Binary sizes are measured after the source
rebuild. The tool also smoke-tests `--version` and `--help` without logging in
or calling the service.

Save reports outside `target/` if they must survive a manual clean. Compare
multiple runs before setting a hard CI time budget. Inspect Cargo timings and
`cargo tree -e features` to distinguish dependency work from root-crate work.

## Profiling and debugging

Release symbols are intentionally stripped from distributed executables.
For a local profiling build, preserve symbols without changing the manifest:

```sh
CARGO_PROFILE_RELEASE_STRIP=none mise exec -- cargo build --release --locked
```

Add `CARGO_PROFILE_RELEASE_DEBUG=1` when the profiler needs line tables. Keep
these overrides out of the size baseline. Development/test profiles retain
Cargo's existing debug behavior.

## Validation

```sh
python3 scripts/measure_build_test.py
mise exec -- make check
mise exec -- cargo machete
mise exec -- cargo audit --file Cargo.lock
mise exec -- python3 scripts/verify_tunnel_v3_release_tests.py
```

Local validation for the initial measurements passed:

- 488 Rust unit/snapshot tests plus six integration tests in the development
  test profile; formatting and Clippy with warnings denied.
- All 30 required V3 release-test inventory entries, followed by all 488 Rust
  unit/snapshot tests in the release profile.
- Cargo Machete: no unused direct dependencies.
- Six measurement-tool tests and 29 release-workflow contract tests.

Cargo Audit exited 0, but **the security audit is not warning-free**:

- `lru 0.16.4`, through `ratatui-core 0.1.0`, has panic-safety advisory
  [RUSTSEC-2026-0253](https://rustsec.org/advisories/RUSTSEC-2026-0253).
  The fix is in `lru >= 0.18.2`, outside Ratatui's current `0.16` requirement.
- The lockfile contains yanked `der 0.8.0` (not in this Linux build's active
  dependency graph).

Both versions also occur in the baseline lockfile; this patch introduces
neither warning and adds no audit ignores. Resolve them in a separate
transitive-dependency update before treating the release audit as clean.

The measurement tool's fast unit tests run in CI; expensive footprint
measurements are opt-in. Existing CI builds continue to cover Linux, both
macOS architectures, and Windows. Native builds and TLS/clipboard smoke checks
on those platforms remain necessary before release.
