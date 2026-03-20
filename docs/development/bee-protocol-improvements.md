# Recommended Bee Protocol Improvements

This document tracks protocol-level changes that would improve interoperability and type safety in the Swarm network. These are suggestions for upstream Bee changes.

## Pricing Protocol

**File**: `bee/pkg/pricing/pb/pricing.proto`

The following table compares the current and recommended protobuf message formats for `AnnouncePaymentThreshold`:

| Aspect | Current | Recommended |
|--------|---------|-------------|
| Field type | `bytes` | `bytes` (fixed 32 bytes, big-endian uint256) |
| Encoding | Go's `big.Int.Bytes()` (big-endian, no leading zeros) | Fixed-width 32-byte big-endian encoding |
| Determinism | Implementation-specific; depends on Go's `big.Int` serialization behaviour | Unambiguous across implementations |

**Issue**: Using `bytes` with Go's `big.Int` serialization is ambiguous. The wire format depends on Go's `big.Int.Bytes()` behaviour (big-endian, no leading zeros), which is implementation-specific. Alternatively, explicit encoding rules should be defined in the protocol specification.

**Rationale**:
- Current practical values fit in `u64` (max ~108,000,000)
- Using `uint256` future-proofs for larger values
- Fixed-width encoding is unambiguous across implementations
- Aligns with Ethereum's native 256-bit integer type

## See Also

- [Differences from Bee](../swarm/differences-from-bee.md) - Architectural differences
- [Protocols](../swarm/protocols.md) - Protocol patterns
