//! Pins the Arbitrum One Nitro genesis: parsing `genesis/arbitrum-one.json`
//! must reproduce the canonical block-22207818 hash, or forward sync would
//! diverge at the first produced block.

use std::path::PathBuf;

use alloy_primitives::{b256, B256};
use arb_node::chainspec::ArbChainSpecParser;
use reth_chainspec::EthChainSpec;
use reth_cli::chainspec::ChainSpecParser;

/// Canonical values for Arbitrum One block 22207818 (the Nitro migration
/// genesis), read from the live chain.
const GENESIS_HASH: B256 = b256!("d1882c626699cd19548720713669993ac8f51500056dfbc1afc180496c7f8e2f");
const STATE_ROOT: B256 = b256!("d764f1e1df4c2dbdc9f1785f86081734f1310f937ed09b5c753750f8b4d31bbd");
const GENESIS_BLOCK: u64 = 22_207_818;

#[test]
fn arbitrum_one_genesis_spec_matches_canonical() {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../genesis/arbitrum-one.json");
    let spec = ArbChainSpecParser::parse(path.to_str().expect("utf8 path"))
        .expect("parse genesis/arbitrum-one.json");

    assert_eq!(spec.genesis_header().number, GENESIS_BLOCK, "genesis block number");
    assert_eq!(spec.genesis_header().state_root, STATE_ROOT, "migration state root");
    assert!(
        spec.genesis().alloc.is_empty(),
        "migrated chain alloc must be empty (state comes from init-state)"
    );
    // The re-sealed genesis header must hash to the canonical block.
    assert_eq!(spec.genesis_hash(), GENESIS_HASH, "genesis block hash parity");
}
