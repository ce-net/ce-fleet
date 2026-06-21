//! Enrollment wire types and the bootstrap policy that turns an enroll request into working-cap
//! parameters. Kept free of I/O so the policy is unit-testable in isolation.

use serde::{Deserialize, Serialize};

/// What a first-booting node POSTs to the delegate `/enroll` endpoint. Mirrors the `ce-enroll`
/// oneshot payload: identity + a light self-description so the delegate can scope the working cap.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollRequest {
    /// The node's CE id (64-hex). Becomes the working cap's audience.
    pub node_id: String,
    /// Machine hostname (display only; never a trust input).
    pub hostname: String,
    /// `"linux" | "windows" | "macos"` (display + policy hint).
    pub os: String,
    /// Self-tier from the `ce-infer` probe, e.g. `"GpuMid"`, `"CpuLow"`, `"Ineligible"`.
    pub tier: String,
    /// The one-time bootstrap nonce carried by the enroll token (burned on first use).
    pub nonce: String,
    /// The bootstrap secret / short-code presented as the bearer of the bootstrap cap. The delegate
    /// checks it against its configured bootstrap policy. Never logged.
    pub bootstrap_secret: String,
}

/// The delegate's response: the audience-bound working-cap token the node stores in its wallet,
/// plus the scope it was granted (for the admin console + the node's own audit).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnrollOutcome {
    /// `[..org-root-chain, delegate->node]` hex-encoded (`ce_cap::encode_chain`). Empty on failure.
    pub working_cap: String,
    /// Abilities actually granted (intersection of policy ⨯ delegate-held).
    pub abilities: Vec<String>,
    /// Fleet tag the node was scoped to, if any.
    pub tag: Option<String>,
    /// Unix seconds the working cap expires (`0` = none, though the policy always sets a finite one).
    pub not_after: u64,
}

/// The delegate's enrollment policy: which bootstrap secret is accepted, what tag/abilities/TTL a
/// node gets. In production this would be loaded per-site from config/Vault; here it is an explicit,
/// testable value.
#[derive(Debug, Clone)]
pub struct BootstrapPolicy {
    /// The shared bootstrap secret a node must present (Tailscale-authkey analogue). Compared in
    /// constant time at the call site.
    pub bootstrap_secret: String,
    /// Fleet tag every node enrolled through this policy is scoped to (e.g. `"radiology"`).
    pub tag: Option<String>,
    /// Abilities to grant (intersected with the delegate's own at issue time).
    pub abilities: Vec<String>,
    /// Working-cap lifetime in seconds (e.g. 30 days). Clamped to the delegate's own expiry.
    pub working_ttl_secs: u64,
    /// How long the bootstrap nonce itself is valid from `enroll_window_start` (seconds). `0` =
    /// no window (the nonce is one-time but not separately time-boxed).
    pub bootstrap_ttl_secs: u64,
    /// Unix seconds the enrollment window opened (the bootstrap cap's `not_before`). `0` = always.
    pub enroll_window_start: u64,
}

impl BootstrapPolicy {
    /// The absolute unix-seconds deadline by which a bootstrap nonce must be claimed, or `0` for no
    /// window. Used by the nonce ledger to refuse late replays.
    pub fn nonce_valid_until(&self) -> u64 {
        if self.bootstrap_ttl_secs == 0 {
            0
        } else {
            self.enroll_window_start.saturating_add(self.bootstrap_ttl_secs)
        }
    }
}

/// Constant-time-ish equality for the bootstrap secret (length-independent, no early return on the
/// first differing byte). Not a substitute for a real MAC, but avoids the trivial timing oracle.
pub fn secret_eq(a: &str, b: &str) -> bool {
    let (a, b) = (a.as_bytes(), b.as_bytes());
    let mut diff = (a.len() ^ b.len()) as u8;
    let n = a.len().max(b.len());
    for i in 0..n {
        let x = a.get(i).copied().unwrap_or(0);
        let y = b.get(i).copied().unwrap_or(0);
        diff |= x ^ y;
    }
    diff == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_eq_matches_only_identical_secrets() {
        assert!(secret_eq("hunter2", "hunter2"));
        assert!(!secret_eq("hunter2", "hunter3"));
        assert!(!secret_eq("short", "shorter"));
        assert!(secret_eq("", ""));
    }

    #[test]
    fn nonce_valid_until_respects_window() {
        let p = BootstrapPolicy {
            bootstrap_secret: "s".into(),
            tag: None,
            abilities: vec![],
            working_ttl_secs: 0,
            bootstrap_ttl_secs: 600,
            enroll_window_start: 1_000,
        };
        assert_eq!(p.nonce_valid_until(), 1_600);

        let open = BootstrapPolicy {
            bootstrap_ttl_secs: 0,
            ..p.clone()
        };
        assert_eq!(open.nonce_valid_until(), 0);
    }
}
