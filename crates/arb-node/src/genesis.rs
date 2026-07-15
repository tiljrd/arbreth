//! ArbOS genesis state initialization.
//!
//! Initializes the ArbOS system state in the database when the chain boots.
//! Runs when the first message (Kind=11, Initialize) is received from the
//! consensus sidecar.

use alloy_primitives::{address, Address, Bytes};
use revm::{database::State, Database};
use tracing::info;

use arb_storage::{set_account_code, Storage};
use arbos::{
    arbos_state::initialize::bootstrap, arbos_types::ParsedInitMessage, burn::SystemBurner,
};

use crate::error::GenesisError;

/// Precompile addresses that exist at genesis (version 0).
/// Only these get the `[0xFE]` invalid code marker at init time.
/// Later precompiles (ArbWasm, ArbWasmCache, etc.) get code when their
/// ArbOS version is reached during the upgrade path.
const GENESIS_PRECOMPILE_ADDRESSES: [Address; 14] = [
    address!("0000000000000000000000000000000000000064"), // ArbSys
    address!("0000000000000000000000000000000000000065"), // ArbInfo
    address!("0000000000000000000000000000000000000066"), // ArbAddressTable
    address!("0000000000000000000000000000000000000067"), // ArbBLS
    address!("0000000000000000000000000000000000000068"), // ArbFunctionTable
    address!("0000000000000000000000000000000000000069"), // ArbosTest
    address!("000000000000000000000000000000000000006b"), // ArbOwnerPublic
    address!("000000000000000000000000000000000000006c"), // ArbGasInfo
    address!("000000000000000000000000000000000000006d"), // ArbAggregator
    address!("000000000000000000000000000000000000006e"), // ArbRetryableTx
    address!("000000000000000000000000000000000000006f"), // ArbStatistics
    address!("0000000000000000000000000000000000000070"), // ArbOwner
    address!("00000000000000000000000000000000000000ff"), // ArbDebug
    address!("00000000000000000000000000000000000a4b05"), // ArbosActs
];

/// The initial ArbOS version for Arbitrum Sepolia genesis.
/// The upgrade_arbos_version path handles stepping through all intermediate versions.
pub const INITIAL_ARBOS_VERSION: u64 = 10;

/// Default chain owner for Arbitrum Sepolia.
pub const DEFAULT_CHAIN_OWNER: Address = address!("0000000000000000000000000000000000000000");

/// Initialize ArbOS state in a freshly created database.
///
/// This sets up:
/// - ArbOS version (set to 1, then upgrade to target version)
/// - All precompile accounts with `[0xFE]` invalid code marker
/// - L1 pricing state (initial base fee, batch poster table)
/// - L2 pricing state (base fee, gas pool, speed limit)
/// - Retryable state, address table, merkle accumulator, blockhashes
/// - Chain owner and chain config
///
/// The `init_msg` comes from parsing the L1 Initialize message (Kind=11).
#[derive(Debug, Clone, Copy, Default)]
pub struct ArbOSInit {
    pub native_token_supply_management_enabled: bool,
    pub transaction_filtering_enabled: bool,
}

pub fn initialize_arbos_state<D: Database>(
    state: &mut State<D>,
    init_msg: &ParsedInitMessage,
    chain_id: u64,
    target_arbos_version: u64,
    chain_owner: Address,
    genesis_block_num: u64,
    arbos_init: ArbOSInit,
) -> Result<(), GenesisError> {
    if is_arbos_initialized(state) {
        return Err(GenesisError::AlreadyInitialized);
    }

    info!(
        target: "arb::genesis",
        chain_id,
        target_arbos_version,
        genesis_block_num,
        initial_l1_base_fee = %init_msg.initial_l1_base_fee,
        "Initializing ArbOS state"
    );

    // Install the [0xFE] code marker on every version-0 precompile address —
    // Solidity requires call targets have code, and ArbOS precompiles don't.
    // Bootstrap doesn't touch account code; this is a node-level concern.
    for addr in &GENESIS_PRECOMPILE_ADDRESSES {
        set_account_code(state, *addr, Bytes::from_static(&[0xFE]));
    }

    // `bootstrap` performs the full ArbOS state init Nitro does in
    // `arbosstate.go::InitializeArbosState`: root-namespace slots, all
    // subspace inits, initial chain-owner add, version upgrade chain.
    let arb_state = bootstrap(
        state,
        chain_id,
        chain_owner,
        genesis_block_num,
        &init_msg.serialized_chain_config,
        init_msg.initial_l1_base_fee,
        target_arbos_version,
        SystemBurner::new(None, false),
    )
    .map_err(|e| GenesisError::InitSubsystem {
        subsystem: "bootstrap",
        source: e,
    })?;

    // Optional ArbOS features the init message may enable.
    if arbos_init.native_token_supply_management_enabled {
        // SAFETY: arb_state is the only live `Storage<D>` handle.
        let s = unsafe { arb_state.backing_storage.state_mut() };
        arb_state
            .set_native_token_management_from_time(s, 1)
            .map_err(|source| GenesisError::InitSubsystem {
                subsystem: "native token management",
                source,
            })?;
    }
    if arbos_init.transaction_filtering_enabled {
        // SAFETY: see above.
        let s = unsafe { arb_state.backing_storage.state_mut() };
        arb_state
            .set_transaction_filtering_from_time(s, 1)
            .map_err(|source| GenesisError::InitSubsystem {
                subsystem: "transaction filtering",
                source,
            })?;
    }

    info!(
        target: "arb::genesis",
        final_version = arb_state.arbos_version(),
        "ArbOS state initialized"
    );

    Ok(())
}

/// Check if ArbOS state is already initialized in the given state database.
pub fn is_arbos_initialized<D: Database>(state: &mut State<D>) -> bool {
    let backing = Storage::new(state, alloy_primitives::B256::ZERO);
    backing.get_uint64_by_uint64(0).unwrap_or(0) != 0
}
