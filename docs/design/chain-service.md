# Chain service

The chain (Ethereum RPC access) is a node-wide service, not a capability owned by any single consumer. A storer reads the price oracle and plays redistribution rounds; the SWAP settlement service deploys and cashes chequebooks; a future staking flow talks to the stake registry. All of them want one shared view of the chain, one transaction sender that owns nonce ordering, and one place that decides whether the chain is even enabled.

This note records how that service is structured and why. It supersedes the earlier approach in #181, which embedded an alloy-provider-backed chequebook backend inside the chequebook crate behind a cargo feature. That put the provider in the wrong place: chain access leaked into a settlement primitive, and the cone purity of a default light node depended on a feature flag staying off rather than on a crate boundary.

## Goal

A light node is the default node type, and a client node must be able to run on `wasm32-unknown-unknown`. The default and wasm builds must contain zero chain transport in their dependency cone: no `alloy-provider`, no `alloy-contract`, no `reqwest`, no native-tls, no tokio net features. A storer, by contrast, requires the chain. The structure has to satisfy both from the same workspace without a consumer opting into a feature to stay lean.

## Two-crate split

The chain service is two crates.

`vertex-chain-api` (crate at `crates/chain/api/`) is the wasm-safe trait and data surface. It is traits and plain data only. It depends on `alloy-primitives`, `alloy-sol-types`, `nectar-contracts`, `async-trait`, `auto_impl`, `thiserror`, `strum`, `bytes`, and the pure cheque codec in `vertex-swarm-bandwidth-chequebook` for the `SignedCheque` type. It pulls no transport. Consumers and the node builder depend only on this crate.

`vertex-chain-service` (later PR, native-only) holds the `alloy-provider`-backed implementations of those traits: the reader over a real RPC transport, the transaction sender with nonce management and fee bumping, the chequebook backend over the `nectar_contracts` bindings, and the health probe. It is an optional dependency of `bin/vertex` and of the storer builder path, and of nothing else.

## Cone purity by crate boundary, not by feature

The provider lives in a separate crate that library code never names. Because nothing in the default or wasm cone has `vertex-chain-service` in its dependency graph, those builds cannot pull a provider in even by accident. There is no feature to forget to turn off. A library crate that needs chain access takes a `vertex-chain-api` trait object (`Arc<dyn ChainReader>`, `Arc<dyn TransactionSender>`, `Arc<dyn ChequebookChain>`) and never sees the implementation. The binary and the storer builder are the only places that select the concrete service and inject it.

This is verified the usual way: `cargo build -p vertex-chain-api --target wasm32-unknown-unknown` builds, and `cargo tree` over the api crate shows no provider crates.

## Trait surface

`vertex-chain-api` defines, all object-safe via `#[async_trait]` so they inject as `Arc<dyn ...>`:

- `ChainReader`: read-only access. Chain id, head block number, block timestamp, native balance, `eth_call`, and log queries.
- `ChainHealth: ChainReader`: adds `is_synced(max_delay)` so a node can refuse time-sensitive on-chain games while the transport lags the network head.
- `TransactionSender`: `send`, `confirm`, a default `send_and_confirm`, plus `resend`, `cancel`, and `recover_pending` for replacement and restart recovery.
- `ChequebookChain`: the consumer-facing trait the SWAP settlement service injects. Balance and payout reads keyed by chequebook address, factory deploy, and the two cashout paths over a `SignedCheque`.

Data types: `TxRequest` carries the caller's intent and gas bounds as typed fields (`gas_limit`, `min_gas_limit`, `tip_boost_percent`), not values smuggled through a context side channel; `TxReceipt` is a confirmed-transaction summary; `LogFilter` is a small transport-agnostic query; `ChainConfig` is the contract address book plus chain id, with constructors that read the canonical `nectar_contracts` deployment constants. Errors are `thiserror` enums (`ProviderError`, `TxError`, `ChainError`) with `strum::IntoStaticStr` so each variant maps to a metric `reason` label; `TxError` carries `ProviderError` through `#[from]`.

`DisabledChain` is a pure zero-implementation of every trait that returns `ProviderError::Disabled`. A chain-off node injects it so consumers wired for the chain surface compile and run with a clear typed answer instead of a panic or an `Option` threaded through every call.

## Chequebook stays a pure codec

`vertex-swarm-bandwidth-chequebook` remains a pure, wasm-safe cheque codec: cheque types, EIP-712 signing-hash derivation, signer recovery, and the byte-exact wire JSON. It does not embed a provider. To keep it wasm-safe it takes the settlement chain id as a `u64` for EIP-712 domain construction rather than depending on `vertex-swarm-spec`, and it depends on `alloy-primitives` with the `k256` feature for signer recovery rather than on a full signer crate. The settlement service consumes `ChequebookChain` from `vertex-chain-api`; the codec and the service agree on the shared `SignedCheque` type.

## Node-type to component matrix

| Node type | Chain access |
|---|---|
| Bootnode | None. `DisabledChain` or no chain consumer at all. |
| Client, light (default) | None. Wasm cone stays provider-free. |
| Client, wasm | None. Provider crates excluded by target. |
| Storer | Required. `vertex-chain-service` injected by the storer builder. |
| Client with SWAP, native | Chain enabled behind a binary-level feature; the binary selects the service. |

Wire bytes are never gated by a cargo feature. The presence or absence of the chain service is a node-configuration choice realized through dependency selection and trait injection, not a protocol fork.

## PR stack

1. `vertex-chain-api`: the wasm-safe traits and data crate. This PR. Also makes `vertex-swarm-bandwidth-chequebook` genuinely wasm-safe (drops its spec dependency, wires the `getrandom` browser backend for the secp256k1 stack) so the api crate can depend on it without dragging a native cone into wasm.
2. `vertex-chain-service`: the native-only provider-backed implementations.
3. Builder and binary wiring: inject the service for the storer and for the native SWAP client; inject `DisabledChain` elsewhere.
4. Settlement and redistribution consumers move onto the trait objects.
