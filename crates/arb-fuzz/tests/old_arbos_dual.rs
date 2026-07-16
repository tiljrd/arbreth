//! Arb1-era (ArbOS v6..v9) forward-execution regression suite: pins arbreth
//! against a Nitro reference on the core message/tx surface — a deposit,
//! EIP-1559 and legacy value transfers, and a contract deploy + call — across
//! the fee/tip/L1-block gates that change in this version band.
//!
//! Run (needs Docker + a release arb-reth):
//!   ARB_SPEC_BINARY=$(pwd)/target/release/arb-reth \
//!     cargo test -p arb-fuzz --test old_arbos_dual --release -- --ignored --nocapture

use std::sync::Mutex;

use alloy_primitives::{address, Address, Bytes, B256, U256};
use arb_fuzz::dual_scaffold::{create_address, msg, Idx, Rig, DUAL_L2_CHAIN_ID};
use arb_test_harness::{
    messaging::{
        signed_tx::{derive_address, L2TxKind, SignedL2TxBuilder},
        DepositBuilder, MessageBuilder,
    },
    node::{BlockId, ExecutionNode},
    scenario::{Scenario, ScenarioSetup},
};

/// Each case spawns its own Nitro + arbreth pair; serialize so concurrent
/// `cargo test` threads don't contend on Docker / ports / processes.
static SERIAL: Mutex<()> = Mutex::new(());

const L1_BASE_FEE: u64 = 30_000_000_000;
const SEQUENCER_ALIAS: Address = address!("a4b000000000000000000073657175656e636572");
const FUNDER: Address = Address::new([0xa1; 20]);

fn payer_key() -> B256 {
    B256::repeat_byte(0x37)
}

/// A minimal deployable contract: constructor returns a 1-byte STOP runtime.
/// init = PUSH1 1, PUSH1 0x0b, PUSH1 0, CODECOPY, PUSH1 1, PUSH1 0, RETURN;
/// runtime byte 0x00 (STOP) at offset 0x0b.
const DEPLOY_INIT: [u8; 12] = [
    0x60, 0x01, 0x60, 0x0b, 0x60, 0x00, 0x39, 0x60, 0x01, 0x60, 0x00, 0xf3,
];

fn payer_tx(
    nonce: u64,
    to: Option<Address>,
    value: U256,
    data: Vec<u8>,
    gas_limit: u64,
    kind: L2TxKind,
) -> SignedL2TxBuilder {
    SignedL2TxBuilder {
        chain_id: DUAL_L2_CHAIN_ID,
        nonce,
        to,
        value,
        data: Bytes::from(data),
        gas_limit,
        gas_price: 1_000_000_000,
        max_fee_per_gas: 1_000_000_000,
        max_priority_fee_per_gas: 0,
        access_list: Vec::new(),
        authorization_list: Vec::new(),
        kind,
        signing_key: payer_key(),
        l1_block_number: 1,
        timestamp: 1_700_000_000,
        request_id: None,
        sender: SEQUENCER_ALIAS,
        // Low L1 base fee so the poster-cost component stays well under the gas
        // limit; otherwise a deploy/call tx is silently dropped (nonce never
        // bumped) and the dual-exec would trivially agree on a no-op.
        base_fee_l1: 100_000_000,
    }
}

/// Runs the core-tx scenario against a fresh node pair at `version` and asserts
/// arbreth matches Nitro on every diffed field.
fn assert_clean_at(version: u64) {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let payer = derive_address(payer_key());
    let recipient = address!("00000000000000000000000000000000d00d0001");
    let mut rig = Rig::spawn(version, payer);
    let idx = Idx::new();
    let mut steps = Vec::new();

    // Fund the payer via an L1->L2 deposit.
    let dep_idx = idx.next();
    steps.push(msg(
        dep_idx,
        DepositBuilder {
            from: FUNDER,
            to: payer,
            amount: U256::from(10u128).pow(U256::from(20u64)),
            l1_block_number: 1,
            timestamp: 1_700_000_000,
            request_seq: dep_idx,
            base_fee_l1: L1_BASE_FEE,
        }
        .build()
        .expect("deposit"),
    ));

    // EIP-1559 value transfer (exercises tip / poster / network+infra fee split).
    let i = idx.next();
    steps.push(msg(
        i,
        payer_tx(
            0,
            Some(recipient),
            U256::from(1_000_000u64),
            Vec::new(),
            1_000_000,
            L2TxKind::Eip1559,
        )
        .build()
        .expect("eip1559 transfer"),
    ));

    // Legacy value transfer (different tx-type fee handling).
    let i = idx.next();
    steps.push(msg(
        i,
        payer_tx(
            1,
            Some(recipient),
            U256::from(2_000_000u64),
            Vec::new(),
            1_000_000,
            L2TxKind::Legacy,
        )
        .build()
        .expect("legacy transfer"),
    ));

    // Contract deploy (CREATE) ...
    let i = idx.next();
    steps.push(msg(
        i,
        payer_tx(
            2,
            None,
            U256::ZERO,
            DEPLOY_INIT.to_vec(),
            30_000_000,
            L2TxKind::Eip1559,
        )
        .build()
        .expect("contract deploy"),
    ));

    // ... then a call to the freshly-deployed contract.
    let deployed = create_address(payer, 2);
    let i = idx.next();
    steps.push(msg(
        i,
        payer_tx(
            3,
            Some(deployed),
            U256::ZERO,
            Vec::new(),
            1_000_000,
            L2TxKind::Eip1559,
        )
        .build()
        .expect("contract call"),
    ));

    let scenario = Scenario {
        name: format!("old_arbos_core_v{version}"),
        description: format!("arb1-era core tx surface at ArbOS v{version}"),
        setup: ScenarioSetup {
            l2_chain_id: DUAL_L2_CHAIN_ID,
            arbos_version: version,
            genesis: None,
        },
        steps,
    };

    let report = rig.dual.run(&scenario).expect("run scenario");

    // Guard against a hollow pass: the deploy must have landed (else the call
    // hits an empty account and both nodes trivially agree on a no-op).
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
        .code(deployed, at.clone())
        .map(|c| c.len())
        .unwrap_or(0);
    assert!(
        code_len > 0,
        "deploy did not land (code empty) — test would be hollow"
    );
    // All 4 payer txs (2 transfers, deploy, call) must have executed.
    let payer_nonce = rig.dual.right.nonce(payer, at.clone()).expect("nonce");
    assert_eq!(payer_nonce, 4, "not every payer tx executed");

    assert!(
        report.is_clean(),
        "ArbOS v{version} core-tx surface diverged from Nitro:\n\
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
fn core_tx_surface_matches_nitro_v6() {
    assert_clean_at(6);
}

#[test]
#[ignore]
fn core_tx_surface_matches_nitro_v7() {
    assert_clean_at(7);
}

#[test]
#[ignore]
fn core_tx_surface_matches_nitro_v8() {
    assert_clean_at(8);
}

#[test]
#[ignore]
fn core_tx_surface_matches_nitro_v9() {
    assert_clean_at(9);
}
