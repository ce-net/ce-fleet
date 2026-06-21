//! Working-cap issuance — the security spine of enrollment, kept pure and fully unit-tested.
//!
//! This is the same delegation discipline `replicator` uses (`onward_abilities` / `attenuate` /
//! `delegate`), specialised for the enrollment case: the delegate holds an org-root chain whose
//! audience is the delegate, and issues an **audience-bound working cap** to one enrolling node.
//! The result is `[..delegate_chain, delegate -> node]`, which `ce-cap::authorize` accepts for the
//! node on any host rooted at the org key.
//!
//! Invariants enforced here (so privilege can only shrink):
//!   1. abilities ⊆ the delegate's own abilities (intersection, never addition);
//!   2. expiry clamped to `min(delegate.not_after, now + ttl)` (a node never outlives its delegate);
//!   3. resource ⊆ the delegate's resource (we narrow `Any` -> `Tag` for the node's fleet tag, but
//!      never widen a delegate that is already tag-confined);
//!   4. audience fixed to the enrolling node id (so a stolen token names the wrong node and fails).

use anyhow::{Result, anyhow, bail};
use ce_cap::{Caveats, Resource, SignedCapability};
use ce_identity::{Identity, NodeId};

/// Parameters for the working cap the delegate issues to a freshly enrolled node.
#[derive(Debug, Clone)]
pub struct WorkingCapParams {
    /// The enrolling node's id (becomes the cap audience — binds the cap to that one machine).
    pub node: NodeId,
    /// Abilities to grant, e.g. `["status", "infer:chat", "infer:summarize"]`. Intersected with
    /// what the delegate actually holds — anything the delegate lacks is silently dropped.
    pub want_abilities: Vec<String>,
    /// Fleet tag this node is scoped to, e.g. `"radiology"`. Narrows the resource to `Tag(tag)`
    /// when the delegate holds `Any`; if the delegate is already tag-confined it must match.
    pub tag: Option<String>,
    /// Requested lifetime in seconds; clamped so the working cap never outlives the delegate's cap.
    pub ttl_secs: u64,
    /// Issuer-chosen nonce for the new link (unique per issuer; names it for revocation).
    pub nonce: u64,
}

/// Intersect the abilities the policy wants with those the delegate actually holds. Delegation can
/// never *add* power, so anything the parent lacks is dropped. Order follows `want`.
pub fn working_abilities(parent_abilities: &[String], want: &[String]) -> Vec<String> {
    want.iter()
        .filter(|a| parent_abilities.iter().any(|p| p == *a))
        .cloned()
        .collect()
}

/// Clamp a child's expiry so it never outlives the parent. `0` means "no bound" (infinite); a
/// `0` parent still yields a finite child (`now + ttl`), exactly like `replicator::attenuate`.
pub fn clamp_not_after(parent_not_after: u64, now: u64, ttl_secs: u64) -> u64 {
    let want = now.saturating_add(ttl_secs);
    match parent_not_after {
        0 => want,
        p => p.min(want),
    }
}

/// Narrow the parent's resource to the node's fleet tag. Returns an error rather than ever widening:
/// if the parent is already confined to a *different* tag, or to a specific node, we refuse.
fn working_resource(parent: &Resource, tag: &Option<String>) -> Result<Resource> {
    match (parent, tag) {
        // Delegate authority over any node → narrow to the requested tag (or stay `Any`).
        (Resource::Any, Some(t)) => Ok(Resource::Tag(t.clone())),
        (Resource::Any, None) => Ok(Resource::Any),
        // Already confined to a tag → the requested tag must match (no widening).
        (Resource::Tag(pt), Some(t)) if pt == t => Ok(Resource::Tag(pt.clone())),
        (Resource::Tag(pt), None) => Ok(Resource::Tag(pt.clone())),
        (Resource::Tag(pt), Some(t)) => {
            bail!("delegate is confined to tag '{pt}', cannot issue for tag '{t}'")
        }
        // `AllOf` / `Node` parents: keep verbatim (the node-set can only stay or shrink, and
        // `ce-cap::is_subset_of` will reject anything that does not hold).
        (other, _) => Ok(other.clone()),
    }
}

/// Issue the audience-bound working cap and return the full chain `[..delegate_chain, child]`.
///
/// `delegate` must be the audience of the last link in `delegate_chain` (it signs the new link).
/// The returned chain is what the node stores in its wallet and presents on every inference/admin
/// request; `ce-cap::authorize` accepts it because each link attenuates and it roots at the org key.
pub fn issue_working_cap(
    delegate_chain: &[SignedCapability],
    delegate: &Identity,
    params: &WorkingCapParams,
    now: u64,
) -> Result<Vec<SignedCapability>> {
    let parent = delegate_chain
        .last()
        .ok_or_else(|| anyhow!("empty delegate capability chain"))?;
    if parent.cap.audience != delegate.node_id() {
        bail!("the signing identity is not the audience of the held delegate capability");
    }

    let abilities = working_abilities(&parent.cap.abilities, &params.want_abilities);
    if abilities.is_empty() {
        bail!(
            "delegate holds none of the requested abilities {:?}",
            params.want_abilities
        );
    }

    let resource = working_resource(&parent.cap.resource, &params.tag)?;
    let caveats = Caveats {
        not_after: clamp_not_after(parent.cap.caveats.not_after, now, params.ttl_secs),
        ..parent.cap.caveats.clone()
    };

    let child = SignedCapability::issue(
        delegate,
        params.node,
        abilities,
        resource,
        caveats,
        params.nonce,
        Some(parent.id()),
    );

    let mut chain = delegate_chain.to_vec();
    chain.push(child);
    Ok(chain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use ce_cap::authorize;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn id(tag: &str) -> Identity {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("enroll-test-{}-{n}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        Identity::load_or_generate(&dir).expect("identity")
    }

    fn root_cap(
        root: &Identity,
        aud: NodeId,
        abilities: &[&str],
        resource: Resource,
        not_after: u64,
    ) -> SignedCapability {
        let caveats = Caveats {
            not_after,
            ..Default::default()
        };
        SignedCapability::issue(
            root,
            aud,
            abilities.iter().map(|s| s.to_string()).collect(),
            resource,
            caveats,
            1,
            None,
        )
    }

    fn never() -> impl Fn(&NodeId, u64) -> bool {
        |_, _| false
    }

    #[test]
    fn working_abilities_is_always_a_subset() {
        let parent = vec![
            "status".to_string(),
            "infer:chat".to_string(),
            "infer:summarize".to_string(),
        ];
        // request a superset → only the intersection is granted (no escalation)
        let got = working_abilities(
            &parent,
            &[
                "infer:chat".to_string(),
                "infer:admin".to_string(), // delegate lacks this → dropped
                "status".to_string(),
            ],
        );
        assert_eq!(got, vec!["infer:chat".to_string(), "status".to_string()]);
    }

    #[test]
    fn clamp_never_outlives_parent() {
        let now = 1_000;
        assert_eq!(clamp_not_after(1_500, now, 10_000), 1_500); // long ttl clamped to parent
        assert_eq!(clamp_not_after(1_500, now, 100), 1_100); // short ttl wins
        assert_eq!(clamp_not_after(0, now, 50), 1_050); // infinite parent → finite child
    }

    #[test]
    fn working_resource_narrows_any_and_refuses_widening() {
        // Any + tag → Tag
        assert_eq!(
            working_resource(&Resource::Any, &Some("radiology".into())).expect("ok"),
            Resource::Tag("radiology".into())
        );
        // already confined to a tag, same tag → ok
        assert!(working_resource(&Resource::Tag("rad".into()), &Some("rad".into())).is_ok());
        // confined to a tag, different tag → refused (would widen)
        assert!(working_resource(&Resource::Tag("rad".into()), &Some("cards".into())).is_err());
    }

    #[test]
    fn issued_working_cap_authorizes_the_node_on_a_fleet_host() {
        let now = 10_000;
        let root = id("wc-root");
        let delegate = id("wc-delegate");
        let node = id("wc-node");
        let host = id("wc-host"); // a fleet node that lists root as an accepted root

        // org root → delegate, broad abilities over any node, finite expiry.
        let chain = vec![root_cap(
            &root,
            delegate.node_id(),
            &["status", "infer:chat", "infer:summarize"],
            Resource::Any,
            now + 100_000,
        )];

        let params = WorkingCapParams {
            node: node.node_id(),
            want_abilities: vec!["status".into(), "infer:chat".into()],
            tag: Some("radiology".into()),
            ttl_secs: 30 * 24 * 3600,
            nonce: 42,
        };
        let working = issue_working_cap(&chain, &delegate, &params, now).expect("issue");
        assert_eq!(working.len(), 2);

        // The node is authorized for the granted ability on a host rooted at the org key...
        assert!(
            authorize(
                &host.node_id(),
                &[root.node_id()],
                &["radiology".into()],
                now,
                &node.node_id(),
                "infer:chat",
                &working,
                &never(),
            )
            .is_ok()
        );
        // ...but NOT for an ability the working cap never granted (summarize was not requested).
        assert!(
            authorize(
                &host.node_id(),
                &[root.node_id()],
                &["radiology".into()],
                now,
                &node.node_id(),
                "infer:summarize",
                &working,
                &never(),
            )
            .is_err()
        );
    }

    #[test]
    fn a_stolen_working_cap_names_the_wrong_node() {
        let now = 10_000;
        let root = id("steal-root");
        let delegate = id("steal-delegate");
        let node = id("steal-node");
        let thief = id("steal-thief");
        let host = id("steal-host");

        let chain = vec![root_cap(
            &root,
            delegate.node_id(),
            &["infer:chat"],
            Resource::Any,
            0,
        )];
        let params = WorkingCapParams {
            node: node.node_id(),
            want_abilities: vec!["infer:chat".into()],
            tag: None,
            ttl_secs: 3600,
            nonce: 7,
        };
        let working = issue_working_cap(&chain, &delegate, &params, now).expect("issue");
        // A thief presenting the node's working cap is rejected: audience binds it to `node`.
        assert!(
            authorize(
                &host.node_id(),
                &[root.node_id()],
                &[],
                now,
                &thief.node_id(),
                "infer:chat",
                &working,
                &never(),
            )
            .is_err()
        );
    }

    #[test]
    fn refuses_when_signer_is_not_the_delegate() {
        let now = 10_000;
        let root = id("ns-root");
        let delegate = id("ns-delegate");
        let stranger = id("ns-stranger");
        let node = id("ns-node");
        let chain = vec![root_cap(
            &root,
            delegate.node_id(),
            &["infer:chat"],
            Resource::Any,
            0,
        )];
        let params = WorkingCapParams {
            node: node.node_id(),
            want_abilities: vec!["infer:chat".into()],
            tag: None,
            ttl_secs: 3600,
            nonce: 7,
        };
        // a stranger (not the chain audience) cannot issue a working cap
        assert!(issue_working_cap(&chain, &stranger, &params, now).is_err());
    }

    #[test]
    fn refuses_when_delegate_holds_none_of_the_requested_abilities() {
        let now = 10_000;
        let root = id("none-root");
        let delegate = id("none-delegate");
        let node = id("none-node");
        let chain = vec![root_cap(
            &root,
            delegate.node_id(),
            &["status"],
            Resource::Any,
            0,
        )];
        let params = WorkingCapParams {
            node: node.node_id(),
            want_abilities: vec!["infer:admin".into()], // delegate has none of this
            tag: None,
            ttl_secs: 3600,
            nonce: 7,
        };
        assert!(issue_working_cap(&chain, &delegate, &params, now).is_err());
    }
}
