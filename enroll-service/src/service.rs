//! The delegate HTTP service: `/enroll`, `/fleet/rollup`, token generation, and the revoke hook
//! that backs the admin Trust panel. An app over `ce-rs` — no node endpoints are added.
//!
//! The delegate holds an org-root-anchored capability chain (`delegate_chain`) whose audience is
//! the delegate's own identity. On `/enroll` it mints a one-time-nonce-checked, audience-bound
//! working cap (see [`crate::attenuate`]) and returns it; on `/fleet/rollup` it aggregates its
//! subtree's mesh view (see [`crate::rollup`]).

use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Result;
use axum::{
    Json, Router,
    extract::State,
    http::StatusCode,
    response::IntoResponse,
    routing::{get, post},
};
use ce_cap::{SignedCapability, decode_chain, encode_chain};
use ce_identity::{Identity, NodeId};
use ce_rs::{AtlasEntry, CeClient, NodeHistory};
use serde::{Deserialize, Serialize};

use crate::attenuate::{WorkingCapParams, issue_working_cap};
use crate::nonce::NonceLedger;
use crate::rollup::{MeshSource, RollupAggregator, SubtreeRollup};
use crate::token::{BootstrapPolicy, EnrollOutcome, EnrollRequest, secret_eq};

/// Static delegate configuration (identity + the chain it holds + enrollment policy). Loaded once
/// at startup; the running service treats it as immutable.
pub struct DelegateConfig {
    /// The delegate's signing identity (must be the audience of the last link in `delegate_chain`).
    pub identity: Identity,
    /// `[..org-root-chain, root->delegate]` — the authority this delegate attenuates from.
    pub delegate_chain: Vec<SignedCapability>,
    /// The bootstrap-cap policy this delegate enforces.
    pub policy: BootstrapPolicy,
    /// Seconds before a node's atlas `last_seen` marks it offline in the rollup.
    pub fresh_window_secs: u64,
}

/// Shared service state behind the axum router.
#[derive(Clone)]
pub struct AppState {
    pub client: Arc<CeClient>,
    pub config: Arc<DelegateConfig>,
    pub nonces: Arc<NonceLedger>,
}

fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or_default()
}

/// Build the delegate's axum router. Wire it to a `TcpListener` in `main`.
pub fn router(state: AppState) -> Router {
    Router::new()
        .route("/healthz", get(healthz))
        .route("/enroll", post(enroll))
        .route("/fleet/rollup", get(fleet_rollup))
        .route("/fleet/token", post(gen_token))
        .route("/fleet/revoked", get(revoked))
        .route("/fleet/revoke", post(revoke))
        .with_state(state)
}

async fn healthz() -> impl IntoResponse {
    Json(serde_json::json!({ "ok": true, "service": "ce-fleet-enroll" }))
}

// ---------- /enroll ----------

async fn enroll(State(st): State<AppState>, Json(req): Json<EnrollRequest>) -> impl IntoResponse {
    match do_enroll(&st, &req, now()) {
        Ok(outcome) => (StatusCode::OK, Json(outcome)).into_response(),
        Err(e) => {
            // Never echo the bootstrap secret; the request struct keeps it out of Display anyway.
            tracing::warn!(node = %req.node_id, hostname = %req.hostname, error = %e, "enroll rejected");
            (
                StatusCode::FORBIDDEN,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

/// Pure-ish enrollment: validate the bootstrap secret, burn the one-time nonce, mint the working
/// cap. Separated from the axum glue so it is directly unit-testable.
pub fn do_enroll(st: &AppState, req: &EnrollRequest, now: u64) -> Result<EnrollOutcome> {
    let cfg = &st.config;
    // 1. Authenticate the bootstrap secret (Tailscale-authkey bearer check, constant-time-ish).
    if !secret_eq(&req.bootstrap_secret, &cfg.policy.bootstrap_secret) {
        anyhow::bail!("invalid bootstrap secret");
    }
    // 2. Burn the one-time nonce (replay defense within the bootstrap-cap TTL).
    st.nonces
        .claim(&req.nonce, now, cfg.policy.nonce_valid_until())
        .map_err(|e| anyhow::anyhow!("{e}"))?;
    // 3. Parse the enrolling node id (the working cap's audience).
    let node = parse_node_id(&req.node_id)?;
    // 4. Mint the audience-bound working cap, attenuated from the delegate's chain.
    let params = WorkingCapParams {
        node,
        want_abilities: cfg.policy.abilities.clone(),
        tag: cfg.policy.tag.clone(),
        ttl_secs: cfg.policy.working_ttl_secs,
        nonce: now.wrapping_add(node[0] as u64), // delegate-unique link nonce
    };
    let chain = issue_working_cap(&cfg.delegate_chain, &cfg.identity, &params, now)?;
    let leaf = chain
        .last()
        .ok_or_else(|| anyhow::anyhow!("issued empty chain"))?;
    tracing::info!(node = %req.node_id, tier = %req.tier, os = %req.os, "node enrolled");
    Ok(EnrollOutcome {
        working_cap: encode_chain(&chain),
        abilities: leaf.cap.abilities.clone(),
        tag: cfg.policy.tag.clone(),
        not_after: leaf.cap.caveats.not_after,
    })
}

// ---------- /fleet/rollup ----------

/// Live `MeshSource` over the delegate's local CE node. History is best-effort.
struct ClientSource(Arc<CeClient>);

impl MeshSource for ClientSource {
    async fn atlas(&self) -> Result<Vec<AtlasEntry>> {
        self.0.atlas().await
    }
    async fn history(&self, node_id: &str) -> Result<Option<NodeHistory>> {
        match self.0.history(node_id).await {
            Ok(h) => Ok(Some(h)),
            Err(_) => Ok(None),
        }
    }
}

async fn fleet_rollup(State(st): State<AppState>) -> impl IntoResponse {
    let agg = RollupAggregator::new(
        ClientSource(st.client.clone()),
        st.config.fresh_window_secs,
    );
    match agg.rollup().await {
        Ok(r) => (StatusCode::OK, Json(r)).into_response(),
        Err(e) => {
            tracing::warn!(error = %e, "rollup failed");
            (
                StatusCode::BAD_GATEWAY,
                Json(serde_json::json!({ "error": e.to_string() })),
            )
                .into_response()
        }
    }
}

/// Re-exported for callers/tests that want to build a rollup without the HTTP layer.
pub async fn build_rollup(st: &AppState) -> Result<SubtreeRollup> {
    RollupAggregator::new(ClientSource(st.client.clone()), st.config.fresh_window_secs)
        .rollup()
        .await
}

// ---------- /fleet/token (admin Trust panel: "Generate enrollment token") ----------

#[derive(Debug, Deserialize)]
struct GenTokenReq {
    /// Optional node id to bind the enrollment cap to (audience); omitted = reusable bootstrap mode.
    #[serde(default)]
    node_id: Option<String>,
    /// Abilities the enrollment cap grants (e.g. `["status","infer:chat"]`).
    #[serde(default)]
    abilities: Vec<String>,
    /// Fleet tag scope (e.g. `"radiology"`).
    #[serde(default)]
    tag: Option<String>,
    /// TTL in seconds (clamped to the delegate's own expiry).
    ttl_secs: u64,
}

#[derive(Debug, Serialize)]
struct GenTokenResp {
    /// The enrollment-cap token (hex chain). For reusable bootstrap mode this is the bearer the
    /// node presents; the delegate still requires a fresh one-time nonce per `/enroll`.
    token: String,
    abilities: Vec<String>,
    tag: Option<String>,
    not_after: u64,
    /// A fresh one-time nonce for a single enrollment (admin can paste it into a single node's
    /// `CE_ENROLL_TOKEN` flow). For mass rollout the node generates its own nonce per boot.
    nonce: String,
}

async fn gen_token(State(st): State<AppState>, Json(req): Json<GenTokenReq>) -> impl IntoResponse {
    match do_gen_token(&st, &req, now()) {
        Ok(resp) => (StatusCode::OK, Json(resp)).into_response(),
        Err(e) => (
            StatusCode::BAD_REQUEST,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

fn do_gen_token(st: &AppState, req: &GenTokenReq, now: u64) -> Result<GenTokenResp> {
    let cfg = &st.config;
    // A bound enrollment cap (node_id given) is just an audience-bound working cap with a short TTL.
    // A reusable bootstrap cap omits the node binding; we issue it to the delegate itself as a
    // placeholder audience so the chain stays well-formed, and rely on the per-enroll nonce + the
    // node-bound working cap issued at /enroll for the real binding.
    let audience = match &req.node_id {
        Some(nid) => parse_node_id(nid)?,
        None => cfg.identity.node_id(),
    };
    let params = WorkingCapParams {
        node: audience,
        want_abilities: req.abilities.clone(),
        tag: req.tag.clone().or_else(|| cfg.policy.tag.clone()),
        ttl_secs: req.ttl_secs,
        nonce: now,
    };
    let chain = issue_working_cap(&cfg.delegate_chain, &cfg.identity, &params, now)?;
    let leaf = chain
        .last()
        .ok_or_else(|| anyhow::anyhow!("issued empty chain"))?;
    Ok(GenTokenResp {
        token: encode_chain(&chain),
        abilities: leaf.cap.abilities.clone(),
        tag: params.tag.clone(),
        not_after: leaf.cap.caveats.not_after,
        nonce: fresh_nonce(now, &audience),
    })
}

/// A deterministic-but-unguessable one-time nonce string: hash(delegate-secret-free public inputs).
/// Uses the audience + a monotonic-ish `now` so two calls in the same second still differ via the
/// audience; for true uniqueness the caller (node) generates its own per boot.
fn fresh_nonce(now: u64, audience: &NodeId) -> String {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"ce-fleet-enroll-nonce");
    h.update(now.to_le_bytes());
    h.update(audience);
    hex::encode(&h.finalize()[..16])
}

// ---------- /fleet/revoked + /fleet/revoke (admin Trust panel) ----------

async fn revoked(State(st): State<AppState>) -> impl IntoResponse {
    match st.client.revoked().await {
        Ok(set) => {
            let items: Vec<_> = set
                .into_iter()
                .map(|(issuer, nonce)| serde_json::json!({ "issuer": issuer, "nonce": nonce }))
                .collect();
            (StatusCode::OK, Json(serde_json::json!({ "revoked": items }))).into_response()
        }
        Err(e) => (
            StatusCode::BAD_GATEWAY,
            Json(serde_json::json!({ "error": e.to_string() })),
        )
            .into_response(),
    }
}

#[derive(Debug, Deserialize)]
struct RevokeReq {
    /// Issuer (hex node id) of the cap link to revoke.
    issuer: String,
    /// The link's nonce.
    nonce: u64,
}

async fn revoke(State(_st): State<AppState>, Json(req): Json<RevokeReq>) -> impl IntoResponse {
    // Revocation is an ON-CHAIN action (`RevokeCapability` tx) and is authoritative only when
    // signed by the org root or a delegate that issued the link — and the org root is OFFLINE by
    // design (trust spine §0). ce-rs exposes the revoked SET (read) but no revoke-write method, so
    // the delegate cannot itself broadcast the tx here. We return the exact revocation instruction
    // for an admin holding the issuing key to sign and submit (e.g. `ce revoke <issuer> <nonce>`).
    //
    // TODO: when ce-rs gains a `revoke()` write that submits a RevokeCapability tx signed by a
    // delegate-held key for links the delegate itself issued, perform it here directly.
    tracing::info!(issuer = %req.issuer, nonce = req.nonce, "revoke instruction requested");
    (
        StatusCode::ACCEPTED,
        Json(serde_json::json!({
            "status": "pending_signature",
            "instruction": format!("ce revoke {} {}", req.issuer, req.nonce),
            "note": "Revocation is an on-chain RevokeCapability tx signed by the issuing key (org root is offline by design). This subtree-kills the link and everything delegated under it."
        })),
    )
        .into_response()
}

// ---------- helpers ----------

fn parse_node_id(s: &str) -> Result<NodeId> {
    let b = hex::decode(s.trim()).map_err(|_| anyhow::anyhow!("node id must be hex"))?;
    b.try_into()
        .map_err(|_| anyhow::anyhow!("node id must be 32 bytes (64 hex chars)"))
}

/// Load a delegate chain token from a file (the org-root-issued cap the delegate holds).
pub fn load_delegate_chain(token: &str) -> Result<Vec<SignedCapability>> {
    decode_chain(token).map_err(|_| anyhow::anyhow!("bad delegate cap token"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use ce_cap::{Caveats, Resource};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn id(tag: &str) -> Identity {
        static SEQ: AtomicU64 = AtomicU64::new(0);
        let n = SEQ.fetch_add(1, Ordering::Relaxed);
        let dir = std::env::temp_dir().join(format!("svc-test-{}-{n}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("temp dir");
        Identity::load_or_generate(&dir).expect("identity")
    }

    fn state_with(policy: BootstrapPolicy) -> (AppState, Identity, Identity) {
        let root = id("svc-root");
        let delegate = id("svc-delegate");
        let chain = vec![SignedCapability::issue(
            &root,
            delegate.node_id(),
            vec!["status".into(), "infer:chat".into(), "infer:summarize".into()],
            Resource::Any,
            Caveats {
                not_after: 0,
                ..Default::default()
            },
            1,
            None,
        )];
        let cfg = DelegateConfig {
            identity: delegate,
            delegate_chain: chain,
            policy,
            fresh_window_secs: 180,
        };
        let delegate_clone = id("svc-unused"); // not used, keeps signature simple
        let st = AppState {
            client: Arc::new(CeClient::new("http://127.0.0.1:1".to_string())),
            config: Arc::new(cfg),
            nonces: Arc::new(NonceLedger::new(3600)),
        };
        (st, root, delegate_clone)
    }

    fn policy() -> BootstrapPolicy {
        BootstrapPolicy {
            bootstrap_secret: "rollout-2026".into(),
            tag: Some("radiology".into()),
            abilities: vec!["status".into(), "infer:chat".into()],
            working_ttl_secs: 30 * 24 * 3600,
            bootstrap_ttl_secs: 0,
            enroll_window_start: 0,
        }
    }

    #[test]
    fn enroll_happy_path_issues_a_working_cap() {
        let (st, root, _) = state_with(policy());
        let node = id("svc-node");
        let req = EnrollRequest {
            node_id: node.node_id_hex(),
            hostname: "rad-ws-01".into(),
            os: "windows".into(),
            tier: "GpuMid".into(),
            nonce: "nonce-001".into(),
            bootstrap_secret: "rollout-2026".into(),
        };
        let now = 1_000_000;
        let outcome = do_enroll(&st, &req, now).expect("enroll");
        assert_eq!(outcome.abilities, vec!["status", "infer:chat"]);
        assert_eq!(outcome.tag.as_deref(), Some("radiology"));
        assert!(outcome.not_after > now);

        // The returned chain authorizes the node for infer:chat on a fleet host rooted at the org key.
        let chain = decode_chain(&outcome.working_cap).expect("decode");
        let host = id("svc-host");
        let never = |_: &NodeId, _: u64| false;
        assert!(
            ce_cap::authorize(
                &host.node_id(),
                &[root.node_id()],
                &["radiology".into()],
                now,
                &node.node_id(),
                "infer:chat",
                &chain,
                &never,
            )
            .is_ok()
        );
    }

    #[test]
    fn enroll_rejects_wrong_secret() {
        let (st, _, _) = state_with(policy());
        let node = id("svc-node2");
        let req = EnrollRequest {
            node_id: node.node_id_hex(),
            hostname: "h".into(),
            os: "linux".into(),
            tier: "CpuLow".into(),
            nonce: "n".into(),
            bootstrap_secret: "WRONG".into(),
        };
        assert!(do_enroll(&st, &req, 1_000).is_err());
    }

    #[test]
    fn enroll_nonce_is_one_time() {
        let (st, _, _) = state_with(policy());
        let node = id("svc-node3");
        let mk = |nonce: &str| EnrollRequest {
            node_id: node.node_id_hex(),
            hostname: "h".into(),
            os: "linux".into(),
            tier: "CpuLow".into(),
            nonce: nonce.into(),
            bootstrap_secret: "rollout-2026".into(),
        };
        assert!(do_enroll(&st, &mk("dup"), 1_000).is_ok());
        assert!(do_enroll(&st, &mk("dup"), 1_001).is_err(), "replayed nonce must be refused");
        assert!(do_enroll(&st, &mk("fresh"), 1_002).is_ok(), "a fresh nonce still works");
    }

    #[test]
    fn gen_token_bound_to_a_node_is_audience_scoped() {
        let (st, root, _) = state_with(policy());
        let node = id("svc-gt-node");
        let req = GenTokenReq {
            node_id: Some(node.node_id_hex()),
            abilities: vec!["status".into(), "infer:chat".into()],
            tag: Some("radiology".into()),
            ttl_secs: 2 * 3600,
        };
        let now = 2_000_000;
        let resp = do_gen_token(&st, &req, now).expect("token");
        assert!(resp.not_after <= now + 2 * 3600 && resp.not_after > now);
        let chain = decode_chain(&resp.token).expect("decode");
        let host = id("svc-gt-host");
        let never = |_: &NodeId, _: u64| false;
        assert!(
            ce_cap::authorize(
                &host.node_id(),
                &[root.node_id()],
                &["radiology".into()],
                now,
                &node.node_id(),
                "infer:chat",
                &chain,
                &never,
            )
            .is_ok()
        );
    }
}
