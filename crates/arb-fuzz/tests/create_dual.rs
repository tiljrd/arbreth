//! CREATE / CREATE2 dual-exec vs Nitro.
//!
//! Deploys factory contracts that CREATE / CREATE2 caller-supplied init code,
//! plus top-level deploys, to exercise the contract-creation surface arb1 hits
//! constantly and the arbreth-specific pieces (the CREATE trampoline, the
//! EIP-3541 0xEF re-apply with its Stylus-marker exception, code-deposit, and
//! collision handling): nested CREATE success / constructor-revert / value
//! endowment, CREATE2 with a salt and a same-salt collision, a top-level deploy
//! whose runtime starts 0xEF (EIP-3541 reject), and an empty-init deploy.
//! Compared at v6 (pre-Shanghai), v11 (Shanghai / EIP-3860) and v60 (Stylus).
//!
//! Run (needs Docker + release arb-reth):
//!   ARB_SPEC_BINARY=$(pwd)/target/release/arb-reth \
//!     cargo test -p arb-fuzz --test create_dual --release -- --ignored --nocapture

use std::sync::{
    atomic::{AtomicU64, Ordering},
    Mutex,
};

use alloy_primitives::{address, keccak256, Address, Bytes, B256, U256};
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
const FUNDER: Address = Address::new([0xa7; 20]);
const OWNER: Address = address!("000000000000000000000000000000000c1a0002");

fn payer_key() -> B256 {
    B256::repeat_byte(0x2c)
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

/// Constructor returning `runtime` verbatim (12-byte prefix).
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

/// Init code that reverts immediately (constructor revert -> no code).
fn revert_init() -> Vec<u8> {
    vec![0x60, 0x00, 0x60, 0x00, 0xfd]
}

/// Factory whose runtime CREATEs the caller-supplied calldata as init code and
/// stores the resulting address at slot 0 (0 on failure).
/// CALLDATACOPY(0,0,size); CREATE(callvalue,0,size); SSTORE(0,addr); STOP.
fn factory_create_runtime() -> Vec<u8> {
    vec![
        0x36, 0x60, 0x00, 0x60, 0x00, 0x37, // CALLDATASIZE,0,0,CALLDATACOPY
        0x36, 0x60, 0x00, 0x34, 0xf0, // CALLDATASIZE,0,CALLVALUE,CREATE
        0x60, 0x00, 0x55, 0x00, // SSTORE(0,addr),STOP
    ]
}

/// Factory using CREATE2 with a fixed salt (0x07).
fn factory_create2_runtime() -> Vec<u8> {
    vec![
        0x36, 0x60, 0x00, 0x60, 0x00, 0x37, // CALLDATACOPY(0,0,size)
        0x60, 0x07, // PUSH1 salt
        0x36, 0x60, 0x00, 0x34, 0xf5, // CALLDATASIZE,0,CALLVALUE,CREATE2
        0x60, 0x00, 0x55, 0x00, // SSTORE(0,addr),STOP
    ]
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

fn tx(nonce: u64, to: Option<Address>, value: U256, data: Vec<u8>) -> SignedL2TxBuilder {
    SignedL2TxBuilder {
        chain_id: L2_CHAIN_ID,
        nonce,
        to,
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

fn assert_clean_at(version: u64) {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let payer = derive_address(payer_key());
    let factory_create = create_address(payer, 0);
    let factory_create2 = create_address(payer, 1);
    let mut rig = Rig::spawn(version);
    let idx = Idx::new();
    let mut steps = Vec::new();

    steps.push(msg(
        idx.next(),
        DepositBuilder {
            from: FUNDER,
            to: payer,
            amount: U256::from(10u128).pow(U256::from(20u64)),
            l1_block_number: 1,
            timestamp: 1_700_000_000,
            request_seq: 1,
            base_fee_l1: L1_BASE_FEE,
        }
        .build()
        .expect("deposit"),
    ));

    // Deploy the two factories (nonces 0, 1).
    steps.push(msg(
        idx.next(),
        tx(0, None, U256::ZERO, deploy_init(&factory_create_runtime()))
            .build()
            .unwrap(),
    ));
    steps.push(msg(
        idx.next(),
        tx(1, None, U256::ZERO, deploy_init(&factory_create2_runtime()))
            .build()
            .unwrap(),
    ));

    let store_init = deploy_init(&store_runtime());

    // nonce 2: factory CREATE of STORE (success).
    steps.push(msg(
        idx.next(),
        tx(2, Some(factory_create), U256::ZERO, store_init.clone())
            .build()
            .unwrap(),
    ));
    // nonce 3: factory CREATE of reverting init (failure -> slot 0 stays 0).
    steps.push(msg(
        idx.next(),
        tx(3, Some(factory_create), U256::ZERO, revert_init())
            .build()
            .unwrap(),
    ));
    // nonce 4: factory CREATE with value endowment.
    steps.push(msg(
        idx.next(),
        tx(
            4,
            Some(factory_create),
            U256::from(4_242u64),
            store_init.clone(),
        )
        .build()
        .unwrap(),
    ));
    // nonce 5: factory CREATE2 of STORE (success).
    steps.push(msg(
        idx.next(),
        tx(5, Some(factory_create2), U256::ZERO, store_init.clone())
            .build()
            .unwrap(),
    ));
    // nonce 6: factory CREATE2 same salt+init -> address collision (failure).
    steps.push(msg(
        idx.next(),
        tx(6, Some(factory_create2), U256::ZERO, store_init.clone())
            .build()
            .unwrap(),
    ));
    // nonce 7: top-level deploy whose runtime starts 0xEF -> EIP-3541 reject.
    steps.push(msg(
        idx.next(),
        tx(7, None, U256::ZERO, deploy_init(&[0xEF, 0x01]))
            .build()
            .unwrap(),
    ));
    // nonce 8: top-level deploy with empty init -> empty-code account.
    steps.push(msg(
        idx.next(),
        tx(8, None, U256::ZERO, Vec::new()).build().unwrap(),
    ));

    let scenario = Scenario {
        name: format!("create_v{version}"),
        description: format!("CREATE/CREATE2 surface at ArbOS v{version}"),
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
    let fc = rig
        .dual
        .right
        .code(factory_create, BlockId::Number(latest))
        .map(|c| c.len())
        .unwrap_or(0);
    assert!(fc > 0, "factory deploy did not land — test would be hollow");

    assert!(
        report.is_clean(),
        "ArbOS v{version} CREATE/CREATE2 surface diverged from Nitro:\n\
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
fn create_matches_nitro_v6() {
    assert_clean_at(6);
}

#[test]
#[ignore]
fn create_matches_nitro_v11() {
    assert_clean_at(11);
}

#[test]
#[ignore]
fn create_matches_nitro_v60() {
    assert_clean_at(60);
}
