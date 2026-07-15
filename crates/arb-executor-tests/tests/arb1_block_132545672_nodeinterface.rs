//! Regression for the Arbitrum One block 132,545,672 consensus divergence.
//!
//! That block's second tx is a top-level legacy call to 0xc8 (NodeInterface).
//! NodeInterface is RPC-only, never a consensus precompile, so a committed call
//! falls through to whatever code lives at the address. On arb1 every precompile
//! address (incl. 0xc8) carries the 0xfe (INVALID) stub byte, so the call burns
//! all gas and fails — canonical receipt: status 0, gasUsed = the full 2,100,000
//! limit. A regression that re-registers 0xc8 as a precompile would instead
//! decode and answer gasEstimateComponents and succeed cheaply.

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
use alloy_primitives::{address, hex, Address, Bytes, TxKind, B256, U256};
use arb_evm::config::ArbEvmConfig;
use arb_executor_tests::helpers::{
    alice, alice_key, deploy_contract, fund_account, recover, sign_legacy,
};
use arb_test_utils::ArbosHarness;
use reth_chainspec::ChainSpec;
use reth_evm::{ConfigureEvm, EvmEnv};
use revm::primitives::hardfork::SpecId;

// arb1 block 132,545,672 is ArbOS v10. The divergence is chain-agnostic — it is
// driven entirely by 0xc8's on-chain 0xfe stub — so the v10 harness reproduces
// it directly without an arb1-specific genesis.
const CHAIN_ID: u64 = 421614;
const ARBOS_VERSION: u64 = 10;
const BLOCK_NUMBER: u64 = 132_545_672;
const BLOCK_TIMESTAMP: u64 = 1_695_131_265;
const L1_BLOCK_NUMBER: u64 = 18_170_349;
const HEADER_BASE_FEE: u64 = 100_000_000;
const GAS_LIMIT: u64 = 2_100_000;

const NODE_INTERFACE: Address = address!("00000000000000000000000000000000000000c8");

/// Well-formed `gasEstimateComponents(address,bool,bytes)` with empty inner data
/// — a call the precompile would decode and answer if 0xc8 were (wrongly)
/// registered, so the buggy path succeeds where the de-registered path fails.
fn gas_estimate_components_calldata() -> Bytes {
    let mut d = hex::decode("c94e6eeb").unwrap();
    d.extend_from_slice(&[0u8; 32]); // address to
    d.extend_from_slice(&[0u8; 32]); // bool contractCreation
    let mut off = [0u8; 32];
    off[31] = 0x60;
    d.extend_from_slice(&off); // bytes offset
    d.extend_from_slice(&[0u8; 32]); // bytes length = 0
    Bytes::from(d)
}

fn run_call_to_0xc8(stub_code: Option<Vec<u8>>) -> (bool, u64) {
    let mut harness = ArbosHarness::new()
        .with_arbos_version(ARBOS_VERSION)
        .with_chain_id(CHAIN_ID)
        .initialize();

    fund_account(harness.state(), alice(), U256::from(1u128 << 100));
    if let Some(code) = stub_code {
        deploy_contract(harness.state(), NODE_INTERFACE, code, U256::ZERO);
    }
    harness
        .state()
        .merge_transitions(revm::database::states::bundle_state::BundleRetention::Reverts);

    let cfg = ArbEvmConfig::new(Arc::new(ChainSpec::default()));
    let mut env: EvmEnv<SpecId> = EvmEnv {
        cfg_env: revm::context::CfgEnv::new()
            .with_chain_id(CHAIN_ID)
            .with_spec_and_mainnet_gas_params(arb_chainspec::spec_id_by_arbos_version(
                ARBOS_VERSION,
            )),
        block_env: revm::context::BlockEnv::default(),
    };
    env.cfg_env.disable_base_fee = true;
    env.cfg_env.tx_gas_limit_cap = Some(u64::MAX);
    env.block_env.basefee = HEADER_BASE_FEE;
    env.block_env.gas_limit = 1_125_899_906_842_624;
    env.block_env.number = U256::from(BLOCK_NUMBER);
    env.block_env.timestamp = U256::from(BLOCK_TIMESTAMP);

    let evm_factory = cfg.block_executor_factory().evm_factory();
    let block_ctx = arb_context::BlockCtx::new(
        ARBOS_VERSION,
        BLOCK_TIMESTAMP,
        L1_BLOCK_NUMBER,
        BLOCK_NUMBER,
        false,
    );
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
    executor.apply_pre_execution_changes().expect("pre-exec");

    let tx = sign_legacy(
        CHAIN_ID,
        0,
        HEADER_BASE_FEE as u128,
        GAS_LIMIT,
        TxKind::Call(NODE_INTERFACE),
        U256::ZERO,
        gas_estimate_components_calldata(),
        alice_key(),
    );
    let r = executor
        .execute_transaction_without_commit(recover(tx))
        .expect("execute");
    (r.result.result.is_success(), r.result.result.gas_used())
}

/// arb1 case: 0xc8 holds the 0xfe stub, so the committed call runs INVALID and
/// burns the whole gas limit — matching canonical block 132,545,672.
#[test]
fn nodeinterface_0xc8_with_invalid_stub_burns_all_gas() {
    let (success, gas_used) = run_call_to_0xc8(Some(vec![0xfe]));
    assert!(
        !success,
        "call to 0xc8 (0xfe stub) must fail, not succeed via a consensus precompile",
    );
    assert_eq!(
        gas_used, GAS_LIMIT,
        "an INVALID stub must burn the full gas limit (canonical gasUsed = {GAS_LIMIT})",
    );
}

/// Control: with no code at 0xc8 the committed call hits an empty account and
/// succeeds cheaply — proving 0xc8 is not dispatched as a consensus precompile.
#[test]
fn nodeinterface_0xc8_empty_account_succeeds() {
    let (success, gas_used) = run_call_to_0xc8(None);
    assert!(
        success,
        "call to an empty 0xc8 must succeed (no precompile dispatch)",
    );
    assert!(
        gas_used < 100_000,
        "empty-account call must be cheap, got {gas_used}",
    );
}
