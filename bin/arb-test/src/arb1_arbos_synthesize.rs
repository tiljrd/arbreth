//! Synthesize the migration's add-on state — the ArbOS-system account at
//! 0xA4B05Fff..., per-precompile code markers, per-retryable escrow accounts,
//! and expired-retryable beneficiary credits — as JSONL lines to concatenate
//! with the `arb1-state-convert` output before `arb-reth init-state`.
//!
//! Mirrors Nitro's `arbos/arbosState/initialize.go::InitializeArbosInDatabase`
//! by driving our `bootstrap` + `chain_owners.add` + `address_table.register`
//! + `initialize_retryables` against an in-memory `State<EmptyDb>`, then
//! reading the resulting `0xA4B05Fff...` storage out of the cache.

use std::{
    collections::HashSet,
    fs::{File, OpenOptions},
    io::{BufRead, BufReader, BufWriter, Write},
    path::PathBuf,
};

use alloy_primitives::{address, hex, Address, Bytes, B256, U256};
use anyhow::{anyhow, bail, Context, Result};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use arb_storage::{set_account_code, ARBOS_STATE_ADDRESS};
use arb_test_utils::harness::ArbosHarness;
use arbos::arbos_state::initialize::{initialize_retryables, InitRetryableData};

const ARB1_GENESIS_BLOCK_NUM: u64 = 22_207_818;
// Hex of `json.Marshal(params.ChainConfig)` for arb1, captured from
// nitro/cmd/chaininfo/arbitrum_chain_info.json via a Go helper. 554 bytes.
const ARB1_CHAIN_CONFIG_HEX: &str = include_str!("arb1_data/chain_config.hex");

// Arb1 v6 migration constants.
const ARB1_CHAIN_ID: u64 = 42_161;
const ARB1_ARBOS_VERSION: u64 = 6;
const ARB1_TIMESTAMP: u64 = 1_661_960_726;
const ARB1_CHAIN_OWNER: Address = address!("d345e41ae2cb00311956aa7109fc801ae8c81a52");
// Nitro's default; tunable via `--initial-l1-base-fee-wei` if the canonical
// migration encoded a different value into its Type-11 init message.
const DEFAULT_L1_INITIAL_BASE_FEE_WEI: u64 = 50_000_000_000;

// The version-0 precompile addresses that Nitro's `InitializeArbosState`
// stamps with `[0xFE]` code (arbos/arbosState/arbosstate.go:234-238). Classic
// did not — so the user-account converter sees them as empty and skips them
// — and we must emit them here.
const GENESIS_PRECOMPILE_ADDRESSES: [Address; 14] = [
    address!("0000000000000000000000000000000000000064"), // ArbSys
    address!("0000000000000000000000000000000000000065"), // ArbInfo
    address!("0000000000000000000000000000000000000066"), // ArbAddressTable
    address!("0000000000000000000000000000000000000067"), // ArbBLS
    address!("0000000000000000000000000000000000000068"), // ArbFunctionTable
    address!("0000000000000000000000000000000000000069"), // ArbosTest
    address!("000000000000000000000000000000000000006b"), // ArbOwnerPublic
    address!("000000000000000000000000000000000000006c"), // ArbGasInfo
    address!("000000000000000000000000000000000000006d"), // ArbAggregator
    address!("000000000000000000000000000000000000006e"), // ArbRetryableTx
    address!("000000000000000000000000000000000000006f"), // ArbStatistics
    address!("0000000000000000000000000000000000000070"), // ArbOwner
    address!("00000000000000000000000000000000000000ff"), // ArbDebug
    address!("00000000000000000000000000000000000a4b05"), // ArbosActs
];

#[derive(Debug, clap::Args)]
pub struct Arb1ArbosSynthesizeArgs {
    /// Foundation classic-export `addresstable.json` (one "0x..." per line).
    #[arg(long)]
    pub addresstable: PathBuf,

    /// Foundation classic-export `retryables.json` (one JSON object per line).
    #[arg(long)]
    pub retryables: PathBuf,

    /// User-accounts JSONL (from `arb1-state-convert`). Used only to skip
    /// emitting expired-retryable balance credits for addresses that
    /// Nitro's `SetBalance` would clobber anyway (`initialize.go:178`).
    #[arg(long)]
    pub user_accounts: PathBuf,

    /// Output JSONL containing only the synthesized lines.
    #[arg(long)]
    pub out: PathBuf,

    /// L1 initial base fee (wei) at migration. Default = Nitro 50 gwei.
    #[arg(long, default_value_t = DEFAULT_L1_INITIAL_BASE_FEE_WEI)]
    pub initial_l1_base_fee_wei: u64,
}

#[derive(Deserialize)]
struct RetryableJson {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Timeout")]
    timeout: u64,
    #[serde(rename = "From")]
    from: String,
    #[serde(rename = "To")]
    to: String,
    #[serde(rename = "Callvalue")]
    callvalue: String,
    #[serde(rename = "Beneficiary")]
    beneficiary: String,
    #[serde(rename = "Calldata", default)]
    calldata: Vec<u8>,
}

pub fn run(args: Arb1ArbosSynthesizeArgs) -> Result<()> {
    let table = read_address_table(&args.addresstable)?;
    eprintln!("addresstable: {} entries", table.len());

    let retryables = read_retryables(&args.retryables)?;
    eprintln!("retryables: {} entries", retryables.len());

    let user_addresses = read_user_addresses(&args.user_accounts)?;
    eprintln!("user_accounts: {} addresses", user_addresses.len());

    // Bootstrap (via the harness) now matches Nitro's InitializeArbosState
    // exactly: writes version/chain_id/network_fee_account/genesis_block_num,
    // installs chain_config bytes, initialises every subspace, adds the
    // initial chain owner, upgrades to the target version.
    let chain_config_bytes = hex::decode(ARB1_CHAIN_CONFIG_HEX.trim())
        .context("decode embedded arb1 chain_config hex")?;
    eprintln!("chain_config: {} bytes", chain_config_bytes.len());

    let initial_l1_base_fee = U256::from(args.initial_l1_base_fee_wei);
    let mut harness = ArbosHarness::new()
        .with_arbos_version(ARB1_ARBOS_VERSION)
        .with_chain_id(ARB1_CHAIN_ID)
        .with_initial_chain_owner(ARB1_CHAIN_OWNER)
        .with_genesis_block_num(ARB1_GENESIS_BLOCK_NUM)
        .with_serialized_chain_config(chain_config_bytes)
        .with_l1_initial_base_fee(initial_l1_base_fee)
        .initialize();

    // Install the [0xFE] code marker on each v0 precompile (matches Nitro
    // `arbosstate.go:236`). Bootstrap doesn't touch account code — that's a
    // node-level concern, same as in `arb-node::initialize_arbos_state`.
    {
        let state = harness.state();
        for addr in &GENESIS_PRECOMPILE_ADDRESSES {
            set_account_code(state, *addr, Bytes::from_static(&[0xFE]));
        }
    }

    // Drive the remaining migration steps (these come from Nitro's
    // `InitializeArbosInDatabase`, not `InitializeArbosState`): import
    // address-table contents and retryables. Bootstrap has already added the
    // initial chain owner, so we don't call `chain_owners.add` here.
    let (balance_credits, escrow_credits) = {
        let state_ptr = harness.state_ptr();
        let arbos_state = harness.arbos_state();

        let size = arbos_state
            .address_table
            .size(unsafe { &mut *state_ptr })
            .map_err(|e| anyhow!("address_table.size: {e}"))?;
        if size != 0 {
            bail!("address table not empty at synthesis start: {size}");
        }
        for (i, addr) in table.iter().enumerate() {
            let (slot, _) = arbos_state
                .address_table
                .register(unsafe { &mut *state_ptr }, *addr)
                .map_err(|e| anyhow!("address_table.register[{i}]: {e}"))?;
            if slot != i as u64 {
                bail!("address table slot mismatch at {i}: got {slot}");
            }
            if (i + 1) % 100_000 == 0 {
                eprintln!("registered {} addresses", i + 1);
            }
        }
        eprintln!("registered all {} address-table entries", table.len());

        let (balance_credits, escrow_credits) = initialize_retryables(
            unsafe { &mut *state_ptr },
            &arbos_state.retryable_state,
            retryables,
            ARB1_TIMESTAMP,
        )
        .map_err(|e| anyhow!("initialize_retryables: {e}"))?;
        (balance_credits, escrow_credits)
    };
    eprintln!(
        "retryables: {} expired (beneficiary credits), {} active (escrows + storage)",
        balance_credits.len(),
        escrow_credits.len()
    );

    // Pull the ArbOS account's resulting state out of the revm cache.
    let state = harness.state();
    let arbos_ca = state
        .cache
        .accounts
        .get(&ARBOS_STATE_ADDRESS)
        .ok_or_else(|| anyhow!("ArbOS-system account not in cache after init"))?;
    let arbos_pa = arbos_ca
        .account
        .as_ref()
        .ok_or_else(|| anyhow!("ArbOS-system account exists but has no PlainAccount"))?;
    eprintln!(
        "ArbOS-system account: balance={}, nonce={}, code_hash={}, storage_slots={}",
        arbos_pa.info.balance,
        arbos_pa.info.nonce,
        arbos_pa.info.code_hash,
        arbos_pa.storage.len()
    );

    if let Some(parent) = args.out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let f = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&args.out)
        .with_context(|| format!("open {}", args.out.display()))?;
    let mut w = BufWriter::with_capacity(1 << 20, f);

    // 1. ArbOS-system account.
    let arbos_storage_pairs: Vec<(U256, U256)> = arbos_pa
        .storage
        .iter()
        .filter_map(|(k, v): (&U256, &U256)| (!v.is_zero()).then_some((*k, *v)))
        .collect();
    emit_account(
        &mut w,
        ARBOS_STATE_ADDRESS,
        arbos_pa.info.balance,
        arbos_pa.info.nonce,
        None,
        &arbos_storage_pairs,
    )?;
    eprintln!(
        "wrote ArbOS-system account (storage: {} non-zero slots)",
        arbos_storage_pairs.len()
    );

    // 2. Precompile code markers.
    let invalid_code = Bytes::from_static(&[0xFE]);
    let mut precompiles_emitted = 0;
    for addr in &GENESIS_PRECOMPILE_ADDRESSES {
        // For addresses present in user accounts, the user's JSONL line has
        // (possibly) balance/nonce; reth's init_from_state_dump treats a
        // later line as an upsert. We emit code+0-balance and rely on the
        // earlier user-line setting balance/nonce. If init-state errors on
        // duplicate addresses we will merge in a follow-up.
        emit_account(&mut w, *addr, U256::ZERO, 0, Some(&invalid_code), &[])?;
        precompiles_emitted += 1;
    }
    eprintln!("wrote {} precompile code markers", precompiles_emitted);

    // 3. Active-retryable escrow accounts.
    let mut escrow_emitted = 0;
    for (addr, value) in &escrow_credits {
        emit_account(&mut w, *addr, *value, 0, None, &[])?;
        escrow_emitted += 1;
    }
    eprintln!("wrote {} escrow accounts", escrow_emitted);

    // 4. Expired-retryable beneficiary credits — only if not in user accounts
    // (else Nitro's later SetBalance overwrites them anyway).
    let mut credits_emitted = 0;
    let mut credits_skipped = 0;
    for (addr, value) in &balance_credits {
        if user_addresses.contains(addr) {
            credits_skipped += 1;
            continue;
        }
        emit_account(&mut w, *addr, *value, 0, None, &[])?;
        credits_emitted += 1;
    }
    eprintln!(
        "wrote {} beneficiary credits (skipped {} overlapping user accounts)",
        credits_emitted, credits_skipped
    );

    w.flush()?;
    eprintln!("done: synthesized JSONL → {}", args.out.display());
    Ok(())
}

/// Emit one JSONL line in reth's `init-state` schema. `storage` slots with
/// zero value are dropped (geth and reth treat them as absent for trie root
/// purposes); if the resulting line carries no balance/nonce/code/storage it
/// is suppressed.
fn emit_account<W: Write>(
    w: &mut W,
    addr: Address,
    balance: U256,
    nonce: u64,
    code: Option<&Bytes>,
    storage: &[(U256, U256)],
) -> Result<()> {
    let mut entry = Map::new();
    entry.insert("address".into(), Value::String(format!("0x{:040x}", addr)));
    entry.insert("balance".into(), Value::String(format!("0x{:x}", balance)));
    if nonce != 0 {
        entry.insert("nonce".into(), Value::String(format!("0x{:x}", nonce)));
    }
    if let Some(c) = code {
        if !c.is_empty() {
            entry.insert("code".into(), Value::String(format!("0x{}", hex::encode(c))));
        }
    }
    if !storage.is_empty() {
        let mut s = Map::new();
        for (k, v) in storage {
            if !v.is_zero() {
                s.insert(
                    format!("0x{:064x}", k),
                    Value::String(format!("0x{:064x}", v)),
                );
            }
        }
        if !s.is_empty() {
            entry.insert("storage".into(), Value::Object(s));
        }
    }
    let _ = B256::ZERO; // silence unused import for some build configurations
    let _ = json!({}); // silence unused import for some build configurations
    let is_empty = balance.is_zero()
        && nonce == 0
        && !entry.contains_key("code")
        && !entry.contains_key("storage");
    if is_empty {
        return Ok(());
    }
    writeln!(w, "{}", serde_json::to_string(&Value::Object(entry))?)?;
    Ok(())
}

/// Read the Foundation's classic `addresstable.json`, one quoted "0x..."
/// per line.
fn read_address_table(path: &PathBuf) -> Result<Vec<Address>> {
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut out = Vec::with_capacity(700_000);
    for (lineno, line) in BufReader::with_capacity(1 << 20, f).lines().enumerate() {
        let line = line?;
        let s = line.trim();
        if s.is_empty() {
            continue;
        }
        // Each line is a JSON string literal: `"0x..."` (possibly trailing comma).
        let s = s.trim_end_matches(',');
        let parsed: String = serde_json::from_str(s)
            .with_context(|| format!("line {}: parse address string", lineno + 1))?;
        out.push(
            parsed
                .parse()
                .with_context(|| format!("line {}: parse Address {parsed:?}", lineno + 1))?,
        );
    }
    Ok(out)
}

fn read_retryables(path: &PathBuf) -> Result<Vec<InitRetryableData>> {
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut out = Vec::new();
    for (lineno, line) in BufReader::with_capacity(1 << 20, f).lines().enumerate() {
        let line = line?;
        let s = line.trim();
        if s.is_empty() {
            continue;
        }
        let r: RetryableJson = serde_json::from_str(s)
            .with_context(|| format!("line {}: parse retryable", lineno + 1))?;
        let to_addr: Address = r
            .to
            .parse()
            .with_context(|| format!("line {}: parse To", lineno + 1))?;
        out.push(InitRetryableData {
            id: r.id.parse().context("Id")?,
            timeout: r.timeout,
            from: r.from.parse().context("From")?,
            to: if to_addr == Address::ZERO { None } else { Some(to_addr) },
            callvalue: parse_uint(&r.callvalue).context("Callvalue")?,
            beneficiary: r.beneficiary.parse().context("Beneficiary")?,
            calldata: r.calldata,
        });
    }
    Ok(out)
}

fn read_user_addresses(path: &PathBuf) -> Result<HashSet<Address>> {
    let f = File::open(path).with_context(|| format!("open {}", path.display()))?;
    let mut out: HashSet<Address> = HashSet::with_capacity(1_300_000);
    for line in BufReader::with_capacity(1 << 20, f).lines() {
        let line = line?;
        let s = line.trim();
        if s.is_empty() {
            continue;
        }
        // First line is `{"root": ...}` — has no `address` field, so the
        // get(...) below skips it without special-casing.
        let v: Value = match serde_json::from_str(s) {
            Ok(v) => v,
            Err(_) => continue,
        };
        if let Some(a) = v.get("address").and_then(Value::as_str) {
            if let Ok(addr) = a.parse::<Address>() {
                out.insert(addr);
            }
        }
    }
    Ok(out)
}

fn parse_uint(s: &str) -> Result<U256> {
    let s = s.trim();
    if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        Ok(U256::from_str_radix(h, 16)?)
    } else {
        Ok(U256::from_str_radix(s, 10)?)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn address_table_parses_quoted_lines() {
        let path = std::env::temp_dir().join("arb1_synth_test_addresstable.json");
        std::fs::write(
            &path,
            "\"0x0000000000000000000000000000000000000000\"\n\
             \"0x000000000000000000000000000000000000006d\"\n",
        )
        .unwrap();
        let table = read_address_table(&path).unwrap();
        assert_eq!(table.len(), 2);
        assert_eq!(
            table[1],
            address!("000000000000000000000000000000000000006d")
        );
    }

    #[test]
    fn retryable_parses_one_line() {
        let path = std::env::temp_dir().join("arb1_synth_test_retryables.json");
        std::fs::write(
            &path,
            r#"{"Beneficiary":"0x416e52ef44f534bb5a842184764e668a64487bd7","Calldata":[],"Callvalue":"100","From":"0x416e52ef44f534bb5a842184764e668a64487bd7","Id":"0xadbbc56d54da2df60f0afb20efc5ec600120ef0265575dc5020a9d0c91030350","Timeout":1662559890,"To":"0x416e52ef44f534bb5a842184764e668a64487bd7"}"#,
        )
        .unwrap();
        let r = read_retryables(&path).unwrap();
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].timeout, 1662559890);
        assert_eq!(r[0].callvalue, U256::from(100u64));
    }

    #[test]
    fn user_addresses_skip_root_header() {
        let path = std::env::temp_dir().join("arb1_synth_test_user.jsonl");
        std::fs::write(
            &path,
            "{\"root\":\"0x12\"}\n\
             {\"address\":\"0x000000000000000000000000000000000000000a\",\"balance\":\"0x1\"}\n",
        )
        .unwrap();
        let set = read_user_addresses(&path).unwrap();
        assert_eq!(set.len(), 1);
        assert!(set.contains(&address!("000000000000000000000000000000000000000a")));
    }

    #[test]
    fn parse_uint_accepts_decimal_and_hex() {
        assert_eq!(parse_uint("100").unwrap(), U256::from(100u64));
        assert_eq!(parse_uint("0xa").unwrap(), U256::from(10u64));
    }
}
