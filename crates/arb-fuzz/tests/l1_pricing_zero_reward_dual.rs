//! Zero-amount L1-pricing reward payouts must reproduce the reference's
//! EIP-161 touch on the reward recipient: a present-empty recipient leaf is
//! pruned by the zero-value transfer a batch report performs when no units
//! were allocated. Compared at ArbOS v6 (pre-v10 pool path) and v10
//! (fees-available path).
//!
//! Run (needs Docker + release arb-reth):
//!   ARB_SPEC_BINARY=$(pwd)/target/release/arb-reth \
//!     cargo test -p arb-fuzz --test l1_pricing_zero_reward_dual --release -- --ignored --nocapture

use std::sync::Mutex;

use alloy_primitives::{Address, Bytes, B256, U256};
use arb_fuzz::{
    dual_scaffold::{Idx, Rig, DUAL_L2_CHAIN_ID},
    scaffolding::selector4,
};
use arb_test_harness::messaging::L1Message;

fn step(idx: u64, delayed_read: u64, message: L1Message) -> ScenarioStep {
    ScenarioStep::Message {
        idx,
        message,
        delayed_messages_read: delayed_read,
    }
}
use alloy_eips::eip2718::Encodable2718;
use alloy_primitives::keccak256;
use arb_test_harness::{
    messaging::{
        encoding::request_id_from_seq,
        kinds::KIND_SUBMIT_RETRYABLE,
        signed_tx::{derive_address, L2TxKind, SignedL2TxBuilder},
        BatchBuilder, BatchPostingVariant, DepositBuilder, MessageBuilder, RetryableSubmitBuilder,
    },
    node::{BlockId, ExecutionNode},
    scenario::{Scenario, ScenarioSetup, ScenarioStep},
};
use arbos::{
    parse_l2::{parse_l2_transactions, parsed_tx_to_signed},
    retryables::retryable_escrow_address,
};

static SERIAL: Mutex<()> = Mutex::new(());

const L1_BASE_FEE: u64 = 30_000_000_000;
const SEQUENCER_ALIAS: Address = Address::new([
    0xa4, 0xb0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x73, 0x65, 0x71, 0x75, 0x65, 0x6e, 0x63, 0x65, 0x72,
]);
const FUNDER: Address = Address::new([0xa9; 20]);
const ARBOWNER: Address = Address::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0x70,
]);
const BATCH_POSTER: Address = Address::new([0xbb; 20]);
const RETRY_TO: Address = Address::new([
    0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0xcc, 0x01,
]);
const L1_SENDER: Address = Address::new([0xd1; 20]);
const T0: u64 = 1_700_000_000;

fn owner_key() -> B256 {
    B256::repeat_byte(0x42)
}

fn owner_call(nonce: u64, calldata: Vec<u8>, timestamp: u64) -> SignedL2TxBuilder {
    SignedL2TxBuilder {
        chain_id: DUAL_L2_CHAIN_ID,
        nonce,
        to: Some(ARBOWNER),
        value: U256::ZERO,
        data: Bytes::from(calldata),
        // Ample headroom over the poster-cost component so admission cannot
        // silently drop the tx at a 30 gwei declared L1 base fee.
        gas_limit: 30_000_000,
        gas_price: 1_000_000_000,
        max_fee_per_gas: 1_000_000_000,
        max_priority_fee_per_gas: 0,
        access_list: Vec::new(),
        authorization_list: Vec::new(),
        kind: L2TxKind::Eip1559,
        signing_key: owner_key(),
        l1_block_number: 1,
        timestamp,
        request_id: None,
        sender: SEQUENCER_ALIAS,
        base_fee_l1: L1_BASE_FEE,
    }
}

fn batch_report(seq: u64, batch_number: u64, batch_timestamp: u64, timestamp: u64) -> BatchBuilder {
    BatchBuilder {
        batch_poster: BATCH_POSTER,
        batch_timestamp,
        data_hash: B256::repeat_byte(0x77),
        batch_number,
        l1_base_fee: U256::from(L1_BASE_FEE),
        variant: BatchPostingVariant::V1,
        l1_block_number: 1,
        timestamp,
        request_seq: seq,
        base_fee_l1: L1_BASE_FEE,
        batch_gas_cost: 100_000,
    }
}

fn set_reward_recipient_calldata(recipient: Address) -> Vec<u8> {
    let mut out = selector4("setL1PricingRewardRecipient(address)").to_vec();
    let mut w = [0u8; 32];
    w[12..].copy_from_slice(recipient.as_slice());
    out.extend_from_slice(&w);
    out
}

fn assert_clean_at(version: u64) {
    let _serial = SERIAL.lock().unwrap_or_else(|e| e.into_inner());
    let owner = derive_address(owner_key());
    let mut rig = Rig::spawn(version, owner);
    let idx = Idx::new();
    let mut steps = Vec::new();

    // Fund the owner (schedules the recipient) via deposit.
    let dep = idx.next();
    steps.push(step(
        dep,
        1,
        DepositBuilder {
            from: FUNDER,
            to: owner,
            amount: U256::from(10u128).pow(U256::from(19u64)),
            l1_block_number: 1,
            timestamp: T0,
            request_seq: dep,
            base_fee_l1: L1_BASE_FEE,
        }
        .build()
        .expect("deposit"),
    ));

    // Zero-callvalue retryable with auto-redeem: its escrow account survives
    // the block as a present-empty leaf (pre-Stylus zombie).
    let sub_idx = idx.next();
    let submit = RetryableSubmitBuilder {
        l1_sender: L1_SENDER,
        to: RETRY_TO,
        l2_call_value: U256::ZERO,
        deposit_value: U256::from(10u128).pow(U256::from(18u64)),
        max_submission_fee: U256::from(10u128).pow(U256::from(15u64)),
        excess_fee_refund_address: owner,
        call_value_refund_address: owner,
        gas_limit: 1_000_000,
        max_fee_per_gas: U256::from(1_000_000_000u64),
        data: Bytes::new(),
        l1_block_number: 1,
        timestamp: T0,
        request_id: Some(request_id_from_seq(sub_idx)),
    };
    // Derive the ticket id through the production message parser so the
    // escrow address matches what both nodes compute; the receipt lookup
    // below fails loudly if this drifts.
    let body = submit.encode_body().expect("encode submit body");
    let parsed = parse_l2_transactions(
        KIND_SUBMIT_RETRYABLE,
        submit.aliased_sender(),
        &body,
        Some(request_id_from_seq(sub_idx)),
        Some(U256::ZERO),
        DUAL_L2_CHAIN_ID,
    )
    .expect("parse submit message");
    let submit_tx =
        parsed_tx_to_signed(&parsed[0], DUAL_L2_CHAIN_ID).expect("submit tx from parsed");
    let ticket_id = keccak256(submit_tx.encoded_2718());
    let escrow = retryable_escrow_address(ticket_id);
    steps.push(step(sub_idx, 2, submit.build().expect("submit build")));

    // Report #1 establishes lastUpdateTime and pays the accrued reward to the
    // default recipient, while the escrow leaf stays empty.
    let r1 = idx.next();
    steps.push(step(
        r1,
        3,
        batch_report(r1, 0, T0, T0).build().expect("report 1"),
    ));

    // Only now point the payout at the present-empty escrow leaf; report #2
    // reuses the same batch timestamp, so zero units are allocated and the
    // payout is a zero-amount transfer to the still-empty leaf.
    let set_idx = idx.next();
    steps.push(step(
        set_idx,
        3,
        owner_call(0, set_reward_recipient_calldata(escrow), T0)
            .build()
            .expect("owner call"),
    ));
    steps.push(ScenarioStep::AdvanceTime { seconds: 10 });
    let r2 = idx.next();
    steps.push(step(
        r2,
        4,
        batch_report(r2, 1, T0, T0 + 10).build().expect("report 2"),
    ));

    let scenario = Scenario {
        name: format!("l1_pricing_zero_reward_v{version}"),
        description: format!("zero-amount reward payout at ArbOS v{version}"),
        setup: ScenarioSetup {
            l2_chain_id: DUAL_L2_CHAIN_ID,
            arbos_version: version,
            genesis: None,
        },
        steps,
    };

    let report = rig.dual.run(&scenario).expect("run scenario");

    // Setup guards: the submit must have landed under the derived hash (else
    // the escrow address is wrong and the scenario proves nothing), and the
    // owner's recipient call must have executed.
    let receipt = rig.dual.right.receipt(ticket_id).expect("submit receipt");
    assert_eq!(receipt.status, 1, "retryable submit failed");
    let at = BlockId::Latest;
    let owner_nonce = rig.dual.right.nonce(owner, at).expect("owner nonce");
    assert_eq!(owner_nonce, 1, "recipient-setting call did not execute");

    assert!(
        report.is_clean(),
        "ArbOS v{version} zero-reward payout diverged from Nitro:\n\
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
fn zero_reward_payout_matches_nitro_v6() {
    assert_clean_at(6);
}

#[test]
#[ignore]
fn zero_reward_payout_matches_nitro_v10() {
    assert_clean_at(10);
}
