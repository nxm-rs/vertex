# Design Proposal: Chunk Size as Const Generic

## Summary

Make chunk body size a compile-time const generic that flows through the type system from `ChunkTypeSet` to `SwarmSpec`, enabling compile-time guarantees and eliminating runtime size lookups.

## Current State

### Constants (nectar-primitives)

```rust
// bmt/constants.rs
pub(crate) const HASH_SIZE: usize = 32;
pub(crate) const BRANCHES: usize = 128;
pub const MAX_DATA_LENGTH: usize = BRANCHES * SEGMENT_SIZE; // 4096
```

### ChunkTypeSet (nectar-primitives)

```rust
pub trait ChunkTypeSet: Send + Sync + 'static {
    fn supports(type_id: ChunkTypeId) -> bool;
    fn deserialize(bytes: &[u8]) -> Result<AnyChunk>;
    fn supported_types() -> &'static [ChunkTypeId];
}

pub struct StandardChunkSet;
impl ChunkTypeSet for StandardChunkSet { ... }
```

### SwarmSpec (vertex-swarmspec)

```rust
pub trait SwarmSpec: Send + Sync + Unpin + Debug + 'static {
    type ChunkSet: ChunkTypeSet;

    fn chunk_size(&self) -> usize;  // Runtime method
    // ...
}

impl SwarmSpec for Hive {
    type ChunkSet = StandardChunkSet;

    fn chunk_size(&self) -> usize {
        self.chunk_size  // Stored as field
    }
}
```

## Proposed Design

### Core Idea

Single const generic `BODY_SIZE` on `ChunkTypeSet`, defaulting to 4096:

```rust
pub trait ChunkTypeSet<const BODY_SIZE: usize = 4096>: Send + Sync + 'static {
    fn supports(type_id: ChunkTypeId) -> bool;
    fn deserialize(bytes: &[u8]) -> Result<AnyChunk<BODY_SIZE>>;
    fn supported_types() -> &'static [ChunkTypeId];

    /// Body size in bytes (derived from const generic)
    const BODY_SIZE: usize = BODY_SIZE;
}
```

### Layer 1: nectar-primitives

**bmt/constants.rs** - Add default constant:

```rust
pub const DEFAULT_BODY_SIZE: usize = 4096;  // 128 * 32
```

**chunk/traits.rs** - Parameterize core traits:

```rust
pub trait Chunk<const BODY_SIZE: usize = 4096>: Send + Sync + 'static {
    type Header: ChunkHeader;

    fn address(&self) -> &ChunkAddress;
    fn header(&self) -> &Self::Header;
    fn data(&self) -> &Bytes;

    fn size(&self) -> usize {
        self.header().bytes().len() + self.data().len()
    }

    /// Maximum data size for this chunk type
    const MAX_DATA_SIZE: usize = BODY_SIZE;
}

pub trait BmtChunk<const BODY_SIZE: usize = 4096>: Chunk<BODY_SIZE> {
    fn span(&self) -> u64;
}
```

**chunk/content.rs** - Parameterize ContentChunk:

```rust
pub struct ContentChunk<const BODY_SIZE: usize = 4096> {
    address: ChunkAddress,
    header: ContentHeader,
    data: Bytes,
}

impl<const BODY_SIZE: usize> ContentChunk<BODY_SIZE> {
    pub fn new(data: &[u8]) -> Result<Self> {
        if data.len() > BODY_SIZE {
            return Err(ChunkError::data_too_large(data.len(), BODY_SIZE));
        }
        // ...
    }
}

impl<const BODY_SIZE: usize> Chunk<BODY_SIZE> for ContentChunk<BODY_SIZE> { ... }
```

**chunk/any_chunk.rs** - Parameterize AnyChunk:

```rust
pub enum AnyChunk<const BODY_SIZE: usize = 4096> {
    Content(ContentChunk<BODY_SIZE>),
    SingleOwner(SingleOwnerChunk<BODY_SIZE>),
}
```

**chunk/chunk_type_set.rs** - Parameterize ChunkTypeSet:

```rust
pub trait ChunkTypeSet<const BODY_SIZE: usize = 4096>: Send + Sync + 'static {
    const BODY_SIZE: usize = BODY_SIZE;

    fn supports(type_id: ChunkTypeId) -> bool;
    fn deserialize(bytes: &[u8]) -> Result<AnyChunk<BODY_SIZE>>;
    fn supported_types() -> &'static [ChunkTypeId];
}

pub struct StandardChunkSet<const BODY_SIZE: usize = 4096>;

impl<const BODY_SIZE: usize> ChunkTypeSet<BODY_SIZE> for StandardChunkSet<BODY_SIZE> {
    fn deserialize(bytes: &[u8]) -> Result<AnyChunk<BODY_SIZE>> {
        // ...
    }
    // ...
}
```

### Layer 2: vertex-swarmspec

**api.rs** - SwarmSpec uses ChunkSet's const:

```rust
pub trait SwarmSpec: Send + Sync + Unpin + Debug + 'static {
    /// Body size for chunks on this network
    const BODY_SIZE: usize = 4096;

    /// The set of chunk types supported by this network
    type ChunkSet: ChunkTypeSet<{ Self::BODY_SIZE }>;

    // Other methods...

    /// Returns the chunk body size (derived from const)
    fn chunk_size(&self) -> usize {
        Self::BODY_SIZE
    }
}
```

**spec.rs** - Hive implementation:

```rust
// For standard networks (mainnet, testnet)
impl SwarmSpec for Hive {
    const BODY_SIZE: usize = 4096;
    type ChunkSet = StandardChunkSet<4096>;

    // chunk_size() uses default impl
}

// For custom networks with different sizes, use a generic Hive
pub struct Hive<const BODY_SIZE: usize = 4096> {
    // fields...
}

impl<const BODY_SIZE: usize> SwarmSpec for Hive<BODY_SIZE> {
    const BODY_SIZE: usize = BODY_SIZE;
    type ChunkSet = StandardChunkSet<BODY_SIZE>;
}
```

### Type Aliases for Convenience

```rust
// nectar-primitives
pub type DefaultChunk = ContentChunk<4096>;
pub type DefaultChunkSet = StandardChunkSet<4096>;

// vertex-swarmspec
pub type MainnetHive = Hive<4096>;
```

## Migration Path

### Phase 1: Add const generic with default (non-breaking)

1. Add `const BODY_SIZE: usize = 4096` to traits
2. Existing code continues to work (uses default)
3. All type aliases point to `<4096>` variants

### Phase 2: Update downstream consumers

1. Update `SwarmSpec` to use const from `ChunkSet`
2. Update storage layers to be generic over body size
3. Update networking to validate chunk sizes at compile time

### Phase 3: Remove runtime chunk_size field

1. Remove `chunk_size` field from `Hive`
2. Remove `chunk_size()` method or make it return `Self::BODY_SIZE`
3. `HiveBuilder::chunk_size()` becomes a type-level choice

## Trade-offs

### Benefits

- **Compile-time safety**: Mismatched chunk sizes caught at compile time
- **Zero runtime overhead**: No size field storage or lookup
- **Type-level documentation**: Chunk size is visible in type signatures
- **Optimization opportunities**: Compiler can optimize fixed-size operations

### Costs

- **Type complexity**: More generic parameters in signatures
- **Monomorphization**: Separate code generated per size (minimal impact)
- **Less runtime flexibility**: Can't change chunk size without recompilation

### Why single const generic?

Starting with just `BODY_SIZE` keeps the design simple. If needed later, we could decompose into `BRANCHES` and `HASH_SIZE`:

```rust
// Future refinement (not proposed now)
pub trait ChunkTypeSet<
    const BRANCHES: usize = 128,
    const HASH_SIZE: usize = 32,
>: Send + Sync + 'static {
    const BODY_SIZE: usize = BRANCHES * HASH_SIZE;
}
```

This can be done as a follow-up refactor without breaking the API.

## Design Decisions

1. **BMT Hasher**: Yes, `Hasher` should be generic over body size. This ensures the BMT computation is consistent with the chunk type.

2. **Span handling**: Body size does NOT affect span validation. Span encodes actual data length which can be less than body size.

3. **Network compatibility**: Chunks with mismatched body sizes will fail BMT calculation, providing natural protocol-level incompatibility. The const generic makes this explicit at compile time for same-binary guarantees.

## Implementation Order

1. `nectar-primitives/bmt/constants.rs` - Add `DEFAULT_BODY_SIZE`
2. `nectar-primitives/bmt/hasher.rs` - Parameterize `Hasher<const BODY_SIZE: usize>`
3. `nectar-primitives/chunk/traits.rs` - Add const generic to `Chunk`, `BmtChunk`
4. `nectar-primitives/chunk/content.rs` - Parameterize `ContentChunk`
5. `nectar-primitives/chunk/single_owner.rs` - Parameterize `SingleOwnerChunk`
6. `nectar-primitives/chunk/any_chunk.rs` - Parameterize `AnyChunk`
7. `nectar-primitives/chunk/chunk_type_set.rs` - Parameterize `ChunkTypeSet`
8. `nectar-primitives/lib.rs` - Add type aliases, update exports
9. `vertex-swarmspec/api.rs` - Update `SwarmSpec` trait
10. `vertex-swarmspec/spec.rs` - Update `Hive` implementation
11. Downstream crates - Update as needed
