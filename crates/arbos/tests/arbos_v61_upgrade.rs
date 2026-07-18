use arb_test_utils::ArbosHarness;

#[test]
fn upgrade_v60_to_v61_propagates_version() {
    let mut h = ArbosHarness::new().with_arbos_version(60).initialize();
    let state_ptr = h.state_ptr();
    let mut arbos = h.arbos_state();
    let b = unsafe { &mut *state_ptr };

    assert_eq!(arbos.arbos_version, 60);
    arbos
        .upgrade_arbos_version(b, 61, false)
        .expect("upgrade 60 -> 61");

    // MultiGasRefundFix (v61) makes no ArbOS state changes at the upgrade step; it
    // only advances the version, which must propagate to every subsystem.
    assert_eq!(arbos.arbos_version, 61);
    assert_eq!(arbos.l1_pricing_state.arbos_version, 61);
    assert_eq!(arbos.l2_pricing_state.arbos_version, 61);
    assert_eq!(arbos.retryable_state.arbos_version, 61);
    assert_eq!(arbos.programs.arbos_version, 61);
}

#[test]
fn upgrade_beyond_supported_version_is_rejected() {
    let mut h = ArbosHarness::new().with_arbos_version(61).initialize();
    let state_ptr = h.state_ptr();
    let mut arbos = h.arbos_state();
    let b = unsafe { &mut *state_ptr };

    assert_eq!(arbos.max_arbos_version_supported, 61);
    // 62 is not implemented: the cascade must reject it rather than advancing.
    assert!(arbos.upgrade_arbos_version(b, 62, false).is_err());
    assert_eq!(arbos.arbos_version, 61);
}
