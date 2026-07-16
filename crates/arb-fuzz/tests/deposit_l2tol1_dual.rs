//! Deposit + L2->L1 messaging dual-exec vs Nitro: value minting on deposits
//! (fresh EOA, contract, zero-value touch), ArbSys.sendTxToL1 merkle-
//! accumulator growth, and ArbSys.withdrawEth value burn, compared at ArbOS
//! v6, v9, and v60.
//!
//! Run (needs Docker + release arb-reth):
//!   ARB_SPEC_BINARY=$(pwd)/target/release/arb-reth \
//!     cargo test -p arb-fuzz --test deposit_l2tol1_dual --release -- --ignored --nocapture

use std::sync::Mutex;

use alloy_primitives::{address, Address, Bytes, B256, U256};
use arb_fuzz::{
    dual_scaffold::{
        create_address, msg, store_runtime, wrap_init_code, Idx, Rig, DUAL_L2_CHAIN_ID,
    },
    scaffolding::selector4,
};
use arb_test_harness::{
    messaging::{
        signed_tx::{derive_address, L2TxKind, SignedL2TxBuilder},
        DepositBuilder, MessageBuilder,
    },
    node::{BlockId, ExecutionNode},
    scenario::{Scenario, ScenarioSetup, ScenarioStep},
};

static SERIAL: Mutex<()> = Mutex::new(());

const L1_BASE_FEE: u64 = 30_000_000_000;
const SEQUENCER_ALIAS: Address = address!("a4b000000000000000000073657175656e636572");
const FUNDER: Address = Address::new([0xa6; 20]);
const OWNER: Address = address!("000000000000000000000000000000000c1a0001");
const ARBSYS: Address = address!("0000000000000000000000000000000000000064");
const L1_DEST: Address = address!("00000000000000000000000000000000d0570001");

fn payer_key() -> B256 {
    B256::repeat_byte(0x4d)
}

fn word_addr(a: Address) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(a.as_slice());
    w
}

fn word_u64(v: u64) -> [u8; 32] {
    let mut w = [0u8; 32];
    w[24..].copy_from_slice(&v.to_be_bytes());
    w
}

/// ABI-encode ArbSys.sendTxToL1(address destination, bytes data).
fn send_tx_to_l1_calldata(dest: Address, data: &[u8]) -> Vec<u8> {
    let mut out = selector4("sendTxToL1(address,bytes)").to_vec();
    out.extend_from_slice(&word_addr(dest));
    out.extend_from_slice(&word_u64(0x40)); // offset to bytes
    out.extend_from_slice(&word_u64(data.len() as u64));
    let mut padded = data.to_vec();
    while !padded.len().is_multiple_of(32) {
        padded.push(0);
    }
    out.extend_from_slice(&padded);
    out
}

/// ABI-encode ArbSys.withdrawEth(address destination).
fn withdraw_eth_calldata(dest: Address) -> Vec<u8> {
    let mut out = selector4("withdrawEth(address)").to_vec();
    out.extend_from_slice(&word_addr(dest));
    out
}

fn deposit(idx: u64, to: Address, amount: U256) -> ScenarioStep {
    msg(
        idx,
        DepositBuilder {
            from: FUNDER,
            to,
            amount,
            l1_block_number: 1,
            timestamp: 1_700_000_000,
            request_seq: idx,
            base_fee_l1: L1_BASE_FEE,
        }
        .build()
        .expect("deposit"),
    )
}

fn payer_tx(nonce: u64, to: Address, value: U256, data: Vec<u8>) -> SignedL2TxBuilder {
    SignedL2TxBuilder {
        chain_id: DUAL_L2_CHAIN_ID,
        nonce,
        to: Some(to),
        value,
        data: Bytes::from(data),
        gas_limit: 30_000_000,
        gas_price: 1_000_000_000,
        max_fee_per_gas: 1_000_000_000,
        max_priority_fee_per_gas: 0,
        access_list: Vec::new(),
        authorization_list: Vec::new(),
        kind: L2TxKind::Eip1559,
        signing_key: payer_key(),
        l1_block_number: 1,
        timestamp: 1_700_000_000,
        request_id: None,
        sender: SEQUENCER_ALIAS,
        base_fee_l1: 100_000_000,
    }
}

fn deploy_store(nonce: u64) -> SignedL2TxBuilder {
    let mut b = payer_tx(
        nonce,
        Address::ZERO,
        U256::ZERO,
        wrap_init_code(&store_runtime()),
    );
    b.to = None;
    b
}

fn assert_clean_at(version: u64) {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let payer = derive_address(payer_key());
    let store = create_address(payer, 0);
    let fresh_eoa = address!("00000000000000000000000000000000eee00001");
    let mut rig = Rig::spawn(version, OWNER);
    let idx = Idx::new();
    let mut steps = Vec::new();

    // Fund the payer; deploy STORE (nonce 0).
    steps.push(deposit(
        idx.next(),
        payer,
        U256::from(10u128).pow(U256::from(20u64)),
    ));
    steps.push(msg(
        idx.next(),
        deploy_store(0).build().expect("deploy store"),
    ));

    // Deposits: fresh EOA, deployed contract, zero-value touch.
    steps.push(deposit(idx.next(), fresh_eoa, U256::from(5_000_000u64)));
    steps.push(deposit(idx.next(), store, U256::from(7_000_000u64)));
    steps.push(deposit(
        idx.next(),
        address!("00000000000000000000000000000000eee00002"),
        U256::ZERO,
    ));

    // L2->L1: sendTxToL1 (merkle append + L2ToL1Tx log), repeated to grow the
    // accumulator; withdrawEth burns value.
    steps.push(msg(
        idx.next(),
        payer_tx(
            1,
            ARBSYS,
            U256::ZERO,
            send_tx_to_l1_calldata(L1_DEST, b"hello-l1"),
        )
        .build()
        .expect("sendTxToL1 #1"),
    ));
    steps.push(msg(
        idx.next(),
        payer_tx(
            2,
            ARBSYS,
            U256::from(123_456u64),
            withdraw_eth_calldata(L1_DEST),
        )
        .build()
        .expect("withdrawEth"),
    ));
    steps.push(msg(
        idx.next(),
        payer_tx(
            3,
            ARBSYS,
            U256::ZERO,
            send_tx_to_l1_calldata(L1_DEST, b"second-message-payload"),
        )
        .build()
        .expect("sendTxToL1 #2"),
    ));
    steps.push(msg(
        idx.next(),
        payer_tx(4, ARBSYS, U256::ZERO, send_tx_to_l1_calldata(L1_DEST, &[]))
            .build()
            .expect("sendTxToL1 #3 empty"),
    ));

    let scenario = Scenario {
        name: format!("deposit_l2tol1_v{version}"),
        description: format!("deposit + L2->L1 messaging at ArbOS v{version}"),
        setup: ScenarioSetup {
            l2_chain_id: DUAL_L2_CHAIN_ID,
            arbos_version: version,
            genesis: None,
        },
        steps,
    };

    let report = rig.dual.run(&scenario).expect("run scenario");

    let latest = rig
        .dual
        .right
        .block(BlockId::Latest)
        .expect("latest")
        .number;
    let at = BlockId::Number(latest);
    let code_len = rig
        .dual
        .right
        .code(store, at.clone())
        .map(|c| c.len())
        .unwrap_or(0);
    assert!(
        code_len > 0,
        "STORE deploy did not land — test would be hollow"
    );
    // All 5 payer txs (STORE deploy + 4 ArbSys calls) must have executed; a
    // dropped tx cascades through the nonce sequence and no-ops identically on
    // both nodes.
    let payer_nonce = rig.dual.right.nonce(payer, at.clone()).expect("nonce");
    assert_eq!(payer_nonce, 5, "not every payer tx executed");

    assert!(
        report.is_clean(),
        "ArbOS v{version} deposit/L2->L1 surface diverged from Nitro:\n\
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
fn deposit_l2tol1_matches_nitro_v6() {
    assert_clean_at(6);
}

#[test]
#[ignore]
fn deposit_l2tol1_matches_nitro_v9() {
    assert_clean_at(9);
}

#[test]
#[ignore]
fn deposit_l2tol1_matches_nitro_v60() {
    assert_clean_at(60);
}
