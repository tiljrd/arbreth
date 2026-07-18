use alloy_primitives::U256;
use arb_primitives::multigas::{ResourceKind, NUM_RESOURCE_KIND};
use arb_test_utils::ArbosHarness;

const ARBOS_V30: u64 = 30;
const ARBOS_V60: u64 = 60;

fn weights(pairs: &[(ResourceKind, u64)]) -> [u64; NUM_RESOURCE_KIND] {
    let mut out = [0u64; NUM_RESOURCE_KIND];
    for &(kind, w) in pairs {
        out[kind as usize] = w;
    }
    out
}

#[test]
fn legacy_pricing_model_steady_state_and_escalation() {
    let mut h = ArbosHarness::new()
        .with_arbos_version(ARBOS_V30)
        .initialize();
    let state_ptr = h.state_ptr();
    let p = h.l2_pricing_state();
    let b = unsafe { &mut *state_ptr };

    let min_price = p.min_base_fee_wei(b).unwrap();
    let limit = p.speed_limit_per_second(b).unwrap();
    assert_eq!(p.base_fee_wei(b).unwrap(), min_price);

    for seconds in 0u64..4 {
        let prev = p.gas_backlog(b).unwrap();
        p.set_gas_backlog(b, prev.saturating_add(seconds.saturating_mul(limit)))
            .unwrap();
        p.update_pricing_model(b, seconds, ARBOS_V30).unwrap();
        assert_eq!(p.base_fee_wei(b).unwrap(), min_price);
    }

    let mut last = p.base_fee_wei(b).unwrap();
    let mut escalated = false;
    for _ in 0..200 {
        let prev = p.gas_backlog(b).unwrap();
        p.set_gas_backlog(b, prev.saturating_add(8 * limit))
            .unwrap();
        p.update_pricing_model(b, 1, ARBOS_V30).unwrap();
        let new_price = p.base_fee_wei(b).unwrap();
        assert!(new_price >= last);
        if new_price > last {
            escalated = true;
            break;
        }
        last = new_price;
    }
    assert!(escalated);

    let baseline = p.base_fee_wei(b).unwrap();
    p.set_gas_backlog(b, limit.saturating_mul(1000)).unwrap();
    p.update_pricing_model(b, 0, ARBOS_V30).unwrap();
    p.update_pricing_model(b, 1, ARBOS_V30).unwrap();
    assert!(p.base_fee_wei(b).unwrap() > baseline);
}

#[test]
fn gas_constraints_add_open_clear() {
    let mut h = ArbosHarness::new()
        .with_arbos_version(ARBOS_V60)
        .initialize();
    let state_ptr = h.state_ptr();
    let p = h.l2_pricing_state();
    let b = unsafe { &mut *state_ptr };

    assert_eq!(p.gas_constraints_length(b).unwrap(), 0);

    const N: u64 = 10;
    for i in 0..N {
        p.add_gas_constraint(b, 100 * i + 1, 100 * i + 2, 100 * i + 3)
            .unwrap();
    }
    assert_eq!(p.gas_constraints_length(b).unwrap(), N);

    for i in 0..N {
        let c = p.open_gas_constraint_at(i);
        assert_eq!(c.target(b).unwrap(), 100 * i + 1);
        assert_eq!(c.adjustment_window(b).unwrap(), 100 * i + 2);
        assert_eq!(c.backlog(b).unwrap(), 100 * i + 3);
    }

    p.clear_gas_constraints(b).unwrap();
    assert_eq!(p.gas_constraints_length(b).unwrap(), 0);
}

#[test]
fn multi_gas_constraints_add_open_clear() {
    let mut h = ArbosHarness::new()
        .with_arbos_version(ARBOS_V60)
        .initialize();
    let state_ptr = h.state_ptr();
    let p = h.l2_pricing_state();
    let b = unsafe { &mut *state_ptr };

    assert_eq!(p.multi_gas_constraints_length(b).unwrap(), 0);

    const N: u64 = 5;
    for i in 0..N {
        let w = weights(&[
            (ResourceKind::Computation, 10 + i),
            (ResourceKind::StorageAccessRead, 20 + i),
        ]);
        p.add_multi_gas_constraint(b, 100 * i + 1, (100 * i + 2) as u32, 100 * i + 3, &w)
            .unwrap();
    }

    assert_eq!(p.multi_gas_constraints_length(b).unwrap(), N);

    for i in 0..N {
        let c = p.open_multi_gas_constraint_at(i);
        assert_eq!(c.target(b).unwrap(), 100 * i + 1);
        assert_eq!(c.adjustment_window(b).unwrap(), (100 * i + 2) as u32);
        assert_eq!(c.backlog(b).unwrap(), 100 * i + 3);
        assert_eq!(
            c.resource_weight(b, ResourceKind::Computation).unwrap(),
            10 + i
        );
        assert_eq!(
            c.resource_weight(b, ResourceKind::StorageAccessRead)
                .unwrap(),
            20 + i
        );
    }

    p.clear_multi_gas_constraints(b).unwrap();
    assert_eq!(p.multi_gas_constraints_length(b).unwrap(), 0);
}

#[derive(Clone, Copy)]
enum GasModelSetup {
    Legacy,
    Single,
    Multi,
}

#[test]
fn multi_gas_refund_applies_truth_table() {
    fn refund_applies(version: u64, model: GasModelSetup) -> bool {
        let mut h = ArbosHarness::new().with_arbos_version(version).initialize();
        let sp = h.state_ptr();
        let p = h.l2_pricing_state();
        let b = unsafe { &mut *sp };
        match model {
            GasModelSetup::Legacy => {}
            GasModelSetup::Single => {
                p.add_gas_constraint(b, 100, 200, 300).unwrap();
            }
            GasModelSetup::Multi => {
                let w = weights(&[(ResourceKind::Computation, 10)]);
                p.add_multi_gas_constraint(b, 100, 200u32, 300, &w).unwrap();
            }
        }
        p.multi_gas_refund_applies(b).unwrap()
    }

    use GasModelSetup::{Legacy, Multi, Single};

    // Below MultiGasConstraints (v60) the refund never applies, regardless of
    // model. Arbitrum One is at v40 today, so it is unaffected.
    for v in [30, 40, 50] {
        for m in [Legacy, Single, Multi] {
            assert!(
                !refund_applies(v, m),
                "v{v}: refund must not apply below v60"
            );
        }
    }

    // At v60 the refund applies for every model, preserving pre-fix behavior.
    // Arbitrum Sepolia is at v60 today.
    for m in [Legacy, Single, Multi] {
        assert!(refund_applies(60, m), "v60: refund applies for every model");
    }

    // From MultiGasRefundFix (v61) the refund applies only on MultiGasConstraints
    // chains; single-constraint and legacy chains (e.g. Robinhood) skip it.
    assert!(!refund_applies(61, Legacy), "v61 legacy: refund suppressed");
    assert!(!refund_applies(61, Single), "v61 single: refund suppressed");
    assert!(refund_applies(61, Multi), "v61 multi: refund applies");
}

#[test]
fn multi_gas_base_fee_source_switches_at_v61() {
    let block_base_fee = U256::from(999u64);

    // v60: the single-dimensional resource is valued at the stored base fee; the
    // block base fee argument is ignored.
    let mut h = ArbosHarness::new()
        .with_arbos_version(ARBOS_V60)
        .initialize();
    let sp = h.state_ptr();
    let p = h.l2_pricing_state();
    let b = unsafe { &mut *sp };
    let stored = p.base_fee_wei(b).unwrap();
    assert_ne!(stored, block_base_fee);
    let fees = p
        .get_multi_gas_base_fee_per_resource(b, block_base_fee)
        .unwrap();
    assert_eq!(fees[ResourceKind::SingleDim as usize], stored);

    // v61 (MultiGasRefundFix): the single-dimensional resource uses the block
    // base fee passed by the caller.
    let mut h = ArbosHarness::new().with_arbos_version(61).initialize();
    let sp = h.state_ptr();
    let p = h.l2_pricing_state();
    let b = unsafe { &mut *sp };
    let fees = p
        .get_multi_gas_base_fee_per_resource(b, block_base_fee)
        .unwrap();
    assert_eq!(fees[ResourceKind::SingleDim as usize], block_base_fee);
}

#[test]
fn multi_gas_constraints_exponents() {
    let mut h = ArbosHarness::new()
        .with_arbos_version(ARBOS_V60)
        .initialize();
    let state_ptr = h.state_ptr();
    let p = h.l2_pricing_state();
    let b = unsafe { &mut *state_ptr };

    p.add_multi_gas_constraint(b, 100, 10, 100, &weights(&[(ResourceKind::Computation, 1)]))
        .unwrap();
    p.add_multi_gas_constraint(
        b,
        40,
        20,
        200,
        &weights(&[(ResourceKind::StorageAccessRead, 2)]),
    )
    .unwrap();

    let exps = p.calc_multi_gas_constraints_exponents(b).unwrap();
    assert_eq!(exps[ResourceKind::Computation as usize], 1000);
    assert_eq!(exps[ResourceKind::StorageAccessRead as usize], 2500);
}

#[test]
fn initial_base_fee_equals_min() {
    let mut h = ArbosHarness::new().initialize();
    let state_ptr = h.state_ptr();
    let p = h.l2_pricing_state();
    let b = unsafe { &mut *state_ptr };
    let base = p.base_fee_wei(b).unwrap();
    let min = p.min_base_fee_wei(b).unwrap();
    assert_eq!(base, min);
    assert!(base > U256::ZERO);
}
