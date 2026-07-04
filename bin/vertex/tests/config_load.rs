//! The real `ProtocolConfig` default must round-trip through figment.
//!
//! `FullNodeConfig::load` merges the serialized defaults through figment, whose
//! value model cannot carry a bare `u128`. This exercises the production
//! `ProtocolConfig` (not a stub) so a figment-incompatible config field fails
//! here rather than at node startup.

use std::io::Write;

use vertex_node_core::config::FullNodeConfig;
use vertex_swarm_node::ProtocolConfig;

#[test]
fn real_protocol_default_config_loads() {
    let result = FullNodeConfig::<ProtocolConfig>::load(None);
    assert!(result.is_ok(), "loading default config failed: {result:?}");
}

#[test]
fn quoted_u128_config_file_parses() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    writeln!(file, "[swap]\nbounce_limit = \"200000000\"").unwrap();

    let config = FullNodeConfig::<ProtocolConfig>::load(Some(file.path())).unwrap();
    assert_eq!(config.protocol.swap.bounce_limit, 200_000_000u128);
}

#[test]
fn accounting_section_deserializes() {
    let mut file = tempfile::NamedTempFile::new().unwrap();
    writeln!(file, "[accounting]\npayment_threshold = 42").unwrap();

    let config = FullNodeConfig::<ProtocolConfig>::load(Some(file.path())).unwrap();
    assert_eq!(config.protocol.accounting.payment_threshold, 42);
}

#[test]
fn rpc_section_maps_onto_the_stamp_validation_policy() {
    use vertex_swarm_api::StampValidation;

    let default_config = FullNodeConfig::<ProtocolConfig>::load(None).unwrap();
    assert_eq!(
        default_config.protocol.rpc_stamp_validation(),
        StampValidation::Enforce
    );

    let mut file = tempfile::NamedTempFile::new().unwrap();
    writeln!(file, "[rpc]\nstamp_validation = \"per-request\"").unwrap();

    let config = FullNodeConfig::<ProtocolConfig>::load(Some(file.path())).unwrap();
    assert_eq!(
        config.protocol.rpc_stamp_validation(),
        StampValidation::PerRequest
    );
}
