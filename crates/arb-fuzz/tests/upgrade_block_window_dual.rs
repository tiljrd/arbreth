//! The block whose StartBlock fires a scheduled ArbOS upgrade must give its
//! remaining transactions the NEW precompile band: the reference re-stamps
//! the in-progress header version after the internal tx, so a v59→v60 firing
//! block warm-preloads 0x74 (ArbFilteredTransactionsManager) and dispatches
//! it as an active precompile for the user txs it carries.
//!
//! One batch message packs a BALANCE probe of 0x74 and a direct
//! `isTransactionFiltered` call into the firing block. A producer that keeps
//! the parent-version band for the whole block charges the probe a cold
//! access (+2,500) and runs 0x74's fresh 0xFE stub instead of the precompile.

use std::sync::Mutex;

use alloy_primitives::{address, Address, Bytes, B256, U256};
use arb_fuzz::{arbitrary_impls::interop::wrap_init_code, scaffolding::selector4};
use arb_test_harness::{
    dual_exec::DualExec,
    genesis::GenesisBuilder,
    messaging::{
        b64_l2_msg, kinds,
        signed_tx::{derive_address, L2TxKind, SignedL2TxBuilder},
        DepositBuilder, L1Message, L1MessageHeader, MessageBuilder,
    },
    mock_l1::MockL1,
    node::{
        arbreth::ArbrethProcess, nitro_docker::NitroDocker, BlockId, ExecutionNode, NodeStartCtx,
    },
    scenario::{Scenario, ScenarioSetup, ScenarioStep},
};

static SERIAL: Mutex<()> = Mutex::new(());

const L2_CHAIN_ID: u64 = 412_353;
const L1_CHAIN_ID: u64 = 11_155_111;
const FUZZ_L1_BASE_FEE: u64 = 30_000_000_000;
const PARENT_VERSION: u64 = 59;
const NEW_VERSION: u64 = 60;
const BASE_TS: u64 = 1_700_000_000;
const BLOCK_SECS: u64 = 12;

const ARBOWNER: Address = address!("0000000000000000000000000000000000000070");
const FILTERED_TX_MANAGER: Address = address!("0000000000000000000000000000000000000074");
const FUNDER: Address = Address::new([0xa1; 20]);
const SEQUENCER_ALIAS: Address = address!("a4b000000000000000000073657175656e636572");

const MAX_FEE: u128 = 10_000_000_000;

fn owner_key() -> B256 {
    B256::repeat_byte(0x42)
}

/// Runtime: `PUSH20 0x…74; BALANCE; POP; STOP`.
fn balance_probe_runtime() -> Vec<u8> {
    let mut code = vec![0x73];
    code.extend_from_slice(FILTERED_TX_MANAGER.as_slice());
    code.extend_from_slice(&[0x31, 0x50, 0x00]);
    code
}

fn schedule_upgrade_calldata(version: u64, timestamp: u64) -> Vec<u8> {
    let mut d = selector4("scheduleArbOSUpgrade(uint64,uint64)").to_vec();
    let mut w = [0u8; 32];
    w[24..].copy_from_slice(&version.to_be_bytes());
    d.extend_from_slice(&w);
    let mut w = [0u8; 32];
    w[24..].copy_from_slice(&timestamp.to_be_bytes());
    d.extend_from_slice(&w);
    d
}

fn is_transaction_filtered_calldata() -> Vec<u8> {
    let mut d = selector4("isTransactionFiltered(bytes32)").to_vec();
    d.extend_from_slice(&[0u8; 32]);
    d
}

fn tx(nonce: u64, to: Option<Address>, data: Vec<u8>, ts: u64) -> SignedL2TxBuilder {
    SignedL2TxBuilder {
        chain_id: L2_CHAIN_ID,
        nonce,
        to,
        value: U256::ZERO,
        data: Bytes::from(data),
        gas_limit: 3_000_000,
        gas_price: MAX_FEE,
        max_fee_per_gas: MAX_FEE,
        max_priority_fee_per_gas: 0,
        access_list: Vec::new(),
        authorization_list: Vec::new(),
        kind: L2TxKind::Eip1559,
        signing_key: owner_key(),
        l1_block_number: 1 + (ts - BASE_TS) / BLOCK_SECS,
        timestamp: ts,
        request_id: None,
        sender: SEQUENCER_ALIAS,
        base_fee_l1: FUZZ_L1_BASE_FEE,
    }
}

/// Pack signed L2 transactions into one batch message (one block).
fn batch_message(subs: &[SignedL2TxBuilder], block_number: u64, timestamp: u64) -> L1Message {
    let mut body = vec![0x03u8];
    for s in subs {
        let sub = s.encode_body().expect("encode sub-message");
        body.extend_from_slice(&(sub.len() as u64).to_be_bytes());
        body.extend_from_slice(&sub);
    }
    L1Message {
        header: L1MessageHeader {
            kind: kinds::KIND_L2_MESSAGE,
            sender: SEQUENCER_ALIAS,
            block_number,
            timestamp,
            request_id: None,
            base_fee_l1: FUZZ_L1_BASE_FEE,
        },
        l2_msg: b64_l2_msg(&Bytes::from(body)),
    }
}

fn msg_step(idx: u64, message: L1Message, dmr: u64) -> ScenarioStep {
    ScenarioStep::Message {
        idx,
        message,
        delayed_messages_read: dmr,
    }
}

fn build_scenario(owner: Address, probe: Address) -> Scenario {
    let mut steps = Vec::new();
    let mut idx = 0u64;
    let mut next = || {
        idx += 1;
        idx
    };

    // Fund the owner.
    let i = next();
    steps.push(msg_step(
        i,
        DepositBuilder {
            from: FUNDER,
            to: owner,
            amount: U256::from(10u128).pow(U256::from(21u64)),
            l1_block_number: 1,
            timestamp: BASE_TS,
            request_seq: i,
            base_fee_l1: FUZZ_L1_BASE_FEE,
        }
        .build()
        .expect("deposit"),
        1,
    ));

    // Deploy the BALANCE probe at owner nonce 0.
    let i = next();
    steps.push(msg_step(
        i,
        tx(
            0,
            None,
            wrap_init_code(&balance_probe_runtime()),
            BASE_TS + i * BLOCK_SECS,
        )
        .build()
        .expect("deploy probe"),
        1,
    ));

    // Schedule the v60 upgrade for a flag day between this block and the next.
    let i = next();
    let schedule_ts = BASE_TS + i * BLOCK_SECS;
    let flag_day = schedule_ts + 1;
    steps.push(msg_step(
        i,
        tx(
            1,
            Some(ARBOWNER),
            schedule_upgrade_calldata(NEW_VERSION, flag_day),
            schedule_ts,
        )
        .build()
        .expect("schedule upgrade"),
        1,
    ));

    // The firing block: StartBlock performs the upgrade, then the batched
    // user txs probe 0x74 warmth and dispatch.
    let i = next();
    let ts = BASE_TS + i * BLOCK_SECS;
    assert!(ts >= flag_day);
    let probe_call = tx(2, Some(probe), Vec::new(), ts);
    let dispatch_call = tx(
        3,
        Some(FILTERED_TX_MANAGER),
        is_transaction_filtered_calldata(),
        ts,
    );
    steps.push(msg_step(
        i,
        batch_message(
            &[probe_call, dispatch_call],
            1 + (ts - BASE_TS) / BLOCK_SECS,
            ts,
        ),
        1,
    ));

    // Trailing no-op deposit seals the block before state is queried.
    let i = next();
    steps.push(msg_step(
        i,
        DepositBuilder {
            from: FUNDER,
            to: owner,
            amount: U256::from(1u64),
            l1_block_number: 2,
            timestamp: BASE_TS + i * BLOCK_SECS,
            request_seq: i,
            base_fee_l1: FUZZ_L1_BASE_FEE,
        }
        .build()
        .expect("seal deposit"),
        2,
    ));

    Scenario {
        name: "upgrade_block_window".into(),
        description: "user txs in the upgrade-firing block get the new precompile band".into(),
        setup: ScenarioSetup {
            l2_chain_id: L2_CHAIN_ID,
            arbos_version: PARENT_VERSION,
            genesis: None,
        },
        steps,
    }
}

fn assert_setup_landed(dual: &DualExec<NitroDocker, ArbrethProcess>, owner: Address) {
    for (node, n) in [
        ("reference", &dual.left as &dyn ExecutionNode),
        ("arbreth", &dual.right as &dyn ExecutionNode),
    ] {
        let nonce = n.nonce(owner, BlockId::Latest).expect("owner nonce");
        assert!(
            nonce >= 4,
            "[{node}] owner must advance past the batch, got nonce {nonce}"
        );
        // The upgrade must actually have fired: v60 writes 0x74's 0xFE stub.
        let code = n
            .code(FILTERED_TX_MANAGER, BlockId::Latest)
            .expect("0x74 code");
        assert!(
            !code.is_empty(),
            "[{node}] the scheduled upgrade must fire (0x74 carries no stub)"
        );
    }
}

#[test]
#[ignore]
fn upgrade_block_window_clean() {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let owner = derive_address(owner_key());

    let mock = MockL1::start(L1_CHAIN_ID).expect("mock l1 start");
    let genesis = GenesisBuilder::new(L2_CHAIN_ID, PARENT_VERSION)
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
    let mut dual = DualExec::new(nitro, arbreth);

    let probe = arb_fuzz::arbitrary_impls::interop::create_address(owner, 0);
    let scenario = build_scenario(owner, probe);
    let report = dual.run(&scenario).expect("dual run");

    assert_setup_landed(&dual, owner);

    assert!(
        report.is_clean(),
        "txs in the upgrade-firing block must run with the new precompile band on both \
         nodes\n  block_diffs={:#?}",
        report.block_diffs,
    );
}
