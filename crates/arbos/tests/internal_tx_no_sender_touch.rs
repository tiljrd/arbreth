//! Locks down the invariant that `apply_internal_tx_update` never touches
//! the ArbOS-system sender: internal txs end before any nonce/balance
//! accounting runs.

use std::cell::RefCell;

use alloy_primitives::{Address, U256};
use arb_test_utils::ArbosHarness;
use arbos::{
    internal_tx::{
        apply_internal_tx_update, encode_batch_posting_report, encode_batch_posting_report_v2,
        encode_start_block, InternalTxContext,
    },
    tx_processor::ARBOS_ADDRESS,
};

#[derive(Default)]
struct TouchRecorder {
    addrs: Vec<Address>,
}

impl TouchRecorder {
    fn record(&mut self, addr: Address) {
        self.addrs.push(addr);
    }
    fn contains(&self, addr: Address) -> bool {
        self.addrs.contains(&addr)
    }
}

fn run_internal_tx_and_collect_touches(arbos_version: u64, data: &[u8]) -> TouchRecorder {
    let mut h = ArbosHarness::new()
        .with_arbos_version(arbos_version)
        .with_chain_id(42_161)
        .initialize();
    let state_ptr = h.state_ptr();
    let mut arb_state = h.arbos_state();

    let ctx = InternalTxContext {
        block_number: 22_207_963,
        current_time: 1_661_956_345,
        prev_hash: alloy_primitives::B256::ZERO,
    };

    let touched = RefCell::new(TouchRecorder::default());

    let mut do_transfer = |from: Address, to: Address, _amount: U256| {
        touched.borrow_mut().record(from);
        touched.borrow_mut().record(to);
        Ok::<_, arbos::util::BalanceError>(())
    };
    let mut do_balance = |addr: Address| -> U256 {
        touched.borrow_mut().record(addr);
        U256::from(10u64).pow(U256::from(24u64))
    };

    apply_internal_tx_update(
        unsafe { &mut *state_ptr },
        data,
        &mut arb_state,
        &ctx,
        &mut do_transfer,
        &mut do_balance,
    )
    .expect("internal tx update");
    touched.into_inner()
}

#[test]
fn start_block_does_not_touch_arbos_sender_v6() {
    let data = encode_start_block(U256::from(1_000_000_000u64), 17_000_000, 1_000, 12);
    let touches = run_internal_tx_and_collect_touches(6, &data);
    assert!(
        !touches.contains(ARBOS_ADDRESS),
        "internal-tx start_block touched ARBOS_ADDRESS via transfer/balance fn"
    );
}

#[test]
fn start_block_does_not_touch_arbos_sender_v10() {
    let data = encode_start_block(U256::from(1_000_000_000u64), 17_000_000, 1_000, 12);
    let touches = run_internal_tx_and_collect_touches(10, &data);
    assert!(!touches.contains(ARBOS_ADDRESS));
}

#[test]
fn batch_posting_report_does_not_touch_arbos_sender_v6() {
    let data = encode_batch_posting_report(
        1_661_956_300,
        Address::repeat_byte(0xAB),
        7,
        21_000,
        U256::from(1_000_000_000u64),
    );
    let touches = run_internal_tx_and_collect_touches(6, &data);
    assert!(
        !touches.contains(ARBOS_ADDRESS),
        "internal-tx batch-posting-report touched ARBOS_ADDRESS"
    );
}

#[test]
fn batch_posting_report_v2_does_not_touch_arbos_sender_v60() {
    let data = encode_batch_posting_report_v2(
        1_700_000_000,
        Address::repeat_byte(0xCD),
        15,
        320,
        180,
        2_400,
        U256::from(1_500_000_000u64),
    );
    let touches = run_internal_tx_and_collect_touches(60, &data);
    assert!(!touches.contains(ARBOS_ADDRESS));
}
