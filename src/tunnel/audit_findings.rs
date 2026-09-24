//! Regression tests for the 2026-09 security audit findings. Each test
//! asserts the safe behavior and fails on the pre-fix code.
//! Run with: cargo test audit_findings
use super::*;
use std::collections::HashMap;
use std::time::Duration;

fn stacked_encoding_headers(encoding: &str, layers: usize) -> HashMap<String, String> {
    let mut headers = HashMap::new();
    headers.insert(
        "content-encoding".to_string(),
        std::iter::repeat_n(encoding, layers)
            .collect::<Vec<_>>()
            .join(","),
    );
    headers.insert("content-type".to_string(), "text/plain".to_string());
    headers
}

/// Regression test for finding 1: `content_encodings` used to accept an
/// unbounded list of stacked codings and `decode_body_preview` built one
/// nested decoder per entry. A single header value (remote-controlled on
/// the protocol v2 request path, local-server-controlled on every response
/// path) could therefore allocate decoder state proportional to the
/// header length and read through a recursion chain of the same depth.
/// The list is now capped at `MAX_CONTENT_ENCODINGS` before any decoder
/// is constructed.
#[test]
fn body_preview_bounds_stacked_content_encodings() {
    const LAYERS: usize = 20_000; // fits comfortably in a 128 KiB header budget
    let headers = stacked_encoding_headers("zstd", LAYERS);

    let started = std::time::Instant::now();
    let preview = std::thread::Builder::new()
        .stack_size(2 * 1024 * 1024) // tokio worker default
        .spawn(move || body_preview(b"hello", &headers))
        .expect("spawn")
        .join()
        .expect("preview must not panic or overflow the stack");

    assert!(
        started.elapsed() < Duration::from_secs(2),
        "preview took {:?} for {LAYERS} stacked codings",
        started.elapsed()
    );
    let preview = preview.expect("non-empty body yields a preview");
    assert!(
        preview.starts_with("[body preview unavailable"),
        "expected an early rejection, got: {preview:.80}"
    );
}

/// Regression test for finding 5: `is_fatal_error` used to match the bare
/// substrings "401"/"403", so a transient transport error whose text
/// merely contained those digits (a port, a request id, a hostname)
/// permanently stopped reconnection. Only genuine `HTTP 401`/`HTTP 403`
/// status tokens are fatal now.
#[test]
fn transient_errors_containing_401_or_403_digits_are_not_fatal() {
    for message in [
        // Shapes produced by server_response_error / validate_join_mode /
        // TunnelLifecycleError::Api with server-supplied detail text.
        "Relay handshake ticket request failed (HTTP 503 Service Unavailable; code=upstream_timeout; message=gateway timed out after 4013 ms)",
        "Tunnel join failed: relay shard 403 is draining; retry shortly",
        "Tunnel API request failed (session_lease_busy, HTTP 409): lease 7f401 is still held",
    ] {
        assert!(
            !is_fatal_error(message),
            "transient error was classified fatal: {message}"
        );
    }
}
