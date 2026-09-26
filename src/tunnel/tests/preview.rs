//! Bounded body previews and content decoding.

use super::*;

fn preview_headers(values: &[(&str, &str)]) -> HashMap<String, String> {
    values
        .iter()
        .map(|(name, value)| ((*name).to_string(), (*value).to_string()))
        .collect()
}

fn compressed_with<W>(mut encoder: W, body: &[u8]) -> Vec<u8>
where
    W: Write + FinishEncoder,
{
    encoder.write_all(body).unwrap();
    encoder.finish_encoder()
}

trait FinishEncoder {
    fn finish_encoder(self) -> Vec<u8>;
}

impl FinishEncoder for GzEncoder<Vec<u8>> {
    fn finish_encoder(self) -> Vec<u8> {
        self.finish().unwrap()
    }
}

impl FinishEncoder for ZlibEncoder<Vec<u8>> {
    fn finish_encoder(self) -> Vec<u8> {
        self.finish().unwrap()
    }
}

impl FinishEncoder for DeflateEncoder<Vec<u8>> {
    fn finish_encoder(self) -> Vec<u8> {
        self.finish().unwrap()
    }
}

#[test]
fn test_body_preview_is_bounded() {
    let body = vec![b'a'; UI_BODY_PREVIEW_BYTES * 2];
    let preview = body_preview(&body, &HashMap::new()).unwrap();

    assert!(preview.len() <= UI_BODY_PREVIEW_BYTES + 64);
    assert!(preview.contains("truncated"));
    assert!(preview.contains(&(UI_BODY_PREVIEW_BYTES * 2).to_string()));
}

#[test]
fn test_gzip_text_body_preview_is_decoded_without_changing_transport_bytes() {
    let body = b"<html><body>Not found</body></html>";
    let compressed = compressed_with(GzEncoder::new(Vec::new(), Compression::default()), body);
    let headers = preview_headers(&[
        ("content-type", "text/html; charset=utf-8"),
        ("content-encoding", "gzip"),
    ]);

    assert_eq!(
        body_preview(&compressed, &headers).as_deref(),
        Some("<html><body>Not found</body></html>")
    );

    let (transport_body, transport_encoding) = encode_response_body(&compressed);
    assert_eq!(transport_encoding, "base64");
    assert_eq!(URL_SAFE_NO_PAD.decode(transport_body).unwrap(), compressed);
}

#[test]
fn test_standard_and_legacy_deflate_body_previews_are_decoded() {
    let body = b"deflate response";
    let headers = preview_headers(&[
        ("content-type", "text/plain"),
        ("content-encoding", "deflate"),
    ]);
    let zlib = compressed_with(ZlibEncoder::new(Vec::new(), Compression::default()), body);
    let raw = compressed_with(
        DeflateEncoder::new(Vec::new(), Compression::default()),
        body,
    );

    assert_eq!(
        body_preview(&zlib, &headers).as_deref(),
        Some("deflate response")
    );
    assert_eq!(
        body_preview(&raw, &headers).as_deref(),
        Some("deflate response")
    );
}

#[test]
fn test_brotli_and_zstd_body_previews_are_decoded() {
    let body = b"compressed response";

    let mut brotli = Vec::new();
    {
        let mut encoder = brotli::CompressorWriter::new(&mut brotli, 4_096, 5, 22);
        encoder.write_all(body).unwrap();
    }
    let brotli_headers =
        preview_headers(&[("content-type", "text/plain"), ("content-encoding", "br")]);
    assert_eq!(
        body_preview(&brotli, &brotli_headers).as_deref(),
        Some("compressed response")
    );

    let zstd = zstd::stream::encode_all(Cursor::new(body), 0).unwrap();
    let zstd_headers =
        preview_headers(&[("content-type", "text/plain"), ("content-encoding", "zstd")]);
    assert_eq!(
        body_preview(&zstd, &zstd_headers).as_deref(),
        Some("compressed response")
    );
}

#[test]
fn test_stacked_content_encodings_are_decoded_in_reverse_order() {
    let body = b"stacked response";
    let gzip = compressed_with(GzEncoder::new(Vec::new(), Compression::default()), body);
    let mut gzip_then_brotli = Vec::new();
    {
        let mut encoder = brotli::CompressorWriter::new(&mut gzip_then_brotli, 4_096, 5, 22);
        encoder.write_all(&gzip).unwrap();
    }
    let headers = preview_headers(&[
        ("content-type", "text/plain"),
        ("content-encoding", "gzip, br"),
    ]);

    assert_eq!(
        body_preview(&gzip_then_brotli, &headers).as_deref(),
        Some("stacked response")
    );
}

#[test]
fn test_content_encoding_list_is_bounded() {
    let at_limit = preview_headers(&[("content-encoding", "gzip, br, zstd, deflate")]);
    let over_limit = preview_headers(&[("content-encoding", "gzip, br, zstd, deflate, gzip")]);

    assert_eq!(
        content_encodings(&at_limit).unwrap().len(),
        MAX_CONTENT_ENCODINGS
    );
    assert_eq!(
        content_encodings(&over_limit).unwrap_err(),
        "too many content encodings (5 > 4)"
    );
    assert!(
        body_preview(b"encoded", &over_limit)
            .unwrap()
            .starts_with("[body preview unavailable: too many content encodings")
    );
}

#[test]
fn test_text_body_preview_honors_declared_charset() {
    let headers = preview_headers(&[("Content-Type", "text/plain; charset=iso-8859-1")]);

    assert_eq!(body_preview(b"caf\xe9", &headers).as_deref(), Some("café"));
}

#[test]
fn test_binary_body_preview_uses_metadata_instead_of_lossy_text() {
    let body = b"\x89PNG\r\n\x1a\n\0\xff";
    let headers = preview_headers(&[("content-type", "image/png")]);
    let preview = body_preview(body, &headers).unwrap();

    assert_eq!(preview, "[binary body: 10 bytes; content-type: image/png]");
    assert!(!preview.contains('\u{fffd}'));
}

#[test]
fn test_unsupported_or_malformed_content_encoding_has_safe_preview() {
    let unsupported = preview_headers(&[("content-encoding", "compress")]);
    let malformed = preview_headers(&[("content-encoding", "gzip")]);

    let unsupported_preview = body_preview(b"encoded", &unsupported).unwrap();
    assert!(unsupported_preview.contains("unsupported content-encoding"));

    let malformed_preview = body_preview(b"not gzip", &malformed).unwrap();
    assert!(malformed_preview.contains("preview unavailable"));
    assert!(!malformed_preview.contains('\u{fffd}'));
}

#[test]
fn test_compressed_body_preview_is_bounded_after_decompression() {
    let body = vec![b'a'; UI_BODY_PREVIEW_BYTES * 2];
    let compressed = compressed_with(GzEncoder::new(Vec::new(), Compression::default()), &body);
    let headers = preview_headers(&[("content-type", "text/plain"), ("content-encoding", "gzip")]);
    let preview = body_preview(&compressed, &headers).unwrap();

    assert!(preview.starts_with("aaaa"));
    assert!(preview.contains("decoded preview truncated"));
    assert!(preview.len() <= UI_BODY_PREVIEW_BYTES + 96);
}

#[test]
fn test_log_text_is_control_neutral_and_bounded() {
    let input = format!(
        "relay\u{1b}]52;c;owned\u{7}\n{}",
        "a".repeat(LOG_TEXT_MAX_BYTES)
    );

    let sanitized = bounded_control_neutral_log_text(&input);

    assert!(sanitized.len() <= LOG_TEXT_MAX_BYTES);
    assert!(!sanitized.chars().any(char::is_control));
    assert!(sanitized.contains("\\u{1b}"));
    assert!(sanitized.contains("\\n"));
    assert!(sanitized.ends_with("..."));
}
