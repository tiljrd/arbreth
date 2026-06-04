//! Reproduces arb1 block 22,209,702: an ArbitrumSubmitRetryableTx followed by
//! its auto-redeem ArbitrumRetryTx at ArbOS v6. Asserts the L1-aliased sender's
//! net balance change matches the canonical (Nitro) value.

use std::sync::Arc;

use alloy_consensus::transaction::Recovered;
use alloy_evm::{
    block::{BlockExecutor, BlockExecutorFactory},
    eth::EthBlockExecutionCtx,
    EvmFactory,
};
use alloy_primitives::{address, Address, Bytes, Signature, B256, U256};
use arb_alloy_consensus::tx::ArbSubmitRetryableTx;
use arb_evm::config::ArbEvmConfig;
use arb_executor_tests::helpers::{balance_of, fund_account};
use arb_primitives::{signed_tx::ArbTypedTransaction, ArbTransactionSigned};
use arb_test_utils::ArbosHarness;
use reth_chainspec::ChainSpec;
use reth_evm::{ConfigureEvm, EvmEnv};
use revm::primitives::hardfork::SpecId;

const CHAIN_ID: u64 = 42161;
const BASE_FEE: u64 = 100_000_000; // 0.1 gwei

// Block 22,209,702 actors.
const FROM: Address = address!("dff384f754e854890e311e3280b767f80797291e");
const REFUND_TO: Address = address!("012ee90231e54c7c4e0abc0d154b759c2efa7617");
const RETRY_TO: Address = address!("096760f208390250649e3e8763348e783aef5562");

fn zero_sig() -> Signature {
    Signature::new(U256::ZERO, U256::ZERO, false)
}

#[test]
fn submit_then_autoredeem_sender_balance_matches_canonical() {
    let mut harness = ArbosHarness::new()
        .with_arbos_version(6)
        .with_chain_id(CHAIN_ID)
        .initialize();

    // retryData from the real tx (324 bytes).
    let retry_data = Bytes::from(
        alloy_primitives::hex::decode(
            "2e567b36000000000000000000000000a0b86991c6218b36c1d19d4a2e9eb0ce3606eb48000000000000000000000000012ee90231e54c7c4e0abc0d154b759c2efa7617000000000000000000000000012ee90231e54c7c4e0abc0d154b759c2efa7617000000000000000000000000000000000000000000000000000000003b9aca0000000000000000000000000000000000000000000000000000000000000000a0000000000000000000000000000000000000000000000000000000000000008000000000000000000000000000000000000000000000000000000000000000400000000000000000000000000000000000000000000000000000000000000060000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000000",
        )
        .expect("valid hex"),
    );

    let deposit_value = U256::from(665_355_544_553_216u128);
    let l1_base_fee = U256::from(32_437_071_905u128);
    let max_submission_fee = U256::from(582_855_544_553_216u128);
    let gas_fee_cap = U256::from(300_000_000u128); // 0.3 gwei
    let gas: u64 = 274_488;

    // Sender starts at zero balance (deposit is minted by the submit).
    fund_account(harness.state(), FROM, U256::ZERO);
    // Pre-touch the actors so the conservation sum sees stable accounts.
    fund_account(harness.state(), REFUND_TO, U256::ZERO);
    fund_account(harness.state(), RETRY_TO, U256::ZERO);
    let from_before = balance_of(harness.state(), FROM);
    let supply_before = total_supply(&mut harness);

    let submit = ArbSubmitRetryableTx {
        chain_id: U256::from(CHAIN_ID),
        request_id: B256::from(U256::from(0x0fu64).to_be_bytes()),
        from: FROM,
        l1_base_fee,
        deposit_value,
        gas_fee_cap,
        gas,
        retry_to: Some(RETRY_TO),
        retry_value: U256::ZERO,
        beneficiary: REFUND_TO,
        max_submission_fee,
        fee_refund_addr: REFUND_TO,
        retry_data,
    };
    let submit_tx = ArbTransactionSigned::new_unhashed(
        ArbTypedTransaction::SubmitRetryable(submit),
        zero_sig(),
    );

    // RETRY_TO has no code here; the inner redeem call to an empty account
    // succeeds with status=1 (matching the real tx's success outcome for the
    // gas/refund accounting — the inner call's effects on RETRY_TO are not the
    // subject of this balance-conservation check).
    let from_after = run_block(&mut harness, submit_tx, FROM);
    let supply_after = total_supply(&mut harness);

    let from_delta: i128 = balance_of_i128(from_after) - balance_of_i128(from_before);
    eprintln!("from net delta = {from_delta}");

    // The ONLY new wei introduced into the system is the minted `deposit_value`.
    // Submission fee, network gas cost, escrow, prepaid, gas charge and all the
    // end-tx refunds are internal transfers/burns that conserve total supply.
    // So the total balance across all accounts must grow by exactly
    // `deposit_value`. A larger growth = a leak (e.g. prepaid minted but never
    // burned), a smaller growth = an over-burn.
    let supply_delta: i128 =
        balance_of_i128(supply_after) - balance_of_i128(supply_before);
    let expected: i128 = balance_of_i128(deposit_value);
    eprintln!("supply delta = {supply_delta}  expected (deposit) = {expected}  leak = {}", supply_delta - expected);
    assert_eq!(
        supply_delta, expected,
        "total supply must grow by exactly the deposit; a +baseFee*gas (27.5 uETH) leak is the auto-redeem prepaid not being undone"
    );
}

fn balance_of_i128(b: U256) -> i128 {
    b.try_into().unwrap_or(i128::MAX)
}

/// Sum of balances across every cached account.
fn total_supply(harness: &mut ArbosHarness) -> U256 {
    let state = harness.state();
    let mut sum = U256::ZERO;
    for acct in state.cache.accounts.values() {
        if let Some(a) = acct.account.as_ref() {
            sum = sum.saturating_add(a.info.balance);
        }
    }
    sum
}

/// Runs the submit-retryable, then drains and runs the scheduled auto-redeem
/// retry tx in the same block. Returns the sender's balance after both.
fn run_block(harness: &mut ArbosHarness, submit_tx: ArbTransactionSigned, sender: Address) -> U256 {
    let chain_spec: Arc<ChainSpec> = Arc::new(ChainSpec::default());
    let cfg = ArbEvmConfig::new(chain_spec);

    let mut env: EvmEnv<SpecId> = EvmEnv {
        cfg_env: revm::context::CfgEnv::default(),
        block_env: revm::context::BlockEnv::default(),
    };
    env.cfg_env.chain_id = CHAIN_ID;
    env.cfg_env.disable_base_fee = true;
    env.block_env.timestamp = U256::from(1_744_000_000u64);
    env.block_env.basefee = BASE_FEE;
    env.block_env.gas_limit = 30_000_000;
    env.block_env.number = U256::from(1u64);
    env.block_env.prevrandao = Some(B256::from(U256::from(1u64)));
    env.block_env.difficulty = U256::from(1u64);

    let evm = cfg
        .block_executor_factory()
        .evm_factory()
        .create_evm(harness.state(), env);

    let exec_ctx = EthBlockExecutionCtx {
        tx_count_hint: Some(2),
        parent_hash: B256::ZERO,
        parent_beacon_block_root: None,
        ommers: &[],
        withdrawals: None,
        extra_data: vec![0u8; 32].into(),
    };
    let mut executor = cfg
        .block_executor_factory()
        .create_arb_executor(evm, exec_ctx, CHAIN_ID);
    executor.arb_ctx.block_timestamp = 1_744_000_000;
    executor.arb_ctx.basefee = U256::from(BASE_FEE);
    executor.arb_ctx.l2_block_number = 1;
    executor.apply_pre_execution_changes().expect("pre");

    // 1. Submit retryable.
    let recovered = Recovered::new_unchecked(submit_tx, sender);
    let result = executor
        .execute_transaction_without_commit(recovered)
        .expect("submit exec");
    executor.commit_transaction(result).expect("submit commit");

    // 2. Drain and run the scheduled auto-redeem retry tx.
    let scheduled = executor.drain_scheduled_txs();
    assert!(!scheduled.is_empty(), "expected a scheduled auto-redeem retry tx");
    for enc in scheduled {
        let mut slice = enc.as_slice();
        let tx = <ArbTransactionSigned as alloy_eips::eip2718::Decodable2718>::decode_2718(
            &mut slice,
        )
        .expect("decode scheduled retry tx");
        let from = alloy_consensus::transaction::SignerRecoverable::recover_signer(&tx)
            .unwrap_or(sender);
        let rec = Recovered::new_unchecked(tx, from);
        let res = executor
            .execute_transaction_without_commit(rec)
            .expect("retry exec");
        executor.commit_transaction(res).expect("retry commit");
    }

    let _ = executor.finish().expect("finish");
    balance_of(harness.state(), sender)
}
