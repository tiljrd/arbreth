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
//!
//! A deploy-landed guard fails loudly if the contract tx is dropped (e.g. gas
//! below poster cost) so a clean report cannot be a hollow no-op agreement.
//!
//! STATUS: v6/v7/v8 pass clean. v9 currently DIVERGES — one tx reports
//! gas_used 110600 on Nitro vs 917000 on arbreth; v9 is the always-collect-
//! tips island. Under investigation, not yet root-caused.

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
    node::{arbreth::ArbrethProcess, nitro_docker::NitroDocker, BlockId, ExecutionNode, NodeStartCtx},
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
            1_000_000,
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
            1_000_000,
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
            30_000_000,
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
            l2_chain_id: L2_CHAIN_ID,
            arbos_version: version,
            genesis: None,
        },
        steps,
    };

    let report = rig.dual.run(&scenario).expect("run scenario");

    // Guard against a hollow pass: the deploy must have landed (else the call
    // hits an empty account and both nodes trivially agree on a no-op).
    let latest = rig.dual.right.block(BlockId::Latest).expect("latest").number;
    let code_len = rig
        .dual
        .right
        .code(deployed, BlockId::Number(latest))
        .map(|c| c.len())
        .unwrap_or(0);
    assert!(code_len > 0, "deploy did not land (code empty) — test would be hollow");

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

/// Diagnostic: replay the core-tx scenario at `version` and dump each tx's gas
/// breakdown (gasUsed, gasUsedForL1 poster gas, effectiveGasPrice, status) from
/// both nodes via raw RPC, flagging which tx and which component diverges.
fn dump_gas_at(version: u64) {
    use arb_test_harness::rpc::JsonRpcClient;
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let payer = derive_address(payer_key());
    let recipient = address!("00000000000000000000000000000000d00d0001");
    let mut rig = Rig::spawn(version, payer);
    let idx = Idx::new();
    let mut steps = Vec::new();
    let dep = idx.next();
    steps.push(msg_step(
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
    let i = idx.next();
    steps.push(msg_step(i, payer_tx(0, Some(recipient), U256::from(1_000_000u64), Vec::new(), 1_000_000, L2TxKind::Eip1559).build().unwrap()));
    let i = idx.next();
    steps.push(msg_step(i, payer_tx(1, Some(recipient), U256::from(2_000_000u64), Vec::new(), 1_000_000, L2TxKind::Legacy).build().unwrap()));
    let i = idx.next();
    steps.push(msg_step(i, payer_tx(2, None, U256::ZERO, DEPLOY_INIT.to_vec(), 30_000_000, L2TxKind::Eip1559).build().unwrap()));
    let deployed = {
        use alloy_primitives::keccak256;
        let mut rlp = Vec::with_capacity(23);
        rlp.push(0xd6);
        rlp.push(0x94);
        rlp.extend_from_slice(payer.as_slice());
        rlp.push(0x02);
        Address::from_slice(&keccak256(&rlp)[12..])
    };
    let i = idx.next();
    steps.push(msg_step(i, payer_tx(3, Some(deployed), U256::ZERO, Vec::new(), 1_000_000, L2TxKind::Eip1559).build().unwrap()));

    let scenario = Scenario {
        name: format!("gas_diag_v{version}"),
        description: "gas diag".into(),
        setup: ScenarioSetup { l2_chain_id: L2_CHAIN_ID, arbos_version: version, genesis: None },
        steps,
    };
    let report = rig.dual.run(&scenario).expect("run");
    eprintln!(
        "v{version} REPORT is_clean={} block_diffs={} tx_diffs={}",
        report.is_clean(),
        report.block_diffs.len(),
        report.tx_diffs.len()
    );
    for d in &report.tx_diffs {
        eprintln!("  REPORT-TXDIFF tx={:?} field={} left={} right={}", d.tx_hash, d.field, d.left, d.right);
    }
    let lrpc = JsonRpcClient::new(rig.dual.left.rpc_url().to_string());
    let rrpc = JsonRpcClient::new(rig.dual.right.rpc_url().to_string());
    let latest = rig.dual.left.block(BlockId::Latest).expect("latest").number;
    let field = |v: &serde_json::Value, k: &str| -> String {
        v.get(k).and_then(|x| x.as_str()).map(|s| {
            u128::from_str_radix(s.trim_start_matches("0x"), 16).map(|n| n.to_string()).unwrap_or_else(|_| s.to_string())
        }).unwrap_or_else(|| "-".into())
    };
    for b in 1..=latest {
        let blk = rig.dual.left.block(BlockId::Number(b)).expect("blk");
        for h in &blk.tx_hashes {
            let hs = format!("{h:?}");
            let lr = lrpc.call("eth_getTransactionReceipt", serde_json::json!([hs])).unwrap_or(serde_json::Value::Null);
            let rr = rrpc.call("eth_getTransactionReceipt", serde_json::json!([hs])).unwrap_or(serde_json::Value::Null);
            let lg = field(&lr, "gasUsed");
            let rg = field(&rr, "gasUsed");
            let tag = if lg != rg { "DIFF" } else { "ok  " };
            eprintln!(
                "{tag} v{version} blk{b} to={} created={} status nitro={}/arbreth={}\n      gasUsed   nitro={} arbreth={}\n      gasL1     nitro={} arbreth={}\n      effGasPx  nitro={} arbreth={}",
                field(&lr, "to"), field(&lr, "contractAddress"),
                field(&lr, "status"), field(&rr, "status"),
                lg, rg,
                field(&lr, "gasUsedForL1"), field(&rr, "gasUsedForL1"),
                field(&lr, "effectiveGasPrice"), field(&rr, "effectiveGasPrice"),
            );
        }
    }
    // Balance probe at the latest block to find the diverging account.
    let at = BlockId::Number(latest);
    let cands: [(&str, Address); 6] = [
        ("payer/owner/networkfee", payer),
        ("recipient", recipient),
        ("coinbase/sequencer", SEQUENCER_ALIAS),
        ("l1_pricer_pool", address!("a4b00000000000000000000000000000000000f6")),
        ("deployed", deployed),
        ("zero", Address::ZERO),
    ];
    for (name, a) in cands {
        let lb = rig.dual.left.balance(a, at.clone()).unwrap_or(U256::ZERO);
        let rb = rig.dual.right.balance(a, at.clone()).unwrap_or(U256::ZERO);
        let tag = if lb != rb { "BAL-DIFF" } else { "bal-ok  " };
        eprintln!("{tag} v{version} {name} {a:?}: nitro={lb} arbreth={rb} delta={}",
            if lb >= rb { format!("-{}", lb - rb) } else { format!("+{}", rb - lb) });
    }
}

#[test]
#[ignore]
fn dump_gas_v9() {
    dump_gas_at(9);
}

#[test]
#[ignore]
fn dump_gas_v8() {
    dump_gas_at(8);
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
