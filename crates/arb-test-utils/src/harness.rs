//! In-memory ArbOS state for unit tests.

use alloy_primitives::{Address, B256, U256};
use arb_storage::{Storage, ARBOS_STATE_ADDRESS};
use arbos::{
    arbos_state::{initialize::bootstrap, ArbosState},
    burn::SystemBurner,
    l1_pricing::L1PricingState,
    l2_pricing::L2PricingState,
    retryables::RetryableState,
};
use revm::database::{State, StateBuilder};

use crate::db::{ensure_cache_account, EmptyDb};

/// Builder + handle for an in-memory ArbOS state.
pub struct ArbosHarness {
    state: Box<State<EmptyDb>>,
    arbos_version: u64,
    chain_id: u64,
    initial_chain_owner: Address,
    genesis_block_num: u64,
    serialized_chain_config: Vec<u8>,
    l1_initial_base_fee: U256,
    network_fee_account: Option<Address>,
    initialized: bool,
}

impl Default for ArbosHarness {
    fn default() -> Self {
        Self::new()
    }
}

impl ArbosHarness {
    /// Defaults: ArbOS v30, chain id 412346, L1 base fee 0.1 gwei, chain owner
    /// = zero, genesis block 0, no serialized chain config.
    pub fn new() -> Self {
        let state = Box::new(
            StateBuilder::new()
                .with_database(EmptyDb)
                .with_bundle_update()
                .build(),
        );
        Self {
            state,
            arbos_version: 30,
            chain_id: 412346,
            initial_chain_owner: Address::ZERO,
            genesis_block_num: 0,
            serialized_chain_config: Vec::new(),
            l1_initial_base_fee: U256::from(100_000_000u64),
            network_fee_account: None,
            initialized: false,
        }
    }

    pub fn with_arbos_version(mut self, v: u64) -> Self {
        assert!(!self.initialized, "set version before initialize()");
        self.arbos_version = v;
        self
    }

    pub fn with_chain_id(mut self, id: u64) -> Self {
        assert!(!self.initialized, "set chain id before initialize()");
        self.chain_id = id;
        self
    }

    /// Initial chain owner. For ArbOS v >= 2, `bootstrap` also writes this as
    /// the network fee account.
    pub fn with_initial_chain_owner(mut self, a: Address) -> Self {
        assert!(!self.initialized, "set chain owner before initialize()");
        self.initial_chain_owner = a;
        self
    }

    /// On-chain genesis block (`chain_info.json::GenesisBlockNum`). Non-zero
    /// for migrated chains (arb1 = 22207818); zero for fresh chains.
    pub fn with_genesis_block_num(mut self, n: u64) -> Self {
        assert!(
            !self.initialized,
            "set genesis block num before initialize()"
        );
        self.genesis_block_num = n;
        self
    }

    /// `json.Marshal(*params.ChainConfig)` bytes stored at chain-config
    /// subspace. Empty for fresh chains that don't derive a config from an
    /// init message.
    pub fn with_serialized_chain_config(mut self, bytes: Vec<u8>) -> Self {
        assert!(!self.initialized, "set chain config before initialize()");
        self.serialized_chain_config = bytes;
        self
    }

    pub fn with_l1_initial_base_fee(mut self, fee: U256) -> Self {
        assert!(!self.initialized, "set base fee before initialize()");
        self.l1_initial_base_fee = fee;
        self
    }

    /// Override the network fee account. `bootstrap` defaults it to the chain
    /// owner for ArbOS v >= 2; this applies the equivalent of a later
    /// `ArbOwner.setNetworkFeeAccount` so tests can pin a distinct sink.
    pub fn with_network_fee_account(mut self, a: Address) -> Self {
        assert!(
            !self.initialized,
            "set network fee account before initialize()"
        );
        self.network_fee_account = Some(a);
        self
    }

    pub fn initialize(mut self) -> Self {
        assert!(!self.initialized, "initialize() called twice");

        ensure_cache_account(&mut self.state, ARBOS_STATE_ADDRESS);

        bootstrap(
            &mut self.state,
            self.chain_id,
            self.initial_chain_owner,
            self.genesis_block_num,
            &self.serialized_chain_config,
            self.l1_initial_base_fee,
            self.arbos_version,
            SystemBurner::new(None, false),
        )
        .expect("bootstrap ArbOS state");

        if let Some(account) = self.network_fee_account {
            let state_ptr: *mut State<EmptyDb> = self.state.as_mut();
            // SAFETY: single-threaded test setup; no other live borrow of `state`.
            let state: &mut State<EmptyDb> = unsafe { &mut *state_ptr };
            let arb_state =
                ArbosState::open(state, SystemBurner::new(None, false)).expect("open arbos state");
            // SAFETY: `arb_state` is the sole live handle for this write.
            let backing = unsafe { arb_state.backing_storage.state_mut() };
            arb_state
                .set_network_fee_account(backing, account)
                .expect("set network fee account");
        }

        self.initialized = true;
        self
    }

    pub fn state(&mut self) -> &mut State<EmptyDb> {
        &mut self.state
    }

    pub fn state_ptr(&mut self) -> *mut State<EmptyDb> {
        self.state.as_mut()
    }

    /// Returns an [`ArbosState`] with `'static` lifetime — test-only escape
    /// hatch that decouples the returned value from the harness's `&mut self`
    /// borrow so multiple subsystems can be opened from one harness without
    /// triggering borrow conflicts. Soundness is preserved by the harness
    /// owning the underlying `Box<State>` for its full lifetime; callers must
    /// not retain these handles past the harness drop.
    pub fn arbos_state(&mut self) -> ArbosState<'static, EmptyDb, SystemBurner> {
        assert!(self.initialized, "call initialize() first");
        // SAFETY: see the doc comment above; the harness keeps the `State` in
        // a heap allocation for its lifetime and the harness is the sole
        // owner. This is a test utility.
        let state: &'static mut State<EmptyDb> = unsafe { &mut *self.state_ptr() };
        ArbosState::open(state, SystemBurner::new(None, false)).expect("open arbos state")
    }

    pub fn l1_pricing_state(&mut self) -> L1PricingState<'static, EmptyDb> {
        self.arbos_state().l1_pricing_state
    }

    pub fn l2_pricing_state(&mut self) -> L2PricingState<'static, EmptyDb> {
        self.arbos_state().l2_pricing_state
    }

    pub fn retryable_state(&mut self) -> RetryableState<'static, EmptyDb> {
        self.arbos_state().retryable_state
    }

    /// Test-only `Storage` handle. The `'static` lifetime is a test escape
    /// hatch — see [`Self::arbos_state`].
    pub fn root_storage(&mut self) -> Storage<'static, EmptyDb> {
        // SAFETY: see [`Self::arbos_state`].
        let state: &'static mut State<EmptyDb> = unsafe { &mut *self.state_ptr() };
        Storage::new(state, B256::ZERO)
    }

    pub fn arbos_version(&self) -> u64 {
        self.arbos_version
    }

    pub fn chain_id(&self) -> u64 {
        self.chain_id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn harness_initializes_at_v30() {
        let mut h = ArbosHarness::new().with_arbos_version(30).initialize();
        let s = h.arbos_state();
        assert_eq!(s.arbos_version(), 30);
    }

    #[test]
    fn harness_initializes_at_v60() {
        let mut h = ArbosHarness::new().with_arbos_version(60).initialize();
        let s = h.arbos_state();
        assert_eq!(s.arbos_version(), 60);
    }

    #[test]
    fn l1_pricing_state_starts_at_zero_last_update_time() {
        let mut h = ArbosHarness::new().initialize();
        let state_ptr = h.state_ptr();
        let l1 = h.l1_pricing_state();
        assert_eq!(l1.last_update_time(unsafe { &mut *state_ptr }).unwrap(), 0);
    }

    #[test]
    fn l1_pricing_state_starts_at_configured_base_fee() {
        let initial = U256::from(123u64) * U256::from(1_000_000_000u64);
        let mut h = ArbosHarness::new()
            .with_l1_initial_base_fee(initial)
            .initialize();
        let state_ptr = h.state_ptr();
        let l1 = h.l1_pricing_state();
        assert_eq!(
            l1.price_per_unit(unsafe { &mut *state_ptr }).unwrap(),
            initial
        );
    }

    #[test]
    fn chain_id_round_trips() {
        let mut h = ArbosHarness::new().with_chain_id(421614).initialize();
        let state_ptr = h.state_ptr();
        let s = h.arbos_state();
        assert_eq!(
            s.chain_id(unsafe { &mut *state_ptr }).unwrap(),
            U256::from(421614u64)
        );
    }
}
