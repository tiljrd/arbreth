use alloy_evm::{eth::EthEvmContext, precompiles::PrecompilesMap};
use alloy_primitives::{address, Address};
use arb_precompiles::register_arb_precompiles;
use revm::{
    database::EmptyDB,
    handler::{EthPrecompiles, PrecompileProvider},
    primitives::hardfork::SpecId,
};

const ECRECOVER: Address = address!("0000000000000000000000000000000000000001");
const SHA256: Address = address!("0000000000000000000000000000000000000002");
const RIPEMD: Address = address!("0000000000000000000000000000000000000003");
const IDENTITY: Address = address!("0000000000000000000000000000000000000004");
const MODEXP: Address = address!("0000000000000000000000000000000000000005");
const BN_ADD: Address = address!("0000000000000000000000000000000000000006");
const BN_MUL: Address = address!("0000000000000000000000000000000000000007");
const BN_PAIR: Address = address!("0000000000000000000000000000000000000008");
const BLAKE2F: Address = address!("0000000000000000000000000000000000000009");
const KZG: Address = address!("000000000000000000000000000000000000000a");
const BLS_G1_ADD: Address = address!("000000000000000000000000000000000000000b");
const BLS_G1_MSM: Address = address!("000000000000000000000000000000000000000c");
const BLS_G2_ADD: Address = address!("000000000000000000000000000000000000000d");
const BLS_G2_MSM: Address = address!("000000000000000000000000000000000000000e");
const BLS_PAIRING: Address = address!("000000000000000000000000000000000000000f");
const BLS_MAP_FP: Address = address!("0000000000000000000000000000000000000010");
const BLS_MAP_FP2: Address = address!("0000000000000000000000000000000000000011");
const P256VERIFY: Address = address!("0000000000000000000000000000000000000100");

fn build(spec: SpecId, arbos_version: u64) -> PrecompilesMap {
    let mut map = PrecompilesMap::from(EthPrecompiles::new(spec));
    let block = arb_context::BlockCtx::new(arbos_version, 0, 0, 0, false);
    let ctx = std::sync::Arc::new(arb_context::ArbPrecompileCtx::with_block(
        std::sync::Arc::new(block),
    ));
    register_arb_precompiles(&mut map, ctx);
    map
}

fn contains(map: &PrecompilesMap, addr: &Address) -> bool {
    <PrecompilesMap as PrecompileProvider<EthEvmContext<EmptyDB>>>::contains(map, addr)
}

#[test]
fn arbos_29_excludes_bls_kzg_p256() {
    let map = build(SpecId::SHANGHAI, 29);
    for addr in [
        ECRECOVER, SHA256, RIPEMD, IDENTITY, MODEXP, BN_ADD, BN_MUL, BN_PAIR, BLAKE2F,
    ] {
        assert!(contains(&map, &addr), "expected {addr} for ArbOS 29");
    }
    for addr in [
        KZG,
        BLS_G1_ADD,
        BLS_G1_MSM,
        BLS_G2_ADD,
        BLS_G2_MSM,
        BLS_PAIRING,
        BLS_MAP_FP,
        BLS_MAP_FP2,
        P256VERIFY,
    ] {
        assert!(!contains(&map, &addr), "did not expect {addr} for ArbOS 29");
    }
}

#[test]
fn arbos_30_includes_p256_and_kzg_excludes_bls() {
    let map = build(SpecId::CANCUN, 30);
    for addr in [
        ECRECOVER, SHA256, RIPEMD, IDENTITY, MODEXP, BN_ADD, BN_MUL, BN_PAIR, BLAKE2F, KZG,
        P256VERIFY,
    ] {
        assert!(contains(&map, &addr), "expected {addr} for ArbOS 30");
    }
    for addr in [
        BLS_G1_ADD,
        BLS_G1_MSM,
        BLS_G2_ADD,
        BLS_G2_MSM,
        BLS_PAIRING,
        BLS_MAP_FP,
        BLS_MAP_FP2,
    ] {
        assert!(!contains(&map, &addr), "did not expect {addr} for ArbOS 30");
    }
}

const ARBSYS: Address = address!("0000000000000000000000000000000000000064");
const ARBINFO: Address = address!("0000000000000000000000000000000000000065");
const ARBADDRESSTABLE: Address = address!("0000000000000000000000000000000000000066");
const ARBBLS: Address = address!("0000000000000000000000000000000000000067");
const ARBFUNCTIONTABLE: Address = address!("0000000000000000000000000000000000000068");
const ARBOSTEST: Address = address!("0000000000000000000000000000000000000069");
const ARBOWNERPUBLIC: Address = address!("000000000000000000000000000000000000006b");
const ARBGASINFO: Address = address!("000000000000000000000000000000000000006c");
const ARBAGGREGATOR: Address = address!("000000000000000000000000000000000000006d");
const ARBRETRYABLETX: Address = address!("000000000000000000000000000000000000006e");
const ARBSTATISTICS: Address = address!("000000000000000000000000000000000000006f");
const ARBOWNER: Address = address!("0000000000000000000000000000000000000070");
const ARBWASM: Address = address!("0000000000000000000000000000000000000071");
const ARBWASMCACHE: Address = address!("0000000000000000000000000000000000000072");
const ARBNATIVETOKENMANAGER: Address = address!("0000000000000000000000000000000000000073");
const ARBFILTEREDTXMANAGER: Address = address!("0000000000000000000000000000000000000074");
const ARBDEBUG: Address = address!("00000000000000000000000000000000000000ff");
const ARBOSACTS: Address = address!("00000000000000000000000000000000000a4b05");
const NODE_INTERFACE: Address = address!("00000000000000000000000000000000000000c8");
const NODE_INTERFACE_DEBUG: Address = address!("00000000000000000000000000000000000000c9");

const ALWAYS_ACTIVE_ARB: [Address; 14] = [
    ARBSYS,
    ARBINFO,
    ARBADDRESSTABLE,
    ARBBLS,
    ARBFUNCTIONTABLE,
    ARBOSTEST,
    ARBOWNERPUBLIC,
    ARBGASINFO,
    ARBAGGREGATOR,
    ARBRETRYABLETX,
    ARBSTATISTICS,
    ARBOWNER,
    ARBDEBUG,
    ARBOSACTS,
];

fn build_at(arbos_version: u64) -> PrecompilesMap {
    build(
        arb_chainspec::spec_id_by_arbos_version(arbos_version),
        arbos_version,
    )
}

fn warm(map: &PrecompilesMap, addr: &Address) -> bool {
    <PrecompilesMap as PrecompileProvider<EthEvmContext<EmptyDB>>>::warm_addresses(map)
        .any(|a| a == *addr)
}

#[test]
fn arb_precompiles_activate_at_min_arbos_version() {
    for &(addr, min) in arb_primitives::arbos_versions::PRECOMPILE_MIN_ARBOS_VERSIONS {
        let before = build_at(min - 1);
        assert!(
            !contains(&before, &addr),
            "{addr} must be absent below ArbOS {min}"
        );
        assert!(
            !warm(&before, &addr),
            "{addr} must not be warm-preloaded below ArbOS {min}"
        );
        let at = build_at(min);
        assert!(contains(&at, &addr), "{addr} must be active at ArbOS {min}");
        assert!(
            warm(&at, &addr),
            "{addr} must be warm-preloaded at ArbOS {min}"
        );
    }
}

#[test]
fn arb_precompile_set_per_version() {
    for version in [1, 10, 11, 20, 29, 30, 40, 41, 50, 59, 60] {
        let map = build_at(version);
        for addr in ALWAYS_ACTIVE_ARB {
            assert!(contains(&map, &addr), "expected {addr} at ArbOS {version}");
        }
        for (addr, min) in [
            (ARBWASM, 30),
            (ARBWASMCACHE, 30),
            (ARBNATIVETOKENMANAGER, 41),
            (ARBFILTEREDTXMANAGER, 60),
        ] {
            assert_eq!(
                contains(&map, &addr),
                version >= min,
                "{addr} membership wrong at ArbOS {version}"
            );
        }
        for addr in [NODE_INTERFACE, NODE_INTERFACE_DEBUG] {
            assert!(
                !contains(&map, &addr),
                "{addr} must never be a consensus precompile"
            );
        }
    }
}

#[test]
fn arbos_50_includes_bls_and_p256() {
    let map = build(SpecId::OSAKA, 50);
    for addr in [
        ECRECOVER,
        SHA256,
        RIPEMD,
        IDENTITY,
        MODEXP,
        BN_ADD,
        BN_MUL,
        BN_PAIR,
        BLAKE2F,
        KZG,
        BLS_G1_ADD,
        BLS_G1_MSM,
        BLS_G2_ADD,
        BLS_G2_MSM,
        BLS_PAIRING,
        BLS_MAP_FP,
        BLS_MAP_FP2,
        P256VERIFY,
    ] {
        assert!(contains(&map, &addr), "expected {addr} for ArbOS 50");
    }
}
