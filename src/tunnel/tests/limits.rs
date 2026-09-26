//! Negotiated tunnel limits and local work budgets.

use super::*;

#[test]
fn test_tunnel_limits_clamp_join_response_contract_to_local_maxima() {
    let response = serde_json::json!({
        "limits": {
            "max_body_bytes": u64::MAX,
            "max_response_header_bytes": 16_777_216,
            "max_response_header_items": 2_048,
            "response_headers_format": "ordered_pairs"
        }
    });

    let limits = TunnelLimits::from_join_response(&response);

    assert_eq!(limits.max_request_body_bytes, LOCAL_MAX_TUNNEL_BODY_BYTES);
    assert_eq!(limits.max_response_body_bytes, LOCAL_MAX_TUNNEL_BODY_BYTES);
    assert_eq!(
        limits.max_response_header_bytes,
        LOCAL_MAX_RESPONSE_HEADER_BYTES
    );
    assert_eq!(limits.max_response_header_items, 2_048);
    assert!(limits.ordered_response_headers);
}

#[test]
fn test_tunnel_limits_clamp_untrusted_body_limit_to_local_maximum() {
    let response = serde_json::json!({
        "limits": {
            "max_body_bytes": u64::MAX,
            "max_response_header_bytes": u64::MAX,
            "max_response_header_items": u64::MAX
        }
    });

    let limits = TunnelLimits::from_join_response(&response);

    assert_eq!(
        (
            limits.max_request_body_bytes,
            limits.max_response_body_bytes
        ),
        (LOCAL_MAX_TUNNEL_BODY_BYTES, LOCAL_MAX_TUNNEL_BODY_BYTES)
    );
    assert_eq!(
        limits.max_response_header_bytes,
        LOCAL_MAX_RESPONSE_HEADER_BYTES
    );
    assert_eq!(
        limits.max_response_header_items,
        TUNNEL_MAX_RESPONSE_HEADERS
    );
}

#[test]
fn test_tunnel_limits_keep_legacy_response_header_shape_by_default() {
    let limits = TunnelLimits::from_join_response(&serde_json::json!({}));

    assert_eq!(limits.max_request_body_bytes, LEGACY_MAX_REQUEST_BODY_BYTES);
    assert_eq!(
        limits.max_response_body_bytes,
        LEGACY_MAX_RESPONSE_BODY_BYTES
    );
    assert_eq!(
        limits.max_response_header_bytes,
        LEGACY_MAX_RESPONSE_HEADER_BYTES
    );
    assert_eq!(
        limits.max_response_header_items,
        TUNNEL_MAX_RESPONSE_HEADERS
    );
    assert!(!limits.ordered_response_headers);
}

#[test]
fn test_local_work_budget_prevents_concurrent_ten_megabyte_deliveries() {
    let budget = LocalWorkBudget::new();
    let ten_mib = 10 * 1024 * 1024;
    let permit = budget.try_acquire(ten_mib).unwrap();

    assert_eq!(budget.available_count(), LOCAL_WORK_MAX_COUNT - 1);
    assert_eq!(budget.available_bytes(), 6 * 1024 * 1024);
    assert!(budget.try_acquire(ten_mib).is_none());

    drop(permit);
    assert_eq!(budget.available_count(), LOCAL_WORK_MAX_COUNT);
    assert_eq!(budget.available_bytes(), LOCAL_WORK_MAX_BYTES);
}

#[test]
fn test_local_work_budget_allows_one_advertised_limit_body() {
    let budget = LocalWorkBudget::new();
    let delivery = LocalDelivery {
        request_id: "request-at-limit".to_string(),
        method: "POST".to_string(),
        path: "/metadata-does-not-reduce-the-body-budget".to_string(),
        query_string: "source=conformance".to_string(),
        headers: vec![(
            "content-type".to_string(),
            "application/octet-stream".to_string(),
        )],
        body: Vec::new(),
        deadline_unix_ms: unix_time_ms() + 1_000,
        replay: false,
    };
    assert_eq!(delivery.retained_bytes(), 0);

    let permit = budget.try_acquire(LOCAL_WORK_MAX_BYTES).unwrap();

    assert_eq!(budget.available_bytes(), 0);
    assert!(budget.try_acquire(1).is_none());

    drop(permit);
    assert_eq!(budget.available_bytes(), LOCAL_WORK_MAX_BYTES);
}

#[test]
fn test_local_work_budget_enforces_count_independently_of_bytes() {
    let budget = LocalWorkBudget::new();
    let permits = (0..LOCAL_WORK_MAX_COUNT)
        .map(|_| budget.try_acquire(1).unwrap())
        .collect::<Vec<_>>();

    assert_eq!(budget.available_count(), 0);
    assert!(budget.try_acquire(1).is_none());
    assert!(budget.available_bytes() > LOCAL_WORK_MAX_BYTES - 1024);

    drop(permits);
    assert_eq!(budget.available_count(), LOCAL_WORK_MAX_COUNT);
}

#[test]
fn test_local_work_budget_rejects_oversized_delivery() {
    let budget = LocalWorkBudget::new();

    assert!(budget.try_acquire(LOCAL_WORK_MAX_BYTES + 1).is_none());
    assert_eq!(budget.available_count(), LOCAL_WORK_MAX_COUNT);
    assert_eq!(budget.available_bytes(), LOCAL_WORK_MAX_BYTES);
}

#[test]
fn test_response_buffer_budget_prevents_concurrent_ten_megabyte_responses() {
    let budget = ResponseBufferBudget::new();
    let ten_mib = 10 * 1024 * 1024;
    let reservation = budget.try_reserve(Some(ten_mib)).unwrap();

    assert_eq!(budget.available_bytes(), 6 * 1024 * 1024);
    assert!(budget.try_reserve(Some(ten_mib)).is_none());

    drop(reservation);
    assert_eq!(budget.available_bytes(), RESPONSE_BUFFER_MAX_BYTES);
}

#[test]
fn test_response_buffer_allows_one_advertised_limit_body() {
    let budget = ResponseBufferBudget::new();
    let permit = budget.try_reserve(Some(RESPONSE_BUFFER_MAX_BYTES)).unwrap();

    assert_eq!(budget.available_bytes(), 0);
    assert!(budget.try_reserve(Some(1)).is_none());

    drop(permit);
    assert_eq!(budget.available_bytes(), RESPONSE_BUFFER_MAX_BYTES);
}

#[test]
fn test_response_buffer_budget_rejects_oversized_response() {
    let budget = ResponseBufferBudget::new();

    assert!(
        budget
            .try_reserve(Some(RESPONSE_BUFFER_MAX_BYTES + 1))
            .is_none()
    );
    assert_eq!(budget.available_bytes(), RESPONSE_BUFFER_MAX_BYTES);
}

#[test]
fn test_unknown_response_length_cannot_grow_past_budget() {
    let budget = ResponseBufferBudget::new();
    let mut reservation = budget.try_reserve(None).unwrap();

    assert!(reservation.try_grow_to(RESPONSE_BUFFER_MAX_BYTES / 2));
    assert!(!reservation.try_grow_to(RESPONSE_BUFFER_MAX_BYTES + 1));
    assert_eq!(budget.available_bytes(), RESPONSE_BUFFER_MAX_BYTES / 2);

    drop(reservation);
    assert_eq!(budget.available_bytes(), RESPONSE_BUFFER_MAX_BYTES);
}

#[test]
fn test_inbound_stream_budget_rejects_oversized_stream() {
    let oversized = INBOUND_STREAM_MAX_BYTES.saturating_add(1);
    let encoded_body_bytes = base64::encoded_len(LOCAL_MAX_TUNNEL_BODY_BYTES, false).unwrap();

    assert_eq!(INBOUND_STREAM_MAX_BYTES, 32 * 1024 * 1024);
    assert!(encoded_body_bytes.saturating_add(TUNNEL_MAX_FRAME_BYTES) < INBOUND_STREAM_MAX_BYTES);
    assert!(!inbound_stream_fits_budget(0, oversized));
    assert!(inbound_stream_fits_budget(INBOUND_STREAM_MAX_BYTES - 1, 1));
}

#[test]
fn test_cancellation_classifies_execution_boundary() {
    assert_eq!(
        cancellation_classification(DELIVERY_QUEUED),
        Some(("cancelled_before_forward", "known_not_executed"))
    );
    assert_eq!(
        cancellation_classification(DELIVERY_STARTED),
        Some(("cancelled_after_forward_started", "outcome_unknown"))
    );
    assert_eq!(cancellation_classification(DELIVERY_TERMINAL), None);
}
