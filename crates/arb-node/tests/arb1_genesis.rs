//! Pins the Arbitrum One Nitro genesis: parsing `genesis/arbitrum-one.json`
//! must reproduce the canonical block-22207818 hash, or forward sync would
//! diverge at the first produced block.

use std::path::PathBuf;

use alloy_primitives::{b256, B256};
use arb_node::chainspec::ArbChainSpecParser;
use reth_chainspec::EthChainSpec;
use reth_cli::chainspec::ChainSpecParser;

/// Canonical values for Arbitrum One block 22207817 — the Nitro migration
/// genesis built by `MakeGenesisBlock` after `InitializeArbosInDatabase`
/// commits the migrated state. Block 22207818 is the first message-driven
/// block (executes the init transactions), not the genesis. Verified via
/// Alchemy archive at the canonical ArbOS slot 5 (GENESIS_BLOCK_NUM) which
/// reads back 0x152dd49 = 22207817.
const GENESIS_HASH: B256 = b256!("7d237dd685b96381544e223f8906e35645d63b89c19983f2246db48568c07986");
const STATE_ROOT: B256 = b256!("7f2bfc4481d02bfcfc606ebb949384ef78d03a0f30a2dc9cccd652eb80926ae1");
const GENESIS_BLOCK: u64 = 22_207_817;

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
