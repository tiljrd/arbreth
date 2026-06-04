//! Fee / tip / poster-gas matrix dual-exec vs Nitro.
//!
//! Generalises the two arb1-era divergences found via targeted harnesses (the
//! v9 legacy-tx tip/posterGas bug and the zero-callvalue retryable tombstone):
//! both were Arbitrum-specific accounting bugs that only fired under specific
//! tx conditions the random fuzzers never varied. This harness sweeps tx type
//! (legacy / EIP-2930 / EIP-1559 with and without a priority fee) crossed with
//! a gas price above the base fee (so a tip exists) and value zero / non-zero,
//! against deployed-contract and EOA targets, at the gate versions v6, v9, v11,
//! v60. The chain owner (= network fee account) is distinct from the payer so a
//! mis-routed tip surfaces as a state-root diff.
//!
//! Run (needs Docker + release arb-reth):
//!   ARB_SPEC_BINARY=$(pwd)/target/release/arb-reth \
//!     cargo test -p arb-fuzz --test fee_matrix_dual --release -- --ignored --nocapture

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
const FUNDER: Address = Address::new([0xa5; 20]);
const OWNER: Address = address!("000000000000000000000000000000000c1a0000");
const RECIPIENT: Address = address!("00000000000000000000000000000000d00d0002");
const GWEI: u128 = 1_000_000_000;

fn payer_key() -> B256 {
    B256::repeat_byte(0x39)
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

/// STORE: writes 0x2a to slot 0 (payable).
fn store_runtime() -> Vec<u8> {
    vec![0x60, 0x2a, 0x60, 0x00, 0x55, 0x00]
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
        chain_id: L2_CHAIN_ID,
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

/// One matrix entry: (label, kind, gas_price, max_fee, max_priority, value, to_contract).
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
    let mut rig = Rig::spawn(version);
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
            deploy_init(&store_runtime()),
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
        "ArbOS v{version} fee matrix diverged from Nitro:\n\
         block_diffs={} tx_diffs={} state_diffs={} log_diffs={}\n{:#?}",
        report.block_diffs.len(),
        report.tx_diffs.len(),
        report.state_diffs.len(),
        report.log_diffs.len(),
        report
    );
}

/// Diagnostic: dump each node's per-block tx hashes + (to, status) so a
/// tx-presence divergence can be mapped to a matrix case.
#[test]
#[ignore]
fn dump_fee_v9() {
    dump_fee_at(9);
}

#[test]
#[ignore]
fn dump_fee_v6() {
    dump_fee_at(6);
}

fn dump_fee_at(version: u64) {
    use arb_test_harness::rpc::JsonRpcClient;
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let payer = derive_address(payer_key());
    let store = create_address(payer, 0);
    let mut rig = Rig::spawn(version);
    let idx = Idx::new();
    let mut steps = Vec::new();
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
        .unwrap(),
    ));
    steps.push(msg(
        idx.next(),
        tx(
            0,
            None,
            U256::ZERO,
            deploy_init(&store_runtime()),
            L2TxKind::Eip1559,
            GWEI,
            GWEI,
            0,
        )
        .build()
        .unwrap(),
    ));
    let mut nonce = 1u64;
    let cases = matrix();
    for c in &cases {
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
            .unwrap(),
        ));
        nonce += 1;
    }
    let scenario = Scenario {
        name: "fee_diag".into(),
        description: "diag".into(),
        setup: ScenarioSetup {
            l2_chain_id: L2_CHAIN_ID,
            arbos_version: version,
            genesis: None,
        },
        steps,
    };
    let _ = rig.dual.run(&scenario).expect("run");
    let lrpc = JsonRpcClient::new(rig.dual.left.rpc_url().to_string());
    let rrpc = JsonRpcClient::new(rig.dual.right.rpc_url().to_string());
    let latest = rig.dual.left.block(BlockId::Latest).expect("blk").number;
    let rlatest = rig.dual.right.block(BlockId::Latest).expect("blk").number;
    eprintln!("latest block: nitro={latest} arbreth={rlatest}");
    eprintln!(
        "matrix cases (nonce 1..): {:?}",
        cases
            .iter()
            .enumerate()
            .map(|(i, c)| format!(
                "n{}={:?} gp={} mf={} mp={} val={} contract={}",
                i + 1,
                c.kind,
                c.gas_price,
                c.max_fee,
                c.max_priority,
                c.value,
                c.to_contract
            ))
            .collect::<Vec<_>>()
    );
    let dump = |rpc: &JsonRpcClient, node: &str, top: u64| {
        for b in 1..=top {
            let blk = rpc
                .call(
                    "eth_getBlockByNumber",
                    serde_json::json!([format!("0x{b:x}"), false]),
                )
                .unwrap_or(serde_json::Value::Null);
            if let Some(txs) = blk.get("transactions").and_then(|t| t.as_array()) {
                for h in txs {
                    let hs = h.as_str().unwrap_or("");
                    let r = rpc
                        .call("eth_getTransactionReceipt", serde_json::json!([hs]))
                        .unwrap_or(serde_json::Value::Null);
                    let to = r.get("to").and_then(|x| x.as_str()).unwrap_or("-");
                    let st = r.get("status").and_then(|x| x.as_str()).unwrap_or("-");
                    let ty = r.get("type").and_then(|x| x.as_str()).unwrap_or("-");
                    eprintln!(
                        "  {node} blk{b} tx={} type={ty} to={to} status={st}",
                        &hs[..18.min(hs.len())]
                    );
                }
            }
        }
    };
    eprintln!("--- NITRO ---");
    dump(&lrpc, "nitro", latest);
    eprintln!("--- ARBRETH ---");
    dump(&rrpc, "arbreth", rlatest);
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
