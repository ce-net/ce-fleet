//! One-time enroll-token nonce tracking — the single new piece of app-level state.
//!
//! A *reusable bootstrap cap* (Tailscale-authkey style) lets thousands of machines enroll from one
//! short, tag-scoped secret. To stop a leaked bootstrap secret from being replayed forever, every
//! enrollment carries a **one-time nonce**: the delegate burns it on first use and refuses it
//! thereafter. The ledger also expires nonces (a bootstrap cap is short-TTL by design), so memory
//! stays bounded during a 1500-node rollout.
//!
//! Concurrency: a single `Mutex`-guarded map is enough at delegate scale (one delegate per site,
//! tens of enrollments/second during a wave). HA across delegates is an operator concern (shared
//! store / sticky routing) and is documented as a deployment caveat, not implemented here.

use std::collections::HashMap;
use std::sync::Mutex;

/// Why a `claim` was rejected. Returned to callers (safe to log; carries no secret material).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaimError {
    /// The nonce was already burned by an earlier enrollment.
    AlreadyUsed,
    /// The nonce's window has passed; the bootstrap cap should be re-minted.
    Expired,
}

impl std::fmt::Display for ClaimError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClaimError::AlreadyUsed => write!(f, "enrollment nonce already used"),
            ClaimError::Expired => write!(f, "enrollment nonce expired"),
        }
    }
}

impl std::error::Error for ClaimError {}

struct Burned {
    /// Unix seconds after which this record may be garbage-collected.
    expires_at: u64,
}

/// A bounded, expiring set of burned one-time enrollment nonces.
pub struct NonceLedger {
    burned: Mutex<HashMap<String, Burned>>,
    /// How long a burned nonce is remembered (should exceed the bootstrap-cap TTL so a replay
    /// inside the validity window is always caught). Seconds.
    retain_secs: u64,
}

impl NonceLedger {
    /// New ledger remembering burned nonces for `retain_secs` (e.g. one hour — comfortably longer
    /// than a minutes-TTL bootstrap cap).
    pub fn new(retain_secs: u64) -> Self {
        Self {
            burned: Mutex::new(HashMap::new()),
            retain_secs,
        }
    }

    /// Claim a one-time `nonce` at `now`. On success the nonce is burned and future claims fail
    /// with [`ClaimError::AlreadyUsed`]. `valid_until` is the nonce's own expiry (from the
    /// bootstrap cap): claiming an already-expired nonce fails with [`ClaimError::Expired`].
    pub fn claim(&self, nonce: &str, now: u64, valid_until: u64) -> Result<(), ClaimError> {
        if valid_until != 0 && now > valid_until {
            return Err(ClaimError::Expired);
        }
        let mut map = self.burned.lock().unwrap_or_else(|p| p.into_inner());
        // Opportunistic GC so a long-lived delegate doesn't grow unbounded across a rollout.
        map.retain(|_, b| b.expires_at > now);
        if map.contains_key(nonce) {
            return Err(ClaimError::AlreadyUsed);
        }
        map.insert(
            nonce.to_string(),
            Burned {
                expires_at: now.saturating_add(self.retain_secs),
            },
        );
        Ok(())
    }

    /// Number of currently-remembered burned nonces (test/metrics aid).
    pub fn len(&self) -> usize {
        self.burned.lock().unwrap_or_else(|p| p.into_inner()).len()
    }

    /// True if no nonces are currently remembered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn first_claim_succeeds_replay_is_rejected() {
        let led = NonceLedger::new(3600);
        assert!(led.claim("abc", 1_000, 2_000).is_ok());
        assert_eq!(led.claim("abc", 1_001, 2_000), Err(ClaimError::AlreadyUsed));
    }

    #[test]
    fn distinct_nonces_are_independent() {
        let led = NonceLedger::new(3600);
        assert!(led.claim("one", 1_000, 0).is_ok());
        assert!(led.claim("two", 1_000, 0).is_ok());
        assert_eq!(led.len(), 2);
    }

    #[test]
    fn expired_nonce_is_refused_without_being_burned() {
        let led = NonceLedger::new(3600);
        assert_eq!(led.claim("late", 5_000, 4_000), Err(ClaimError::Expired));
        assert!(led.is_empty(), "an expired claim should not consume memory");
    }

    #[test]
    fn burned_records_are_garbage_collected_after_retention() {
        let led = NonceLedger::new(10);
        assert!(led.claim("g", 1_000, 0).is_ok());
        assert_eq!(led.len(), 1);
        // A later claim past the retention window GCs the old burn; the same nonce can be re-used
        // only because its prior burn record has aged out (the bootstrap cap itself is expired by
        // then, so this is safe — TTL is the real gate, the nonce is replay defense within TTL).
        assert!(led.claim("h", 2_000, 0).is_ok());
        assert_eq!(led.len(), 1, "the aged-out 'g' record was collected");
    }

    // ---- extended nonce coverage ----

    #[test]
    fn valid_until_zero_never_expires() {
        let led = NonceLedger::new(3600);
        // valid_until == 0 disables the nonce-expiry check entirely; a far-future `now` still
        // claims fine (no Expired error).
        assert!(led.claim("eternal", 9_000_000_000, 0).is_ok());
        // and an immediate replay within the retention window is still rejected.
        assert_eq!(led.claim("eternal", 9_000_000_001, 0), Err(ClaimError::AlreadyUsed));
    }

    #[test]
    fn claim_exactly_at_valid_until_is_allowed() {
        // The check is `now > valid_until`, so now == valid_until is still in-window.
        let led = NonceLedger::new(3600);
        assert!(led.claim("edge", 2_000, 2_000).is_ok());
    }

    #[test]
    fn retain_secs_overflow_does_not_panic() {
        let led = NonceLedger::new(u64::MAX);
        assert!(led.claim("n", u64::MAX - 1, 0).is_ok());
        assert_eq!(led.len(), 1);
    }

    #[test]
    fn concurrent_claims_of_one_nonce_burn_exactly_once() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};

        let led = Arc::new(NonceLedger::new(3600));
        let wins = Arc::new(AtomicUsize::new(0));
        let mut handles = Vec::new();
        for _ in 0..16 {
            let led = Arc::clone(&led);
            let wins = Arc::clone(&wins);
            handles.push(std::thread::spawn(move || {
                if led.claim("contended", 1_000, 0).is_ok() {
                    wins.fetch_add(1, Ordering::Relaxed);
                }
            }));
        }
        for h in handles {
            h.join().unwrap();
        }
        assert_eq!(wins.load(Ordering::Relaxed), 1, "exactly one racing claim may win");
        assert_eq!(led.len(), 1);
    }
}
