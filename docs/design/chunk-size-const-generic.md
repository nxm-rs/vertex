# Design Proposal: Chunk Size as Const Generic

## Summary

Make chunk body size a compile-time const generic that flows through the type system from `ChunkTypeSet` to `SwarmSpec`, enabling compile-time guarantees and eliminating runtime size lookups.

## Current State

### Constants (nectar-primitives)

The BMT constants module defines `HASH_SIZE` (32), `BRANCHES` (128), and the derived `MAX_DATA_LENGTH` (4096, computed as `BRANCHES * SEGMENT_SIZE`). `HASH_SIZE` and `BRANCHES` have crate-level visibility, while `MAX_DATA_LENGTH` is public.

### ChunkTypeSet (nectar-primitives)

`ChunkTypeSet` is currently a trait with static methods `supports(ChunkTypeId) -> bool`, `deserialize(&[u8]) -> Result<AnyChunk>`, and `supported_types() -> &'static [ChunkTypeId]`. `StandardChunkSet` is the concrete implementation. Neither the trait nor the struct is parameterized by body size.

### SwarmSpec (vertex-swarm-spec)

`SwarmSpec` is a trait with an associated type `ChunkSet: ChunkTypeSet` and a runtime method `chunk_size(&self) -> usize`. The `Hive` implementation stores chunk size as a field and returns it from `chunk_size()`.

## Proposed Design

### Core Idea

Introduce a single const generic `BODY_SIZE` on `ChunkTypeSet`, defaulting to 4096. This const generic then propagates through all chunk types and up to `SwarmSpec`.

### Layer 1: nectar-primitives

The following table summarises the changes in nectar-primitives:

| File | Change |
|------|--------|
| `bmt/constants.rs` | Add `DEFAULT_BODY_SIZE: usize = 4096` |
| `chunk/traits.rs` | Add `const BODY_SIZE: usize = 4096` to `Chunk` and `BmtChunk` traits; add associated const `MAX_DATA_SIZE` defaulting to `BODY_SIZE` |
| `chunk/content.rs` | Parameterize `ContentChunk` with `const BODY_SIZE: usize = 4096`; validate data length against `BODY_SIZE` in the constructor |
| `chunk/any_chunk.rs` | Parameterize `AnyChunk` with `const BODY_SIZE: usize = 4096`; variants wrap `ContentChunk<BODY_SIZE>` and `SingleOwnerChunk<BODY_SIZE>` |
| `chunk/chunk_type_set.rs` | Add `const BODY_SIZE: usize = 4096` to `ChunkTypeSet` trait and `StandardChunkSet` struct; `deserialize` returns `AnyChunk<BODY_SIZE>` |

All existing code continues to compile using the default value.

### Layer 2: vertex-swarm-spec

The `SwarmSpec` trait gains an associated const `BODY_SIZE: usize = 4096`. Its associated type bound becomes `ChunkSet: ChunkTypeSet<{ Self::BODY_SIZE }>`. The `chunk_size()` method gets a default implementation returning `Self::BODY_SIZE`.

The `Hive` struct becomes parameterized as `Hive<const BODY_SIZE: usize = 4096>`, implementing `SwarmSpec` with `BODY_SIZE` and `ChunkSet = StandardChunkSet<BODY_SIZE>`.

### Type Aliases for Convenience

For ergonomics, type aliases are provided: `DefaultChunk` for `ContentChunk<4096>`, `DefaultChunkSet` for `StandardChunkSet<4096>`, and `MainnetHive` for `Hive<4096>`.

## Migration Path

### Phase 1: Add const generic with default (non-breaking)

Add `const BODY_SIZE: usize = 4096` to all relevant traits and structs. Because the default matches the current hard-coded value, all existing code continues to compile without changes. Introduce type aliases pointing to the `<4096>` variants.

### Phase 2: Update downstream consumers

Update `SwarmSpec` to derive its body size from the `ChunkSet` const. Update storage layers to be generic over body size. Update networking to validate chunk sizes at compile time.

### Phase 3: Remove runtime chunk_size field

Remove the `chunk_size` field from `Hive`. Either remove the `chunk_size()` method or have it return `Self::BODY_SIZE`. The builder's `chunk_size()` setting becomes a type-level choice rather than a runtime value.

## Trade-offs

### Benefits

- **Compile-time safety**: Mismatched chunk sizes caught at compile time
- **Zero runtime overhead**: No size field storage or lookup
- **Type-level documentation**: Chunk size is visible in type signatures
- **Optimization opportunities**: Compiler can optimize fixed-size operations

### Costs

- **Type complexity**: More generic parameters in signatures
- **Monomorphization**: Separate code generated per size (minimal impact)
- **Less runtime flexibility**: Cannot change chunk size without recompilation

### Why single const generic?

Starting with just `BODY_SIZE` keeps the design simple. If needed later, decomposing into separate `BRANCHES` and `HASH_SIZE` const generics (with `BODY_SIZE` derived as their product) can be done as a follow-up refactor without breaking the API.

## Design Decisions

1. **BMT Hasher**: Yes, `Hasher` should be generic over body size. This ensures the BMT computation is consistent with the chunk type.

2. **Span handling**: Body size does NOT affect span validation. Span encodes actual data length which can be less than body size.

3. **Network compatibility**: Chunks with mismatched body sizes will fail BMT calculation, providing natural protocol-level incompatibility. The const generic makes this explicit at compile time for same-binary guarantees.

## Implementation Order

The implementation proceeds bottom-up through the dependency graph. First, add `DEFAULT_BODY_SIZE` to `nectar-primitives/bmt/constants.rs`. Then parameterize the BMT `Hasher` in `nectar-primitives/bmt/hasher.rs`. Next, add the const generic to the `Chunk` and `BmtChunk` traits in `nectar-primitives/chunk/traits.rs`, followed by the concrete types: `ContentChunk`, `SingleOwnerChunk`, `AnyChunk`, and `ChunkTypeSet` (along with `StandardChunkSet`). Update `nectar-primitives/lib.rs` with type aliases and updated exports. Then update `vertex-swarm-spec/api.rs` (`SwarmSpec` trait) and `vertex-swarm-spec/spec.rs` (`Hive` implementation). Finally, update downstream crates as needed.
