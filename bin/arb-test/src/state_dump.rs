//! Export an archive node's state at a block into reth's `init-state` JSONL
//! format via paginated `debug_accountRange`.
//!
//! Used to bootstrap a migrated chain (e.g. Arbitrum One at its Nitro genesis
//! block 22207818) whose state cannot be built from an init message.

use std::{
    fs::OpenOptions,
    io::{BufWriter, Write},
    path::PathBuf,
    time::Duration,
};

use alloy_primitives::U256;
use anyhow::{anyhow, bail, Context, Result};
use base64::{engine::general_purpose::STANDARD as B64, Engine as _};
use serde_json::{json, Map, Value};

use arb_test_harness::rpc::JsonRpcClient;

const ZERO_B256: &str = "0x0000000000000000000000000000000000000000000000000000000000000000";

#[derive(Debug, clap::Args)]
pub struct StateDumpArgs {
    /// Archive RPC exposing `debug_accountRange` (a Nitro geth node).
    #[arg(long, env = "ARB1_ARCHIVE_RPC")]
    pub rpc: String,

    /// Block number whose state to export (the migration genesis block).
    #[arg(long, default_value_t = 22_207_818)]
    pub block: u64,

    /// Output path for the reth `init-state` JSONL dump.
    #[arg(long)]
    pub out: PathBuf,

    /// Accounts requested per page (geth caps `debug_accountRange` at 256).
    #[arg(long, default_value_t = 256)]
    pub page_size: u64,
}

pub fn run(args: StateDumpArgs) -> Result<()> {
    let rpc = JsonRpcClient::new(args.rpc.clone()).with_timeout(Duration::from_secs(120));
    let block_hex = format!("0x{:x}", args.block);

    // Resume support: a sidecar file records the cursor for the next page so a
    // long export survives interruption without restarting from the first page.
    let cursor_path = args.out.with_extension("cursor");
    let resuming = cursor_path.exists();
    let mut start = std::fs::read_to_string(&cursor_path)
        .ok()
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .and_then(|v| v.get("start").and_then(Value::as_str).map(str::to_string))
        .unwrap_or_else(|| "0x".to_string());
    let mut wrote_root = resuming;

    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .append(resuming)
        .truncate(!resuming)
        .open(&args.out)
        .with_context(|| format!("open {}", args.out.display()))?;
    let mut w = BufWriter::new(file);

    let mut expected_root: Option<String> = None;
    let mut total: u64 = 0;
    let mut page: u64 = 0;

    loop {
        let resp = rpc
            .call(
                "debug_accountRange",
                json!([block_hex, start, args.page_size, false, false, false]),
            )
            .map_err(|e| anyhow!("debug_accountRange: {e}"))?;

        if page == 0 {
            // Log the first raw response so the geth field encodings (storage
            // value form, cursor) can be confirmed against the live node.
            let raw = serde_json::to_string(&resp).unwrap_or_default();
            eprintln!(
                "first debug_accountRange response (<=800 chars):\n{}",
                truncate(&raw, 800)
            );
        }

        let root = resp
            .get("root")
            .and_then(Value::as_str)
            .ok_or_else(|| anyhow!("response missing `root`"))?
            .to_string();
        match &expected_root {
            None => expected_root = Some(root.clone()),
            Some(r) if r != &root => bail!("state root changed across pages: {r} -> {root}"),
            _ => {}
        }
        if !wrote_root {
            writeln!(w, "{}", json!({ "root": with_0x(&root) }))?;
            wrote_root = true;
        }

        let accounts = resp
            .get("accounts")
            .and_then(Value::as_object)
            .ok_or_else(|| anyhow!("response missing `accounts`"))?;
        let n = accounts.len();
        for (addr_key, acct) in accounts {
            if let Some(line) = geth_account_to_jsonl(addr_key, acct)? {
                writeln!(w, "{line}")?;
                total += 1;
            }
        }

        let next_start = resp
            .get("next")
            .and_then(Value::as_str)
            .and_then(cursor_to_start);

        w.flush()?;
        if let Some(ns) = &next_start {
            std::fs::write(&cursor_path, serde_json::to_vec(&json!({ "start": ns }))?)?;
        }

        page += 1;
        eprintln!(
            "page {page}: {n} accounts ({total} written), next={}",
            next_start.as_deref().unwrap_or("<end>")
        );

        match next_start {
            Some(ns) if n > 0 => start = ns,
            _ => break,
        }
    }

    w.flush()?;
    let _ = std::fs::remove_file(&cursor_path);
    eprintln!(
        "wrote {} ({total} accounts, state root {})",
        args.out.display(),
        expected_root.as_deref().unwrap_or("<none>")
    );
    Ok(())
}

/// Convert one `debug_accountRange` account into a reth `init-state` JSONL line
/// (a flattened `GenesisAccount` plus its `address`). Returns `None` for an
/// empty account. Fails if the map key is a 32-byte hash rather than a 20-byte
/// address, which means the snapshot lacks preimages.
fn geth_account_to_jsonl(addr_key: &str, acct: &Value) -> Result<Option<String>> {
    let addr = normalize_address(addr_key).ok_or_else(|| {
        anyhow!(
            "account key {addr_key:?} is not a 20-byte address; the snapshot is missing \
             preimages — re-create it with --execution.caching.enable-preimages=true"
        )
    })?;

    let mut entry = Map::new();
    entry.insert("address".into(), Value::String(addr));

    let balance = acct.get("balance").and_then(Value::as_str).unwrap_or("0");
    entry.insert(
        "balance".into(),
        Value::String(decimal_or_hex_to_0x(balance)?),
    );

    let nonce = acct.get("nonce").and_then(Value::as_u64).unwrap_or(0);
    if nonce != 0 {
        entry.insert("nonce".into(), Value::String(format!("0x{nonce:x}")));
    }

    if let Some(code) = acct.get("code").and_then(Value::as_str) {
        let c = code.trim_start_matches("0x");
        if !c.is_empty() {
            entry.insert("code".into(), Value::String(format!("0x{c}")));
        }
    }

    if let Some(storage) = acct.get("storage").and_then(Value::as_object) {
        let mut out = Map::new();
        for (k, v) in storage {
            let val = v
                .as_str()
                .ok_or_else(|| anyhow!("storage value for {k} is not a string"))?;
            let val = pad_b256(val);
            if val != ZERO_B256 {
                out.insert(pad_b256(k), Value::String(val));
            }
        }
        if !out.is_empty() {
            entry.insert("storage".into(), Value::Object(out));
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

/// Normalize a 20-byte address hex string to lowercase `0x`-prefixed form.
/// Returns `None` for anything that is not 40 hex nibbles.
pub(crate) fn normalize_address(s: &str) -> Option<String> {
    let h = s.trim_start_matches("0x").trim_start_matches("0X");
    if h.len() != 40 || !h.bytes().all(|b| b.is_ascii_hexdigit()) {
        return None;
    }
    Some(format!("0x{}", h.to_ascii_lowercase()))
}

/// Parse a decimal or `0x`-hex integer string into canonical `0x`-hex.
pub(crate) fn decimal_or_hex_to_0x(s: &str) -> Result<String> {
    let s = s.trim();
    let v = if let Some(h) = s.strip_prefix("0x").or_else(|| s.strip_prefix("0X")) {
        U256::from_str_radix(h, 16).with_context(|| format!("hex int {s}"))?
    } else {
        U256::from_str_radix(s, 10).with_context(|| format!("decimal int {s}"))?
    };
    Ok(format!("0x{v:x}"))
}

/// Left-pad a hex value to a 32-byte `0x`-prefixed word.
pub(crate) fn pad_b256(s: &str) -> String {
    let h = s.trim_start_matches("0x").trim_start_matches("0X");
    let h = if h.len() > 64 { &h[h.len() - 64..] } else { h };
    format!("0x{:0>64}", h.to_ascii_lowercase())
}

pub(crate) fn with_0x(s: &str) -> String {
    if s.starts_with("0x") || s.starts_with("0X") {
        format!("0x{}", s[2..].to_ascii_lowercase())
    } else {
        format!("0x{}", s.to_ascii_lowercase())
    }
}

/// Translate a `debug_accountRange` `next` cursor into a `start` value for the
/// next request. geth marshals the cursor as base64 (`[]byte`), while `start`
/// expects hex; accept either form.
fn cursor_to_start(next: &str) -> Option<String> {
    if next.is_empty() {
        return None;
    }
    if let Some(h) = next.strip_prefix("0x").or_else(|| next.strip_prefix("0X")) {
        return (!h.is_empty()).then(|| format!("0x{}", h.to_ascii_lowercase()));
    }
    B64.decode(next)
        .ok()
        .filter(|b| !b.is_empty())
        .map(|b| format!("0x{}", hex::encode(b)))
}

fn truncate(s: &str, max: usize) -> String {
    if s.len() <= max {
        s.to_string()
    } else {
        format!("{}…", &s[..max])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_eoa_with_decimal_balance() {
        let acct = json!({ "balance": "1000000000000000000", "nonce": 7, "root": "0x...", "codeHash": "0x..." });
        let line = geth_account_to_jsonl("0x00000000000000000000000000000000000000aa", &acct)
            .unwrap()
            .unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["address"], "0x00000000000000000000000000000000000000aa");
        assert_eq!(v["balance"], "0xde0b6b3a7640000");
        assert_eq!(v["nonce"], "0x7");
        // codeHash / root must be dropped (reth's GenesisAccount denies them).
        assert!(v.get("code").is_none());
        assert!(v.get("storage").is_none());
        assert!(v.as_object().unwrap().get("codeHash").is_none());
        assert!(v.as_object().unwrap().get("root").is_none());
    }

    #[test]
    fn converts_contract_with_code_and_storage() {
        let acct = json!({
            "balance": "0",
            "nonce": 1,
            "code": "0x60016000",
            "storage": { "0x01": "0x2a", "0x02": "0x00" }
        });
        let line = geth_account_to_jsonl("0x00000000000000000000000000000000000000bb", &acct)
            .unwrap()
            .unwrap();
        let v: Value = serde_json::from_str(&line).unwrap();
        assert_eq!(v["code"], "0x60016000");
        // zero-valued slot dropped; non-zero slot padded to B256.
        let st = v["storage"].as_object().unwrap();
        assert_eq!(st.len(), 1);
        assert_eq!(
            st["0x0000000000000000000000000000000000000000000000000000000000000001"],
            "0x000000000000000000000000000000000000000000000000000000000000002a"
        );
    }

    #[test]
    fn skips_empty_account() {
        let acct = json!({ "balance": "0", "nonce": 0 });
        assert!(
            geth_account_to_jsonl("0x00000000000000000000000000000000000000cc", &acct)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn rejects_hashed_key_missing_preimage() {
        let acct = json!({ "balance": "1" });
        // 32-byte key => preimage missing => hard error.
        let err = geth_account_to_jsonl(
            "0x1111111111111111111111111111111111111111111111111111111111111111",
            &acct,
        )
        .unwrap_err();
        assert!(err.to_string().contains("preimages"));
    }

    #[test]
    fn cursor_accepts_hex_and_base64() {
        assert_eq!(cursor_to_start("0xABCD"), Some("0xabcd".to_string()));
        // base64 of bytes [0xde, 0xad] is "3q0="
        assert_eq!(cursor_to_start("3q0="), Some("0xdead".to_string()));
        assert_eq!(cursor_to_start(""), None);
    }
}
