## Swarm protocol guidance

Vertex is a clean-room Rust implementation of the Ethereum Swarm protocol. The Go `bee` node at `/code/nxm/swarm/bee` is the dominant peer on the live network, so wire-level conformance with it is required. Its source style, error handling, and internal architecture are not.

### Bee as guidance, not gospel

Consult `bee/` for:

- Wire formats: protobuf `.proto` files under `bee/pkg/*/pb/`, stream IDs, message ordering on a stream.
- Cryptographic constants: domain separation tags, signature payload layout, hash inputs.
- Smart-contract addresses, ABIs, event signatures, postage contract semantics.
- Edge cases observed live (bin saturation thresholds, depth calculation tie-breaks).

Do NOT copy from `bee/` for:

- Storage iteration via callback closures (`func(key, value) (stop bool, err error)`). Use Rust async streams.
- Error-as-string conventions (`errors.New("invalid foo")`, `err.Error() == "..."` checks). Use flat `thiserror` enums per `docs/protocol-errors.md`.
- Ad-hoc goroutines without lifecycle. Every Vertex task is owned by a `tasks` supervisor or a `NetworkBehaviour`.
- `interface{}`-style dynamic dispatch where a trait or enum fits.
- Manual mutex-around-map patterns. Prefer `tokio::sync` primitives and actor channels.
- Tight coupling of types to storage backends (see postage; `nectar-postage` is the model).

If a bee pattern leaks into Vertex code review, the reviewer rejects it. Architectural notes about why we diverge belong in the relevant `lib.rs`/`mod.rs`, kept brief.

### V1 conformance contract

For a Vertex node to interoperate on the current testnet, the following must match bee byte-for-byte:

- Handshake (`vertex-swarm-net-handshake`): SYN/ACK message order, no headers exchange first, libp2p protocol id, signed overlay address payload (network id, nonce, signature digest), `0x99`-prefixed multiaddr block in the `bytes` field (`serialize_multiaddrs` in `vertex-swarm-peer`).
- Headered stream prelude: every non-handshake stream sends `Headers` first per `docs/swarm/protocols.md`. Headers are key/value byte pairs; ordering on the wire matches bee.
- Hive (`vertex-swarm-net-hive`): `BroadcastPeers` protobuf shape, peer record signature digest, gossip triggers fire after a successful handshake (peer activation). Per-peer liveness/RTT use stock `libp2p::ping` (`/ipfs/ping`); the reference's `/swarm/pingpong` is an operator-only diagnostic and is not implemented. Ping is a liveness signal only: the `ReachabilityTracker` `Reachable` verdict is set solely by AutoNAT v2 dial-back or a successful outbound dial to a public-scope address, never by ping or handshake success. The verdict enum (`Unreachable`/`Unknown`/`Reachable`) is deliberately kept distinct from address scope (`AddressScope::Public`/`Private`, the RFC IP ranges).
- Kademlia bin semantics: proximity order is XOR-distance leading-zero count over the 32-byte overlay address. Saturation, depth, and neighbourhood (`po >= depth`) follow book of swarm chapter 2.1 and section 3.15.
- Pricing: `AnnouncePaymentThreshold` value encoding currently follows bee's `big.Int.Bytes()` (big-endian, no leading zeros). The fixed-width 32-byte change tracked in `docs/development/bee-protocol-improvements.md` is gated behind a hardfork, not unilaterally applied.
- Pseudosettle: balance accounting direction and units match bee. No optimistic deviation pre-hardfork.
- Pushsync: forwarding rules along the chunk address path, receipt signature payload, stamp attached on the request leg.
- Retrieval: chunk address request, response with chunk bytes and stamp, price deduction.
- Postage stamps: bucket index derivation, owner signature digest (`batch_id || chunk_addr || index || timestamp`), batch contract event decoding. `nectar-postage` may verify faster but must accept the same set of valid stamps and reject the same invalid ones as bee.
- CAC and SOC chunk address derivation: BMT hash with span prefix for CAC; keccak256 of `id || owner` for SOC. These are consensus.

Add a conformance test against bee fixtures whenever touching any of the above. See `crates/swarm/net/handshake` interop vectors as the pattern.

### Where Vertex diverges deliberately

Internal infrastructure may diverge freely if it does not change wire bytes:

- `SwarmSpec` trait and the `Hive`/`HiveBuilder` concretes in `crates/swarm/spec` and `crates/swarm/builder`.
- Hardforks: any breaking change goes through `SwarmHardfork` and `ForkDigest` in `crates/swarm/forks`. The digest is exchanged in handshake so non-upgraded peers cleanly fail. Never ship a wire change without a fork gate.
- Postage caching, parallel verification, `BatchStore`/`StampValidator` traits (`nectar-postage`).
- Metrics, observability, error enums, the headered stream trait machinery (`vertex-swarm-net-headers`).
- Storage layer (`crates/storage`, `crates/swarm/localstore`) is entirely ours.

If you want to experiment with a protocol change, add a new `SwarmHardfork` variant and gate the new behaviour on its activation timestamp. Do not feature-flag wire changes with cargo features.

### Book of Swarm anchors

Path: `docs/swarm/reference/book-of-swarm.txt`. Re-read before touching:

- Topology, kademlia, proximity order: chapter 2.1.
- Swarm storage, DISC, chunk responsibility: chapter 2.2.
- Push and pull, pushsync, pullsync, forwarding: chapter 2.3.
- Bandwidth sharing and accounting: chapter 3.1, 3.2.
- Postage stamps, batches, buckets, signing: chapter 3.3.
- Neighbourhood selection, depth, redistribution game: chapter 3.4.
- Adaptive pricing: figure 3.19.
- Swarm hash, BMT, intermediate chunks, manifests, SOC: chapter 4.1.
- Feeds and epoch grid: chapter 4.3.
- PSS, trojan chunks, envelopes: chapter 4.4.
- Erasure, redundancy, recovery: chapter 5.1.

### Wire-compat shims and SWIPs

Some parts of the live Swarm wire format are not idiomatic protobuf. The reference implementation jams custom byte layouts into `bytes` fields (for example the `0x99`-prefixed multi-multiaddr block in the handshake), routes JSON inside protobuf payloads, and uses `big.Int.Bytes()` for variable-width numeric encodings where a fixed-width uint256 would be unambiguous. V1 conformance means we implement these shims byte-for-byte so peers interoperate, but every one of them is a code smell we want to retire upstream.

How to handle a wire-compat shim:

1. **Implement the shim faithfully.** The wire bytes must match the reference. Do not silently "improve" them; that breaks interop.
2. **Tag the site with a greppable marker.** Use `// BEE-COMPAT(SWIP-NN): <short reason>` at the call site, the constant, or the type that carries the quirk. If no SWIP is filed yet, use `BEE-COMPAT(TBD): ...` and file the SWIP candidate before merging. A `grep -rn "BEE-COMPAT"` across the workspace must surface every such shim. Existing sites: `crates/swarm/peers/peer/src/serde_multiaddr.rs` (the `0x99` prefix); `vertex-swarm-net-pricing` (variable-width `big.Int.Bytes()` for the payment threshold). Add this marker whenever you introduce a new one.
3. **File a SWIP candidate.** Open an issue in `nxm-rs/SWIPs` (our staging repo for upstream proposals) describing the current wire, the proposed fix, and the migration path through a Vertex hardfork. When the proposal is reviewed internally, mirror it upstream at `ethersphere/SWIPs`. Do not block Vertex work waiting for upstream review; Ethereum Swarm moves slowly.
4. **Gate the corrected wire behind a `SwarmHardfork`.** When the SWIP lands, add a `SwarmHardfork` variant that flips the encoding at activation timestamp, and select via `ForkDigest` at handshake. The shim and the corrected path coexist until peers cut over.
5. **Cross-link in code and tracker.** The `BEE-COMPAT(SWIP-NN)` marker references the issue number; the issue links back to the code site. When the hardfork ships, remove the marker.

The point of the marker is to make the technical debt visible. A reader skimming `serde_multiaddr.rs` or the pricing codec should see immediately why a custom byte layout exists and where the open SWIP lives. Reviewers reject new wire-compat shims that arrive without a `BEE-COMPAT` marker and a linked SWIP candidate.

### Protocol error rules

Summarised from `docs/protocol-errors.md`:

- Flat `thiserror` enums per protocol, derive `strum::IntoStaticStr` with `serialize_all = "snake_case"`.
- Required variants: `ConnectionClosed`, `Protobuf(#[from] quick_protobuf_codec::Error)`, `Io(#[from] std::io::Error)`.
- No `Protocol(String)` catch-all, no nested error enums, no manufactured `io::Error` for logical failures.
- Variants with fields need `#[strum(serialize = "...")]` to avoid `_0` suffixes on the metric label.
- `LabelValue` is automatic via the blanket impl; do not write `fn label()` by hand.

### Naming and terminology

- `multiaddrs`, never `underlay`. Underlay stays in bee. Applies to code, comments, docs, commits, PR bodies.
- `OverlayAddress` is the 32-byte network identity. Do not call it "swarm address" (the book uses "swarm address" generically; in code we are specific).
- `chunk` is the unit of storage. `CAC` is content-addressed chunk (BMT-hash addressed). `SOC` is single-owner chunk (keccak of id and owner). `feed` is the SOC-based mutable construct from book chapter 4.3.
- `neighbourhood` (British spelling, matches book and existing docs). `bin` for kademlia bucket, `po` for proximity order. `depth` is the local node's connectivity boundary (`NeighborhoodDepth`, supply-side, local-only); `storage radius` is a node's reserve / storage-responsibility radius (`StorageRadius`, demand-side, valid for local or remote nodes). The two can diverge (Book of Swarm 3.3.4), so they are distinct types and are not interchangeable.
- `PeerId` is the libp2p identity; `OverlayAddress` is the Swarm identity. Their boundary is enforced in `vertex-swarm-peer`.
- `batch` and `stamp` are distinct: a batch is purchased on-chain, a stamp is an attestation derived from it for a specific chunk.
