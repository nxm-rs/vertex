# Protocol Error Architecture

This document defines the error handling architecture for Vertex network protocols.

## Design Principles

1. **Explicit over implicit**: Every error variant must be explicitly defined
2. **Metrics-first**: All errors derive `IntoStaticStr` for automatic `LabelValue` support
3. **Flat enums**: Single-level error enums (no nesting)
4. **No escape hatches**: No `Protocol(String)` catch-alls

## Error Pattern

All protocols follow this pattern:

```rust
// error.rs
use strum::IntoStaticStr;

#[derive(Debug, thiserror::Error, IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum ProtocolError {
    // === Lifecycle errors ===

    /// Connection closed before message received.
    #[error("connection closed")]
    ConnectionClosed,

    /// Operation timeout.
    #[error("timeout")]
    Timeout,

    // === Validation errors (protocol-specific) ===

    /// Invalid field length.
    #[error("invalid length: expected {expected}, got {actual}")]
    #[strum(serialize = "invalid_length")]
    InvalidLength { expected: usize, actual: usize },

    // === Infrastructure errors ===

    /// Protobuf encoding/decoding error.
    #[error("protobuf error: {0}")]
    #[strum(serialize = "protobuf_error")]
    Protobuf(#[from] quick_protobuf_codec::Error),

    /// I/O error during stream operations.
    #[error("io error: {0}")]
    #[strum(serialize = "io_error")]
    Io(#[from] std::io::Error),
}
```

## LabelValue Trait (Automatic)

The `LabelValue` trait in `vertex-swarm-observability` has a blanket implementation:

```rust
pub trait LabelValue {
    fn label_value(&self) -> &'static str;
}

// Automatic for any type deriving IntoStaticStr
impl<T> LabelValue for T
where
    for<'a> &'a T: Into<&'static str>,
{
    fn label_value(&self) -> &'static str {
        self.into()
    }
}
```

**No manual `label()` methods needed.** Just derive `IntoStaticStr`.

Usage in metrics:

```rust
use vertex_swarm_observability::LabelValue;

counter!(
    "protocol_errors_total",
    "reason" => error.label_value()
).increment(1);
```

## Error Categories

### Lifecycle Errors

Protocol state machine failures:

| Error | Description |
|-------|-------------|
| `ConnectionClosed` | Stream closed before expected message |
| `Timeout` | Operation exceeded time limit |
| `PickerRejection` | Connection rejected by peer picker |

### Validation Errors

Message content fails semantic validation:

| Error | Description |
|-------|-------------|
| `MissingField` | Required field not present |
| `InvalidLength` | Field has wrong byte length |
| `FieldTooLong` | Field exceeds maximum |
| `InvalidSignature` | Cryptographic signature invalid |
| `InvalidMultiaddr` | Multiaddr parsing failed |
| `NetworkIdMismatch` | Peer on different network |

### Infrastructure Errors

Low-level failures from underlying libraries:

| Error | Description |
|-------|-------------|
| `Protobuf` | Wire format parsing failed |
| `Io` | Stream read/write failed |

## File Organization

```
protocol/src/
├── lib.rs           # Public exports
├── error.rs         # Protocol error enum
├── codec.rs         # Codec implementation
├── protocol.rs      # Protocol handlers
└── metrics.rs       # Metrics tracking (optional)
```

## Codec Trait Requirements

The `Codec<M, E>` from `vertex-net-codec` requires:

```rust
impl Decoder for Codec<M, E>
where
    M::DecodeError: Into<E>,
    quick_protobuf_codec::Error: Into<E>,
    E: From<std::io::Error>,
```

A flat error enum satisfies these via `#[from]` attributes:

```rust
#[derive(Debug, thiserror::Error)]
pub enum MyError {
    #[error("protobuf: {0}")]
    Protobuf(#[from] quick_protobuf_codec::Error),

    #[error("io: {0}")]
    Io(#[from] std::io::Error),

    // Validation errors...
}
```

## Strum Serialization

Use `#[strum(serialize = "...")]` to override generated names when needed:

```rust
#[derive(IntoStaticStr)]
#[strum(serialize_all = "snake_case")]
pub enum Error {
    // Generates "connection_closed"
    ConnectionClosed,

    // Override to "missing_field" (not "missing_field_0")
    #[strum(serialize = "missing_field")]
    MissingField(&'static str),

    // Override for clarity
    #[strum(serialize = "protobuf_error")]
    Protobuf(quick_protobuf_codec::Error),
}
```

## Anti-Patterns

### Manufacturing Io Errors

```rust
// BAD
.ok_or_else(|| {
    Error::Io(std::io::Error::new(
        std::io::ErrorKind::UnexpectedEof,
        "connection closed",
    ))
})

// GOOD
.ok_or(Error::ConnectionClosed)
```

### String Escape Hatch

```rust
// BAD
.map_err(|e| Error::protocol(e.to_string()))?;

// GOOD
.map_err(Error::InvalidAddress)?;
```

### Nested Error Hierarchies

```rust
// BAD - requires custom LabelValue impl
pub enum OuterError {
    Codec(InnerCodecError),
}

// GOOD - flat enum, automatic LabelValue
pub enum Error {
    NetworkIdMismatch,
    InvalidSignature(SignatureError),
    // ... all variants at same level
}
```

## Checklist for New Protocols

- [ ] Create `error.rs` with flat error enum
- [ ] Derive `thiserror::Error` and `strum::IntoStaticStr`
- [ ] Add `#[strum(serialize_all = "snake_case")]`
- [ ] Include `ConnectionClosed` variant
- [ ] Include `Protobuf(#[from] quick_protobuf_codec::Error)` variant
- [ ] Include `Io(#[from] std::io::Error)` variant
- [ ] Add protocol-specific validation errors as needed
- [ ] Use `#[strum(serialize = "...")]` for variants with parameters
- [ ] Export error type from `lib.rs`
