//! Permanent arb1-era (ArbOS v6..v9) forward-execution regression suite.
//!
//! Arbitrum One launched from its classic-migration genesis at ArbOS v6 and
//! upgrades through v7, v8, v9 before reaching the Nitro-era versions that the
//! Sepolia replay corpus already covers. This test pins arbreth's forward
//! execution of the core message/tx surface against a Nitro reference at each
//! of those versions: a deposit (kind 12), EIP-1559 and legacy value
//! transfers, and a contract deploy followed by a call. The version band
//! straddles the gates that change in this range — pre-v8 `l1BlockNumber++`,
//! pre-v9 tip drop vs the v9 always-collect island, the v5 infra-fee split,
//! and the pre-v10 batch-poster spending path — so a regression in any of them
//! surfaces as a block/receipt/log/state divergence here.
//!
//! Run (needs Docker + a release arb-reth):
//!   ARB_SPEC_BINARY=$(pwd)/target/release/arb-reth \
//!     cargo test -p arb-fuzz --test old_arbos_dual --release -- --ignored --nocapture

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};

use alloy_primitives::{address, Address, Bytes, B256, U256};
use arb_test_harness::{
    dual_exec::DualExec,
    genesis::GenesisBuilder,
    messaging::{
        signed_tx::{derive_address, L2TxKind, SignedL2TxBuilder},
        DepositBuilder, L1Message, MessageBuilder,
    },
    mock_l1::MockL1,
    node::{arbreth::ArbrethProcess, nitro_docker::NitroDocker, NodeStartCtx},
    scenario::{Scenario, ScenarioSetup, ScenarioStep},
};

/// Each case spawns its own Nitro + arbreth pair; serialize so concurrent
/// `cargo test` threads don't contend on Docker / ports / processes.
static SERIAL: Mutex<()> = Mutex::new(());

const L2_CHAIN_ID: u64 = 412_346;
const L1_CHAIN_ID: u64 = 11_155_111;
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

struct Rig {
    dual: DualExec<NitroDocker, ArbrethProcess>,
}

impl Rig {
    fn spawn(version: u64, owner: Address) -> Self {
        let mock = MockL1::start(L1_CHAIN_ID).expect("mock l1 start");
        let genesis = GenesisBuilder::new(L2_CHAIN_ID, version)
            .with_initial_chain_owner(owner)
            .build()
            .expect("genesis build");
        let ctx = NodeStartCtx {
            binary: None,
            l2_chain_id: L2_CHAIN_ID,
            l1_chain_id: L1_CHAIN_ID,
            mock_l1_rpc: mock.rpc_url(),
            genesis,
            jwt_hex: String::new(),
            workdir: std::path::PathBuf::new(),
            http_port: 0,
            authrpc_port: 0,
        };
        let nitro = NitroDocker::start(&ctx).expect("nitro docker start");
        let arbreth = ArbrethProcess::start(&ctx).expect("arbreth start");
        std::mem::forget(mock);
        Rig {
            dual: DualExec::new(nitro, arbreth),
        }
    }
}

struct Idx(AtomicU64);
impl Idx {
    fn new() -> Self {
        Self(AtomicU64::new(1))
    }
    fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst)
    }
}

fn msg_step(idx: u64, message: L1Message) -> ScenarioStep {
    ScenarioStep::Message {
        idx,
        message,
        delayed_messages_read: 1,
    }
}

#[allow(clippy::too_many_arguments)]
fn payer_tx(
    nonce: u64,
    to: Option<Address>,
    value: U256,
    data: Vec<u8>,
    gas_limit: u64,
    kind: L2TxKind,
) -> SignedL2TxBuilder {
    SignedL2TxBuilder {
        chain_id: L2_CHAIN_ID,
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
        base_fee_l1: L1_BASE_FEE,
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
    steps.push(msg_step(
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
    steps.push(msg_step(
        i,
        payer_tx(
            0,
            Some(recipient),
            U256::from(1_000_000u64),
            Vec::new(),
            100_000,
            L2TxKind::Eip1559,
        )
        .build()
        .expect("eip1559 transfer"),
    ));

    // Legacy value transfer (different tx-type fee handling).
    let i = idx.next();
    steps.push(msg_step(
        i,
        payer_tx(
            1,
            Some(recipient),
            U256::from(2_000_000u64),
            Vec::new(),
            100_000,
            L2TxKind::Legacy,
        )
        .build()
        .expect("legacy transfer"),
    ));

    // Contract deploy (CREATE) ...
    let i = idx.next();
    steps.push(msg_step(
        i,
        payer_tx(
            2,
            None,
            U256::ZERO,
            DEPLOY_INIT.to_vec(),
            200_000,
            L2TxKind::Eip1559,
        )
        .build()
        .expect("contract deploy"),
    ));

    // ... then a call to the freshly-deployed contract.
    let deployed = {
        use alloy_primitives::keccak256;
        let mut rlp = Vec::with_capacity(23);
        rlp.push(0xd6);
        rlp.push(0x94);
        rlp.extend_from_slice(payer.as_slice());
        rlp.push(0x02); // nonce 2
        Address::from_slice(&keccak256(&rlp)[12..])
    };
    let i = idx.next();
    steps.push(msg_step(
        i,
        payer_tx(
            3,
            Some(deployed),
            U256::ZERO,
            Vec::new(),
            100_000,
            L2TxKind::Eip1559,
        )
        .build()
        .expect("contract call"),
    ));

    let scenario = Scenario {
        name: format!("old_arbos_core_v{version}"),
        description: format!("arb1-era core tx surface at ArbOS v{version}"),
        setup: ScenarioSetup {
            l2_chain_id: L2_CHAIN_ID,
            arbos_version: version,
            genesis: None,
        },
        steps,
    };

    let report = rig.dual.run(&scenario).expect("run scenario");
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
