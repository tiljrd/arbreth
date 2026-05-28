//! Derive and verify a migrated chain's genesis header from a live RPC and
//! emit its reth chain-spec JSON.
//!
//! Fetches the genesis (migration) block, re-hashes the decoded consensus
//! header to confirm our encoding reproduces the canonical block hash, then
//! writes a chain spec carrying the real header fields plus a custom top-level
//! `stateRoot` (consumed by `ArbChainSpecParser` for non-zero-genesis chains).

use std::{path::PathBuf, time::Duration};

use alloy_rlp::Encodable;
use anyhow::{anyhow, bail, Context, Result};
use serde_json::{json, Value};

use arb_test_harness::rpc::JsonRpcClient;

#[derive(Debug, clap::Args)]
pub struct Arb1HeaderArgs {
    /// RPC for the chain (Alchemy arb-mainnet works; archive not required).
    #[arg(long, env = "ARB_MAINNET_RPC")]
    pub rpc: String,

    /// The Nitro migration genesis block number. For arb1 this is 22,207,817
    /// (the block built by `MakeGenesisBlock` after `InitializeArbosInDatabase`),
    /// not 22,207,818 (which is the first message-driven block running init txs).
    #[arg(long, default_value_t = 22_207_817)]
    pub block: u64,

    /// L2 chain id.
    #[arg(long, default_value_t = 42161)]
    pub chain_id: u64,

    /// InitialArbOSVersion at genesis.
    #[arg(long, default_value_t = 6)]
    pub arbos_version: u64,

    /// InitialChainOwner.
    #[arg(long, default_value = "0xd345e41ae2cb00311956aa7109fc801ae8c81a52")]
    pub chain_owner: String,

    /// Output chain-spec path.
    #[arg(long, default_value = "genesis/arbitrum-one.json")]
    pub out: PathBuf,
}

pub fn run(args: Arb1HeaderArgs) -> Result<()> {
    let rpc = JsonRpcClient::new(args.rpc.clone()).with_timeout(Duration::from_secs(60));
    let block_hex = format!("0x{:x}", args.block);

    let raw = rpc
        .call("eth_getBlockByNumber", json!([block_hex, false]))
        .map_err(|e| anyhow!("eth_getBlockByNumber({block_hex}): {e}"))?;
    if raw.is_null() {
        bail!("block {} not found on {}", args.block, args.rpc);
    }

    // Decode as an RPC block and re-hash the consensus header: a match proves
    // our header RLP encoding reproduces the canonical genesis hash, so a node
    // built from this spec will agree on the genesis block.
    let block: alloy_rpc_types_eth::Block = serde_json::from_value(raw.clone())
        .context("decode eth_getBlockByNumber result as a block")?;
    let reported_hash = block.header.hash;
    let header = block.header.inner.clone();
    let computed_hash = header.hash_slow();
    if computed_hash != reported_hash {
        bail!(
            "genesis header re-hash mismatch: computed {computed_hash} != reported {reported_hash} \
             (header encoding does not reproduce the canonical hash)"
        );
    }
    if header.number != args.block {
        bail!("fetched block number {} != requested {}", header.number, args.block);
    }
    eprintln!("genesis block {} hash verified: {reported_hash}", args.block);
    eprintln!("  stateRoot  = {}", header.state_root);
    eprintln!("  parentHash = {}", header.parent_hash);
    eprintln!("  timestamp  = {}", header.timestamp);
    let arbos_in_header = u64::from_be_bytes(header.mix_hash.0[16..24].try_into().unwrap_or_default());
    eprintln!("  mixHash    = {} (ArbOS version {arbos_in_header})", header.mix_hash);
    if arbos_in_header != args.arbos_version {
        eprintln!(
            "  WARNING: header mixHash ArbOS version {arbos_in_header} != --arbos-version {}",
            args.arbos_version
        );
    }

    let spec = build_chain_spec(&args, &header);
    let body = serde_json::to_vec_pretty(&spec).context("serialize chain spec")?;
    if let Some(parent) = args.out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    std::fs::write(&args.out, &body).with_context(|| format!("write {}", args.out.display()))?;
    eprintln!("wrote {} (genesis hash {reported_hash})", args.out.display());
    Ok(())
}

/// Build the reth chain-spec JSON for a migrated chain: the real block header
/// fields verbatim, a custom top-level `stateRoot`, an empty alloc, and the
/// arbitrum config with `SkipGenesisInjection` so the parser uses the header
/// as-is and the state comes from `init-state`.
fn build_chain_spec(args: &Arb1HeaderArgs, h: &alloy_consensus::Header) -> Value {
    let base_fee = h
        .base_fee_per_gas
        .map(|f| format!("0x{f:x}"))
        .unwrap_or_else(|| "0x0".to_string());
    // The full canonical header, RLP-encoded; `ArbChainSpecParser` installs it
    // verbatim so the genesis hash matches (the genesis block is not empty).
    let mut rlp = Vec::new();
    h.encode(&mut rlp);
    let header_rlp = format!("0x{}", hex::encode(&rlp));
    json!({
        "config": {
            "chainId": args.chain_id,
            "homesteadBlock": 0,
            "daoForkSupport": true,
            "eip150Block": 0,
            "eip155Block": 0,
            "eip158Block": 0,
            "byzantiumBlock": 0,
            "constantinopleBlock": 0,
            "petersburgBlock": 0,
            "istanbulBlock": 0,
            "muirGlacierBlock": 0,
            "berlinBlock": 0,
            "londonBlock": 0,
            "clique": { "period": 0, "epoch": 0 },
            "arbitrum": {
                "EnableArbOS": true,
                "AllowDebugPrecompiles": false,
                "DataAvailabilityCommittee": false,
                "InitialArbOSVersion": args.arbos_version,
                "InitialChainOwner": args.chain_owner.to_lowercase(),
                "GenesisBlockNum": args.block,
                "SkipGenesisInjection": true,
            }
        },
        "number": format!("0x{:x}", h.number),
        "genesisHeaderRlp": header_rlp,
        "stateRoot": h.state_root.to_string(),
        "parentHash": h.parent_hash.to_string(),
        "nonce": h.nonce.to_string(),
        "timestamp": format!("0x{:x}", h.timestamp),
        "extraData": h.extra_data.to_string(),
        "gasLimit": format!("0x{:x}", h.gas_limit),
        "difficulty": format!("0x{:x}", h.difficulty),
        "mixHash": h.mix_hash.to_string(),
        "coinbase": h.beneficiary.to_string(),
        "baseFeePerGas": base_fee,
        "alloc": {},
    })
}
