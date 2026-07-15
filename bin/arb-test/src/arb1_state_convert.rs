//! Convert the Foundation's classic state export (`accounts.json` from
//! `snapshot.arbitrum.foundation/arb1/classic-export.tar`) into reth's
//! `init-state` JSONL format.
//!
//! One input line per user account, with the Go-RPC `AccountInitializationInfo`
//! schema (`Addr`, `Balance` decimal, `Nonce`, `ContractInfo: null | {Code:
//! [bytes], ContractStorage}`, `AggregatorInfo`, `AggregatorToPay`). Output is
//! a flattened `GenesisAccountWithAddress` per line, prefixed by a `{"root":
//! ...}` header carrying the expected migration state root.
//!
//! Emits **user accounts only**. The ArbOS-system account at 0xA4B05Fff... is
//! synthesized separately by porting Nitro's `InitializeArbosInDatabase`; the
//! two streams are concatenated before `arb-reth init-state`.

use std::{
    fs::OpenOptions,
    io::{BufRead, BufReader, BufWriter, Write},
    path::PathBuf,
};

use anyhow::{anyhow, Context, Result};
use serde::Deserialize;
use serde_json::{json, Map, Value};

use crate::state_dump::{decimal_or_hex_to_0x, normalize_address, pad_b256, with_0x};

const ZERO_B256: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, clap::Args)]
pub struct Arb1StateConvertArgs {
    /// Path to the classic export `accounts.json` (JSONL, one account per line).
    #[arg(long)]
    pub input: PathBuf,

    /// Output path for the reth `init-state` JSONL.
    #[arg(long)]
    pub out: PathBuf,

    /// Expected migration state root. Written as the first line so reth's
    /// `init-state` can gate the dump root against the chainspec genesis root.
    /// Default = block 22,207,817's stateRoot (the actual Nitro migration
    /// genesis); block 22,207,818's stateRoot 0xd764f1… is post-init-tx.
    #[arg(
        long,
        default_value = "0x7f2bfc4481d02bfcfc606ebb949384ef78d03a0f30a2dc9cccd652eb80926ae1"
    )]
    pub state_root: String,

    /// Progress log cadence (number of input lines per status line).
    #[arg(long, default_value_t = 100_000)]
    pub progress_every: u64,
}

#[derive(Deserialize)]
struct ClassicAccount {
    #[serde(rename = "Addr")]
    addr: String,
    #[serde(rename = "Balance")]
    balance: String,
    #[serde(rename = "Nonce")]
    nonce: u64,
    #[serde(rename = "ContractInfo", default)]
    contract_info: Option<ClassicContract>,
    // AggregatorInfo / AggregatorToPay only feed the ArbOS-system account's
    // BatchPosterTable; they do not affect user state.
}

#[derive(Deserialize)]
struct ClassicContract {
    #[serde(rename = "Code", default)]
    code: Vec<u8>,
    #[serde(rename = "ContractStorage", default)]
    contract_storage: Option<std::collections::BTreeMap<String, String>>,
}

pub fn run(args: Arb1StateConvertArgs) -> Result<()> {
    let in_file = std::fs::File::open(&args.input)
        .with_context(|| format!("open input {}", args.input.display()))?;
    let reader = BufReader::with_capacity(1 << 20, in_file);

    if let Some(parent) = args.out.parent() {
        std::fs::create_dir_all(parent).ok();
    }
    let out_file = OpenOptions::new()
        .create(true)
        .write(true)
        .truncate(true)
        .open(&args.out)
        .with_context(|| format!("open output {}", args.out.display()))?;
    let mut w = BufWriter::with_capacity(1 << 20, out_file);

    writeln!(w, "{}", json!({ "root": with_0x(&args.state_root) }))?;

    let mut total: u64 = 0;
    let mut skipped: u64 = 0;
    let mut processed: u64 = 0;
    for (lineno, line) in reader.lines().enumerate() {
        let line = line.with_context(|| format!("read line {}", lineno + 1))?;
        if line.trim().is_empty() {
            continue;
        }
        let acct: ClassicAccount = serde_json::from_str(&line)
            .with_context(|| format!("line {}: parse classic account", lineno + 1))?;
        match classic_account_to_jsonl(&acct)? {
            Some(out_line) => {
                writeln!(w, "{out_line}")?;
                total += 1;
            }
            None => skipped += 1,
        }
        processed += 1;
        if args.progress_every > 0 && processed % args.progress_every == 0 {
            eprintln!("processed {processed} (wrote {total}, skipped empty {skipped})");
        }
    }

    w.flush()?;
    eprintln!(
        "done: wrote {total} accounts to {} (skipped {skipped} empty out of {processed} read)",
        args.out.display()
    );
    Ok(())
}

/// Map one classic-export account to a reth `init-state` JSONL entry, dropping
/// truly empty accounts (geth's commit-time pruning would discard them too, so
/// keeping them would alter the computed state root).
fn classic_account_to_jsonl(acct: &ClassicAccount) -> Result<Option<String>> {
    let addr = normalize_address(&acct.addr)
        .ok_or_else(|| anyhow!("address {:?} is not 40-hex", acct.addr))?;

    let mut entry = Map::new();
    entry.insert("address".into(), Value::String(addr));
    entry.insert(
        "balance".into(),
        Value::String(decimal_or_hex_to_0x(&acct.balance)?),
    );
    if acct.nonce != 0 {
        entry.insert("nonce".into(), Value::String(format!("0x{:x}", acct.nonce)));
    }
    if let Some(ci) = &acct.contract_info {
        if !ci.code.is_empty() {
            entry.insert(
                "code".into(),
                Value::String(format!("0x{}", hex::encode(&ci.code))),
            );
        }
        if let Some(storage) = &ci.contract_storage {
            let mut out = Map::new();
            for (k, v) in storage {
                let padded_v = pad_b256(v);
                if padded_v != ZERO_B256 {
                    out.insert(pad_b256(k), Value::String(padded_v));
                }
            }
            if !out.is_empty() {
                entry.insert("storage".into(), Value::Object(out));
            }
        }
    }

    let is_empty = entry.get("balance").and_then(Value::as_str) == Some("0x0")
        && !entry.contains_key("nonce")
        && !entry.contains_key("code")
        && !entry.contains_key("storage");
    if is_empty {
        return Ok(None);
    }
    Ok(Some(serde_json::to_string(&Value::Object(entry))?))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(line: &str) -> ClassicAccount {
        serde_json::from_str(line).unwrap()
    }

    #[test]
    fn eoa_with_decimal_balance() {
        let acct = parse(
            r#"{"Addr":"0xf124579b4d0a56cf720d601283f45d6ce4198279","AggregatorInfo":null,"AggregatorToPay":null,"Balance":"114024661353161","ContractInfo":null,"Nonce":1}"#,
        );
        let line = classic_account_to_jsonl(&acct).unwrap().unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["address"], "0xf124579b4d0a56cf720d601283f45d6ce4198279");
        assert_eq!(v["balance"], "0x67b46f6f82c9"); // 114024661353161 decimal
        assert_eq!(v["nonce"], "0x1");
        assert!(v.get("code").is_none());
        assert!(v.get("storage").is_none());
    }

    #[test]
    fn contract_with_byte_array_code_and_storage() {
        // ContractInfo.Code is a JSON array of bytes (Go []byte default JSON).
        let acct = parse(
            r#"{"Addr":"0xe66092c38c2a56e63009946550407902934376da","AggregatorInfo":null,"AggregatorToPay":null,"Balance":"0","ContractInfo":{"Code":[54,61,243],"ContractStorage":{"0x0631038f468f2276d9a272e30fc10dc70c868e349eda452a58680e3420363b34":"0x0000000000000000000000000000000000000000000000000000000000000001"}},"Nonce":1}"#,
        );
        let line = classic_account_to_jsonl(&acct).unwrap().unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["code"], "0x363df3");
        let st = v["storage"].as_object().unwrap();
        assert_eq!(st.len(), 1);
        assert_eq!(
            st["0x0631038f468f2276d9a272e30fc10dc70c868e349eda452a58680e3420363b34"],
            "0x0000000000000000000000000000000000000000000000000000000000000001"
        );
    }

    #[test]
    fn contract_with_null_storage() {
        let acct = parse(
            r#"{"Addr":"0xd5d17ed51e462334e9b74a3f93db7b55bb41039a","AggregatorInfo":null,"AggregatorToPay":null,"Balance":"0","ContractInfo":{"Code":[54,61,243],"ContractStorage":null},"Nonce":1}"#,
        );
        let line = classic_account_to_jsonl(&acct).unwrap().unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["code"], "0x363df3");
        assert!(v.get("storage").is_none());
    }

    #[test]
    fn zero_storage_value_is_dropped() {
        let acct = parse(
            r#"{"Addr":"0xd5d17ed51e462334e9b74a3f93db7b55bb41039a","AggregatorInfo":null,"AggregatorToPay":null,"Balance":"0","ContractInfo":{"Code":[1],"ContractStorage":{"0x0000000000000000000000000000000000000000000000000000000000000001":"0x0000000000000000000000000000000000000000000000000000000000000000"}},"Nonce":1}"#,
        );
        let line = classic_account_to_jsonl(&acct).unwrap().unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        // Zero-valued slot dropped; storage map removed because it ends up empty.
        assert!(v.get("storage").is_none());
    }

    #[test]
    fn empty_account_skipped() {
        let acct = parse(
            r#"{"Addr":"0x000000000000000000000000000000000000006c","AggregatorInfo":null,"AggregatorToPay":null,"Balance":"0","ContractInfo":null,"Nonce":0}"#,
        );
        assert!(classic_account_to_jsonl(&acct).unwrap().is_none());
    }

    #[test]
    fn unprefixed_balance_rejected_as_address() {
        let acct = ClassicAccount {
            addr: "not-an-address".into(),
            balance: "0".into(),
            nonce: 0,
            contract_info: None,
        };
        let err = classic_account_to_jsonl(&acct).unwrap_err();
        assert!(err.to_string().contains("40-hex"));
    }

    #[test]
    fn empty_code_field_is_dropped() {
        let acct = parse(
            r#"{"Addr":"0xf124579b4d0a56cf720d601283f45d6ce4198279","AggregatorInfo":null,"AggregatorToPay":null,"Balance":"5","ContractInfo":{"Code":[],"ContractStorage":null},"Nonce":3}"#,
        );
        let line = classic_account_to_jsonl(&acct).unwrap().unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert!(v.get("code").is_none());
        assert_eq!(v["balance"], "0x5");
        assert_eq!(v["nonce"], "0x3");
    }
}
