//! Retryable lifecycle dual-exec where the retry target is a REAL deployed
//! contract — the arb1 block-22,209,702 pattern that synthetic submit-only
//! fuzzing (empty `to`) never exercised.
//!
//! Deploys a state-mutating STORE contract and an always-REVERT contract, then
//! submits retryables targeting them and compares arbreth vs Nitro across the
//! full block/receipt/log/state surface:
//!   - auto-redeem to STORE: inner contract code runs, slot written, ticket
//!     deleted (escrow drained, numTries=1);
//!   - auto-redeem to REVERT: retry fails, callvalue returns to escrow, ticket
//!     retained, submission fee not refunded;
//!   - gas_limit=0: no auto-redeem, ticket sits in escrow, gas cost refunded.
//!
//! Run (needs Docker + release arb-reth):
//!   ARB_SPEC_BINARY=$(pwd)/target/release/arb-reth \
//!     cargo test -p arb-fuzz --test retryable_to_contract_dual --release -- --ignored --nocapture
//!
//! STATUS: clean at v6/v9/v11/v60. The v6/v9/v11 zero-callvalue auto-redeem
//! tombstone (arbreth dropped an empty escrow leaf Nitro keeps as a zombie at
//! ArbOS<30) is fixed (commit 82db241: zero-value mint touch-if-empty).

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
        DepositBuilder, L1Message, MessageBuilder, RetryableSubmitBuilder,
    },
    mock_l1::MockL1,
    node::{arbreth::ArbrethProcess, nitro_docker::NitroDocker, NodeStartCtx},
    scenario::{Scenario, ScenarioSetup, ScenarioStep},
};

static SERIAL: Mutex<()> = Mutex::new(());

const L2_CHAIN_ID: u64 = 412_346;
const L1_CHAIN_ID: u64 = 11_155_111;
const L1_BASE_FEE: u64 = 30_000_000_000;
const SEQUENCER_ALIAS: Address = address!("a4b000000000000000000073657175656e636572");
const FUNDER: Address = Address::new([0xa3; 20]);

fn deployer_key() -> B256 {
    B256::repeat_byte(0x6c)
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

/// STORE: writes 0x2a to slot 0 (state-mutating, payable).
fn store_runtime() -> Vec<u8> {
    vec![0x60, 0x2a, 0x60, 0x00, 0x55, 0x00]
}

/// REVERT: reverts with empty data.
fn revert_runtime() -> Vec<u8> {
    vec![0x60, 0x00, 0x60, 0x00, 0xfd]
}

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

fn msg(idx: u64, message: L1Message) -> ScenarioStep {
    ScenarioStep::Message {
        idx,
        message,
        delayed_messages_read: 1,
    }
}

fn deposit(idx: u64, to: Address, amount: U256, l1_block: u64, ts: u64) -> ScenarioStep {
    msg(
        idx,
        DepositBuilder {
            from: FUNDER,
            to,
            amount,
            l1_block_number: l1_block,
            timestamp: ts,
            request_seq: idx,
            base_fee_l1: L1_BASE_FEE,
        }
        .build()
        .expect("deposit"),
    )
}

fn deploy(nonce: u64, runtime: &[u8], l1_block: u64, ts: u64) -> SignedL2TxBuilder {
    SignedL2TxBuilder {
        chain_id: L2_CHAIN_ID,
        nonce,
        to: None,
        value: U256::ZERO,
        data: Bytes::from(deploy_init(runtime)),
        gas_limit: 30_000_000,
        gas_price: 1_000_000_000,
        max_fee_per_gas: 1_000_000_000,
        max_priority_fee_per_gas: 0,
        access_list: Vec::new(),
        authorization_list: Vec::new(),
        kind: L2TxKind::Eip1559,
        signing_key: deployer_key(),
        l1_block_number: l1_block,
        timestamp: ts,
        request_id: None,
        sender: SEQUENCER_ALIAS,
        base_fee_l1: 100_000_000,
    }
}

#[allow(clippy::too_many_arguments)]
fn submit_retryable(
    l1_sender: Address,
    to: Address,
    gas_limit: u64,
    l2_call_value: U256,
    l1_block: u64,
    ts: u64,
    req: B256,
) -> RetryableSubmitBuilder {
    RetryableSubmitBuilder {
        l1_sender,
        to,
        l2_call_value,
        // Generously over-fund: submission fee (l1BaseFee*1400 for empty data) +
        // callvalue + gas*maxfee all well under 1 ETH.
        deposit_value: U256::from(10u128).pow(U256::from(18u64)),
        max_submission_fee: U256::from(10u128).pow(U256::from(17u64)),
        excess_fee_refund_address: address!("000000000000000000000000000000000fee0001"),
        call_value_refund_address: address!("000000000000000000000000000000000bef0001"),
        gas_limit,
        max_fee_per_gas: U256::from(1_000_000_000u64),
        data: Bytes::new(),
        l1_block_number: l1_block,
        timestamp: ts,
        request_id: Some(req),
    }
}

fn req_id(tag: u8) -> B256 {
    B256::repeat_byte(tag)
}

fn assert_clean_at(version: u64) {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let owner = derive_address(deployer_key());
    let mut rig = Rig::spawn(version, owner);
    let idx = Idx::new();
    let mut steps = Vec::new();

    let deployer = derive_address(deployer_key());
    let store = create_address(deployer, 0);
    let revert_c = create_address(deployer, 1);

    let t = 1_700_000_000u64;

    // Block 1: fund the deployer, deploy STORE + REVERT.
    steps.push(deposit(
        idx.next(),
        deployer,
        U256::from(10u128).pow(U256::from(18u64)),
        1,
        t,
    ));
    steps.push(msg(
        idx.next(),
        deploy(0, &store_runtime(), 1, t)
            .build()
            .expect("deploy store"),
    ));
    steps.push(msg(
        idx.next(),
        deploy(1, &revert_runtime(), 1, t)
            .build()
            .expect("deploy revert"),
    ));

    // Block 2: submit retryables targeting the deployed contracts.
    let l1_sender_a = Address::repeat_byte(0x51);
    let l1_sender_b = Address::repeat_byte(0x52);
    let l1_sender_c = Address::repeat_byte(0x53);

    // (a) auto-redeem to STORE: inner code runs, slot 0 := 0x2a, ticket deleted.
    steps.push(msg(
        idx.next(),
        submit_retryable(
            l1_sender_a,
            store,
            200_000,
            U256::ZERO,
            2,
            t + 4,
            req_id(0xa1),
        )
        .build()
        .expect("submit->store"),
    ));
    // (b) auto-redeem to REVERT: retry fails, ticket retained, fee not refunded.
    steps.push(msg(
        idx.next(),
        submit_retryable(
            l1_sender_b,
            revert_c,
            200_000,
            U256::ZERO,
            2,
            t + 4,
            req_id(0xb2),
        )
        .build()
        .expect("submit->revert"),
    ));
    // (c) gas_limit=0: no auto-redeem, ticket sits in escrow with callvalue.
    steps.push(msg(
        idx.next(),
        submit_retryable(
            l1_sender_c,
            store,
            0,
            U256::from(12_345u64),
            2,
            t + 4,
            req_id(0xc3),
        )
        .build()
        .expect("submit no-redeem"),
    ));

    let scenario = Scenario {
        name: format!("retryable_to_contract_v{version}"),
        description: format!("retryable auto-redeem into a real contract at ArbOS v{version}"),
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
        "ArbOS v{version} retryable-to-contract surface diverged from Nitro:\n\
         block_diffs={} tx_diffs={} state_diffs={} log_diffs={}\n{:#?}",
        report.block_diffs.len(),
        report.tx_diffs.len(),
        report.state_diffs.len(),
        report.log_diffs.len(),
        report
    );
}

/// Isolation diagnostic: run a single submit `case` (a=auto-redeem-to-STORE,
/// b=auto-redeem-to-REVERT, c=escrow-only gas_limit=0) at `version`.
fn run_single(version: u64, case: char) {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let owner = derive_address(deployer_key());
    let mut rig = Rig::spawn(version, owner);
    let idx = Idx::new();
    let mut steps = Vec::new();
    let deployer = derive_address(deployer_key());
    let store = create_address(deployer, 0);
    let revert_c = create_address(deployer, 1);
    let t = 1_700_000_000u64;
    steps.push(deposit(
        idx.next(),
        deployer,
        U256::from(10u128).pow(U256::from(18u64)),
        1,
        t,
    ));
    steps.push(msg(
        idx.next(),
        deploy(0, &store_runtime(), 1, t).build().unwrap(),
    ));
    steps.push(msg(
        idx.next(),
        deploy(1, &revert_runtime(), 1, t).build().unwrap(),
    ));
    let l1s = Address::repeat_byte(0x51);
    let sub = match case {
        'a' => submit_retryable(l1s, store, 200_000, U256::ZERO, 2, t + 4, req_id(0xa1)),
        'b' => submit_retryable(l1s, revert_c, 200_000, U256::ZERO, 2, t + 4, req_id(0xb2)),
        'c' => submit_retryable(l1s, store, 0, U256::from(12_345u64), 2, t + 4, req_id(0xc3)),
        // (d) success redeem WITH callvalue: escrow is still drained to empty on
        //     success, so it should diverge like (a) if the empty escrow is the cause.
        'd' => submit_retryable(
            l1s,
            store,
            200_000,
            U256::from(12_345u64),
            2,
            t + 4,
            req_id(0xd4),
        ),
        // (e) revert redeem WITH callvalue: the failed redeem RETAINS the callvalue
        //     in escrow, so the escrow stays non-empty. If (e) is clean while
        //     (b) (callvalue=0) diverges, the empty escrow's tombstone is the cause.
        'e' => submit_retryable(
            l1s,
            revert_c,
            200_000,
            U256::from(12_345u64),
            2,
            t + 4,
            req_id(0xe5),
        ),
        _ => unreachable!(),
    };
    steps.push(msg(idx.next(), sub.build().unwrap()));
    let scenario = Scenario {
        name: format!("retryable_single_{case}_v{version}"),
        description: format!("isolated retryable case {case} at v{version}"),
        setup: ScenarioSetup {
            l2_chain_id: L2_CHAIN_ID,
            arbos_version: version,
            genesis: None,
        },
        steps,
    };
    let report = rig.dual.run(&scenario).expect("run");
    assert!(
        report.is_clean(),
        "case {case} v{version}: block_diffs={} tx_diffs={} state_diffs={} log_diffs={}\n{:#?}",
        report.block_diffs.len(),
        report.tx_diffs.len(),
        report.state_diffs.len(),
        report.log_diffs.len(),
        report
    );
}

/// Diagnostic: run case a at v6, then diff candidate account state between
/// Nitro (left) and arbreth (right) at the latest block to pinpoint the
/// account whose trie entry diverges.
#[test]
#[ignore]
fn diff_accounts_autoredeem_v6() {
    diff_accounts_autoredeem_at(6);
}

#[test]
#[ignore]
fn diff_accounts_autoredeem_v60() {
    diff_accounts_autoredeem_at(60);
}

fn diff_accounts_autoredeem_at(version: u64) {
    use arb_test_harness::messaging::apply_l1_to_l2_alias;
    use arb_test_harness::node::{BlockId, ExecutionNode};
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let owner = derive_address(deployer_key());
    let mut rig = Rig::spawn(version, owner);
    let idx = Idx::new();
    let mut steps = Vec::new();
    let deployer = derive_address(deployer_key());
    let store = create_address(deployer, 0);
    let t = 1_700_000_000u64;
    steps.push(deposit(
        idx.next(),
        deployer,
        U256::from(10u128).pow(U256::from(18u64)),
        1,
        t,
    ));
    steps.push(msg(
        idx.next(),
        deploy(0, &store_runtime(), 1, t).build().unwrap(),
    ));
    steps.push(msg(
        idx.next(),
        deploy(1, &revert_runtime(), 1, t).build().unwrap(),
    ));
    let l1s = Address::repeat_byte(0x51);
    steps.push(msg(
        idx.next(),
        submit_retryable(l1s, store, 200_000, U256::ZERO, 2, t + 4, req_id(0xa1))
            .build()
            .unwrap(),
    ));
    let scenario = Scenario {
        name: "diag".into(),
        description: "diag".into(),
        setup: ScenarioSetup {
            l2_chain_id: L2_CHAIN_ID,
            arbos_version: version,
            genesis: None,
        },
        steps,
    };
    let _ = rig.dual.run(&scenario).expect("run");
    let latest = rig.dual.left.block(BlockId::Latest).expect("blk").number;
    // Did the deploys land? Probe store code + deployer nonce/balance each block.
    for b in 0..=latest {
        let bid = BlockId::Number(b);
        let dep_nonce = rig.dual.right.nonce(deployer, bid.clone()).unwrap_or(0);
        let dep_bal = rig
            .dual
            .right
            .balance(deployer, bid.clone())
            .unwrap_or(U256::ZERO);
        let store_code = rig
            .dual
            .right
            .code(store, bid.clone())
            .map(|c| c.len())
            .unwrap_or(0);
        eprintln!("block {b}: deployer nonce={dep_nonce} bal={dep_bal} store_codelen={store_code}");
    }
    let at = BlockId::Number(latest);
    let candidates: [(&str, Address); 11] = [
        ("aliased_sender", apply_l1_to_l2_alias(l1s)),
        (
            "excess_fee_refund",
            address!("000000000000000000000000000000000fee0001"),
        ),
        (
            "call_value_refund",
            address!("000000000000000000000000000000000bef0001"),
        ),
        ("store", store),
        ("owner/networkfee", owner),
        (
            "l1_pricer_pool",
            address!("a4b00000000000000000000000000000000000f6"),
        ),
        ("batch_poster", SEQUENCER_ALIAS),
        (
            "arbsys_0x64",
            address!("0000000000000000000000000000000000000064"),
        ),
        (
            "escrow",
            arbos::retryables::retryable_escrow_address(req_id(0xa1)),
        ),
        ("zero", Address::ZERO),
        (
            "arbos_state",
            address!("a4b05fffffffffffffffffffffffffffffffffff"),
        ),
    ];
    // eth_getProof existence probe: codeHash == keccak("") (0xc5d2..) => account
    // EXISTS (even if empty); == 0x000..0 => ABSENT. This is the only way to see
    // a zombie/tombstone (balance/nonce/codelen are identical either way).
    use arb_test_harness::rpc::JsonRpcClient;
    let left_rpc = JsonRpcClient::new(rig.dual.left.rpc_url().to_string());
    let right_rpc = JsonRpcClient::new(rig.dual.right.rpc_url().to_string());
    let block_hex = format!("0x{latest:x}");
    let exists = |rpc: &JsonRpcClient, a: Address| -> String {
        match rpc.call(
            "eth_getProof",
            serde_json::json!([format!("{a:?}"), [], block_hex]),
        ) {
            Ok(v) => v
                .get("codeHash")
                .and_then(|c| c.as_str())
                .unwrap_or("?")
                .to_string(),
            Err(e) => format!("err:{e}"),
        }
    };
    let mut any = false;
    for (name, a) in candidates {
        let lh = exists(&left_rpc, a);
        let rh = exists(&right_rpc, a);
        let lc = rig
            .dual
            .left
            .code(a, at.clone())
            .map(|c| c.len())
            .unwrap_or(0);
        let rc = rig
            .dual
            .right
            .code(a, at.clone())
            .map(|c| c.len())
            .unwrap_or(0);
        let tag = if lh != rh {
            any = true;
            "DIFF"
        } else {
            "ok  "
        };
        eprintln!(
            "{tag} v{version} {name} {a:?}: codeHash nitro={lh} arbreth={rh} (codelen {lc}/{rc})"
        );
    }
    eprintln!("v{version}: any_existence_diff={any}");
}

#[test]
#[ignore]
fn isolate_a_autoredeem_store_v6() {
    run_single(6, 'a');
}

#[test]
#[ignore]
fn isolate_b_autoredeem_revert_v6() {
    run_single(6, 'b');
}

#[test]
#[ignore]
fn isolate_c_escrow_only_v6() {
    run_single(6, 'c');
}

#[test]
#[ignore]
fn isolate_d_autoredeem_store_callvalue_v6() {
    run_single(6, 'd');
}

#[test]
#[ignore]
fn isolate_e_autoredeem_revert_callvalue_v6() {
    run_single(6, 'e');
}

#[test]
#[ignore]
fn retryable_to_contract_matches_nitro_v6() {
    assert_clean_at(6);
}

#[test]
#[ignore]
fn retryable_to_contract_matches_nitro_v9() {
    assert_clean_at(9);
}

#[test]
#[ignore]
fn retryable_to_contract_matches_nitro_v11() {
    assert_clean_at(11);
}

#[test]
#[ignore]
fn retryable_to_contract_matches_nitro_v60() {
    assert_clean_at(60);
}
