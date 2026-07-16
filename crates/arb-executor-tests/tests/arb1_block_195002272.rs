//! Replays Arbitrum One block 195,002,272 tx 6 (v20) — a legacy airdrop batch
//! whose contract CALLs the empty account 0x74 — and asserts the canonical
//! receipt: gasUsedForL1 2,298,214 and total gasUsed 2,465,035. Canonically the
//! 0x74 access is charged cold (2600) in the caller frame; a port that keeps a
//! precompile registered at 0x74 pre-warms it (100) and comes up 2,500 short.

#[cfg(target_arch = "x86_64")]
#[no_mangle]
#[allow(clippy::missing_safety_doc)]
pub unsafe extern "C" fn __rust_probestack() {}

use std::{collections::BTreeMap, sync::Arc};

use alloy_consensus::{transaction::Recovered, TxReceipt};
use alloy_eips::Decodable2718;
use alloy_evm::{
    block::{BlockExecutor, BlockExecutorFactory},
    eth::EthBlockExecutionCtx,
    EvmFactory,
};
use alloy_primitives::{address, b256, hex, Address, Bytes, Signature, B256, U256};
use arb_alloy_consensus::tx::ArbInternalTx;
use arb_evm::config::ArbEvmConfig;
use arb_primitives::{signed_tx::ArbTypedTransaction, ArbTransactionSigned};
use arb_storage::{set_account_code, set_account_nonce, write_storage_at};
use arb_test_utils::{ArbosHarness, EmptyDb};
use arbos::internal_tx::encode_start_block;
use reth_chainspec::ChainSpec;
use reth_evm::{ConfigureEvm, EvmEnv};
use revm::{database::State, primitives::hardfork::SpecId};
use serde::Deserialize;

const CHAIN_ID: u64 = 42161;
const ARBOS_VERSION: u64 = 20;
const BLOCK_NUMBER: u64 = 195_002_272;
const BLOCK_TIMESTAMP: u64 = 0x66050ba5;
const HEADER_BASE_FEE: u128 = 0x989680; // 10,000,000
const L1_BLOCK_NUMBER: u64 = 0x12a0415;
const PARENT_HASH: B256 = b256!("90442be985008264fdaedf73074984d5790bbe6c53245f7b87f72c5de317be40");
const SEQUENCER: Address = address!("a4b000000000000000000073657175656e636572");
const ARBOS_ADDRESS: Address = address!("00000000000000000000000000000000000A4B05");

const SENDER: Address = address!("000039953626e7f07ef6e1042a34273ae51c0013");

const CANON_GAS_USED: u64 = 2_465_035;
const CANON_GAS_USED_FOR_L1: u64 = 2_298_214;
const CANON_SENDER_POST: U256 = U256::from_limbs([0x2821feda294b1c80, 0, 0, 0]);

// Captured with prestateTracer for tx 6, i.e. mid-block state after txs 0..5.
const PRESTATE_JSON: &str = include_str!(concat!(
    "../../arb-spec-tests/fixtures/regression/arb1_195002272/tx6_prestate.json"
));
const TX6: &str = include_str!(concat!(
    "../../arb-spec-tests/fixtures/regression/arb1_195002272/tx6_raw.hex"
));

#[derive(Debug, Deserialize)]
struct AccountSnapshot {
    #[serde(default)]
    balance: Option<String>,
    #[serde(default)]
    nonce: Option<u64>,
    #[serde(default)]
    code: Option<String>,
    #[serde(default)]
    storage: BTreeMap<String, String>,
}

fn hu(s: &str) -> U256 {
    let s = s.strip_prefix("0x").unwrap_or(s);
    if s.is_empty() {
        U256::ZERO
    } else {
        U256::from_str_radix(s, 16).unwrap()
    }
}
fn hb(s: &str) -> Vec<u8> {
    hex::decode(s.trim().trim_start_matches("0x")).unwrap()
}
fn addr(s: &str) -> Address {
    Address::from_slice(&hb(s))
}

fn seed_prestate(state: &mut State<EmptyDb>, snap: &BTreeMap<String, AccountSnapshot>) {
    use revm::database::states::bundle_state::BundleRetention;
    for (a, acct) in snap {
        let ad = addr(a);
        if let Some(b) = acct.balance.as_deref() {
            let v = hu(b);
            if !v.is_zero() {
                arb_executor_tests::helpers::fund_account(state, ad, v);
            }
        }
        if let Some(c) = acct.code.as_deref() {
            let by = hb(c);
            if !by.is_empty() {
                set_account_code(state, ad, Bytes::from(by));
            }
        }
        if let Some(n) = acct.nonce {
            if n > 0 {
                set_account_nonce(state, ad, n);
            }
        }
        for (slot, val) in &acct.storage {
            write_storage_at(state, ad, hu(slot), hu(val)).unwrap();
        }
    }
    state.merge_transitions(BundleRetention::Reverts);
}

// The prestate fixture carries the slots the block's hooks read (L1
// price-per-unit, min base fee, brotli level), but not the L2 base fee slot
// itself; the harness bootstraps it at the 0.1 gwei default, which would
// reject this block's 0.01 gwei gas price. Seed the block's canonical value.
fn seed_l2_base_fee(harness: &mut ArbosHarness, fee: U256) {
    use revm::database::states::bundle_state::BundleRetention;
    {
        let state_ptr = harness.state_ptr();
        let state = harness.arbos_state();
        let backend = unsafe { &mut *state_ptr };
        state
            .l2_pricing_state
            .set_base_fee_wei(backend, fee)
            .expect("set base fee");
    }
    harness.state().merge_transitions(BundleRetention::Reverts);
}

fn read_balance(state: &mut State<EmptyDb>, a: Address) -> U256 {
    state
        .cache
        .accounts
        .get(&a)
        .and_then(|c| c.account.as_ref())
        .map(|x| x.info.balance)
        .unwrap_or(U256::ZERO)
}

#[test]
fn arb1_195002272_tx6_gas_matches_canonical() {
    let mut harness = ArbosHarness::new()
        .with_arbos_version(ARBOS_VERSION)
        .with_chain_id(CHAIN_ID)
        .initialize();

    let prestate: BTreeMap<String, AccountSnapshot> = serde_json::from_str(PRESTATE_JSON).unwrap();
    seed_prestate(harness.state(), &prestate);
    seed_l2_base_fee(&mut harness, U256::from(HEADER_BASE_FEE));

    // Forks active at genesis like the real chain config, so the executor
    // runs with EIP-161 state clear enabled (`ChainSpec::default()` has no
    // hardforks and would leave touched-empty accounts materialized).
    // Shanghai is the highest activation without the EIP-4788 beacon-root
    // system call, which Arbitrum blocks do not carry; the EVM spec itself
    // is pinned via `CfgEnv` above.
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
                ARBOS_VERSION,
            )),
        block_env: revm::context::BlockEnv::default(),
    };
    env.cfg_env.disable_base_fee = true;
    env.cfg_env.tx_gas_limit_cap = Some(u64::MAX);
    env.block_env.timestamp = U256::from(BLOCK_TIMESTAMP);
    env.block_env.basefee = HEADER_BASE_FEE as u64;
    env.block_env.gas_limit = 1_125_899_906_842_624;
    env.block_env.number = U256::from(BLOCK_NUMBER);
    env.block_env.prevrandao = Some(B256::from(U256::from(1u64)));
    env.block_env.difficulty = U256::from(1u64);
    env.block_env.beneficiary = SEQUENCER;

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
        tx_count_hint: Some(2),
        parent_hash: PARENT_HASH,
        parent_beacon_block_root: None,
        ommers: &[],
        withdrawals: None,
        extra_data: vec![0u8; 32].into(),
    };
    let mut executor = cfg
        .block_executor_factory()
        .create_arb_executor(evm, exec_ctx, CHAIN_ID);
    executor.arb_ctx.block_timestamp = BLOCK_TIMESTAMP;
    executor.arb_ctx.basefee = U256::from(HEADER_BASE_FEE);
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

    let bytes = hb(TX6);
    let tx = ArbTransactionSigned::decode_2718(&mut bytes.as_slice()).expect("decode 2718");
    let recovered = arb_executor_tests::helpers::recover(tx);
    assert_eq!(recovered.signer(), SENDER, "sender recovery");
    let r = executor
        .execute_transaction_without_commit(recovered)
        .expect("user tx");
    let success = r.result.result.is_success();
    executor.commit_transaction(r).expect("commit user");
    let (_evm, result) = executor.finish().expect("finish");

    assert_eq!(result.receipts.len(), 2, "startblock + user tx receipts");
    let start_cumulative = result.receipts[0].cumulative_gas_used();
    let gas_used = result.receipts[1]
        .cumulative_gas_used()
        .saturating_sub(start_cumulative);
    let gas_used_for_l1 = result.receipts[1].gas_used_for_l1;
    let sender_post = read_balance(harness.state(), SENDER);

    println!(
        "observed: status={} gasUsed={} gasUsedForL1={} senderPost={sender_post:#x}",
        success, gas_used, gas_used_for_l1,
    );
    println!(
        "canonical: status=true gasUsed={CANON_GAS_USED} gasUsedForL1={CANON_GAS_USED_FOR_L1} \
         senderPost={CANON_SENDER_POST:#x}",
    );

    assert!(success, "tx must succeed");
    assert_eq!(
        gas_used_for_l1,
        CANON_GAS_USED_FOR_L1,
        "gasUsedForL1 {gas_used_for_l1} != canonical {CANON_GAS_USED_FOR_L1} (drift {})",
        gas_used_for_l1 as i128 - CANON_GAS_USED_FOR_L1 as i128,
    );
    assert_eq!(
        gas_used,
        CANON_GAS_USED,
        "gasUsed {gas_used} != canonical {CANON_GAS_USED} (drift {})",
        gas_used as i128 - CANON_GAS_USED as i128,
    );
    assert_eq!(
        sender_post,
        CANON_SENDER_POST,
        "sender net balance must match canonical (got {sender_post:#x}, want \
         {CANON_SENDER_POST:#x}; delta {} wei)",
        CANON_SENDER_POST.abs_diff(sender_post),
    );

    // The zero-value CALL to 0x74 takes the EIP-158 early return: the account
    // must not materialize (canonically absent before and after this block).
    harness
        .state()
        .merge_transitions(revm::database::states::bundle_state::BundleRetention::Reverts);
    let filtered_tx_manager = address!("0000000000000000000000000000000000000074");
    let materialized = harness
        .state()
        .bundle_state
        .state
        .get(&filtered_tx_manager)
        .and_then(|a| a.info.as_ref())
        .cloned();
    assert!(
        materialized.is_none(),
        "0x74 must stay absent after the block, got {materialized:?}"
    );
}
