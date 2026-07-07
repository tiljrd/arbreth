//! A precompile address must join the EIP-2929 warm-preload set exactly at
//! its introduction version. Below it the address is a plain account: a
//! `BALANCE` probe pays the same cold access as any untouched twin address,
//! and a value transfer to it lands on the account instead of being routed
//! to precompile handling.

#[cfg(target_arch = "x86_64")]
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn __rust_probestack() {}

use std::sync::Arc;

use alloy_evm::{
    block::{BlockExecutor, BlockExecutorFactory},
    eth::EthBlockExecutionCtx,
    EvmFactory,
};
use alloy_primitives::{address, Address, Bytes, TxKind, B256, U256};
use arb_evm::config::ArbEvmConfig;
use arb_executor_tests::helpers::{
    alice, alice_key, balance_of, deploy_contract, fund_account, recover, sign_legacy,
};
use arb_test_utils::ArbosHarness;
use reth_chainspec::ChainSpec;
use reth_evm::{ConfigureEvm, EvmEnv};
use revm::primitives::hardfork::SpecId;

const CHAIN_ID: u64 = 42161;
const ARBWASM: Address = address!("0000000000000000000000000000000000000071");
const ARBNATIVETOKENMANAGER: Address = address!("0000000000000000000000000000000000000073");
const FILTERED_TX_MANAGER: Address = address!("0000000000000000000000000000000000000074");
const TWIN: Address = address!("4444444444444444444444444444444444444444");
const CALLER: Address = address!("3333333333333333333333333333333333333333");

const COLD_WARM_DELTA: u64 = 2500;

const GATED: [(Address, u64); 3] = [
    (ARBWASM, 30),
    (ARBNATIVETOKENMANAGER, 41),
    (FILTERED_TX_MANAGER, 60),
];

/// `PUSH20 probe; BALANCE; POP; STOP` — identical cost apart from the
/// warm/cold state of `probe`.
fn balance_probe_code(probe: Address) -> Vec<u8> {
    let mut code = vec![0x73];
    code.extend_from_slice(probe.as_slice());
    code.extend_from_slice(&[0x31, 0x50, 0x00]);
    code
}

fn run(arbos_version: u64, deploy: Option<(Address, Vec<u8>)>, to: Address, value: U256) -> u64 {
    let mut harness = ArbosHarness::new()
        .with_arbos_version(arbos_version)
        .with_chain_id(CHAIN_ID)
        .initialize();

    fund_account(harness.state(), alice(), U256::from(1u128 << 100));
    if let Some((addr, code)) = deploy {
        deploy_contract(harness.state(), addr, code, U256::ZERO);
    }
    harness
        .state()
        .merge_transitions(revm::database::states::bundle_state::BundleRetention::Reverts);

    let cfg = ArbEvmConfig::new(Arc::new(ChainSpec::default()));
    let mut env: EvmEnv<SpecId> = EvmEnv {
        cfg_env: revm::context::CfgEnv::new()
            .with_chain_id(CHAIN_ID)
            .with_spec_and_mainnet_gas_params(arb_chainspec::spec_id_by_arbos_version(
                arbos_version,
            )),
        block_env: revm::context::BlockEnv::default(),
    };
    env.cfg_env.disable_base_fee = true;
    env.cfg_env.tx_gas_limit_cap = Some(u64::MAX);
    env.block_env.basefee = 100_000_000;
    env.block_env.gas_limit = 1_125_899_906_842_624;
    env.block_env.number = U256::from(1u64);

    let evm_factory = cfg.block_executor_factory().evm_factory();
    let block_ctx = arb_context::BlockCtx::new(arbos_version, 1_700_000_000, 1, 1, false);
    evm_factory.stage_ctx(Arc::new(arb_context::ArbPrecompileCtx::with_block(
        Arc::new(block_ctx),
    )));
    let evm = evm_factory.create_evm(harness.state(), env);
    let exec_ctx = EthBlockExecutionCtx {
        tx_count_hint: Some(1),
        parent_hash: B256::ZERO,
        parent_beacon_block_root: None,
        ommers: &[],
        withdrawals: None,
        extra_data: vec![0u8; 32].into(),
    };
    let mut executor = cfg
        .block_executor_factory()
        .create_arb_executor(evm, exec_ctx, CHAIN_ID);
    executor
        .apply_pre_execution_changes()
        .expect("pre-execution changes");

    let tx = sign_legacy(
        CHAIN_ID,
        0,
        100_000_000,
        30_000_000,
        TxKind::Call(to),
        value,
        Bytes::new(),
        alice_key(),
    );
    let exec_result = executor
        .execute_transaction_without_commit(recover(tx))
        .expect("execute tx");
    assert!(exec_result.result.result.is_success(), "tx must succeed");
    let gas_used = exec_result.result.result.gas_used();
    executor.commit_transaction(exec_result).expect("commit");
    let _ = executor.finish().expect("finish");
    gas_used
}

fn probe_gas(arbos_version: u64, probe: Address) -> u64 {
    run(
        arbos_version,
        Some((CALLER, balance_probe_code(probe))),
        CALLER,
        U256::ZERO,
    )
}

#[test]
fn below_activation_address_is_cold() {
    for (addr, min) in GATED {
        let version = min - 1;
        let gated = probe_gas(version, addr);
        let twin = probe_gas(version, TWIN);
        assert_eq!(
            gated,
            twin,
            "{addr} must cost the same as an untouched twin at ArbOS {version} (delta {})",
            twin as i64 - gated as i64,
        );
    }
}

#[test]
fn at_activation_address_is_warm() {
    for (addr, min) in GATED {
        let gated = probe_gas(min, addr);
        let twin = probe_gas(min, TWIN);
        assert_eq!(
            twin - gated,
            COLD_WARM_DELTA,
            "{addr} must be warm-preloaded at ArbOS {min} (gated {gated}, twin {twin})",
        );
    }
}

#[test]
fn pre_stylus_value_to_arbwasm_transfers() {
    let arbos_version = 20;
    let value = U256::from(12_345u64);

    let mut harness = ArbosHarness::new()
        .with_arbos_version(arbos_version)
        .with_chain_id(CHAIN_ID)
        .initialize();
    fund_account(harness.state(), alice(), U256::from(1u128 << 100));
    harness
        .state()
        .merge_transitions(revm::database::states::bundle_state::BundleRetention::Reverts);

    let cfg = ArbEvmConfig::new(Arc::new(ChainSpec::default()));
    let mut env: EvmEnv<SpecId> = EvmEnv {
        cfg_env: revm::context::CfgEnv::new()
            .with_chain_id(CHAIN_ID)
            .with_spec_and_mainnet_gas_params(arb_chainspec::spec_id_by_arbos_version(
                arbos_version,
            )),
        block_env: revm::context::BlockEnv::default(),
    };
    env.cfg_env.disable_base_fee = true;
    env.cfg_env.tx_gas_limit_cap = Some(u64::MAX);
    env.block_env.basefee = 100_000_000;
    env.block_env.gas_limit = 1_125_899_906_842_624;
    env.block_env.number = U256::from(1u64);

    let evm_factory = cfg.block_executor_factory().evm_factory();
    let block_ctx = arb_context::BlockCtx::new(arbos_version, 1_700_000_000, 1, 1, false);
    evm_factory.stage_ctx(Arc::new(arb_context::ArbPrecompileCtx::with_block(
        Arc::new(block_ctx),
    )));
    let evm = evm_factory.create_evm(harness.state(), env);
    let exec_ctx = EthBlockExecutionCtx {
        tx_count_hint: Some(1),
        parent_hash: B256::ZERO,
        parent_beacon_block_root: None,
        ommers: &[],
        withdrawals: None,
        extra_data: vec![0u8; 32].into(),
    };
    let mut executor = cfg
        .block_executor_factory()
        .create_arb_executor(evm, exec_ctx, CHAIN_ID);
    executor
        .apply_pre_execution_changes()
        .expect("pre-execution changes");

    let tx = sign_legacy(
        CHAIN_ID,
        0,
        100_000_000,
        1_000_000,
        TxKind::Call(ARBWASM),
        value,
        Bytes::new(),
        alice_key(),
    );
    let exec_result = executor
        .execute_transaction_without_commit(recover(tx))
        .expect("execute tx");
    assert!(exec_result.result.result.is_success(), "tx must succeed");
    executor.commit_transaction(exec_result).expect("commit");
    let _ = executor.finish().expect("finish");

    assert_eq!(
        balance_of(harness.state(), ARBWASM),
        value,
        "value sent to a not-yet-active precompile address must land on the account",
    );
}
