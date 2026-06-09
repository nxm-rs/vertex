//! Unit tests for the pure pieces this crate owns: config constructors, the
//! transaction-request newtype, and error label derivation. The provider
//! extension trait is exercised against a live transport in the service crate;
//! there is no provider to mock here.

use alloy_chains::{Chain, NamedChain};
use alloy_primitives::{Address, TxHash, U256};
use alloy_rpc_types_eth::TransactionRequest;
use nectar_swarms::{NamedSwarm, Swarm};

use crate::{ChainError, TxError, TxRequest};

#[test]
fn chain_config_constructors_agree() {
    let mainnet = crate::ChainConfig::mainnet();
    assert_eq!(mainnet.chain, Chain::from(NamedChain::Gnosis));
    assert_eq!(mainnet.chain.id(), 100);
    assert_eq!(
        mainnet.chequebook_factory,
        nectar_contracts::mainnet::CHEQUEBOOK_FACTORY.address
    );
    assert_eq!(
        mainnet.bzz_token,
        nectar_contracts::mainnet::BZZ_TOKEN.address
    );

    let testnet = crate::ChainConfig::testnet();
    assert_eq!(testnet.chain, Chain::from(NamedChain::Sepolia));
    assert_eq!(testnet.chain.id(), 11155111);
    assert_eq!(
        testnet.price_oracle,
        nectar_contracts::testnet::STORAGE_PRICE_ORACLE.address
    );

    let manual = crate::ChainConfig::new(
        mainnet.chain,
        mainnet.chequebook_factory,
        mainnet.bzz_token,
        mainnet.price_oracle,
    );
    assert_eq!(manual, mainnet);
}

#[test]
fn chain_config_derives_chain_from_nectar_swarms() {
    // The named constructors delegate to `for_swarm`, which sources the chain
    // from `nectar_swarms` rather than hardcoding it here.
    assert_eq!(
        crate::ChainConfig::for_swarm(NamedSwarm::Mainnet),
        Some(crate::ChainConfig::mainnet())
    );
    assert_eq!(
        crate::ChainConfig::for_swarm(NamedSwarm::Mainnet).map(|c| c.chain),
        Some(NamedSwarm::Mainnet.chain())
    );

    // Dev and custom networks have no canonical deployment.
    assert_eq!(crate::ChainConfig::for_swarm(NamedSwarm::Dev), None);
    assert_eq!(
        crate::ChainConfig::from_swarm(Swarm::from_id(999_999)),
        None
    );
    assert_eq!(
        crate::ChainConfig::from_swarm(Swarm::from_named(NamedSwarm::Testnet)),
        Some(crate::ChainConfig::testnet())
    );
}

#[test]
fn tx_request_wraps_and_derefs() {
    let inner = TransactionRequest {
        to: Some(Address::ZERO.into()),
        value: Some(U256::from(1u64)),
        ..Default::default()
    };
    let req = TxRequest::new(inner.clone(), "test_call");
    assert_eq!(req.description, "test_call");
    // Deref reaches the inner request's fields.
    assert_eq!(req.value, Some(U256::from(1u64)));
    assert_eq!(req.request, inner);

    // A bare request converts with an empty description.
    let from: TxRequest = inner.into();
    assert_eq!(from.description, "");
}

#[test]
fn tx_error_labels_are_snake_case() {
    let s: &'static str = (&TxError::NoSuchPending { hash: TxHash::ZERO }).into();
    assert_eq!(s, "no_such_pending");
}

#[test]
fn chain_error_labels_are_snake_case() {
    // Build a transport error to confirm the variant maps to its label.
    let err = ChainError::Transport(alloy_provider::transport::TransportErrorKind::custom_str(
        "boom",
    ));
    let s: &'static str = (&err).into();
    assert_eq!(s, "transport");
}
