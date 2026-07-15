use alloy_primitives::{Address, B256, U256};
use arb_storage::{
    set_account_nonce, Storage, StorageBackedAddress, StorageBackedBigUint, StorageBackedBytes,
    StorageBackedUint64, StorageBackend, ARBOS_STATE_ADDRESS,
};
use revm::{database::State, Database};

use crate::{
    address_set,
    burn::Burner,
    l1_pricing,
    l1_pricing::L1PricingState,
    l2_pricing::L2PricingState,
    retryables::{self, RetryableState},
};

use super::{ArbosState, ArbosStateError};

/// Genesis data for a retryable ticket.
#[derive(Debug, Clone)]
pub struct InitRetryableData {
    pub id: B256,
    pub timeout: u64,
    pub from: Address,
    pub to: Option<Address>,
    pub callvalue: U256,
    pub beneficiary: Address,
    pub calldata: Vec<u8>,
}

/// Genesis data for an account.
#[derive(Debug, Clone)]
pub struct AccountInitInfo {
    pub addr: Address,
    pub nonce: u64,
    pub balance: U256,
    pub contract_info: Option<ContractInitInfo>,
    pub aggregator_info: Option<AggregatorInitInfo>,
}

/// Contract info for genesis account initialization.
#[derive(Debug, Clone)]
pub struct ContractInitInfo {
    pub code: Vec<u8>,
    pub storage: Vec<(U256, U256)>,
}

/// Aggregator (batch poster) info for genesis account initialization.
#[derive(Debug, Clone)]
pub struct AggregatorInitInfo {
    pub fee_collector: Address,
}

/// Creates a genesis block header.
///
/// Returns the fields needed for the genesis block. The actual block
/// construction uses reth's block types, so this returns a struct
/// that the genesis pipeline can consume.
#[derive(Debug, Clone)]
pub struct GenesisBlockInfo {
    pub parent_hash: B256,
    pub block_number: u64,
    pub timestamp: u64,
    pub state_root: B256,
    pub gas_limit: u64,
    pub base_fee: u64,
    pub nonce: u64,
    pub arbos_format_version: u64,
}

/// Build genesis block info from chain parameters.
pub fn make_genesis_block(
    parent_hash: B256,
    block_number: u64,
    timestamp: u64,
    state_root: B256,
    initial_arbos_version: u64,
) -> GenesisBlockInfo {
    use crate::l2_pricing;

    GenesisBlockInfo {
        parent_hash,
        block_number,
        timestamp,
        state_root,
        gas_limit: l2_pricing::GETH_BLOCK_GAS_LIMIT,
        base_fee: l2_pricing::INITIAL_BASE_FEE_WEI,
        nonce: 1, // genesis reads the init message
        arbos_format_version: initial_arbos_version,
    }
}

/// Initialize retryable tickets from genesis data.
///
/// Expired retryables (timeout <= current_timestamp) are skipped, and their
/// call value is returned as `(beneficiary, callvalue)` pairs for the caller
/// to credit balances. Active retryables are sorted by timeout and created.
///
/// Returns `(balance_credits, escrow_credits)` where:
/// - `balance_credits`: expired retryable beneficiaries to credit
/// - `escrow_credits`: (escrow_address, callvalue) for active retryable escrow funding
pub fn initialize_retryables<D: Database, C: StorageBackend>(
    backend: &mut C,
    rs: &RetryableState<D>,
    mut retryables_data: Vec<InitRetryableData>,
    current_timestamp: u64,
) -> Result<(Vec<(Address, U256)>, Vec<(Address, U256)>), ArbosStateError> {
    let mut balance_credits = Vec::new();
    let mut active_retryables = Vec::new();

    for r in retryables_data.drain(..) {
        if r.timeout <= current_timestamp {
            balance_credits.push((r.beneficiary, r.callvalue));
            continue;
        }
        active_retryables.push(r);
    }

    active_retryables.sort_by(|a, b| a.timeout.cmp(&b.timeout).then_with(|| a.id.cmp(&b.id)));

    let mut escrow_credits = Vec::new();

    for r in &active_retryables {
        let escrow_addr = retryables::retryable_escrow_address(r.id);
        escrow_credits.push((escrow_addr, r.callvalue));
        rs.create_retryable(
            backend,
            r.id,
            r.timeout,
            r.from,
            r.to,
            r.callvalue,
            r.beneficiary,
            &r.calldata,
        )?;
    }

    Ok((balance_credits, escrow_credits))
}

/// Initialize an account's ArbOS-specific state during genesis.
///
/// If the account has aggregator info and is a known batch poster,
/// sets the batch poster's pay-to (fee collector) address.
pub fn initialize_arbos_account<D: Database, B: Burner, C: StorageBackend>(
    backend: &mut C,
    arbos_state: &ArbosState<'_, D, B>,
    account: &AccountInitInfo,
) -> Result<(), ArbosStateError> {
    if let Some(ref aggregator) = account.aggregator_info {
        let poster_table = arbos_state.l1_pricing_state.batch_poster_table();
        let is_poster = poster_table.contains_poster(backend, account.addr)?;
        if is_poster {
            let poster = poster_table.open_poster(backend, account.addr, false)?;
            poster.set_pay_to(backend, aggregator.fee_collector)?;
        }
    }
    Ok(())
}

/// Full database initialization for ArbOS genesis.
///
/// This is the high-level orchestrator that:
/// 1. Initializes ArbOS state (version upgrades, precompile code)
/// 2. Adds chain owner
/// 3. Imports address table entries
/// 4. Imports retryable tickets
/// 5. Imports account state (balances, nonces, code, storage, batch poster config)
///
/// The caller provides the state database, init data, and handles commits.
/// Balance credits from expired retryables and escrow funding are returned
/// for the caller to execute against the state.
#[derive(Debug)]
pub struct GenesisInitResult {
    /// Expired retryable beneficiaries to credit.
    pub balance_credits: Vec<(Address, U256)>,
    /// Escrow addresses to fund for active retryables.
    pub escrow_credits: Vec<(Address, U256)>,
    /// Accounts to initialize (balances, nonces, code, storage).
    pub accounts: Vec<AccountInitInfo>,
}

/// Initialize ArbOS in the database.
///
/// Creates the ArbOS state, adds the chain owner, imports address table
/// entries, retryable tickets, and accounts. Returns a `GenesisInitResult`
/// containing all balance operations the caller needs to execute.
pub fn initialize_arbos_in_database<D: Database, B: Burner, C: StorageBackend>(
    backend: &mut C,
    arbos_state: &ArbosState<'_, D, B>,
    chain_owner: Address,
    address_table_entries: Vec<Address>,
    retryable_data: Vec<InitRetryableData>,
    accounts: Vec<AccountInitInfo>,
    current_timestamp: u64,
) -> Result<GenesisInitResult, ArbosStateError> {
    if chain_owner != Address::ZERO {
        arbos_state.chain_owners.add(backend, chain_owner)?;
    }

    let table_size = arbos_state.address_table.size(backend)?;
    if table_size != 0 {
        return Err(ArbosStateError::AddressTableNotEmpty);
    }
    for (i, addr) in address_table_entries.iter().enumerate() {
        let (slot, _) = arbos_state.address_table.register(backend, *addr)?;
        if slot != i as u64 {
            return Err(ArbosStateError::AddressTableSlotMismatch);
        }
    }

    let (balance_credits, escrow_credits) = initialize_retryables(
        backend,
        &arbos_state.retryable_state,
        retryable_data,
        current_timestamp,
    )?;

    for account in &accounts {
        initialize_arbos_account(backend, arbos_state, account)?;
    }

    Ok(GenesisInitResult {
        balance_credits,
        escrow_credits,
        accounts,
    })
}

/// Bring a fresh database to a fully-initialised ArbOS state at the requested
/// version. Mirrors Nitro's `arbos/arbosState/arbosstate.go::InitializeArbosState`
/// — sets the well-known root offsets, initialises every subspace, adds the
/// initial chain owner, and upgrades through to `target_arbos_version`.
///
/// `genesis_block_num` is the on-chain genesis block (`GenesisBlockNum` from
/// `chain_info.json`; zero for fresh chains, non-zero for migrated chains
/// like arb1). `serialized_chain_config` is `json.Marshal(*params.ChainConfig)`
/// of the chain's config; pass an empty slice for fresh chains that don't
/// derive a chain config from an init message.
///
/// `network_fee_account` follows Nitro: set to `initial_chain_owner` for
/// `target_arbos_version >= 2`, otherwise zero. `infra_fee_account` is never
/// set here (Nitro leaves it zero until `SetInfraFeeAccount` is called by a
/// chain-owner action).
pub fn bootstrap<'a, D: Database, B: Burner>(
    state: &'a mut State<D>,
    chain_id: u64,
    initial_chain_owner: Address,
    genesis_block_num: u64,
    serialized_chain_config: &[u8],
    l1_initial_base_fee: U256,
    target_arbos_version: u64,
    burner: B,
) -> Result<ArbosState<'a, D, B>, ArbosStateError> {
    set_account_nonce(state, ARBOS_STATE_ADDRESS, 1);

    {
        let backing = Storage::<D>::new(state, B256::ZERO);

        // Root-namespace slot writes (matches arbosstate.go:265-303).
        backing.set_by_uint64(super::VERSION_OFFSET, B256::from(U256::from(1u64)))?;

        // SAFETY: each `state_mut()` borrow is dropped before the next; `backing`
        // is the only live `Storage<D>` handle in this scope.
        let s = unsafe { backing.state_mut() };
        StorageBackedBigUint::new(B256::ZERO, super::CHAIN_ID_OFFSET)
            .set(s, U256::from(chain_id))?;

        // Nitro: networkFeeAccount = initialChainOwner only when v>=2; v1 leaves
        // it zero until a chain-owner action sets it. Same convention here.
        if target_arbos_version >= 2 {
            let s = unsafe { backing.state_mut() };
            StorageBackedAddress::new(B256::ZERO, super::NETWORK_FEE_ACCOUNT_OFFSET)
                .set(s, initial_chain_owner)?;
        }

        let s = unsafe { backing.state_mut() };
        StorageBackedUint64::new(B256::ZERO, super::GENESIS_BLOCK_NUM_OFFSET)
            .set(s, genesis_block_num)?;

        if !serialized_chain_config.is_empty() {
            let cc_sto = backing.open_sub_storage(super::CHAIN_CONFIG_SUBSPACE);
            let s = unsafe { backing.state_mut() };
            StorageBackedBytes::new(cc_sto.base_key()).set(s, serialized_chain_config)?;
        }

        // Subspace inits. The address-set inits write `size = 0` at slot 0,
        // which geth's commit prunes (no trie effect); the merkle-accumulator
        // and blockhash inits are no-ops. We call them anyway to match Nitro's
        // structure and surface any future non-zero-init logic.
        let l1_sto = backing.open_sub_storage(super::L1_PRICING_SUBSPACE);
        let initial_rewards_recipient = if target_arbos_version >= 2 {
            initial_chain_owner
        } else {
            l1_pricing::BATCH_POSTER_ADDRESS
        };
        let s = unsafe { backing.state_mut() };
        L1PricingState::initialize(&l1_sto, s, initial_rewards_recipient, l1_initial_base_fee)?;

        let l2_sto = backing.open_sub_storage(super::L2_PRICING_SUBSPACE);
        let s = unsafe { backing.state_mut() };
        L2PricingState::<D>::initialize(&l2_sto, s)?;

        RetryableState::<D>::initialize(&backing.open_sub_storage(super::RETRYABLES_SUBSPACE))?;

        address_set::initialize_address_set(
            &backing.open_sub_storage(super::CHAIN_OWNER_SUBSPACE),
        )?;
        address_set::initialize_address_set(
            &backing.open_sub_storage(super::NATIVE_TOKEN_SUBSPACE),
        )?;
        address_set::initialize_address_set(
            &backing.open_sub_storage(super::TRANSACTION_FILTERER_SUBSPACE),
        )?;
    }

    // Open ArbosState now that version=1 is persisted, then add the initial
    // chain owner (matches arbosstate.go:333) and step through versions.
    let mut arbos = ArbosState::open(state, burner)?;

    // SAFETY: see `Storage` struct-level invariant.
    let s = unsafe { arbos.backing_storage.state_mut() };
    arbos.chain_owners.add(s, initial_chain_owner)?;

    let s = unsafe { arbos.backing_storage.state_mut() };
    arbos.upgrade_arbos_version(s, target_arbos_version, true)?;
    Ok(arbos)
}
