# Chain service

The chain (Ethereum RPC access) is a node-wide service, not a capability owned by any single consumer. A storer reads the price oracle and plays redistribution rounds; the SWAP settlement service deploys and cashes chequebooks; a future staking flow talks to the stake registry. All of them want one shared view of the chain and one transaction sender that owns nonce ordering.

This note records how that service is structured and why.

## Build on alloy, do not wrap it

Alloy already provides everything a chain consumer needs: an `alloy_provider::Provider` reads state, runs `eth_call`, queries logs, submits transactions, and confirms them, with its fillers handling nonce selection, gas estimation, and fee pricing. Vertex shares one provider rather than defining its own reader, sender, or receipt types over the top. Re-describing alloy's surface in a parallel trait hierarchy is duplication, not abstraction.

Alloy providers run on `wasm32-unknown-unknown` with the right transport, so a client node, including the wasm client, can use the same provider as a native node. The chain code stays wasm-compatible by careful feature selection: depend on `alloy-provider` with `default-features = false` so the `Provider` trait comes in without reqwest or native TLS, and let the consumer pick a transport. There is no separate "chain-free" cone to protect; there is one provider type, and the build that needs no chain simply holds no provider.

## What the chain crate adds

`vertex-chain-api` (crate at `crates/chain/api/`) is a thin layer over alloy. It holds only the parts alloy does not cover for a Swarm node, and nothing that alloy already does:

- `ChainConfig`: the contract address book (chequebook factory, BZZ token, price oracle) plus the settlement chain, keyed on `alloy_chains::NamedChain`. Constructors read the canonical `nectar_contracts` deployment constants for mainnet (Gnosis) and testnet (Sepolia). The chain is a `NamedChain`, not a bare integer, so the EIP-155 id, the chain name, and helper-set membership all come from one type.
- `ChainError` and `TxError`: typed errors that carry alloy's own `TransportError` and `PendingTransactionError` through `#[from]` rather than flattening them into strings, with `strum::IntoStaticStr` discriminants for `reason` metric labels.
- `ProviderExt`: an extension trait on `alloy_provider::Provider<Ethereum>` with a blanket impl, adding the pending-transaction operations alloy has no built-in for: `resend` (rebuild a stuck transaction at the same nonce with a bumped fee) and `cancel` (replace it with a zero-value self-send). Recovery of transactions left pending across a restart is deliberately not here: a provider holds no record of what a previous run broadcast, so that is application-persisted state, and the owning component reconstructs the hashes and calls `resend` or `cancel` on each.
- `TxRequest`: a newtype over `alloy_rpc_types_eth::TransactionRequest` that attaches a `&'static str` description for logs and metrics. It derefs to the inner request, so all of alloy's builder methods and fillers apply directly.

A consumer that needs chain access takes a shared `alloy_provider::Provider` (and `ChainConfig` for the addresses) and calls it directly, using `ProviderExt` and `TxRequest` where they help. A node with no chain configured holds no provider.

## Chequebook stays a pure codec

`vertex-swarm-bandwidth-chequebook` remains a pure, wasm-safe cheque codec: cheque types, EIP-712 signing-hash derivation, signer recovery, and the wire JSON. It does not embed a provider. The settlement chain is passed in as an `alloy_chains::NamedChain` for EIP-712 domain construction rather than depending on the network spec, so the codec names the chain rather than a magic number. It depends on `alloy-primitives` with the `k256` feature for signer recovery rather than on a full signer crate.

The on-chain chequebook client (deploy, cashout, balance reads over the `nectar_contracts` bindings) is an implementation detail of the SWAP settlement service. It lives in a dedicated chequebook chain crate (`vertex-swarm-bandwidth-chequebook` chain client), not in the generic chain crate. The chain crate knows nothing about chequebook semantics.

## Node-type to chain access

| Node type | Chain access |
|---|---|
| Bootnode | None. No provider. |
| Client, light (default) | None. No provider. |
| Client, wasm | Optional. Same alloy provider over a wasm-compatible transport when enabled. |
| Storer | Required. A provider injected by the storer builder. |
| Client with SWAP | Required. A provider injected for the settlement service. |

The presence or absence of chain access is a node-configuration choice realized through whether a provider is constructed, not a protocol fork. Wire bytes are never gated by a cargo feature.
