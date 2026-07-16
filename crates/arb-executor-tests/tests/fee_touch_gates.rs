//! A zero poster fee still touches its destination, so a present-empty
//! L1 pricer pool leaf must be pruned by an L1-funded tx (which pays no
//! poster fee), matching the reference's unconditional poster-fee mint.

use std::sync::Arc;

use alloy_consensus::transaction::Recovered;
use alloy_evm::{
    block::{BlockExecutor, BlockExecutorFactory},
    eth::EthBlockExecutionCtx,
    EvmFactory,
};
use alloy_primitives::{address, Address, Bytes, Signature, B256, U256};
use arb_alloy_consensus::tx::ArbContractTx;
use arb_evm::config::ArbEvmConfig;
use arb_executor_tests::helpers::{fund_account, ExecutorScaffold, ONE_ETH, ONE_GWEI, RECIPIENT};
use arb_primitives::{signed_tx::ArbTypedTransaction, ArbTransactionSigned};
use arbos::l1_pricing::L1_PRICER_FUNDS_POOL_ADDRESS;
use reth_chainspec::ChainSpec;
use reth_evm::{ConfigureEvm, EvmEnv};
use revm::primitives::hardfork::SpecId;

#[test]
fn zero_poster_fee_prunes_present_empty_pool() {
    let mut s = ExecutorScaffold::new();
    let l1_contract: Address = address!("c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0c0");
    fund_account(s.harness.state(), l1_contract, U256::from(10u128 * ONE_ETH));
    fund_account(s.harness.state(), L1_PRICER_FUNDS_POOL_ADDRESS, U256::ZERO);

    let tx = ArbTransactionSigned::new_unhashed(
        ArbTypedTransaction::Contract(ArbContractTx {
            chain_id: U256::from(s.chain_id),
            request_id: B256::repeat_byte(0x21),
            from: l1_contract,
            gas_fee_cap: U256::from(ONE_GWEI),
            gas: 100_000,
            to: Some(RECIPIENT),
            value: U256::from(ONE_ETH),
            data: Bytes::new(),
        }),
        Signature::new(U256::ZERO, U256::ZERO, false),
    );

    let chain_spec: Arc<ChainSpec> = Arc::new(ChainSpec::default());
    let cfg = ArbEvmConfig::new(chain_spec);
    let mut env: EvmEnv<SpecId> = EvmEnv {
        cfg_env: revm::context::CfgEnv::default(),
        block_env: revm::context::BlockEnv::default(),
    };
    env.cfg_env.chain_id = s.chain_id;
    env.cfg_env.disable_base_fee = true;
    env.block_env.timestamp = U256::from(1_700_000_000u64);
    env.block_env.basefee = s.base_fee;
    env.block_env.gas_limit = 30_000_000;
    env.block_env.number = U256::from(1u64);

    let evm = cfg
        .block_executor_factory()
        .evm_factory()
        .create_evm(s.harness.state(), env);
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
        .create_arb_executor(evm, exec_ctx, s.chain_id);
    executor.arb_ctx.block_timestamp = 1_700_000_000;
    executor.arb_ctx.basefee = U256::from(s.base_fee);
    executor.arb_ctx.l2_block_number = 1;
    executor.apply_pre_execution_changes().expect("pre");

    let recovered = Recovered::new_unchecked(tx, l1_contract);
    let result = executor
        .execute_transaction_without_commit(recovered)
        .expect("execute");
    assert!(result.result.result.is_success());
    executor.commit_transaction(result).expect("commit");

    assert!(
        executor
            .finalise_deleted()
            .contains(&L1_PRICER_FUNDS_POOL_ADDRESS),
        "zero poster fee must touch and prune the present-empty pool leaf"
    );
}
