# Accounting and settlement

The accounting layer separates the ledger, the settlement mechanisms, and how they are selected.

## Ledger

`Accounting<C, I>` tracks per-peer balances in accounting units (`Au`, signed; positive means the peer owes us). It owns the reservation lifecycle (`prepare_receive`/`prepare_provide` reserve, commit on apply, release on drop), the payment and disconnect thresholds, affordability via `PeerAffordability`, and breach reporting through `PeerReporter`. It is mechanism-agnostic and generic only over its config and identity.

## Settlement

Two roles on different debt bases, not a pipeline. Soft accounting forgives total debt over time at the configured refresh rate; it is always on for client and storer nodes, realised on the wire by the pseudosettle protocol. Monetary settlement (SWAP) settles only originated debt and defaults on for storers, which provide maximum support, and off for clients; `--swap` overrides either way, so a client (including a wasm client) can opt in. There is no runtime mode enum. The SWAP cheque-exchange path is built to compile for wasm; only on-chain cashout is native.

## Wire conformance

The dominant peer on the live network is the Go reference node, so these are interop-critical and carry conformance vectors. Pseudosettle `Payment`/`PaymentAck` carry the amount as minimal big-endian unsigned bytes; the ack echoes the accepted amount (which may be less than requested) and a server timestamp in seconds, and the payer credits the accepted amount. Pricing announces the payment threshold on every connection. SWAP cheques are EIP-712 signed (`Chequebook` domain, version 1.0, chain id 100) and travel as a JSON object inside a protobuf bytes field, with cumulative payout as a U256 decimal string; exchange rate and deduction ride as stream headers. The refresh allowance and overdraft maths cap elapsed at one second, matching the reference.

## Security invariants

The refresh allowance is bounded by genuine elapsed wall-clock since accounting began for a peer, never the absolute clock, and never resets to unbounded on reconnect. A peer is refused service once its debt to us crosses the payment threshold. Pseudosettle credits at most the offered amount. Balance arithmetic saturates rather than wrapping. Uncashed SWAP cheques are bounded per peer until on-chain cashing confirms.

## Deferred

On-chain SWAP cashout and deploy, persisted batched cheque cashing, dynamic payment-threshold growth, and the hardening backlog are tracked separately.
