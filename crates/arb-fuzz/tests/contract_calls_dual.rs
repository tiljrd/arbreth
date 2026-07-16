//! Deep contract-CALL dual-exec coverage vs Nitro: nested calls, value
//! forwarding, DELEGATECALL context, STATICCALL write-protection, and revert
//! propagation, compared at ArbOS v6, v9, and v60.
//!
//! Run (needs Docker + release arb-reth):
//!   ARB_SPEC_BINARY=$(pwd)/target/release/arb-reth \
//!     cargo test -p arb-fuzz --test contract_calls_dual --release -- --ignored --nocapture

use std::sync::Mutex;

use alloy_primitives::{address, Address, Bytes, B256, U256};
use arb_fuzz::dual_scaffold::{
    create_address, msg, revert_runtime, store_runtime, wrap_init_code, Idx, Rig, DUAL_L2_CHAIN_ID,
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
const FUNDER: Address = Address::new([0xa2; 20]);

fn payer_key() -> B256 {
    B256::repeat_byte(0x71)
}

fn push20(addr: Address) -> Vec<u8> {
    let mut v = vec![0x73];
    v.extend_from_slice(addr.as_slice());
    v
}

/// CALL `target` forwarding msg.value and all gas, store success at slot 0.
fn caller_value_runtime(target: Address) -> Vec<u8> {
    let mut v = vec![0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x34]; // retLen,retOff,argLen,argOff,CALLVALUE
    v.extend_from_slice(&push20(target));
    v.extend_from_slice(&[0x5a, 0xf1, 0x60, 0x00, 0x55, 0x00]); // GAS,CALL,SSTORE(0,success),STOP
    v
}

/// DELEGATECALL `target` (its SSTORE writes our slot 0); store success at slot 1.
fn caller_delegate_runtime(target: Address) -> Vec<u8> {
    let mut v = vec![0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00]; // retLen,retOff,argLen,argOff
    v.extend_from_slice(&push20(target));
    v.extend_from_slice(&[0x5a, 0xf4, 0x60, 0x01, 0x55, 0x00]); // GAS,DELEGATECALL,SSTORE(1,success),STOP
    v
}

/// STATICCALL `target` (whose SSTORE violates write-protection -> fails);
/// store success at slot 1.
fn caller_static_runtime(target: Address) -> Vec<u8> {
    let mut v = vec![0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00];
    v.extend_from_slice(&push20(target));
    v.extend_from_slice(&[0x5a, 0xfa, 0x60, 0x01, 0x55, 0x00]); // GAS,STATICCALL,SSTORE(1,success),STOP
    v
}

/// CALL a reverting `target` (success=0); store success at slot 0.
fn caller_revert_runtime(target: Address) -> Vec<u8> {
    let mut v = vec![0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00, 0x60, 0x00]; // retLen,retOff,argLen,argOff,value
    v.extend_from_slice(&push20(target));
    v.extend_from_slice(&[0x5a, 0xf1, 0x60, 0x00, 0x55, 0x00]); // GAS,CALL,SSTORE(0,success),STOP
    v
}

fn deploy_or_call(
    nonce: u64,
    to: Option<Address>,
    value: U256,
    data: Vec<u8>,
) -> SignedL2TxBuilder {
    SignedL2TxBuilder {
        chain_id: DUAL_L2_CHAIN_ID,
        nonce,
        to,
        value,
        data: Bytes::from(data),
        // High gas + low L1 base fee so the poster-cost component cannot exceed
        // the limit and silently drop the tx (a dropped deploy would leave the
        // call targets code-less, making the dual-exec trivially agree on a
        // no-op — a false pass).
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

fn assert_clean_at(version: u64) {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let payer = derive_address(payer_key());
    let mut rig = Rig::spawn(version, payer);
    let idx = Idx::new();
    let mut steps = Vec::new();

    // Precompute deploy addresses (deployer = payer, nonces 0..5).
    let store = create_address(payer, 0);
    let caller_value = create_address(payer, 1);
    let caller_delegate = create_address(payer, 2);
    let caller_static = create_address(payer, 3);
    let revert_c = create_address(payer, 4);
    let caller_revert = create_address(payer, 5);

    // Fund the payer.
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

    // Deploys (nonces 0..5).
    let deploys: [(u64, Vec<u8>); 6] = [
        (0, store_runtime()),
        (1, caller_value_runtime(store)),
        (2, caller_delegate_runtime(store)),
        (3, caller_static_runtime(store)),
        (4, revert_runtime()),
        (5, caller_revert_runtime(revert_c)),
    ];
    for (nonce, runtime) in deploys {
        let i = idx.next();
        steps.push(msg(
            i,
            deploy_or_call(nonce, None, U256::ZERO, wrap_init_code(&runtime))
                .build()
                .expect("deploy"),
        ));
    }

    // Calls exercising each pattern (nonces 6..9).
    let calls: [(u64, Address, U256); 4] = [
        (6, caller_value, U256::from(7_777u64)), // CALL + value forwarding
        (7, caller_delegate, U256::ZERO),        // DELEGATECALL context
        (8, caller_static, U256::ZERO),          // STATICCALL write-protection
        (9, caller_revert, U256::ZERO),          // CALL revert propagation
    ];
    for (nonce, to, value) in calls {
        let i = idx.next();
        steps.push(msg(
            i,
            deploy_or_call(nonce, Some(to), value, Vec::new())
                .build()
                .expect("call"),
        ));
    }

    let scenario = Scenario {
        name: format!("contract_calls_v{version}"),
        description: format!("deep contract-CALL surface at ArbOS v{version}"),
        setup: ScenarioSetup {
            l2_chain_id: DUAL_L2_CHAIN_ID,
            arbos_version: version,
            genesis: None,
        },
        steps,
    };

    let report = rig.dual.run(&scenario).expect("run scenario");

    // Guard against a hollow pass: if the deploys had been dropped (e.g. gas
    // below poster cost) both nodes would no-op identically and the report
    // would be trivially clean. Require the contracts to actually carry code.
    let latest = rig
        .dual
        .right
        .block(BlockId::Latest)
        .expect("latest")
        .number;
    let at = BlockId::Number(latest);
    for (name, a) in [
        ("store", store),
        ("caller_value", caller_value),
        ("revert", revert_c),
    ] {
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
    // All 10 payer txs (6 deploys + 4 calls) must have executed; a dropped tx
    // cascades through the nonce sequence and no-ops identically on both nodes.
    let payer_nonce = rig.dual.right.nonce(payer, at.clone()).expect("nonce");
    assert_eq!(payer_nonce, 10, "not every payer tx executed");

    assert!(
        report.is_clean(),
        "ArbOS v{version} contract-call surface diverged from Nitro:\n\
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
fn contract_calls_match_nitro_v6() {
    assert_clean_at(6);
}

#[test]
#[ignore]
fn contract_calls_match_nitro_v9() {
    assert_clean_at(9);
}

#[test]
#[ignore]
fn contract_calls_match_nitro_v60() {
    assert_clean_at(60);
}
