//! Minimal writer for the capability wallet at `<data_dir>/wallet.toml`.
//!
//! The `ce` binary owns this file (`ce wallet add/ls/rm`) but keeps the structs private, so we
//! re-declare the exact same serde shape here to store a freshly-paired token without shelling out.
//! `ce tunnel <alias>` / `ce deploy <alias>` then auto-attach it. Keep this in sync with
//! `ce/src/main.rs` (`Wallet` / `WalletEntry`).

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, Default, Serialize, Deserialize)]
pub struct Wallet {
    #[serde(default)]
    pub entries: BTreeMap<String, WalletEntry>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WalletEntry {
    /// The target node (64 hex) this capability applies to.
    pub node_id: String,
    /// The hex capability token from `ce grant` / a pairing accept.
    pub cap: String,
    #[serde(default)]
    pub orgs: Vec<String>,
    #[serde(default)]
    pub workspaces: Vec<String>,
}

fn wallet_path(data_dir: &Path) -> PathBuf {
    data_dir.join("wallet.toml")
}

pub fn load(data_dir: &Path) -> Result<Wallet> {
    let path = wallet_path(data_dir);
    match std::fs::read_to_string(&path) {
        Ok(s) => toml::from_str(&s).with_context(|| format!("parsing {}", path.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(Wallet::default()),
        Err(e) => Err(e).with_context(|| format!("reading {}", path.display())),
    }
}

pub fn save(data_dir: &Path, wallet: &Wallet) -> Result<()> {
    std::fs::create_dir_all(data_dir)
        .with_context(|| format!("creating {}", data_dir.display()))?;
    let path = wallet_path(data_dir);
    let body = toml::to_string_pretty(wallet).context("serializing wallet")?;
    std::fs::write(&path, body).with_context(|| format!("writing {}", path.display()))?;
    Ok(())
}

/// Insert (or replace) one capability entry under `alias` and persist. Returns the alias used.
pub fn add(data_dir: &Path, alias: &str, node_id: &str, cap: &str) -> Result<String> {
    let mut wallet = load(data_dir)?;
    wallet.entries.insert(
        alias.to_string(),
        WalletEntry {
            node_id: node_id.to_string(),
            cap: cap.to_string(),
            orgs: Vec::new(),
            workspaces: Vec::new(),
        },
    );
    save(data_dir, &wallet)?;
    Ok(alias.to_string())
}
