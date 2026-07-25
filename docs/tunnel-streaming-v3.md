# Tunnel Streaming Protocol v3

The CLI contains a protocol v3 streaming engine wired to the normal tunnel
lifecycle. It automatically selects the highest protocol advertised by the
relay ticket; tickets from older services safely select protocol v2 and its 16
MiB local body limit.

The service controls rollout centrally:

```text
Service: TUNNEL_V3_ENABLED=true
```

Protocol v3 currently uses metadata-only capture and a 100 MB request/response
body limit. Buffered captures use the same bounded v3 request and response
streams, so automatic replay no longer changes protocol selection. The service
applies a configurable end-to-end deadline (80 seconds by default), so the
size ceiling is not a promise that arbitrarily slow transfers remain open
indefinitely.

The relay-ticket capability is `[3, 2]` while the service rollout is enabled
and `[2]` otherwise. Missing capability metadata defaults to v2. There is no
customer-facing CLI environment switch.

## Why v3 uses a separate wire path

The existing tunnel connects without a Phoenix `vsn` parameter and therefore
uses the Phoenix v1 JSON serializer. Native binary channel pushes require the
Phoenix v2 serializer. An activated v3 client must:

1. connect with `vsn=2.0.0`;
2. encode JSON controls as `[join_ref, ref, topic, event, payload]` arrays; and
3. encode/decode native binary pushes with Phoenix's binary envelope before
   processing the 29-byte Hooklistener chunk header.

Changing only `protocol_version` would make the connection incompatible. The
codec tests both the Phoenix envelope and the Hooklistener frame.

## Bounded ownership

The request receiver validates UUID, sequence, offset, declared length, total
limit, and incremental SHA-256 state without retaining the complete body. Each
accepted body chunk enters a Tokio channel bounded by item count, per-stream
bytes, and shared connection byte and item semaphores. The stable defaults are
16 items per stream and 256 items per direction across the connection. One
additional channel slot is reserved for terminal abort delivery.

The local HTTP body source releases those permits only when it takes ownership
of a chunk. Its cumulative byte/item notification is bounded as well: if the
notification consumer stops, the HTTP body stream stops instead of accumulating
unbounded credit messages.

The response encoder incrementally emits the same binary chunk format and
tracks cumulative send credit and SHA-256 terminal evidence. It never builds a
complete response body.

Forwarding is currently intentionally half-duplex at the application boundary.
If the local origin responds before the public upload is complete, the CLI
retains that early response and drains the remaining request through the
bounded request stream before relaying it. Protocol v3 does not promise
simultaneous application-level full duplex.

Each request start carries both the service-owned absolute
`deadline_unix_ms` and a positive `timeout_ms` no larger than the negotiated
`request_timeout_ms` (80 seconds by default). The service continues to enforce
the absolute deadline. The CLI starts a local monotonic timer from the relative
budget, so clock skew between the service and the developer machine cannot
instantly reject a valid stream or extend it beyond the negotiated ceiling.

The stable contract separates two concurrency ceilings.
`max_concurrent_streams` (128 by default) is the service transport/coordinator
capacity for one WebSocket connection. `max_active_local_forwards` (8 by
default) is the negotiated local HTTP dispatch ceiling. The CLI also retains
eight as its own safety ceiling, so a server cannot increase local concurrency
beyond the release-qualified value. A start above the effective ceiling is
explicitly aborted with `relay_overloaded_before_forward` and
`known_not_executed`; it is never silently dropped.

## Default-rollout gates

The service's cross-repository release gate builds the real CLI and runs a
dedicated v3 profile. It verifies version 3 negotiation, exact 17 MiB request
and response transfers beyond v2's limit, metadata-only capture, automatic
capability selection, and the full reconnect/fencing matrix. The full v2
profile runs in the same gate to catch rollout compatibility regressions.

Before v3 becomes the default tunnel protocol, release benchmarking must pass:

- exact 100 MB request and response transfers;
- stable resident memory as total body size grows;
- slow local-request and slow public-response backpressure;
- concurrent stream fairness and the aggregate connection byte/item ceilings;
- disconnect, deadline, invalid checksum, and partial-stream cleanup; and
- mixed v2/v3 compatibility during rollout.

`fixtures/tunnel_streaming_v3.json` is copied from the service's published
contract. Its golden binary frame is asserted independently by both codebases.
