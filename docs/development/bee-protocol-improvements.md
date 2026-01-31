# Recommended Bee Protocol Improvements

This document tracks protocol-level changes that would improve interoperability and type safety in the Swarm network. These are suggestions for upstream Bee changes.

## Pricing Protocol

**File**: `bee/pkg/pricing/pb/pricing.proto`

**Current**:
```protobuf
message AnnouncePaymentThreshold {
  bytes PaymentThreshold = 1;
}
```

**Issue**: Using `bytes` with Go's `*big.Int` serialization is ambiguous. The wire format depends on Go's `big.Int.Bytes()` behavior (big-endian, no leading zeros), which is implementation-specific.

**Recommendation**: Use a fixed-width type for deterministic encoding:
```protobuf
message AnnouncePaymentThreshold {
  bytes PaymentThreshold = 1;  // Fixed 32 bytes, big-endian uint256
}
```

Or define explicit encoding rules in the protocol specification.

**Rationale**:
- Current practical values fit in `u64` (max ~108,000,000)
- Using `uint256` future-proofs for larger values
- Fixed-width encoding is unambiguous across implementations
- Aligns with Ethereum's native 256-bit integer type

## See Also

- [Differences from Bee](../swarm/differences-from-bee.md) - Architectural differences
- [Protocols](../swarm/protocols.md) - Protocol patterns
