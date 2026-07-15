//! Deposit + L2->L1 messaging dual-exec vs Nitro.
//!
//! Exercises the Arbitrum-specific bridging accounting (value minting on
//! deposit, value burning + the L2->L1 send-merkle accumulator on withdrawal)
//! that the EVM-opcode fuzzers don't touch and that is core arb1 traffic:
//!   - deposits to a fresh EOA, a deployed contract, and a zero-value touch;
//!   - ArbSys.sendTxToL1 from an EOA (merkle-accumulator append + L2ToL1Tx log) repeated so the
//!     accumulator carries through 1 -> 2 -> 3 leaves;
//!   - ArbSys.withdrawEth (value burn -> total-supply decrease) + send.
//!
//! Compared at the arb1-era versions v6, v9 plus v60 (the sendTxToL1 return
//! value is v4-gated and tips are collected at v9).
//!
//! Run (needs Docker + release arb-reth):
//!   ARB_SPEC_BINARY=$(pwd)/target/release/arb-reth \
//!     cargo test -p arb-fuzz --test deposit_l2tol1_dual --release -- --ignored --nocapture

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};

use alloy_primitives::{address, keccak256, Address, Bytes, B256, U256};
use arb_fuzz::scaffolding::selector4;
use arb_test_harness::{
    dual_exec::DualExec,
    genesis::GenesisBuilder,
    messaging::{
        signed_tx::{derive_address, L2TxKind, SignedL2TxBuilder},
        DepositBuilder, L1Message, MessageBuilder,
    },
    mock_l1::MockL1,
    node::{
        arbreth::ArbrethProcess, nitro_docker::NitroDocker, BlockId, ExecutionNode, NodeStartCtx,
    },
    scenario::{Scenario, ScenarioSetup, ScenarioStep},
};

static SERIAL: Mutex<()> = Mutex::new(());

const L2_CHAIN_ID: u64 = 412_346;
const L1_CHAIN_ID: u64 = 11_155_111;
const L1_BASE_FEE: u64 = 30_000_000_000;
const SEQUENCER_ALIAS: Address = address!("a4b000000000000000000073657175656e636572");
const FUNDER: Address = Address::new([0xa6; 20]);
const OWNER: Address = address!("000000000000000000000000000000000c1a0001");
const ARBSYS: Address = address!("0000000000000000000000000000000000000064");
const L1_DEST: Address = address!("00000000000000000000000000000000d0570001");

fn payer_key() -> B256 {
    B256::repeat_byte(0x4d)
}

fn create_address(deployer: Address, nonce: u64) -> Address {
    let mut rlp = Vec::with_capacity(23);
    rlp.push(0xd6);
    rlp.push(0x94);
    rlp.extend_from_slice(deployer.as_slice());
    if nonce == 0 {
        rlp.push(0x80);
    } else {
        assert!(nonce < 0x80);
        rlp.push(nonce as u8);
    }
    Address::from_slice(&keccak256(&rlp)[12..])
}

fn deploy_init(runtime: &[u8]) -> Vec<u8> {
    let l = runtime.len();
    assert!(l < 256);
    let mut out = vec![
        0x60, l as u8, 0x60, 0x0c, 0x60, 0x00, 0x39, 0x60, l as u8, 0x60, 0x00, 0xf3,
    ];
    out.extend_from_slice(runtime);
    out
}

fn store_runtime() -> Vec<u8> {
    vec![0x60, 0x2a, 0x60, 0x00, 0x55, 0x00]
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

struct Rig {
    dual: DualExec<NitroDocker, ArbrethProcess>,
}

impl Rig {
    fn spawn(version: u64) -> Self {
        let mock = MockL1::start(L1_CHAIN_ID).expect("mock l1 start");
        let genesis = GenesisBuilder::new(L2_CHAIN_ID, version)
            .with_initial_chain_owner(OWNER)
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

fn msg(idx: u64, message: L1Message) -> ScenarioStep {
    ScenarioStep::Message {
        idx,
        message,
        delayed_messages_read: 1,
    }
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
        chain_id: L2_CHAIN_ID,
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
        deploy_init(&store_runtime()),
    );
    b.to = None;
    b
}

fn assert_clean_at(version: u64) {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let payer = derive_address(payer_key());
    let store = create_address(payer, 0);
    let fresh_eoa = address!("00000000000000000000000000000000eee00001");
    let mut rig = Rig::spawn(version);
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
            l2_chain_id: L2_CHAIN_ID,
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
    let code_len = rig
        .dual
        .right
        .code(store, BlockId::Number(latest))
        .map(|c| c.len())
        .unwrap_or(0);
    assert!(
        code_len > 0,
        "STORE deploy did not land — test would be hollow"
    );

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
