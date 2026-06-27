//! Initiating side of pairing: send a `PairRequest` to a target node and, if its operator accepts,
//! store the returned capability in the local wallet. Shared by the `ce-fleet pair` CLI command and
//! the daemon's `/api/pair` web button, so both behave identically.

use anyhow::{Context, Result};
use ce_identity::Identity;
use ce_rs::CeClient;
use std::path::Path;

use crate::proto::{PairRequest, PairResponse, TOPIC};
use crate::util::{now_secs, short_id};
use crate::wallet;

/// Total time A waits for B's human to Accept/Deny. Must exceed the daemon's `DECISION_TIMEOUT`.
const REQUEST_TIMEOUT_MS: u64 = 180_000;

/// The result of an initiate attempt, for display by the CLI or web UI.
#[derive(Debug, serde::Serialize)]
pub struct PairOutcome {
    pub accepted: bool,
    pub alias: Option<String>,
    pub issuer: Option<String>,
    pub granted: Vec<String>,
    pub not_after: u64,
    pub rooted: bool,
    pub reason: Option<String>,
}

/// Send a pairing request to `target_node_hex` and persist the capability on accept.
// The pairing request is defined by exactly these fields (target, abilities, ttl, roots, label,
// reason); threading them as discrete args keeps the call sites explicit. Clippy's 7-arg heuristic
// trips at 8, so allow it here rather than inventing a one-use params struct.
#[allow(clippy::too_many_arguments)]
pub async fn initiate(
    ce: &CeClient,
    identity: &Identity,
    data_dir: &Path,
    target_node_hex: &str,
    abilities: Vec<String>,
    ttl_secs: u64,
    want_roots: bool,
    label: &str,
) -> Result<PairOutcome> {
    let req = PairRequest {
        from_node: identity.node_id_hex(),
        label: label.to_string(),
        abilities,
        ttl_secs,
        want_roots,
        nonce: now_secs(),
    };
    let payload = serde_json::to_vec(&req).context("encoding pair request")?;
    let reply = ce
        .request(target_node_hex, TOPIC, &payload, REQUEST_TIMEOUT_MS)
        .await
        .context("sending pair request over the mesh")?;
    let resp: PairResponse =
        serde_json::from_slice(&reply).context("decoding pair response")?;

    match resp {
        PairResponse::Accepted {
            token,
            granted,
            not_after,
            issuer,
            issuer_label,
            rooted,
        } => {
            let alias = wallet_alias(&issuer_label, &issuer);
            wallet::add(data_dir, &alias, &issuer, &token)?;
            Ok(PairOutcome {
                accepted: true,
                alias: Some(alias),
                issuer: Some(issuer),
                granted,
                not_after,
                rooted,
                reason: None,
            })
        }
        PairResponse::Denied { reason } => Ok(PairOutcome {
            accepted: false,
            alias: None,
            issuer: None,
            granted: Vec::new(),
            not_after: 0,
            rooted: false,
            reason: Some(reason),
        }),
    }
}

/// A wallet-safe alias from the peer's friendly label (lowercased, `[a-z0-9-]`), falling back to a
/// short node id when the label has no usable characters.
fn wallet_alias(label: &str, node_id_hex: &str) -> String {
    let mut s: String = label
        .trim()
        .to_lowercase()
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    while s.contains("--") {
        s = s.replace("--", "-");
    }
    let s = s.trim_matches('-').to_string();
    if s.is_empty() {
        short_id(node_id_hex)
    } else {
        s
    }
}
