mod common;

use alloy_primitives::{address, Address, B256, U256};
use arb_precompiles::create_arbsys_precompile;
use common::{calldata, decode_address, decode_u256, word_address, word_u256, PrecompileTest};
use revm::precompile::PrecompileError;

const ARBOS_V5: u64 = 5;
const ARBOS_V6: u64 = 6;
const ARBOS_V11: u64 = 11;
const ARBOS_V30: u64 = 30;

fn arbsys(
    ctx: std::sync::Arc<arb_context::ArbPrecompileCtx>,
) -> alloy_evm::precompiles::DynPrecompile {
    create_arbsys_precompile(ctx)
}

#[test]
fn arb_block_number_returns_l2_block() {
    let run = PrecompileTest::new()
        .arbos_version(ARBOS_V30)
        .block_number(98_765)
        .arbos_state()
        .call(arbsys, &calldata("arbBlockNumber()", &[]));
    assert_eq!(decode_u256(run.output()), U256::from(98_765));
}

#[test]
fn arb_chain_id_returns_configured_chain_id() {
    let run = PrecompileTest::new()
        .arbos_version(ARBOS_V30)
        .chain_id(421_614)
        .arbos_state()
        .call(arbsys, &calldata("arbChainID()", &[]));
    assert_eq!(decode_u256(run.output()), U256::from(421_614));
}

#[test]
fn arbos_version_returns_55_plus_raw() {
    let run = PrecompileTest::new()
        .arbos_version(ARBOS_V30)
        .arbos_state()
        .call(arbsys, &calldata("arbOSVersion()", &[]));
    assert_eq!(decode_u256(run.output()), U256::from(55 + ARBOS_V30));
}

#[test]
fn get_storage_gas_available_returns_zero() {
    let run = PrecompileTest::new()
        .arbos_version(ARBOS_V30)
        .arbos_state()
        .call(arbsys, &calldata("getStorageGasAvailable()", &[]));
    assert_eq!(decode_u256(run.output()), U256::ZERO);
}

#[test]
fn is_top_level_call_at_depth_one() {
    let run = PrecompileTest::new()
        .arbos_version(ARBOS_V30)
        .evm_depth(1)
        .arbos_state()
        .call(arbsys, &calldata("isTopLevelCall()", &[]));
    assert_eq!(decode_u256(run.output()), U256::from(1));
}

#[test]
fn is_top_level_call_at_depth_three() {
    let run = PrecompileTest::new()
        .arbos_version(ARBOS_V30)
        .evm_depth(3)
        .arbos_state()
        .call(arbsys, &calldata("isTopLevelCall()", &[]));
    assert_eq!(decode_u256(run.output()), U256::ZERO);
}

#[test]
fn map_l1_sender_low_byte_carry() {
    let l1: Address = address!("0123456789abcdef0123456789abcdef01234567");
    let expected: Address = address!("1234456789abcdef0123456789abcdef01235678");
    let run = PrecompileTest::new()
        .arbos_version(ARBOS_V30)
        .arbos_state()
        .call(
            arbsys,
            &calldata(
                "mapL1SenderContractAddressToL2Alias(address,address)",
                &[word_address(l1), word_address(Address::ZERO)],
            ),
        );
    assert_eq!(decode_address(run.output()), expected);
}

#[test]
fn map_l1_sender_with_carry_propagation() {
    let l1: Address = address!("00ef000000000000000000000000000000000000");
    let expected: Address = address!("1200000000000000000000000000000000001111");
    let run = PrecompileTest::new()
        .arbos_version(ARBOS_V30)
        .arbos_state()
        .call(
            arbsys,
            &calldata(
                "mapL1SenderContractAddressToL2Alias(address,address)",
                &[word_address(l1), word_address(Address::ZERO)],
            ),
        );
    assert_eq!(decode_address(run.output()), expected);
}

#[test]
fn was_aliased_returns_false_when_tx_not_aliased() {
    let run = PrecompileTest::new()
        .arbos_version(ARBOS_V30)
        .evm_depth(2)
        .tx_is_aliased(false)
        .arbos_state()
        .call(arbsys, &calldata("wasMyCallersAddressAliased()", &[]));
    assert_eq!(decode_u256(run.output()), U256::ZERO);
}

#[test]
fn was_aliased_returns_true_when_top_level_aliased() {
    let run = PrecompileTest::new()
        .arbos_version(ARBOS_V30)
        .evm_depth(2)
        .tx_is_aliased(true)
        .caller(address!("00000000000000000000000000000000000000aa"))
        .arbos_state()
        .call(arbsys, &calldata("wasMyCallersAddressAliased()", &[]));
    assert_eq!(decode_u256(run.output()), U256::from(1));
}

// ── v6 isTopLevel branch: arbos<6 uses `depth==2`, arbos>=6 uses the
// `origin == caller_at_depth(depth-1)` chain check at depth>2.
// Locks down the per-version semantics flagged as the highest-risk v6 change.

#[test]
fn was_aliased_v5_returns_false_at_depth_three() {
    // v<6: topLevel iff depth==2; at depth 3 the v<6 path returns false even
    // when the inner caller chain would qualify under v6 rules.
    let origin: Address = address!("00000000000000000000000000000000000000aa");
    let intermediate: Address = address!("00000000000000000000000000000000000000bb");
    let run = PrecompileTest::new()
        .arbos_version(ARBOS_V5)
        .evm_depth(3)
        .tx_is_aliased(true)
        .caller(origin)
        .caller_stack(vec![origin, origin, intermediate])
        .arbos_state()
        .call(arbsys, &calldata("wasMyCallersAddressAliased()", &[]));
    assert_eq!(decode_u256(run.output()), U256::ZERO);
}

#[test]
fn was_aliased_v6_returns_true_at_depth_three_when_caller_chain_is_origin() {
    // v>=6: aliased iff `origin == caller_at_depth(depth-1)` (= caller of the
    // depth-2 frame). Models EOA->A->ArbSys where A self-calls before invoking
    // the precompile; the depth-2 frame's caller is still the EOA.
    let origin: Address = address!("00000000000000000000000000000000000000aa");
    let inner: Address = address!("00000000000000000000000000000000000000bb");
    let run = PrecompileTest::new()
        .arbos_version(ARBOS_V6)
        .evm_depth(3)
        .tx_is_aliased(true)
        .caller(origin)
        .caller_stack(vec![origin, origin, inner])
        .arbos_state()
        .call(arbsys, &calldata("wasMyCallersAddressAliased()", &[]));
    assert_eq!(decode_u256(run.output()), U256::from(1));
}

#[test]
fn was_aliased_v6_returns_false_at_depth_three_when_caller_chain_breaks() {
    // v>=6: at depth 3 with caller_at_depth(2) != origin, topLevel=false.
    // Models EOA->A->B->ArbSys where B's caller (A) is not the EOA.
    let origin: Address = address!("00000000000000000000000000000000000000aa");
    let mid: Address = address!("00000000000000000000000000000000000000bb");
    let inner: Address = address!("00000000000000000000000000000000000000cc");
    let run = PrecompileTest::new()
        .arbos_version(ARBOS_V6)
        .evm_depth(3)
        .tx_is_aliased(true)
        .caller(origin)
        .caller_stack(vec![origin, mid, inner])
        .arbos_state()
        .call(arbsys, &calldata("wasMyCallersAddressAliased()", &[]));
    assert_eq!(decode_u256(run.output()), U256::ZERO);
}

#[test]
fn caller_without_alias_v6_unaliases_caller_when_top_level() {
    use arbos::util::inverse_remap_l1_address;
    // v>=6 at depth=3, caller_at_depth(2) == origin → aliased=true → unalias the
    // depth-2 caller (== `caller_at_depth(depth-1)`).
    let origin: Address = address!("00000000000000000000000000000000000000aa");
    let depth_minus_one: Address = address!("11110000000000000000000000000000000022aa");
    let run = PrecompileTest::new()
        .arbos_version(ARBOS_V6)
        .evm_depth(3)
        .tx_is_aliased(true)
        .caller(origin)
        // caller_at_depth(depth-1=2) is the second entry → must equal origin so
        // topLevel=true; `address` returned is itself `caller_at_depth(depth-1)`.
        .caller_stack(vec![origin, origin, depth_minus_one])
        .arbos_state()
        .call(arbsys, &calldata("myCallersAddressWithoutAliasing()", &[]));
    // We push `origin` at index 1 (=caller_at_depth(2)) which is what gets
    // unaliased and returned. inverse_remap_l1_address(origin) yields the
    // pre-aliased L1 address.
    assert_eq!(
        decode_address(run.output()),
        inverse_remap_l1_address(origin)
    );
}

#[test]
fn caller_without_alias_v6_returns_caller_unchanged_when_not_top_level() {
    // v>=6 at depth=3, caller_at_depth(2) != origin → aliased=false → return
    // the depth-2 caller verbatim (no unalias).
    let origin: Address = address!("00000000000000000000000000000000000000aa");
    let mid: Address = address!("00000000000000000000000000000000000000bb");
    let inner: Address = address!("00000000000000000000000000000000000000cc");
    let run = PrecompileTest::new()
        .arbos_version(ARBOS_V6)
        .evm_depth(3)
        .tx_is_aliased(true)
        .caller(origin)
        .caller_stack(vec![origin, mid, inner])
        .arbos_state()
        .call(arbsys, &calldata("myCallersAddressWithoutAliasing()", &[]));
    // depth-1=2 → caller_at_depth(2)=mid; not topLevel so no unalias.
    assert_eq!(decode_address(run.output()), mid);
}

#[test]
fn arb_block_hash_returns_cached_hash_for_recent_block() {
    let target_hash = B256::from_slice(&[0x42; 32]);
    let run = PrecompileTest::new()
        .arbos_version(ARBOS_V30)
        .block_number(100)
        .arbos_state()
        .cache_l2_block_hash(99, target_hash)
        .call(
            arbsys,
            &calldata("arbBlockHash(uint256)", &[word_u256(U256::from(99))]),
        );
    let returned = B256::from(decode_u256(run.output()).to_be_bytes::<32>());
    assert_eq!(returned, target_hash);
}

#[test]
fn arb_block_hash_reverts_for_future_block_arbos11() {
    let run = PrecompileTest::new()
        .arbos_version(ARBOS_V11)
        .block_number(100)
        .arbos_state()
        .call(
            arbsys,
            &calldata("arbBlockHash(uint256)", &[word_u256(U256::from(100))]),
        );
    let out = run.assert_ok();
    assert!(out.reverted);
    assert_eq!(&out.bytes[..4], &[0xd5, 0xdc, 0x64, 0x2d]);
}

#[test]
fn arb_block_hash_reverts_for_too_old_block_arbos11() {
    let run = PrecompileTest::new()
        .arbos_version(ARBOS_V11)
        .block_number(1000)
        .arbos_state()
        .call(
            arbsys,
            &calldata("arbBlockHash(uint256)", &[word_u256(U256::from(500))]),
        );
    let out = run.assert_ok();
    assert!(out.reverted);
}

#[test]
fn arb_block_hash_fatal_for_uncached_in_range_block() {
    let run = PrecompileTest::new()
        .arbos_version(ARBOS_V30)
        .block_number(100)
        .arbos_state()
        .call(
            arbsys,
            &calldata("arbBlockHash(uint256)", &[word_u256(U256::from(99))]),
        );
    assert!(matches!(run.assert_err(), PrecompileError::Fatal(_)));
}
