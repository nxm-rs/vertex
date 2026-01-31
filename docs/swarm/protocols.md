# Bee Protocol Patterns

This document describes the network protocol patterns used in Vertex (compatible with Bee).

## Headered Streams

All Bee protocol streams use a headers exchange **except handshake**.

```
┌─────────────┐                    ┌─────────────┐
│   Dialer    │                    │  Listener   │
└──────┬──────┘                    └──────┬──────┘
       │                                  │
       │──── Send Headers ───────────────>│
       │<─── Receive Headers ─────────────│
       │                                  │
       │──── Protocol Data ──────────────>│  (or receive, depends on protocol)
       │                                  │
```

**Headered protocols:** hive, pricing, pushsync, retrieval, pingpong, pullsync

**Non-headered:** handshake (uses SYN/ACK directly)

## Headered Protocol Abstraction

Use `vertex-net-headers` traits to compose headered protocols:

```rust
// Implement InnerInbound to read data after headers
impl InnerInbound for MyProtocolInner {
    type Output = MyData;
    type Error = MyCodecError;

    fn protocol_name(&self) -> &'static str { PROTOCOL_NAME }
    fn read(self, stream: Stream) -> BoxFuture<Result<(MyData, Stream), MyCodecError>> { ... }
}

// Implement InnerOutbound to write data after headers
impl InnerOutbound for MyProtocolOutbound {
    type Error = MyCodecError;

    fn protocol_name(&self) -> &'static str { PROTOCOL_NAME }
    fn write(self, stream: Stream) -> BoxFuture<Result<Stream, MyCodecError>> { ... }
}

// Compose with headers wrapper
pub type MyInboundProtocol = HeaderedInbound<MyProtocolInner>;
pub type MyOutboundProtocol = HeaderedOutbound<MyProtocolOutbound>;
```

Handler receives `HeaderedInboundOutput<T>` with `.data` field containing protocol output.

## MultiAddr Encoding

Bee uses a custom `0x99` prefix for encoding multiple multiaddrs in a single bytes field. This is **not standard libp2p** - the standard approach uses `repeated bytes` in protobuf.

Located in `vertex-net-primitives`: `serialize_multiaddrs()` / `deserialize_multiaddrs()`

## Protocol Summary

| Protocol | Headered | Direction | Purpose |
|----------|:--------:|-----------|---------|
| handshake | No | Bidirectional | Peer identity exchange, overlay address verification |
| hive | Yes | Request/Response | Peer discovery, neighbor lists |
| pricing | Yes | Bidirectional | Bandwidth price negotiation |
| pingpong | Yes | Request/Response | Liveness checks |
| retrieval | Yes | Request/Response | Fetch chunks by address |
| pushsync | Yes | Request/Response | Push chunks to responsible peers |
| pullsync | Yes | Request/Response | Sync chunks in neighborhood |

## See Also

- [Swarm API](api.md) - Protocol trait definitions
- [Differences from Bee](differences-from-bee.md) - Protocol improvements
