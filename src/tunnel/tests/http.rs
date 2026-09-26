//! Local HTTP request and response handling.

use super::*;

#[test]
fn test_should_forward_request_header_filters_framing_headers() {
    for header in [
        "host",
        "Host",
        "content-length",
        "Content-Length",
        "transfer-encoding",
        "connection",
        "keep-alive",
        "proxy-connection",
        "te",
        "trailer",
        "upgrade",
    ] {
        assert!(!should_forward_request_header(header));
    }

    assert!(should_forward_request_header("content-type"));
    assert!(should_forward_request_header("authorization"));
    assert!(should_forward_request_header("x-custom-header"));
}

#[test]
fn protocol_v3_205_reset_content_forbids_body_and_nonzero_content_length() {
    assert!(response_body_forbidden("GET", 205));
    assert!(ensure_response_chunk_allowed("GET", 205).is_err());
    assert_eq!(
        relayed_response_content_length("GET", 205, Some(123)),
        Some(0)
    );

    let mut headers = reqwest::header::HeaderMap::new();
    assert!(valid_reset_content_length(&headers));

    headers.insert(
        reqwest::header::CONTENT_LENGTH,
        reqwest::header::HeaderValue::from_static("0"),
    );
    assert!(valid_reset_content_length(&headers));

    headers.insert(
        reqwest::header::CONTENT_LENGTH,
        reqwest::header::HeaderValue::from_static("1"),
    );
    assert!(!valid_reset_content_length(&headers));

    headers.append(
        reqwest::header::CONTENT_LENGTH,
        reqwest::header::HeaderValue::from_static("0"),
    );
    assert!(!valid_reset_content_length(&headers));
}

#[test]
fn test_supported_tunnel_method_accepts_token_valid_extension_methods() {
    assert_eq!(supported_tunnel_method("GET"), Some(reqwest::Method::GET));
    assert_eq!(
        supported_tunnel_method("OPTIONS"),
        Some(reqwest::Method::OPTIONS)
    );
    assert_eq!(
        supported_tunnel_method("PURGE"),
        Some(reqwest::Method::from_bytes(b"PURGE").unwrap())
    );
    assert_eq!(supported_tunnel_method("BAD METHOD"), None);
}

#[test]
fn test_tunnel_websocket_config_accepts_advertised_messages() {
    let config = tunnel_websocket_config();

    assert_eq!(config.max_message_size, Some(TUNNEL_MAX_FRAME_BYTES));
    assert_eq!(config.max_frame_size, Some(TUNNEL_MAX_FRAME_BYTES));
}

#[tokio::test]
async fn test_read_response_body_limited_rejects_large_content_length() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/large")
        .with_body("too large")
        .create_async()
        .await;
    let mut response = reqwest::Client::new()
        .get(format!("{}/large", server.url()))
        .send()
        .await
        .unwrap();

    let error = read_response_body_limited(&mut response, 4)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("max 4 bytes"));
    mock.assert_async().await;
}

#[tokio::test]
async fn test_read_response_body_limited_rejects_large_chunked_body() {
    let mut server = mockito::Server::new_async().await;
    let mock = server
        .mock("GET", "/chunked")
        .with_chunked_body(|writer| writer.write_all(b"too large"))
        .create_async()
        .await;
    let mut response = reqwest::Client::new()
        .get(format!("{}/chunked", server.url()))
        .send()
        .await
        .unwrap();

    let error = read_response_body_limited(&mut response, 4)
        .await
        .unwrap_err();

    assert!(error.to_string().contains("max 4 bytes"));
    mock.assert_async().await;
}

#[test]
fn test_ordered_headers_preserve_duplicates() {
    let headers = ordered_header_pairs(Some(&serde_json::json!([
        ["set-cookie", "first=1"],
        ["set-cookie", "second=2"]
    ])))
    .unwrap();

    assert_eq!(
        headers,
        vec![
            ("set-cookie".to_string(), "first=1".to_string()),
            ("set-cookie".to_string(), "second=2".to_string())
        ]
    );
}

#[test]
fn test_response_header_pairs_preserve_repeated_values() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.append("set-cookie", "first=1".parse().unwrap());
    headers.append("set-cookie", "second=2".parse().unwrap());

    assert_eq!(
        response_headers_to_pairs(&headers),
        vec![
            ("set-cookie".to_string(), "first=1".to_string()),
            ("set-cookie".to_string(), "second=2".to_string())
        ]
    );
}

#[test]
fn test_v2_response_header_item_limit_counts_repeated_values() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.append("set-cookie", "first=1".parse().unwrap());
    headers.append("set-cookie", "second=2".parse().unwrap());
    let mut limits = TunnelLimits::from_join_response(&serde_json::json!({
        "limits": {"max_response_header_items": 1}
    }));
    limits.max_response_header_bytes = usize::MAX;

    assert!(
        response_header_limit_error(&headers, limits)
            .unwrap()
            .contains("item limit")
    );

    limits.max_response_header_items = 2;
    assert!(response_header_limit_error(&headers, limits).is_none());
}

#[tokio::test]
async fn test_local_redirects_are_returned_without_following() {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (mut socket, _) = listener.accept().await.unwrap();
        let mut request = [0_u8; 1024];
        let _ = socket.read(&mut request).await.unwrap();
        socket
            .write_all(
                b"HTTP/1.1 302 Found\r\nLocation: /must-not-follow\r\nContent-Length: 0\r\n\r\n",
            )
            .await
            .unwrap();
    });

    let response = tunnel_http_client(Duration::from_secs(1))
        .unwrap()
        .get(format!("http://{address}/start"))
        .send()
        .await
        .unwrap();

    assert_eq!(response.status(), reqwest::StatusCode::FOUND);
    server.await.unwrap();
}

#[test]
fn test_response_headers_preserve_duplicate_order() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.append("set-cookie", "first=1".parse().unwrap());
    headers.append("set-cookie", "second=2".parse().unwrap());

    assert_eq!(
        response_headers_to_ordered_pairs(&headers),
        vec![
            ("set-cookie".to_string(), "first=1".to_string()),
            ("set-cookie".to_string(), "second=2".to_string())
        ]
    );
}

#[test]
fn test_large_text_response_uses_bounded_base64_encoding() {
    let body = vec![b'a'; MAX_RAW_BODY_BYTES + 1];

    let (encoded, encoding) = encode_response_body(&body);

    assert_eq!(encoding, "base64");
    assert_eq!(URL_SAFE_NO_PAD.decode(encoded).unwrap(), body);
}

// Base64 decode roundtrip
#[test]
fn test_base64_body_decode_roundtrip() {
    let original = b"Hello, World!";
    let encoded = URL_SAFE_NO_PAD.encode(original);
    let decoded = URL_SAFE_NO_PAD.decode(&encoded).unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn test_decode_request_body_limited_rejects_oversized_raw_body_before_copying() {
    let error = decode_request_body_limited("raw", "12345", 4).unwrap_err();

    assert!(error.to_string().contains("5 > 4 bytes"));
}

#[test]
fn test_decode_request_body_limited_rejects_oversized_base64_before_decoding() {
    let encoded = URL_SAFE_NO_PAD.encode(b"12345");

    let error = decode_request_body_limited("base64", &encoded, 4).unwrap_err();

    assert!(error.to_string().contains("encoded body is too large"));
}
