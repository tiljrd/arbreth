//! Fee / tip / poster-gas matrix dual-exec vs Nitro: sweeps tx type (legacy /
//! EIP-2930 / EIP-1559 with and without a priority fee) crossed with gas price
//! above / at the base fee and value zero / non-zero, against contract and EOA
//! targets, at ArbOS v6, v9, v11, and v60.
//!
//! Run (needs Docker + release arb-reth):
//!   ARB_SPEC_BINARY=$(pwd)/target/release/arb-reth \
//!     cargo test -p arb-fuzz --test fee_matrix_dual --release -- --ignored --nocapture

use std::sync::Mutex;

use alloy_primitives::{address, Address, Bytes, B256, U256};
use arb_fuzz::dual_scaffold::{
    create_address, msg, store_runtime, wrap_init_code, Idx, Rig, DUAL_L2_CHAIN_ID,
};
use arb_test_harness::{
    messaging::{
        signed_tx::{derive_address, L2TxKind, SignedL2TxBuilder},
        DepositBuilder, MessageBuilder,
    },
    node::{BlockId, ExecutionNode},
    scenario::{Scenario, ScenarioSetup},
};

static SERIAL: Mutex<()> = Mutex::new(());

const L1_BASE_FEE: u64 = 30_000_000_000;
const SEQUENCER_ALIAS: Address = address!("a4b000000000000000000073657175656e636572");
const FUNDER: Address = Address::new([0xa5; 20]);
// Chain owner (= network fee account) distinct from the payer so a mis-routed
// tip surfaces as a state diff.
const OWNER: Address = address!("000000000000000000000000000000000c1a0000");
const RECIPIENT: Address = address!("00000000000000000000000000000000d00d0002");
const GWEI: u128 = 1_000_000_000;

fn payer_key() -> B256 {
    B256::repeat_byte(0x39)
}

#[allow(clippy::too_many_arguments)]
fn tx(
    nonce: u64,
    to: Option<Address>,
    value: U256,
    data: Vec<u8>,
    kind: L2TxKind,
    gas_price: u128,
    max_fee: u128,
    max_priority: u128,
) -> SignedL2TxBuilder {
    SignedL2TxBuilder {
        chain_id: DUAL_L2_CHAIN_ID,
        nonce,
        to,
        value,
        data: Bytes::from(data),
        gas_limit: 30_000_000,
        gas_price,
        max_fee_per_gas: max_fee,
        max_priority_fee_per_gas: max_priority,
        access_list: Vec::new(),
        authorization_list: Vec::new(),
        kind,
        signing_key: payer_key(),
        l1_block_number: 1,
        timestamp: 1_700_000_000,
        request_id: None,
        sender: SEQUENCER_ALIAS,
        base_fee_l1: 100_000_000,
    }
}

/// One matrix entry: (kind, gas_price, max_fee, max_priority, value, to_contract).
struct Case {
    kind: L2TxKind,
    gas_price: u128,
    max_fee: u128,
    max_priority: u128,
    value: U256,
    to_contract: bool,
}

fn matrix() -> Vec<Case> {
    let v = |n: u64| U256::from(n);
    vec![
        // Legacy, gas_price 1 gwei > base_fee 0.1 gwei -> tip 0.9 gwei.
        Case {
            kind: L2TxKind::Legacy,
            gas_price: GWEI,
            max_fee: GWEI,
            max_priority: 0,
            value: v(1000),
            to_contract: false,
        },
        Case {
            kind: L2TxKind::Legacy,
            gas_price: GWEI,
            max_fee: GWEI,
            max_priority: 0,
            value: U256::ZERO,
            to_contract: true,
        },
        // Legacy at exactly base fee -> no tip.
        Case {
            kind: L2TxKind::Legacy,
            gas_price: 100_000_000,
            max_fee: 100_000_000,
            max_priority: 0,
            value: v(1000),
            to_contract: false,
        },
        // EIP-2930 (also no priority field in revm) with a tip.
        Case {
            kind: L2TxKind::Eip2930,
            gas_price: GWEI,
            max_fee: GWEI,
            max_priority: 0,
            value: v(1000),
            to_contract: false,
        },
        Case {
            kind: L2TxKind::Eip2930,
            gas_price: GWEI,
            max_fee: GWEI,
            max_priority: 0,
            value: U256::ZERO,
            to_contract: true,
        },
        // EIP-1559, no priority -> effective == base fee, no tip.
        Case {
            kind: L2TxKind::Eip1559,
            gas_price: GWEI,
            max_fee: GWEI,
            max_priority: 0,
            value: v(1000),
            to_contract: false,
        },
        // EIP-1559, small priority (0.5 gwei) < max_fee - base_fee.
        Case {
            kind: L2TxKind::Eip1559,
            gas_price: GWEI,
            max_fee: GWEI,
            max_priority: 500_000_000,
            value: v(1000),
            to_contract: false,
        },
        // EIP-1559 with priority exactly at the cap (max_fee - base_fee = 0.9 gwei):
        // a valid tx that pays the maximum possible tip.
        Case {
            kind: L2TxKind::Eip1559,
            gas_price: GWEI,
            max_fee: GWEI,
            max_priority: 900_000_000,
            value: U256::ZERO,
            to_contract: true,
        },
    ]
}

fn assert_clean_at(version: u64) {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let payer = derive_address(payer_key());
    let store = create_address(payer, 0);
    let mut rig = Rig::spawn(version, OWNER);
    let idx = Idx::new();
    let mut steps = Vec::new();

    // Fund the payer and deploy STORE (nonce 0).
    let dep = idx.next();
    steps.push(msg(
        dep,
        DepositBuilder {
            from: FUNDER,
            to: payer,
            amount: U256::from(10u128).pow(U256::from(20u64)),
            l1_block_number: 1,
            timestamp: 1_700_000_000,
            request_seq: dep,
            base_fee_l1: L1_BASE_FEE,
        }
        .build()
        .expect("deposit"),
    ));
    steps.push(msg(
        idx.next(),
        tx(
            0,
            None,
            U256::ZERO,
            wrap_init_code(&store_runtime()),
            L2TxKind::Eip1559,
            GWEI,
            GWEI,
            0,
        )
        .build()
        .expect("deploy store"),
    ));

    let mut nonce = 1u64;
    for c in matrix() {
        let to = if c.to_contract {
            Some(store)
        } else {
            Some(RECIPIENT)
        };
        steps.push(msg(
            idx.next(),
            tx(
                nonce,
                to,
                c.value,
                Vec::new(),
                c.kind,
                c.gas_price,
                c.max_fee,
                c.max_priority,
            )
            .build()
            .expect("matrix tx"),
        ));
        nonce += 1;
    }

    let scenario = Scenario {
        name: format!("fee_matrix_v{version}"),
        description: format!("fee/tip/posterGas matrix at ArbOS v{version}"),
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
    // All 9 payer txs (STORE deploy + 8 matrix cases) must have executed; a
    // dropped tx cascades through the nonce sequence and no-ops identically on
    // both nodes.
    let payer_nonce = rig.dual.right.nonce(payer, at.clone()).expect("nonce");
    assert_eq!(payer_nonce, 9, "not every payer tx executed");

    assert!(
        report.is_clean(),
        "ArbOS v{version} fee matrix diverged from Nitro:\n\
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
fn fee_matrix_matches_nitro_v6() {
    assert_clean_at(6);
}

#[test]
#[ignore]
fn fee_matrix_matches_nitro_v9() {
    assert_clean_at(9);
}

#[test]
#[ignore]
fn fee_matrix_matches_nitro_v11() {
    assert_clean_at(11);
}

#[test]
#[ignore]
fn fee_matrix_matches_nitro_v60() {
    assert_clean_at(60);
}
