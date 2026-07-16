//! Retryable lifecycle dual-exec vs Nitro where the retry target is a real
//! deployed contract: auto-redeem into STORE (slot written, ticket deleted),
//! auto-redeem into REVERT (callvalue back to escrow, ticket retained), and a
//! gas_limit=0 submit (no auto-redeem, callvalue sits in escrow).
//!
//! Run (needs Docker + release arb-reth):
//!   ARB_SPEC_BINARY=$(pwd)/target/release/arb-reth \
//!     cargo test -p arb-fuzz --test retryable_to_contract_dual --release -- --ignored --nocapture

use std::sync::Mutex;

use alloy_primitives::{address, Address, Bytes, B256, U256};
use arb_fuzz::dual_scaffold::{
    create_address, msg, revert_runtime, store_runtime, wrap_init_code, Idx, Rig, DUAL_L2_CHAIN_ID,
};
use arb_test_harness::{
    messaging::{
        signed_tx::{derive_address, L2TxKind, SignedL2TxBuilder},
        DepositBuilder, MessageBuilder, RetryableSubmitBuilder,
    },
    node::{BlockId, ExecutionNode},
    scenario::{Scenario, ScenarioSetup, ScenarioStep},
};

static SERIAL: Mutex<()> = Mutex::new(());

const L1_BASE_FEE: u64 = 30_000_000_000;
const SEQUENCER_ALIAS: Address = address!("a4b000000000000000000073657175656e636572");
const FUNDER: Address = Address::new([0xa3; 20]);

fn deployer_key() -> B256 {
    B256::repeat_byte(0x6c)
}

fn deposit(idx: u64, to: Address, amount: U256, l1_block: u64, ts: u64) -> ScenarioStep {
    msg(
        idx,
        DepositBuilder {
            from: FUNDER,
            to,
            amount,
            l1_block_number: l1_block,
            timestamp: ts,
            request_seq: idx,
            base_fee_l1: L1_BASE_FEE,
        }
        .build()
        .expect("deposit"),
    )
}

fn deploy(nonce: u64, runtime: &[u8], l1_block: u64, ts: u64) -> SignedL2TxBuilder {
    SignedL2TxBuilder {
        chain_id: DUAL_L2_CHAIN_ID,
        nonce,
        to: None,
        value: U256::ZERO,
        data: Bytes::from(wrap_init_code(runtime)),
        gas_limit: 30_000_000,
        gas_price: 1_000_000_000,
        max_fee_per_gas: 1_000_000_000,
        max_priority_fee_per_gas: 0,
        access_list: Vec::new(),
        authorization_list: Vec::new(),
        kind: L2TxKind::Eip1559,
        signing_key: deployer_key(),
        l1_block_number: l1_block,
        timestamp: ts,
        request_id: None,
        sender: SEQUENCER_ALIAS,
        base_fee_l1: 100_000_000,
    }
}

fn submit_retryable(
    l1_sender: Address,
    to: Address,
    gas_limit: u64,
    l2_call_value: U256,
    l1_block: u64,
    ts: u64,
    req: B256,
) -> RetryableSubmitBuilder {
    RetryableSubmitBuilder {
        l1_sender,
        to,
        l2_call_value,
        // Generously over-fund: submission fee (l1BaseFee*1400 for empty data) +
        // callvalue + gas*maxfee all well under 1 ETH.
        deposit_value: U256::from(10u128).pow(U256::from(18u64)),
        max_submission_fee: U256::from(10u128).pow(U256::from(17u64)),
        excess_fee_refund_address: address!("000000000000000000000000000000000fee0001"),
        call_value_refund_address: address!("000000000000000000000000000000000bef0001"),
        gas_limit,
        max_fee_per_gas: U256::from(1_000_000_000u64),
        data: Bytes::new(),
        l1_block_number: l1_block,
        timestamp: ts,
        request_id: Some(req),
    }
}

fn req_id(tag: u8) -> B256 {
    B256::repeat_byte(tag)
}

fn assert_clean_at(version: u64) {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let deployer = derive_address(deployer_key());
    let mut rig = Rig::spawn(version, deployer);
    let idx = Idx::new();
    let mut steps = Vec::new();

    let store = create_address(deployer, 0);
    let revert_c = create_address(deployer, 1);

    let t = 1_700_000_000u64;

    // Block 1: fund the deployer, deploy STORE + REVERT.
    steps.push(deposit(
        idx.next(),
        deployer,
        U256::from(10u128).pow(U256::from(18u64)),
        1,
        t,
    ));
    steps.push(msg(
        idx.next(),
        deploy(0, &store_runtime(), 1, t)
            .build()
            .expect("deploy store"),
    ));
    steps.push(msg(
        idx.next(),
        deploy(1, &revert_runtime(), 1, t)
            .build()
            .expect("deploy revert"),
    ));

    // Block 2: submit retryables targeting the deployed contracts.
    let l1_sender_a = Address::repeat_byte(0x51);
    let l1_sender_b = Address::repeat_byte(0x52);
    let l1_sender_c = Address::repeat_byte(0x53);

    // (a) auto-redeem to STORE: inner code runs, slot 0 := 0x2a, ticket deleted.
    steps.push(msg(
        idx.next(),
        submit_retryable(
            l1_sender_a,
            store,
            200_000,
            U256::ZERO,
            2,
            t + 4,
            req_id(0xa1),
        )
        .build()
        .expect("submit->store"),
    ));
    // (b) auto-redeem to REVERT: retry fails, ticket retained, fee not refunded.
    steps.push(msg(
        idx.next(),
        submit_retryable(
            l1_sender_b,
            revert_c,
            200_000,
            U256::ZERO,
            2,
            t + 4,
            req_id(0xb2),
        )
        .build()
        .expect("submit->revert"),
    ));
    // (c) gas_limit=0: no auto-redeem, ticket sits in escrow with callvalue.
    steps.push(msg(
        idx.next(),
        submit_retryable(
            l1_sender_c,
            store,
            0,
            U256::from(12_345u64),
            2,
            t + 4,
            req_id(0xc3),
        )
        .build()
        .expect("submit no-redeem"),
    ));

    let scenario = Scenario {
        name: format!("retryable_to_contract_v{version}"),
        description: format!("retryable auto-redeem into a real contract at ArbOS v{version}"),
        setup: ScenarioSetup {
            l2_chain_id: DUAL_L2_CHAIN_ID,
            arbos_version: version,
            genesis: None,
        },
        steps,
    };

    let report = rig.dual.run(&scenario).expect("run scenario");

    // Guard against a hollow pass: if the deploys had been dropped (e.g. gas
    // below poster cost) the retryables would target empty accounts and both
    // nodes would trivially agree. Require the contracts to actually carry code.
    let latest = rig
        .dual
        .right
        .block(BlockId::Latest)
        .expect("latest")
        .number;
    let at = BlockId::Number(latest);
    for (name, a) in [("store", store), ("revert", revert_c)] {
        let n = rig
            .dual
            .right
            .code(a, at.clone())
            .map(|c| c.len())
            .unwrap_or(0);
        assert!(
            n > 0,
            "deploy of {name} did not land (code empty) — test would be hollow"
        );
    }
    // Both deployer txs (2 deploys) must have executed.
    let deployer_nonce = rig.dual.right.nonce(deployer, at.clone()).expect("nonce");
    assert_eq!(deployer_nonce, 2, "not every deployer tx executed");

    assert!(
        report.is_clean(),
        "ArbOS v{version} retryable-to-contract surface diverged from Nitro:\n\
         block_diffs={} tx_diffs={} state_diffs={} log_diffs={}\n{:#?}",
        report.block_diffs.len(),
        report.tx_diffs.len(),
        report.state_diffs.len(),
        report.log_diffs.len(),
        report
    );
}

#[test]
#[ignore]
fn retryable_to_contract_matches_nitro_v6() {
    assert_clean_at(6);
}

#[test]
#[ignore]
fn retryable_to_contract_matches_nitro_v9() {
    assert_clean_at(9);
}

#[test]
#[ignore]
fn retryable_to_contract_matches_nitro_v11() {
    assert_clean_at(11);
}

#[test]
#[ignore]
fn retryable_to_contract_matches_nitro_v60() {
    assert_clean_at(60);
}
