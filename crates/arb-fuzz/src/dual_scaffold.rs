//! Shared scaffolding for the per-test dual-execution harnesses, which spawn
//! a fresh Nitro-docker + arbreth pair per scenario.

use std::sync::atomic::{AtomicU64, Ordering};

use alloy_primitives::Address;
use arb_test_harness::{
    dual_exec::DualExec,
    genesis::GenesisBuilder,
    messaging::L1Message,
    mock_l1::MockL1,
    node::{arbreth::ArbrethProcess, nitro_docker::NitroDocker, NodeStartCtx},
    scenario::ScenarioStep,
};

pub use crate::arbitrary_impls::interop::{create_address, wrap_init_code};

pub const DUAL_L2_CHAIN_ID: u64 = 412_346;
pub const DUAL_L1_CHAIN_ID: u64 = 11_155_111;

/// STORE: writes 0x2a to slot 0; payable, so it also accepts value.
pub fn store_runtime() -> Vec<u8> {
    vec![0x60, 0x2a, 0x60, 0x00, 0x55, 0x00]
}

/// REVERT: reverts with empty data.
pub fn revert_runtime() -> Vec<u8> {
    vec![0x60, 0x00, 0x60, 0x00, 0xfd]
}

/// A Nitro-docker + arbreth pair sharing one mock L1 and genesis.
pub struct Rig {
    pub dual: DualExec<NitroDocker, ArbrethProcess>,
}

impl Rig {
    pub fn spawn(version: u64, owner: Address) -> Self {
        let mock = MockL1::start(DUAL_L1_CHAIN_ID).expect("mock l1 start");
        let genesis = GenesisBuilder::new(DUAL_L2_CHAIN_ID, version)
            .with_initial_chain_owner(owner)
            .build()
            .expect("genesis build");
        let ctx = NodeStartCtx {
            binary: None,
            l2_chain_id: DUAL_L2_CHAIN_ID,
            l1_chain_id: DUAL_L1_CHAIN_ID,
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

/// Monotonic message-index allocator starting at 1.
pub struct Idx(AtomicU64);

impl Idx {
    pub fn new() -> Self {
        Self(AtomicU64::new(1))
    }
    pub fn next(&self) -> u64 {
        self.0.fetch_add(1, Ordering::SeqCst)
    }
}

impl Default for Idx {
    fn default() -> Self {
        Self::new()
    }
}

/// Wrap a message into a scenario step, threading the running message index
/// as `delayed_messages_read` (the crate convention).
pub fn msg(idx: u64, message: L1Message) -> ScenarioStep {
    ScenarioStep::Message {
        idx,
        message,
        delayed_messages_read: idx,
    }
}
