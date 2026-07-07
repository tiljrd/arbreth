//! A scheduled ArbOS upgrade fires inside StartBlock; user txs in the SAME
//! block must run with the NEW precompile band. The reference re-stamps the
//! in-progress header version right after the internal tx, so every
//! subsequent tx derives its dispatch map and EIP-2929 warm-preload set from
//! the post-upgrade version. A producer that keeps the parent-version band
//! for the whole block under-warms the newly activated address (2,500 gas)
//! and executes its 0xFE stub instead of the precompile.

#[cfg(target_arch = "x86_64")]
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn __rust_probestack() {}

use std::sync::Arc;

use alloy_consensus::transaction::Recovered;
use alloy_evm::{
    block::{BlockExecutor, BlockExecutorFactory},
    eth::EthBlockExecutionCtx,
    EvmFactory,
};
use alloy_primitives::{address, keccak256, Address, Bytes, Signature, TxKind, B256, U256};
use arb_alloy_consensus::tx::ArbInternalTx;
use arb_evm::{
    config::ArbEvmConfig,
    evm::ArbEvmFactory,
    multi_gas::{MultiGasInspector, MultiGasSink},
};
use arb_executor_tests::helpers::{
    alice, alice_key, bob, bob_key, charlie, charlie_key, deploy_contract, fund_account, recover,
    sign_legacy,
};
use arb_primitives::{signed_tx::ArbTypedTransaction, ArbTransactionSigned};
use arb_test_utils::{ArbosHarness, EmptyDb};
use arbos::internal_tx::encode_start_block;
use reth_chainspec::ChainSpec;
use reth_evm::{ConfigureEvm, EvmEnv};
use revm::{database::State, primitives::hardfork::SpecId};

const CHAIN_ID: u64 = 42161;
const SCHEDULED_TS: u64 = 1_700_000_000;
const FIRE_TS: u64 = 1_700_000_100;
const BLOCK_NUMBER: u64 = 5;
const L1_BLOCK_NUMBER: u64 = 100;

const FILTERED_TX_MANAGER: Address = address!("0000000000000000000000000000000000000074");
const ARBWASM: Address = address!("0000000000000000000000000000000000000071");
const TWIN: Address = address!("4444444444444444444444444444444444444444");
const PROBE_GATED: Address = address!("3333333333333333333333333333333333333333");
const PROBE_TWIN: Address = address!("5555555555555555555555555555555555555555");
const ARBOS_ADDRESS: Address = address!("00000000000000000000000000000000000A4B05");

const COLD_WARM_DELTA: i128 = 2500;
const DISPATCH_GAS_LIMIT: u64 = 1_000_000;

/// `PUSH20 probe; BALANCE; POP; STOP`.
fn balance_probe_code(probe: Address) -> Vec<u8> {
    let mut code = vec![0x73];
    code.extend_from_slice(probe.as_slice());
    code.extend_from_slice(&[0x31, 0x50, 0x00]);
    code
}

fn is_transaction_filtered_calldata() -> Bytes {
    let mut d = keccak256("isTransactionFiltered(bytes32)")[..4].to_vec();
    d.extend_from_slice(&[0u8; 32]);
    Bytes::from(d)
}

fn stylus_version_calldata() -> Bytes {
    Bytes::from(keccak256("stylusVersion()")[..4].to_vec())
}

struct RunOut {
    gated_gas: u64,
    twin_gas: u64,
    dispatch_success: bool,
    dispatch_gas: u64,
    stub_written: bool,
}

/// Runs StartBlock (which fires the scheduled upgrade) followed by the three
/// probe txs on an already-created EVM, so the same block drives both the
/// plain and the inspector execution paths.
fn drive<'a, I>(
    cfg: &'a ArbEvmConfig,
    evm: <ArbEvmFactory as EvmFactory>::Evm<&'a mut State<EmptyDb>, I>,
    sink: Option<MultiGasSink>,
    gated: Address,
    dispatch_data: Bytes,
) -> (u64, u64, bool, u64)
where
    I: revm::Inspector<<ArbEvmFactory as EvmFactory>::Context<&'a mut State<EmptyDb>>> + 'a,
{
    let exec_ctx = EthBlockExecutionCtx {
        tx_count_hint: Some(4),
        parent_hash: B256::ZERO,
        parent_beacon_block_root: None,
        ommers: &[],
        withdrawals: None,
        extra_data: vec![0u8; 32].into(),
    };
    let mut executor = cfg
        .block_executor_factory()
        .create_arb_executor(evm, exec_ctx, CHAIN_ID);
    if let Some(sink) = sink {
        executor.set_multi_gas_sink(sink);
    }
    executor.arb_ctx.block_timestamp = FIRE_TS;
    executor.arb_ctx.basefee = U256::from(100_000_000u64);
    executor.arb_ctx.l2_block_number = BLOCK_NUMBER;
    executor.arb_ctx.l1_block_number = L1_BLOCK_NUMBER;
    executor.apply_pre_execution_changes().expect("pre-exec");

    let sb = ArbTransactionSigned::new_unhashed(
        ArbTypedTransaction::Internal(ArbInternalTx {
            chain_id: U256::from(CHAIN_ID),
            data: encode_start_block(U256::ZERO, L1_BLOCK_NUMBER, BLOCK_NUMBER, 1).into(),
        }),
        Signature::new(U256::ZERO, U256::ZERO, false),
    );
    let r = executor
        .execute_transaction_without_commit(Recovered::new_unchecked(sb, ARBOS_ADDRESS))
        .expect("startblock");
    executor.commit_transaction(r).expect("commit sb");

    let mut user_tx = |to: Address, input: Bytes, gas_limit: u64, key: [u8; 32]| -> (bool, u64) {
        let tx = sign_legacy(
            CHAIN_ID,
            0,
            100_000_000,
            gas_limit,
            TxKind::Call(to),
            U256::ZERO,
            input,
            key,
        );
        let r = executor
            .execute_transaction_without_commit(recover(tx))
            .expect("user tx");
        let out = (r.result.result.is_success(), r.result.result.gas_used());
        executor.commit_transaction(r).expect("commit user");
        out
    };

    let (gated_ok, gated_gas) = user_tx(PROBE_GATED, Bytes::new(), 30_000_000, alice_key());
    let (twin_ok, twin_gas) = user_tx(PROBE_TWIN, Bytes::new(), 30_000_000, bob_key());
    let (dispatch_success, dispatch_gas) =
        user_tx(gated, dispatch_data, DISPATCH_GAS_LIMIT, charlie_key());
    assert!(gated_ok && twin_ok, "probe txs must succeed");
    let _ = executor.finish().expect("finish");
    (gated_gas, twin_gas, dispatch_success, dispatch_gas)
}

/// Executes one block: StartBlock (fires the scheduled upgrade), then three
/// user txs — a BALANCE probe of the gated address, a BALANCE probe of a
/// twin address, and a dispatch call to the gated address. `staged_version`
/// selects the band the EVM is registered with: the producer stages the
/// parent's version, a follower stages the sealed header's (post-upgrade)
/// version. `with_inspector` drives the multi-gas inspector path the live
/// producer runs.
fn run(
    staged_version: u64,
    parent_version: u64,
    new_version: u64,
    gated: Address,
    dispatch_data: Bytes,
    with_inspector: bool,
) -> RunOut {
    let mut harness = ArbosHarness::new()
        .with_arbos_version(parent_version)
        .with_chain_id(CHAIN_ID)
        .initialize();

    {
        let state_ptr = harness.state_ptr();
        let state = harness.arbos_state();
        let backend = unsafe { &mut *state_ptr };
        state
            .schedule_arbos_upgrade(backend, new_version, SCHEDULED_TS)
            .expect("schedule upgrade");
    }
    fund_account(harness.state(), alice(), U256::from(1u128 << 100));
    fund_account(harness.state(), bob(), U256::from(1u128 << 100));
    fund_account(harness.state(), charlie(), U256::from(1u128 << 100));
    deploy_contract(
        harness.state(),
        PROBE_GATED,
        balance_probe_code(gated),
        U256::ZERO,
    );
    deploy_contract(
        harness.state(),
        PROBE_TWIN,
        balance_probe_code(TWIN),
        U256::ZERO,
    );
    harness
        .state()
        .merge_transitions(revm::database::states::bundle_state::BundleRetention::Reverts);

    let chain_spec: Arc<ChainSpec> = Arc::new(
        reth_chainspec::ChainSpecBuilder::default()
            .chain(CHAIN_ID.into())
            .genesis(Default::default())
            .shanghai_activated()
            .build(),
    );
    let cfg = ArbEvmConfig::new(chain_spec);
    let mut env: EvmEnv<SpecId> = EvmEnv {
        cfg_env: revm::context::CfgEnv::new()
            .with_chain_id(CHAIN_ID)
            .with_spec_and_mainnet_gas_params(arb_chainspec::spec_id_by_arbos_version(
                staged_version,
            )),
        block_env: revm::context::BlockEnv::default(),
    };
    env.cfg_env.disable_base_fee = true;
    env.cfg_env.tx_gas_limit_cap = Some(u64::MAX);
    env.cfg_env.disable_eip3541 =
        staged_version >= arb_chainspec::arbos_version::ARBOS_VERSION_STYLUS;
    env.block_env.timestamp = U256::from(FIRE_TS);
    env.block_env.basefee = 100_000_000;
    env.block_env.gas_limit = 1_125_899_906_842_624;
    env.block_env.number = U256::from(BLOCK_NUMBER);

    let evm_factory = cfg.block_executor_factory().evm_factory();
    let block_ctx = arb_context::BlockCtx::new(
        staged_version,
        FIRE_TS,
        L1_BLOCK_NUMBER,
        BLOCK_NUMBER,
        false,
    );
    evm_factory.stage_ctx(Arc::new(arb_context::ArbPrecompileCtx::with_block(
        Arc::new(block_ctx),
    )));

    let (gated_gas, twin_gas, dispatch_success, dispatch_gas) = if with_inspector {
        let sink = MultiGasSink::default();
        let evm = evm_factory.create_evm_with_inspector(
            harness.state(),
            env,
            MultiGasInspector::with_sink(sink.clone()),
        );
        drive(&cfg, evm, Some(sink), gated, dispatch_data)
    } else {
        let evm = evm_factory.create_evm(harness.state(), env);
        drive(&cfg, evm, None, gated, dispatch_data)
    };

    // Guard: the upgrade really fired during StartBlock — the newly activated
    // precompile address carries its 0xFE stub.
    harness
        .state()
        .merge_transitions(revm::database::states::bundle_state::BundleRetention::Reverts);
    let stub_written = harness
        .state()
        .bundle_state
        .state
        .get(&gated)
        .and_then(|a| a.info.as_ref())
        .map(|i| i.code_hash == keccak256([0xFEu8]))
        .unwrap_or(false);

    RunOut {
        gated_gas,
        twin_gas,
        dispatch_success,
        dispatch_gas,
        stub_written,
    }
}

fn assert_new_band(out: &RunOut, gated: Address) {
    assert!(out.stub_written, "upgrade must fire during StartBlock");
    let delta = out.twin_gas as i128 - out.gated_gas as i128;
    assert_eq!(
        delta, COLD_WARM_DELTA,
        "{gated} must be warm-preloaded for txs after the upgrade fires \
         (gated {}, twin {})",
        out.gated_gas, out.twin_gas,
    );
    assert!(
        out.dispatch_success,
        "{gated} must dispatch as an active precompile after the upgrade \
         fires (gas {} of {DISPATCH_GAS_LIMIT})",
        out.dispatch_gas,
    );
}

/// The buggy live path: the producer stages the parent's version, so without
/// a mid-block band refresh the upgrade block's user txs run on the stale map.
#[test]
fn producer_staged_v59_to_v60_gets_new_band() {
    let out = run(
        59,
        59,
        60,
        FILTERED_TX_MANAGER,
        is_transaction_filtered_calldata(),
        false,
    );
    assert_new_band(&out, FILTERED_TX_MANAGER);
}

/// The live producer executes through the multi-gas inspector; the refresh
/// must hold on that path too.
#[test]
fn producer_staged_v59_to_v60_inspector_path_gets_new_band() {
    let out = run(
        59,
        59,
        60,
        FILTERED_TX_MANAGER,
        is_transaction_filtered_calldata(),
        true,
    );
    assert_new_band(&out, FILTERED_TX_MANAGER);
}

/// Follower/re-exec semantics: the sealed header carries the post-upgrade
/// version, so a header-staged run must give the same new-band results.
#[test]
fn follower_staged_v59_to_v60_gets_new_band() {
    let out = run(
        60,
        59,
        60,
        FILTERED_TX_MANAGER,
        is_transaction_filtered_calldata(),
        false,
    );
    assert_new_band(&out, FILTERED_TX_MANAGER);
}

/// The Stylus activation boundary: ArbWasm joins the band at v30.
#[test]
fn producer_staged_v29_to_v30_gets_new_band() {
    let out = run(29, 29, 30, ARBWASM, stylus_version_calldata(), false);
    assert_new_band(&out, ARBWASM);
}

/// A multi-step ladder fired by one StartBlock, matching the shape of a real
/// chain jumping several versions at once.
#[test]
fn producer_staged_v20_to_v31_gets_new_band() {
    let out = run(20, 20, 31, ARBWASM, stylus_version_calldata(), false);
    assert_new_band(&out, ARBWASM);
}
