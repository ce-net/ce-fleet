//! The pairing wire protocol, carried as JSON over the CE mesh request/reply primitive
//! (`CeClient::request` -> `serve` handler -> reply). One directed topic; the node authenticates the
//! sender, so `PairRequest.from_node` is cross-checked against the authenticated `Request.from`.

use serde::{Deserialize, Serialize};

/// The single directed request/reply topic for pairing.
pub const TOPIC: &str = "ce-fleet/pair";

/// The default "100% admin" ability set requested when pairing two of your own devices. These are
/// the exact action strings the node + rdev authorize against (no `*` wildcard exists), so this list
/// is what makes a paired device able to do everything: run/stop jobs, exec, sync, tunnel, install
/// apps, and read status. Narrow it with `ce-fleet pair --abilities ...`.
pub fn default_abilities() -> Vec<String> {
    [
        "status",
        "exec",
        "sync",
        "delete",
        "tunnel",
        "deploy",
        "kill",
        "spawn",
        "app:install",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect()
}

/// A request from device A asking device B to authorize A over B's resources.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PairRequest {
    /// A's node id (64 hex). Must equal the mesh-authenticated sender; B rejects a mismatch.
    pub from_node: String,
    /// A's friendly label, shown in B's accept prompt (hostname by default).
    pub label: String,
    /// The abilities A is asking for (B's operator sees and approves these).
    pub abilities: Vec<String>,
    /// Seconds the issued capability should remain valid; `0` = never expires.
    pub ttl_secs: u64,
    /// Also ask B to add A to its `roots` file (full owner; takes effect after the ce service
    /// restarts). The capability alone already authorizes A without this.
    #[serde(default)]
    pub want_roots: bool,
    /// A fresh nonce (unix seconds) — names the issued capability for later revocation.
    pub nonce: u64,
}

/// B's answer.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PairResponse {
    /// Approved: `token` is the hex capability chain (exactly what `ce grant` prints), to be stored
    /// in A's wallet. `granted` echoes the abilities actually granted; `not_after` is the expiry
    /// (`0` = never).
    Accepted {
        token: String,
        granted: Vec<String>,
        not_after: u64,
        /// B's node id, so A can store the wallet entry against the right target.
        issuer: String,
        /// B's friendly label, used as the wallet alias when present.
        issuer_label: String,
        /// True if B also added A to its roots (owner-level).
        rooted: bool,
    },
    /// Declined (operator denied, or the request timed out waiting for a human).
    Denied { reason: String },
}

impl PairResponse {
    pub fn denied(reason: impl Into<String>) -> Self {
        PairResponse::Denied {
            reason: reason.into(),
        }
    }
}
