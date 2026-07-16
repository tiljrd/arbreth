use alloy_primitives::{Address, U256};
use arb_storage::ARBOS_STATE_ADDRESS;
use arb_test_utils::db::{ensure_cache_account, EmptyDb};
use arbos::{
    arbos_state::{initialize::bootstrap, ArbosStateError},
    burn::SystemBurner,
};
use revm_database::{states::StateBuilder, State};

fn fresh_state() -> State<EmptyDb> {
    let mut state = StateBuilder::new()
        .with_database(EmptyDb)
        .with_bundle_update()
        .build();
    ensure_cache_account(&mut state, ARBOS_STATE_ADDRESS);
    state
}

#[test]
fn bootstrap_rejects_version_zero() {
    let mut state = fresh_state();
    let result = bootstrap(
        &mut state,
        412346,
        Address::ZERO,
        0,
        &[],
        U256::from(100_000_000u64),
        0,
        SystemBurner::new(None, false),
    );
    let Err(err) = result else {
        panic!("bootstrap must reject version 0")
    };
    assert!(matches!(err, ArbosStateError::InvalidInitialVersion));
}

#[test]
fn bootstrap_rejects_already_initialised_state() {
    let mut state = fresh_state();
    bootstrap(
        &mut state,
        412346,
        Address::ZERO,
        0,
        &[],
        U256::from(100_000_000u64),
        6,
        SystemBurner::new(None, false),
    )
    .expect("first bootstrap");
    let result = bootstrap(
        &mut state,
        412346,
        Address::ZERO,
        0,
        &[],
        U256::from(100_000_000u64),
        6,
        SystemBurner::new(None, false),
    );
    let Err(err) = result else {
        panic!("bootstrap must reject an initialised state")
    };
    assert!(matches!(err, ArbosStateError::AlreadyInitialised));
}
