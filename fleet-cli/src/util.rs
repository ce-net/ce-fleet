//! Small shared helpers: locating the node's data dir (so we load the SAME identity/wallet the
//! local `ce` node uses) and time/formatting utilities.

use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

/// The CE data directory. Mirrors `ce` and `ce-rs` exactly: `$CE_DATA_DIR` override, else the
/// platform path from `directories::ProjectDirs("", "", "ce")` (macOS:
/// `~/Library/Application Support/ce`, Linux: `~/.local/share/ce`). Falling back to `./.ce` only if
/// no home is resolvable keeps the binary usable in throwaway/test sandboxes.
pub fn data_dir(override_path: Option<PathBuf>) -> PathBuf {
    if let Some(p) = override_path {
        return p;
    }
    if let Some(p) = std::env::var_os("CE_DATA_DIR") {
        return PathBuf::from(p);
    }
    directories::ProjectDirs::from("", "", "ce")
        .map(|d| d.data_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from(".ce"))
}

/// Current unix time in seconds.
pub fn now_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0)
}

/// A short, human-glanceable form of a 64-hex node id (first 12 chars), for logs and aliases.
pub fn short_id(node_id_hex: &str) -> String {
    node_id_hex.chars().take(12).collect()
}

/// This machine's friendly label for a pairing prompt: `$CE_DEVICE_LABEL`, else the OS hostname,
/// else a short node id. Never fails.
pub fn device_label(node_id_hex: &str) -> String {
    if let Ok(l) = std::env::var("CE_DEVICE_LABEL") {
        let l = l.trim().to_string();
        if !l.is_empty() {
            return l;
        }
    }
    for var in ["HOSTNAME", "HOST", "COMPUTERNAME"] {
        if let Ok(h) = std::env::var(var) {
            let h = h.trim().to_string();
            if !h.is_empty() {
                return h;
            }
        }
    }
    // Last resort: the platform `hostname` command, else a short id.
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| short_id(node_id_hex))
}

/// Parse a hex node id (64 chars) into a `[u8; 32]`.
pub fn parse_node_id(hex_str: &str) -> anyhow::Result<[u8; 32]> {
    let bytes = hex::decode(hex_str.trim())
        .map_err(|e| anyhow::anyhow!("invalid node id hex: {e}"))?;
    let arr: [u8; 32] = bytes
        .try_into()
        .map_err(|_| anyhow::anyhow!("node id must be 32 bytes (64 hex chars)"))?;
    Ok(arr)
}
