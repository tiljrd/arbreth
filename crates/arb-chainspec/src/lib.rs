//! Arbitrum chain specification and ArbOS version constants.
//!
//! Defines the ArbOS version progression, hardfork timestamps, and the
//! [`ArbitrumChainSpec`] trait for version-gated EVM spec selection.

use reth_chainspec::ChainSpec;
pub use reth_chainspec::EthChainSpec;
use revm::primitives::hardfork::SpecId;

/// ArbOS version constants.
///
/// These map to EVM spec upgrades gated by the ArbOS version
/// stored in the block header's mix_hash.
pub mod arbos_version {
    pub const ARBOS_VERSION_2: u64 = 2;
    /// Poster fee destination moves from coinbase to L1PricerFundsPoolAddress.
    pub const ARBOS_VERSION_POSTER_FUNDS_TO_POOL: u64 = ARBOS_VERSION_2;
    pub const ARBOS_VERSION_3: u64 = 3;
    /// StartBlock's `time_passed` switches from `l2_block_number` to real
    /// elapsed seconds; L1 pricing begins applying `amortized_cost_cap_bips`.
    pub const ARBOS_VERSION_TIME_PASSED_AS_TIME: u64 = ARBOS_VERSION_3;
    pub const ARBOS_VERSION_AMORTIZED_COST_CAP: u64 = ARBOS_VERSION_3;
    pub const ARBOS_VERSION_4: u64 = 4;
    /// L2->L1 forwarder return value switches from L1 block to L2 block.
    pub const ARBOS_VERSION_L2_TO_L1_RETURN_L2BLOCK: u64 = ARBOS_VERSION_4;
    pub const ARBOS_VERSION_5: u64 = 5;
    /// Infra fee account introduced; tx fees split between network + infra.
    pub const ARBOS_VERSION_INFRA_FEE_SPLIT: u64 = ARBOS_VERSION_5;
    pub const ARBOS_VERSION_6: u64 = 6;
    /// Arbitrum One's launch version. `ArbSys.IsTopLevel` switches from
    /// `depth == 2` to `origin == Contracts[depth-2].Caller()` chain check.
    pub const ARBOS_VERSION_IS_TOP_LEVEL_ORIGIN_CHECK: u64 = ARBOS_VERSION_6;
    pub const ARBOS_VERSION_ARBITRUM_ONE_LAUNCH: u64 = ARBOS_VERSION_6;
    pub const ARBOS_VERSION_7: u64 = 7;
    /// `LastSurplus` storage switches from unsigned-magnitude (pre-v7) to
    /// the signed-encoding used by `StorageBackedSignedInt`.
    pub const ARBOS_VERSION_LAST_SURPLUS_SIGNED: u64 = ARBOS_VERSION_7;
    pub const ARBOS_VERSION_8: u64 = 8;
    /// `StartBlock` stops the pre-v8 `l1_block_number++` adjustment; the
    /// L1 block number recorded matches the sequencer's value verbatim.
    pub const ARBOS_VERSION_L1_BLOCK_NUMBER_DIRECT: u64 = ARBOS_VERSION_8;
    pub const ARBOS_VERSION_9: u64 = 9;
    /// `drop_tip` always returns false; tips are always collected
    /// (`GetPaidGasPrice` switches to returning `gasPrice` not `BaseFee`).
    pub const ARBOS_VERSION_ALWAYS_COLLECT_TIPS: u64 = ARBOS_VERSION_9;
    /// Legacy CollectTips encoding (pre-v10 mix_hash layout).
    pub const ARBOS_VERSION_COLLECT_TIPS_OLD: u64 = ARBOS_VERSION_9;
    pub const ARBOS_VERSION_10: u64 = 10;
    /// L1 pricing per-batch-poster spending switches from the legacy
    /// `_preversion10_UpdateForBatchPosterSpending` path (live pool balance
    /// via balance read) to the v10+ `l1_fees_available` slot path.
    pub const ARBOS_VERSION_L1_PRICING_FROM_POOL_SLOT: u64 = ARBOS_VERSION_10;
    /// ArbOS version 11 — Shanghai EVM rules (PUSH0, etc.).
    pub const ARBOS_VERSION_11: u64 = 11;
    /// Gas for scheduled retry txs is subtracted from parent tx gas used.
    pub const ARBOS_VERSION_FIX_REDEEM_GAS: u64 = ARBOS_VERSION_11;
    /// ArbOS version 20 — Cancun EVM rules (transient storage, blob base fee).
    pub const ARBOS_VERSION_20: u64 = 20;
    /// ArbOS version 30 — Stylus support.
    pub const ARBOS_VERSION_30: u64 = 30;
    pub const ARBOS_VERSION_STYLUS: u64 = ARBOS_VERSION_30;
    /// ArbOS version 31 — Stylus fixes (return data cost check, etc.).
    pub const ARBOS_VERSION_31: u64 = 31;
    pub const ARBOS_VERSION_STYLUS_FIXES: u64 = ARBOS_VERSION_31;
    /// ArbOS version 32 — Stylus charging fixes.
    pub const ARBOS_VERSION_32: u64 = 32;
    pub const ARBOS_VERSION_STYLUS_CHARGING_FIXES: u64 = ARBOS_VERSION_32;
    /// ArbOS version 40 — Prague EVM rules.
    pub const ARBOS_VERSION_40: u64 = 40;
    pub const ARBOS_VERSION_STYLUS_LAST_CODE_CACHE_FIX: u64 = ARBOS_VERSION_40;
    pub const ARBOS_VERSION_41: u64 = 41;
    /// ArbOS version 50 — Dia upgrade.
    pub const ARBOS_VERSION_50: u64 = 50;
    pub const ARBOS_VERSION_DIA: u64 = ARBOS_VERSION_50;
    /// Maximum ArbOS version supported by this node.
    pub const MAX_ARBOS_VERSION_SUPPORTED: u64 = ARBOS_VERSION_60;
    /// ArbOS version 51 — multi-constraint fix.
    pub const ARBOS_VERSION_MULTI_CONSTRAINT_FIX: u64 = 51;
    pub const ARBOS_VERSION_51: u64 = 51;
    pub const ARBOS_VERSION_59: u64 = 59;
    /// ArbOS version 60 — multi-gas constraints + Stylus contract limit + transaction filtering.
    pub const ARBOS_VERSION_MULTI_GAS_CONSTRAINTS: u64 = 60;
    pub const ARBOS_VERSION_60: u64 = 60;
    pub const ARBOS_VERSION_STYLUS_CONTRACT_LIMIT: u64 = ARBOS_VERSION_60;
    pub const ARBOS_VERSION_TRANSACTION_FILTERING: u64 = ARBOS_VERSION_60;

    /// Version-gated precompile addresses and the ArbOS version that
    /// activates them. Both precompile-map registration and the `[0xFE]`
    /// code install during version upgrades read this table; a skew between
    /// the two would be a consensus fork.
    pub static PRECOMPILE_MIN_ARBOS_VERSIONS: &[(alloy_primitives::Address, u64)] = &[
        (
            // ArbWasm
            alloy_primitives::address!("0000000000000000000000000000000000000071"),
            ARBOS_VERSION_STYLUS,
        ),
        (
            // ArbWasmCache
            alloy_primitives::address!("0000000000000000000000000000000000000072"),
            ARBOS_VERSION_STYLUS,
        ),
        (
            // ArbNativeTokenManager
            alloy_primitives::address!("0000000000000000000000000000000000000073"),
            ARBOS_VERSION_41,
        ),
        (
            // ArbFilteredTransactionsManager
            alloy_primitives::address!("0000000000000000000000000000000000000074"),
            ARBOS_VERSION_TRANSACTION_FILTERING,
        ),
    ];

    /// Activation version for a version-banded precompile; `None` for
    /// addresses that are always registered.
    pub fn precompile_min_arbos_version(addr: alloy_primitives::Address) -> Option<u64> {
        PRECOMPILE_MIN_ARBOS_VERSIONS
            .iter()
            .find(|(a, _)| *a == addr)
            .map(|(_, v)| *v)
    }
}

/// Trait for Arbitrum chain specifications.
///
/// Provides the chain ID and version-gated spec ID mapping
/// needed by the EVM configuration layer.
pub trait ArbitrumChainSpec {
    /// Returns the chain ID.
    fn chain_id(&self) -> u64;

    /// Maps a timestamp to a `SpecId`.
    ///
    /// Not on the execution path: block execution selects the EVM spec from
    /// the header's ArbOS version via [`spec_id_by_arbos_version`], which is
    /// authoritative and chain-agnostic. This method only encodes Arbitrum
    /// Sepolia's schedule and must not be relied on for other chains.
    fn spec_id_by_timestamp(&self, timestamp: u64) -> SpecId;

    /// Maps an ArbOS version to the appropriate SpecId.
    fn spec_id_by_arbos_version(&self, arbos_version: u64) -> SpecId;
}

/// Map ArbOS version to the appropriate SpecId.
pub fn spec_id_by_arbos_version(arbos_version: u64) -> SpecId {
    if arbos_version >= arbos_version::ARBOS_VERSION_50 {
        SpecId::OSAKA
    } else if arbos_version >= arbos_version::ARBOS_VERSION_40 {
        SpecId::PRAGUE
    } else if arbos_version >= arbos_version::ARBOS_VERSION_20 {
        SpecId::CANCUN
    } else if arbos_version >= arbos_version::ARBOS_VERSION_11 {
        SpecId::SHANGHAI
    } else {
        SpecId::MERGE
    }
}

/// Arbitrum Sepolia hardfork timestamps.
pub const ARBITRUM_SEPOLIA_SHANGHAI_TIMESTAMP: u64 = 1_706_634_000;
pub const ARBITRUM_SEPOLIA_CANCUN_TIMESTAMP: u64 = 1_709_229_600;
pub const ARBITRUM_SEPOLIA_PRAGUE_TIMESTAMP: u64 = 1_746_543_285;

/// Map timestamp to SpecId for Arbitrum Sepolia.
pub fn arbitrum_sepolia_spec_id_by_timestamp(timestamp: u64) -> SpecId {
    if timestamp >= ARBITRUM_SEPOLIA_PRAGUE_TIMESTAMP {
        SpecId::PRAGUE
    } else if timestamp >= ARBITRUM_SEPOLIA_CANCUN_TIMESTAMP {
        SpecId::CANCUN
    } else if timestamp >= ARBITRUM_SEPOLIA_SHANGHAI_TIMESTAMP {
        SpecId::SHANGHAI
    } else {
        SpecId::MERGE
    }
}

/// Simple Arbitrum chain spec.
#[derive(Clone, Debug, Default)]
pub struct ArbChainSpec {
    pub chain_id: u64,
}

impl ArbitrumChainSpec for ArbChainSpec {
    fn chain_id(&self) -> u64 {
        self.chain_id
    }

    fn spec_id_by_timestamp(&self, timestamp: u64) -> SpecId {
        arbitrum_sepolia_spec_id_by_timestamp(timestamp)
    }

    fn spec_id_by_arbos_version(&self, arbos_version: u64) -> SpecId {
        spec_id_by_arbos_version(arbos_version)
    }
}

/// Blanket implementation for reth's `ChainSpec`.
impl ArbitrumChainSpec for ChainSpec {
    fn chain_id(&self) -> u64 {
        self.chain().id()
    }

    fn spec_id_by_timestamp(&self, timestamp: u64) -> SpecId {
        arbitrum_sepolia_spec_id_by_timestamp(timestamp)
    }

    fn spec_id_by_arbos_version(&self, arbos_version: u64) -> SpecId {
        spec_id_by_arbos_version(arbos_version)
    }
}

/// Arbitrum One chain ID.
pub const ARBITRUM_ONE_CHAIN_ID: u64 = 42161;

/// Arbitrum Sepolia chain ID.
pub const ARBITRUM_SEPOLIA_CHAIN_ID: u64 = 421614;
