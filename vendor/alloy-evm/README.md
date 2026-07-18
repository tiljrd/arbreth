# Vendored alloy-evm 0.37.1

Vendored copy of the crates.io `alloy-evm 0.37.1` release, applied workspace-wide
via `[patch.crates-io]` in the root `Cargo.toml`.

## Why

`BlockExecutorFactory::Executor` is a GAT that must implement `BlockExecutor`
for every `DB: StateDB`, where upstream `StateDB` is a blanket alias over
`Database + DatabaseCommit`. The ArbOS end-of-transaction machinery (system
storage access, state overlay retouches, EIP-161 zombie handling) operates on
the concrete `revm::database::State` transition layer, which cannot be
expressed through `Database + DatabaseCommit` alone — so the executor could
not satisfy the universal bound against the upstream trait.

## The patch

A single change in `src/block/state.rs`: `StateDB` gains an `Inner` associated
type and an `as_state_mut()` accessor, implemented for `State<D>` and `&mut T`.
This keeps the GAT universal while giving executors typed access to the
`State` wrapper. reth never names `StateDB` and instantiates executors
exclusively with `&mut State<_>`, so the patch is transparent to it.

## Maintenance

On every reth/alloy-evm bump: re-download the matching `alloy-evm` release,
re-apply the `state.rs` diff (or drop the vendor entirely if upstream grows an
equivalent seam), and verify with `cargo check --workspace --all-targets`.

---

# alloy-evm

EVM interface.

This crate contains constants, types, and functions for interacting with the Ethereum Virtual Machine (EVM). 
It is compatible with the types from the [alloy](https://crates.io/crates/alloy) ecosystem and comes with batteries included for [revm](https://crates.io/crates/revm)

