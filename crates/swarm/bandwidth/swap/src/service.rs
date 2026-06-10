//! Swap service actor (runs in its own tokio task).
//!
//! The service owns the cheque-exchange state machine: it issues signed cheques
//! when a peer's debt crosses the payment threshold, validates cheques received
//! from peers, and credits the accounting balances accordingly. Cheque exchange
//! is fully chain-free. Cashing a received cheque on-chain is optional and gated
//! behind the `swap-chequebook` feature.

use std::collections::HashMap;
use std::future::Future;
use std::sync::Arc;

use alloy_chains::NamedChain;
use alloy_primitives::{Address, U256};
use alloy_signer::SignerSync;
use tokio::sync::{mpsc, oneshot};
use tracing::{debug, warn};
use vertex_swarm_api::{Direction, SwarmBandwidthAccounting, SwarmPeerBandwidth};
use vertex_swarm_bandwidth_chequebook::{Cheque, ChequeExt, SignedCheque};
use vertex_swarm_node::{ClientCommand, SwapEvent};
use vertex_swarm_primitives::OverlayAddress;
use vertex_tasks::{GracefulShutdown, SpawnableTask};

use crate::error::SwapSettlementError;

/// Per-peer SWAP identity learned from the swap handshake.
///
/// The beneficiary is the address a cheque we issue to this peer must pay; the
/// issuer is the chequebook owner whose key must sign cheques the peer sends us.
/// Populated through [`SwapCommand::SetPeerInfo`] by the node layer (handshake
/// wiring is the builder's job).
#[derive(Debug, Clone, Copy)]
pub struct PeerSwapInfo {
    /// The peer's beneficiary address (where cheques we issue are paid).
    pub beneficiary: Address,
    /// The peer's chequebook issuer (signs cheques the peer sends us).
    pub issuer: Address,
}

/// Commands from the handle to the service.
pub enum SwapCommand {
    /// Request settlement with a peer.
    Settle {
        /// The peer to settle with.
        peer: OverlayAddress,
        /// The amount to settle, in accounting units.
        amount: u64,
        /// Channel to send the result.
        response_tx: oneshot::Sender<Result<u64, SwapSettlementError>>,
    },
    /// Register the SWAP identity learned for a peer during the handshake.
    SetPeerInfo {
        /// The peer the identity belongs to.
        peer: OverlayAddress,
        /// The peer's beneficiary and chequebook issuer.
        info: PeerSwapInfo,
    },
}

/// Per-peer cheque accounting state.
#[derive(Debug, Default)]
struct PeerChequeState {
    /// SWAP identity learned from the handshake, if any.
    info: Option<PeerSwapInfo>,
    /// Cumulative payout of the last cheque we issued to this peer.
    last_sent_payout: U256,
    /// Cumulative payout of the last cheque we accepted from this peer.
    last_received_payout: U256,
}

/// Processes settlement commands from handles and network events.
pub struct SwapService<A: SwarmBandwidthAccounting, S> {
    /// Receive commands from handles.
    command_rx: mpsc::UnboundedReceiver<SwapCommand>,
    /// Receive events routed from the network layer.
    event_rx: mpsc::UnboundedReceiver<SwapEvent>,
    /// Send commands to the network layer.
    command_tx: mpsc::UnboundedSender<ClientCommand>,
    /// Reference to accounting for balance updates.
    accounting: Arc<A>,
    /// The Ethereum signer used to sign cheques.
    signer: Arc<S>,
    /// Our chequebook address (the drawer of cheques we issue).
    chequebook: Address,
    /// Our beneficiary, the only address a cheque sent to us may name.
    beneficiary: Address,
    /// The settlement chain the EIP-712 domain is bound to.
    chain: NamedChain,
    /// Per-peer cheque accounting state.
    peers: HashMap<OverlayAddress, PeerChequeState>,
    /// Track pending outbound settlements (waiting for the wire ack).
    pending: HashMap<OverlayAddress, PendingSettlement>,
    /// Optional on-chain chequebook client for cashing received cheques.
    #[cfg(feature = "swap-chequebook")]
    cashout: Option<crate::cashout::Cashout>,
}

struct PendingSettlement {
    amount: u64,
    response_tx: oneshot::Sender<Result<u64, SwapSettlementError>>,
}

impl<A, S> SwapService<A, S>
where
    A: SwarmBandwidthAccounting + 'static,
    S: SignerSync + Send + Sync + 'static,
{
    /// Create a new swap service.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        command_rx: mpsc::UnboundedReceiver<SwapCommand>,
        event_rx: mpsc::UnboundedReceiver<SwapEvent>,
        command_tx: mpsc::UnboundedSender<ClientCommand>,
        accounting: Arc<A>,
        signer: Arc<S>,
        chequebook: Address,
        beneficiary: Address,
        chain: NamedChain,
    ) -> Self {
        Self {
            command_rx,
            event_rx,
            command_tx,
            accounting,
            signer,
            chequebook,
            beneficiary,
            chain,
            peers: HashMap::new(),
            pending: HashMap::new(),
            #[cfg(feature = "swap-chequebook")]
            cashout: None,
        }
    }

    /// Attach an on-chain chequebook client so received cheques can be cashed.
    #[cfg(feature = "swap-chequebook")]
    pub fn with_cashout(mut self, cashout: crate::cashout::Cashout) -> Self {
        self.cashout = Some(cashout);
        self
    }

    /// Run the service event loop with graceful shutdown support.
    async fn run(mut self, shutdown: GracefulShutdown) {
        let mut shutdown = std::pin::pin!(shutdown);

        loop {
            tokio::select! {
                guard = &mut shutdown => {
                    debug!("Swap service received shutdown signal");
                    drop(guard);
                    break;
                }
                Some(cmd) = self.command_rx.recv() => {
                    self.handle_command(cmd).await;
                }
                Some(event) = self.event_rx.recv() => {
                    self.handle_event(event).await;
                }
                else => {
                    debug!("Swap service channels closed");
                    break;
                }
            }
        }
        debug!("Swap service shutdown complete");
    }

    async fn handle_command(&mut self, cmd: SwapCommand) {
        match cmd {
            SwapCommand::SetPeerInfo { peer, info } => {
                debug!(
                    %peer,
                    beneficiary = %info.beneficiary,
                    issuer = %info.issuer,
                    "Learned swap identity for peer"
                );
                self.peers.entry(peer).or_default().info = Some(info);
            }
            SwapCommand::Settle {
                peer,
                amount,
                response_tx,
            } => {
                if self.pending.contains_key(&peer) {
                    let _ = response_tx.send(Err(SwapSettlementError::SettlementInProgress));
                    return;
                }

                match self.issue_cheque(peer, amount) {
                    Ok(cheque) => {
                        debug!(
                            %peer,
                            %amount,
                            cumulative_payout = %cheque.cheque.cumulativePayout,
                            "Issuing swap cheque"
                        );

                        if let Err(e) = self
                            .command_tx
                            .send(ClientCommand::SendCheque { peer, cheque })
                        {
                            // Roll back the optimistic cumulative-payout bump so a
                            // retry re-issues at the same level.
                            if let Some(state) = self.peers.get_mut(&peer) {
                                state.last_sent_payout -= U256::from(amount);
                            }
                            let _ = response_tx
                                .send(Err(SwapSettlementError::NetworkError(e.to_string())));
                            return;
                        }

                        self.pending.insert(
                            peer,
                            PendingSettlement {
                                amount,
                                response_tx,
                            },
                        );
                    }
                    Err(e) => {
                        let _ = response_tx.send(Err(e));
                    }
                }
            }
        }
    }

    /// Build and sign a cheque for `amount`, advancing the per-peer cumulative
    /// payout monotonically.
    fn issue_cheque(
        &mut self,
        peer: OverlayAddress,
        amount: u64,
    ) -> Result<SignedCheque, SwapSettlementError> {
        let state = self.peers.entry(peer).or_default();
        let beneficiary = state
            .info
            .ok_or(SwapSettlementError::UnknownBeneficiary)?
            .beneficiary;

        let next_payout = state.last_sent_payout + U256::from(amount);
        let cheque = Cheque::new(self.chequebook, beneficiary, next_payout);

        let hash = cheque.signing_hash(self.chain);
        let sig = self
            .signer
            .sign_hash_sync(&hash)
            .map_err(|e| SwapSettlementError::SigningFailed(e.to_string()))?;

        // Commit the new cumulative payout only once signing succeeds.
        state.last_sent_payout = next_payout;
        Ok(SignedCheque::from_signature(cheque, sig))
    }

    async fn handle_event(&mut self, event: SwapEvent) {
        match event {
            SwapEvent::ChequeSent { peer, peer_rate } => {
                debug!(%peer, %peer_rate, "Cheque sent acknowledgment received");

                if let Some(pending) = self.pending.remove(&peer) {
                    // We paid, so our debt to the peer is reduced.
                    let handle = self.accounting.for_peer(peer);
                    handle.record(pending.amount, Direction::Upload);
                    let _ = pending.response_tx.send(Ok(pending.amount));
                } else {
                    warn!(%peer, "Received cheque sent ack for unknown settlement");
                }
            }
            SwapEvent::ChequeReceived {
                peer,
                cheque,
                peer_rate,
            } => {
                debug!(
                    %peer,
                    %peer_rate,
                    beneficiary = %cheque.cheque.beneficiary,
                    cumulative_payout = %cheque.cheque.cumulativePayout,
                    "Cheque received from peer"
                );

                match self.accept_cheque(peer, &cheque) {
                    Ok(amount) => {
                        // The peer paid us, so what they owe us is reduced.
                        let handle = self.accounting.for_peer(peer);
                        handle.record(amount, Direction::Download);
                        debug!(%peer, %amount, "Credited received cheque");

                        #[cfg(feature = "swap-chequebook")]
                        self.maybe_cash(peer, cheque).await;
                    }
                    Err(e) => {
                        warn!(%peer, error = %e, "Rejected received cheque");
                    }
                }
            }
        }
    }

    /// Validate a received cheque and return the incremental amount to credit.
    ///
    /// Requires a learned swap identity for the peer, then verifies the cheque
    /// names our beneficiary, that the signature recovers to the peer's expected
    /// issuer, that the cumulative payout strictly increases versus the last
    /// accepted cheque, and that the increment fits the accounting-unit type. On
    /// success the per-peer last-received payout is advanced.
    fn accept_cheque(
        &mut self,
        peer: OverlayAddress,
        cheque: &SignedCheque,
    ) -> Result<u64, SwapSettlementError> {
        // An unauthenticated cheque must never reduce the peer's debt: without a
        // learned identity we cannot bind the signature to a real chequebook, so
        // anyone could mint free credit with a self-signed cheque.
        let issuer = self
            .peers
            .get(&peer)
            .and_then(|s| s.info)
            .ok_or(SwapSettlementError::UnknownPeerIdentity)?
            .issuer;

        if cheque.cheque.beneficiary != self.beneficiary {
            return Err(SwapSettlementError::BeneficiaryMismatch {
                expected: self.beneficiary,
                got: cheque.cheque.beneficiary,
            });
        }

        let recovered = cheque
            .recover_signer(self.chain)
            .map_err(|e| SwapSettlementError::ValidationFailed(e.to_string()))?;
        if recovered != issuer {
            return Err(SwapSettlementError::IssuerMismatch {
                expected: issuer,
                recovered,
            });
        }

        let state = self.peers.entry(peer).or_default();
        let received = cheque.cheque.cumulativePayout;
        let last = state.last_received_payout;
        if received <= last {
            return Err(SwapSettlementError::NonIncreasingPayout { last, received });
        }

        let increment = received - last;
        let amount: u64 = increment
            .try_into()
            .map_err(|_| SwapSettlementError::AmountOverflow(increment))?;

        state.last_received_payout = received;
        Ok(amount)
    }

    /// Cash a received cheque on-chain if a chequebook client is attached.
    #[cfg(feature = "swap-chequebook")]
    async fn maybe_cash(&self, peer: OverlayAddress, cheque: SignedCheque) {
        if let Some(cashout) = &self.cashout
            && let Err(e) = cashout.cash(&cheque).await
        {
            warn!(%peer, error = %e, "Failed to cash received cheque");
        }
    }
}

impl<A, S> SpawnableTask for SwapService<A, S>
where
    A: SwarmBandwidthAccounting + 'static,
    S: SignerSync + Send + Sync + 'static,
{
    fn into_task(self, shutdown: GracefulShutdown) -> impl Future<Output = ()> + Send {
        self.run(shutdown)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;
    use alloy_signer_local::PrivateKeySigner;
    use vertex_swarm_bandwidth::{Accounting, BandwidthConfig};
    use vertex_swarm_test_utils::{Identity, test_identity, test_peer};

    const CHAIN: NamedChain = NamedChain::Gnosis;

    /// Our payout address; the only beneficiary a cheque sent to us may name.
    const OUR_BENEFICIARY: Address = Address::repeat_byte(0xbe);

    type TestService = SwapService<Accounting<BandwidthConfig, Identity>, PrivateKeySigner>;

    fn build_service(signer: PrivateKeySigner) -> TestService {
        let (_cmd_tx, command_rx) = mpsc::unbounded_channel();
        let (_evt_tx, event_rx) = mpsc::unbounded_channel();
        let (client_tx, _client_rx) = mpsc::unbounded_channel();
        let accounting = Arc::new(Accounting::new(BandwidthConfig::default(), test_identity()));

        SwapService::new(
            command_rx,
            event_rx,
            client_tx,
            accounting,
            Arc::new(signer),
            Address::repeat_byte(0xcb),
            OUR_BENEFICIARY,
            CHAIN,
        )
    }

    /// Sign a cheque the way a peer would, returning the signed cheque.
    fn peer_cheque(
        signer: &PrivateKeySigner,
        chequebook: Address,
        beneficiary: Address,
        payout: u64,
    ) -> SignedCheque {
        let cheque = Cheque::new(chequebook, beneficiary, U256::from(payout));
        let hash = cheque.signing_hash(CHAIN);
        let sig = signer.sign_hash_sync(&hash).unwrap();
        SignedCheque::from_signature(cheque, sig)
    }

    #[test]
    fn issue_cheque_advances_cumulative_payout() {
        let mut svc = build_service(PrivateKeySigner::random());
        let peer = test_peer();
        svc.peers.entry(peer).or_default().info = Some(PeerSwapInfo {
            beneficiary: Address::repeat_byte(0x22),
            issuer: Address::repeat_byte(0x33),
        });

        let first = svc.issue_cheque(peer, 1_000).unwrap();
        assert_eq!(first.cheque.cumulativePayout, U256::from(1_000u64));

        let second = svc.issue_cheque(peer, 500).unwrap();
        assert_eq!(second.cheque.cumulativePayout, U256::from(1_500u64));
    }

    #[test]
    fn issue_cheque_without_beneficiary_fails() {
        let mut svc = build_service(PrivateKeySigner::random());
        assert!(matches!(
            svc.issue_cheque(test_peer(), 1_000),
            Err(SwapSettlementError::UnknownBeneficiary)
        ));
    }

    #[test]
    fn accept_cheque_credits_incremental_amount() {
        let issuer = PrivateKeySigner::random();
        let mut svc = build_service(PrivateKeySigner::random());
        let peer = test_peer();
        svc.peers.entry(peer).or_default().info = Some(PeerSwapInfo {
            beneficiary: Address::repeat_byte(0x22),
            issuer: issuer.address(),
        });

        let cheque = peer_cheque(&issuer, Address::repeat_byte(0xaa), OUR_BENEFICIARY, 1_000);
        assert_eq!(svc.accept_cheque(peer, &cheque).unwrap(), 1_000);

        // A second cheque at a higher cumulative payout credits only the delta.
        let cheque = peer_cheque(&issuer, Address::repeat_byte(0xaa), OUR_BENEFICIARY, 1_700);
        assert_eq!(svc.accept_cheque(peer, &cheque).unwrap(), 700);
    }

    #[test]
    fn accept_cheque_rejects_non_increasing_payout() {
        let issuer = PrivateKeySigner::random();
        let mut svc = build_service(PrivateKeySigner::random());
        let peer = test_peer();
        svc.peers.entry(peer).or_default().info = Some(PeerSwapInfo {
            beneficiary: Address::repeat_byte(0x22),
            issuer: issuer.address(),
        });

        let cheque = peer_cheque(&issuer, Address::repeat_byte(0xaa), OUR_BENEFICIARY, 1_000);
        assert_eq!(svc.accept_cheque(peer, &cheque).unwrap(), 1_000);

        // Same cumulative payout: no new funds, must be rejected.
        let replay = peer_cheque(&issuer, Address::repeat_byte(0xaa), OUR_BENEFICIARY, 1_000);
        assert!(matches!(
            svc.accept_cheque(peer, &replay),
            Err(SwapSettlementError::NonIncreasingPayout { .. })
        ));

        // A lower cumulative payout is also rejected.
        let lower = peer_cheque(&issuer, Address::repeat_byte(0xaa), OUR_BENEFICIARY, 500);
        assert!(matches!(
            svc.accept_cheque(peer, &lower),
            Err(SwapSettlementError::NonIncreasingPayout { .. })
        ));
    }

    #[test]
    fn accept_cheque_rejects_wrong_issuer() {
        let issuer = PrivateKeySigner::random();
        let imposter = PrivateKeySigner::random();
        let mut svc = build_service(PrivateKeySigner::random());
        let peer = test_peer();
        svc.peers.entry(peer).or_default().info = Some(PeerSwapInfo {
            beneficiary: Address::repeat_byte(0x22),
            issuer: issuer.address(),
        });

        let cheque = peer_cheque(
            &imposter,
            Address::repeat_byte(0xaa),
            OUR_BENEFICIARY,
            1_000,
        );
        assert!(matches!(
            svc.accept_cheque(peer, &cheque),
            Err(SwapSettlementError::IssuerMismatch { .. })
        ));
    }

    #[test]
    fn accept_cheque_rejects_amount_overflow() {
        let issuer = PrivateKeySigner::random();
        let mut svc = build_service(PrivateKeySigner::random());
        let peer = test_peer();
        svc.peers.entry(peer).or_default().info = Some(PeerSwapInfo {
            beneficiary: Address::repeat_byte(0x22),
            issuer: issuer.address(),
        });

        // A cumulative payout above u64::MAX cannot be credited as an accounting
        // unit; the conversion must reject rather than wrap.
        let payout = U256::from(u64::MAX) + U256::from(1u64);
        let cheque = Cheque::new(Address::repeat_byte(0xaa), OUR_BENEFICIARY, payout);
        let hash = cheque.signing_hash(CHAIN);
        let sig = issuer.sign_hash_sync(&hash).unwrap();
        let signed = SignedCheque::from_signature(cheque, sig);

        assert!(matches!(
            svc.accept_cheque(peer, &signed),
            Err(SwapSettlementError::AmountOverflow(_))
        ));
    }

    #[test]
    fn accept_cheque_roundtrips_our_own_issuance() {
        // A cheque we sign with our own signer recovers to our own address, so
        // pointing a peer's expected issuer at us lets the validation path accept
        // a cheque produced by `issue_cheque`. This exercises sign -> recover. The
        // peer's beneficiary is set to our own so the issued cheque also passes
        // the received-cheque beneficiary check.
        let signer = PrivateKeySigner::random();
        let mut svc = build_service(signer.clone());
        let peer = test_peer();
        svc.peers.entry(peer).or_default().info = Some(PeerSwapInfo {
            beneficiary: OUR_BENEFICIARY,
            issuer: signer.address(),
        });

        let issued = svc.issue_cheque(peer, 4_200).unwrap();
        assert_eq!(svc.accept_cheque(peer, &issued).unwrap(), 4_200);
    }

    #[test]
    fn accept_cheque_rejects_unknown_peer_identity() {
        // Without a learned identity the issuer cannot be authenticated, so a
        // self-signed cheque must not mint credit.
        let issuer = PrivateKeySigner::random();
        let mut svc = build_service(PrivateKeySigner::random());
        let peer = test_peer();

        let cheque = peer_cheque(&issuer, Address::repeat_byte(0xaa), OUR_BENEFICIARY, 1_000);
        assert!(matches!(
            svc.accept_cheque(peer, &cheque),
            Err(SwapSettlementError::UnknownPeerIdentity)
        ));
    }

    #[test]
    fn accept_cheque_rejects_wrong_beneficiary() {
        // A cheque drawn for someone other than our payout address must be
        // rejected before it is credited or cashed.
        let issuer = PrivateKeySigner::random();
        let mut svc = build_service(PrivateKeySigner::random());
        let peer = test_peer();
        svc.peers.entry(peer).or_default().info = Some(PeerSwapInfo {
            beneficiary: OUR_BENEFICIARY,
            issuer: issuer.address(),
        });

        let cheque = peer_cheque(
            &issuer,
            Address::repeat_byte(0xaa),
            Address::repeat_byte(0x77),
            1_000,
        );
        assert!(matches!(
            svc.accept_cheque(peer, &cheque),
            Err(SwapSettlementError::BeneficiaryMismatch { .. })
        ));
    }
}
