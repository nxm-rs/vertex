# Differences from Bee

This document tracks architectural and design differences between Vertex and the reference [Bee](https://github.com/ethersphere/bee) implementation.

## Network Specification

### SwarmSpec Trait

Vertex introduces a `SwarmSpec` trait that defines network identity and protocol rules. This separates *what network* a node connects to from *how the node operates*.

**SwarmSpec provides:**
- Network identity (ID, name, underlying chain)
- Bootstrap nodes for peer discovery
- Hardfork activation schedule
- Token contract address

**SwarmSpec excludes (by design):**
- Storage capacity and policies
- Bandwidth pricing
- Cache strategies

This separation allows light clients and full nodes to share the same spec while differing in operational parameters.

### Hardfork Support

Vertex has first-class support for protocol upgrades via hardforks:

- **`SwarmHardforks`** - Manages fork activation conditions (timestamp-based)
- **`SwarmHardfork`** - Enum of known protocol versions (e.g., `Accord`)
- **`ForkDigest`** - 4-byte keccak256-based identifier for verifying peer compatibility during handshake

Peers exchange `ForkDigest` values during connection to ensure they're running compatible protocol versions. The digest incorporates network ID, genesis timestamp, and active fork timestamps.

```rust
// Check if a fork is active
if spec.is_fork_active_at_timestamp(SwarmHardfork::Accord, now) {
    // Post-Accord protocol behavior
}

// Get next scheduled fork for handshake
if let Some(next_fork) = spec.next_fork_timestamp(now) {
    // Communicate upcoming protocol change to peer
}
```

Bee does not have explicit hardfork management infrastructure.

## Concrete Implementations

### Hive

`Hive` is the concrete implementation of `SwarmSpec` for mainnet, testnet, and development networks. Pre-configured specs are available via:

- `init_mainnet()` - Production network on Gnosis Chain
- `init_testnet()` - Test network on Sepolia
- `init_dev()` - Local development with auto-generated network ID

Custom networks can be built with `HiveBuilder`.

## Postage Stamp Verification

The `nectar-postage` crate provides optimized postage stamp verification with several performance improvements over Bee.

### Cached Public Key Verification (~10x faster)

Bee performs ECDSA public key recovery for every stamp verification. nectar-postage allows caching the owner's public key after the first stamp in a batch, then using direct signature verification for subsequent stamps:

```rust
// First stamp: recover and cache the public key
let pubkey = first_stamp.recover_pubkey(&first_address)?;

// Subsequent stamps: ~10x faster verification with cached pubkey
for (stamp, addr) in remaining_stamps {
    stamp.verify_with_pubkey(&addr, &pubkey)?;
}
```

This is particularly beneficial when validating many chunks from the same batch (common in retrieval and push-sync operations).

### Parallel Verification

The `parallel` feature enables rayon-based parallel verification across all CPU cores:

```rust
use nectar_postage::parallel::{verify_stamps_parallel, verify_stamps_parallel_with_pubkey};

// Parallel verification with full recovery
let results = verify_stamps_parallel(&stamps_and_addresses);

// Parallel verification with cached pubkey (fastest)
let pubkey = first_stamp.recover_pubkey(&first_address)?;
let results = verify_stamps_parallel_with_pubkey(&stamps_and_addresses, &pubkey);
```

### Structural Validation Separation

nectar-postage separates structural validation (batch existence, expiry, index bounds, bucket matching) from cryptographic verification. This allows quick rejection of invalid stamps before expensive signature operations:

```rust
// Quick structural check (no crypto)
validator.validate_structure(&stamp, &address).await?;

// Full validation including signature
validator.validate(&stamp, &address).await?;
```

### no_std Support

Core types (`Stamp`, `Batch`, `StampIndex`, `StampDigest`) work without the standard library, enabling use in constrained environments. Storage and event handling require the `std` feature.

### k256 Precomputed Tables

nectar-postage enables k256's `precomputed-tables` feature for faster ECDSA operations, trading ~30KB binary size for improved verification speed.

### Architecture

| Component | Purpose |
|-----------|---------|
| `Stamp` | Immutable stamp with signature verification methods |
| `Batch` | Batch parameters with validation helpers |
| `StampValidator` | Trait for custom validation strategies |
| `StoreValidator` | Combines `BatchStore` lookup with validation |
| `BatchStore` | Async batch persistence trait |
| `BatchEventHandler` | Process blockchain events |

Bee's postage implementation is tightly coupled to its internal storage. nectar-postage provides composable traits that can be implemented for different storage backends.
