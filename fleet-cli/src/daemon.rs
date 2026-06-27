//! The pairing daemon: runs on every device, supervised by the ce service.
//!
//! It does two things at once:
//!   1. Serves the `ce-fleet/pair` mesh topic — when another device asks to pair, it parks the
//!      request and waits for a human to Accept/Deny. On Accept it mints a self-signed capability
//!      authorizing the requester over THIS node (issuer-self is always an accepted root, so no
//!      roots edit or restart is needed) and ships the token back over the same request/reply.
//!   2. Hosts a tiny local web UI (loopback only) that lists nearby nodes and shows the pending
//!      Accept/Deny prompts. The `ce-fleet pending|accept|deny` CLI talks to the same local API.

use anyhow::{Context, Result};
use ce_cap::{encode_chain, Caveats, Resource, SignedCapability};
use ce_identity::Identity;
use ce_rs::serve::{Handler, Request};
use ce_rs::CeClient;
use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::oneshot;

use crate::proto::{PairRequest, PairResponse, TOPIC};
use crate::util::{now_secs, parse_node_id};

/// How long the daemon parks a pairing request waiting for a human, before auto-denying. Kept under
/// the requester's request timeout (see `cmd_pair`) so A always gets a clean answer.
const DECISION_TIMEOUT: Duration = Duration::from_secs(170);

struct Pending {
    req: PairRequest,
    created: u64,
    decide: Option<oneshot::Sender<bool>>,
}

/// Shared daemon state, behind an `Arc` for the serve loop + every axum handler.
pub struct DaemonState {
    identity: Identity,
    data_dir: PathBuf,
    ce: CeClient,
    pending: Mutex<HashMap<u64, Pending>>,
    next_id: AtomicU64,
}

impl DaemonState {
    pub fn new(identity: Identity, data_dir: PathBuf, ce: CeClient) -> Arc<Self> {
        Arc::new(Self {
            identity,
            data_dir,
            ce,
            pending: Mutex::new(HashMap::new()),
            next_id: AtomicU64::new(1),
        })
    }

    /// Resolve a pending request: send the decision to the waiting serve handler. Returns false if
    /// the id is unknown or was already decided.
    fn decide(&self, id: u64, accept: bool) -> bool {
        let tx = {
            let mut g = self.pending.lock().expect("pending lock");
            g.get_mut(&id).and_then(|p| p.decide.take())
        };
        match tx {
            Some(tx) => tx.send(accept).is_ok(),
            None => false,
        }
    }

    fn snapshot(&self) -> Vec<PendingView> {
        let now = now_secs();
        let g = self.pending.lock().expect("pending lock");
        let mut v: Vec<PendingView> = g
            .iter()
            .filter(|(_, p)| p.decide.is_some())
            .map(|(id, p)| PendingView {
                id: *id,
                from_node: p.req.from_node.clone(),
                label: p.req.label.clone(),
                abilities: p.req.abilities.clone(),
                ttl_secs: p.req.ttl_secs,
                want_roots: p.req.want_roots,
                age_secs: now.saturating_sub(p.created),
            })
            .collect();
        v.sort_by_key(|p| p.id);
        v
    }
}

#[derive(serde::Serialize)]
struct PendingView {
    id: u64,
    from_node: String,
    label: String,
    abilities: Vec<String>,
    ttl_secs: u64,
    want_roots: bool,
    age_secs: u64,
}

/// The mesh request handler — one per daemon, wrapping shared state.
struct PairHandler(Arc<DaemonState>);

impl Handler for PairHandler {
    async fn handle(&self, req: Request) -> Vec<u8> {
        let resp = self.handle_inner(req).await;
        serde_json::to_vec(&resp).unwrap_or_else(|_| b"{}".to_vec())
    }
}

impl PairHandler {
    async fn handle_inner(&self, req: Request) -> PairResponse {
        if req.topic != TOPIC {
            return PairResponse::denied("wrong topic");
        }
        let pr: PairRequest = match serde_json::from_slice(&req.payload) {
            Ok(p) => p,
            Err(e) => return PairResponse::denied(format!("bad request: {e}")),
        };
        // The node already authenticated the sender; reject if the claimed node id disagrees.
        if pr.from_node != req.from {
            return PairResponse::denied("sender identity mismatch");
        }
        if pr.abilities.is_empty() {
            return PairResponse::denied("no abilities requested");
        }

        // Park the request and wait for a human (or timeout).
        let (tx, rx) = oneshot::channel::<bool>();
        let id = self.0.next_id.fetch_add(1, Ordering::Relaxed);
        {
            let mut g = self.0.pending.lock().expect("pending lock");
            g.insert(
                id,
                Pending {
                    req: pr.clone(),
                    created: now_secs(),
                    decide: Some(tx),
                },
            );
        }
        tracing::info!(%id, from = %pr.from_node, label = %pr.label, "pairing request parked");

        let accepted = match tokio::time::timeout(DECISION_TIMEOUT, rx).await {
            Ok(Ok(decision)) => decision,
            _ => false, // timed out, or the sender was dropped
        };
        self.0.pending.lock().expect("pending lock").remove(&id);

        if !accepted {
            tracing::info!(%id, "pairing request denied/expired");
            return PairResponse::denied("denied or timed out");
        }

        // Accept: mint a self-signed capability granting the requester over THIS node.
        match self.mint(&pr) {
            Ok(resp) => {
                tracing::info!(%id, from = %pr.from_node, "pairing accepted; capability issued");
                resp
            }
            Err(e) => PairResponse::denied(format!("mint failed: {e}")),
        }
    }

    fn mint(&self, pr: &PairRequest) -> Result<PairResponse> {
        let audience = parse_node_id(&pr.from_node)?;
        let now = now_secs();
        let not_after = if pr.ttl_secs == 0 { 0 } else { now + pr.ttl_secs };
        let caveats = Caveats {
            not_after,
            ..Default::default()
        };
        let cap = SignedCapability::issue(
            &self.0.identity,
            audience,
            pr.abilities.clone(),
            Resource::Node(self.0.identity.node_id()),
            caveats,
            now, // nonce = issue time (revoke with `ce revoke <nonce>`)
            None,
        );
        let token = encode_chain(&[cap]);

        let mut rooted = false;
        if pr.want_roots {
            rooted = append_root(&self.0.data_dir, &pr.from_node).unwrap_or(false);
        }

        Ok(PairResponse::Accepted {
            token,
            granted: pr.abilities.clone(),
            not_after,
            issuer: self.0.identity.node_id_hex(),
            issuer_label: crate::util::device_label(&self.0.identity.node_id_hex()),
            rooted,
        })
    }
}

/// Append a node id to `<data_dir>/roots` if not already present. Returns true if the file now lists
/// it. NOTE: the node reads `roots` only at startup, so this takes effect after the ce service
/// restarts; the capability alone already authorizes the peer.
fn append_root(data_dir: &std::path::Path, node_id_hex: &str) -> Result<bool> {
    use std::io::Write;
    let path = data_dir.join("roots");
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    if existing
        .lines()
        .any(|l| l.split('#').next().unwrap_or("").trim() == node_id_hex)
    {
        return Ok(true);
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .with_context(|| format!("opening {}", path.display()))?;
    writeln!(f, "{node_id_hex}").with_context(|| format!("writing {}", path.display()))?;
    Ok(true)
}

// ----- local web UI + control API (loopback only) -----

pub async fn run(state: Arc<DaemonState>, ui_port: u16) -> Result<()> {
    use axum::routing::{get, post};
    use axum::Router;

    let app = Router::new()
        .route("/", get(ui_index))
        .route("/api/pending", get(api_pending))
        .route("/api/nodes", get(api_nodes))
        .route("/api/decide", post(api_decide))
        .route("/api/pair", post(api_pair))
        .with_state(state.clone());

    let addr = format!("127.0.0.1:{ui_port}");
    let listener = tokio::net::TcpListener::bind(&addr)
        .await
        .with_context(|| format!("binding {addr}"))?;
    tracing::info!("ce-fleet daemon: serving topic {TOPIC}; admin UI on http://{addr}");

    let handler = PairHandler(state.clone());
    let serve_fut = ce_rs::serve::serve(&state.ce, &[TOPIC], &handler, std::future::pending::<()>());
    let ui_fut = async move {
        axum::serve(listener, app)
            .await
            .map_err(anyhow::Error::from)
    };

    tokio::select! {
        r = serve_fut => r.context("mesh serve loop ended")?,
        r = ui_fut => r.context("admin UI server ended")?,
    }
    Ok(())
}

async fn ui_index() -> axum::response::Html<&'static str> {
    axum::response::Html(INDEX_HTML)
}

async fn api_pending(
    axum::extract::State(state): axum::extract::State<Arc<DaemonState>>,
) -> axum::Json<Vec<PendingView>> {
    axum::Json(state.snapshot())
}

async fn api_nodes(
    axum::extract::State(state): axum::extract::State<Arc<DaemonState>>,
) -> axum::Json<serde_json::Value> {
    let me = state.identity.node_id_hex();
    match state.ce.atlas().await {
        Ok(list) => {
            let nodes: Vec<serde_json::Value> = list
                .into_iter()
                .map(|e| {
                    serde_json::json!({
                        "node_id": e.node_id,
                        "is_self": e.node_id == me,
                        "cpu_cores": e.cpu_cores,
                        "mem_mb": e.mem_mb,
                        "running_jobs": e.running_jobs,
                        "last_seen_secs": e.last_seen_secs,
                        "tags": e.tags,
                    })
                })
                .collect();
            axum::Json(serde_json::json!({ "self": me, "nodes": nodes }))
        }
        Err(e) => axum::Json(serde_json::json!({ "self": me, "error": e.to_string(), "nodes": [] })),
    }
}

#[derive(serde::Deserialize)]
struct DecideBody {
    id: u64,
    accept: bool,
}

async fn api_decide(
    axum::extract::State(state): axum::extract::State<Arc<DaemonState>>,
    axum::Json(body): axum::Json<DecideBody>,
) -> axum::Json<serde_json::Value> {
    let ok = state.decide(body.id, body.accept);
    axum::Json(serde_json::json!({ "ok": ok }))
}

#[derive(serde::Deserialize)]
struct PairBody {
    node_id: String,
    #[serde(default)]
    abilities: Option<Vec<String>>,
    #[serde(default)]
    ttl_secs: Option<u64>,
    #[serde(default)]
    want_roots: bool,
}

/// Initiate pairing FROM this device to `node_id` (the "Pair" button). Blocks until the peer's
/// operator decides, then returns the outcome (and stores the capability on accept).
async fn api_pair(
    axum::extract::State(state): axum::extract::State<Arc<DaemonState>>,
    axum::Json(body): axum::Json<PairBody>,
) -> axum::Json<serde_json::Value> {
    let abilities = body.abilities.unwrap_or_else(crate::proto::default_abilities);
    let ttl = body.ttl_secs.unwrap_or(0);
    let label = crate::util::device_label(&state.identity.node_id_hex());
    let outcome = crate::pair::initiate(
        &state.ce,
        &state.identity,
        &state.data_dir,
        &body.node_id,
        abilities,
        ttl,
        body.want_roots,
        &label,
    )
    .await;
    match outcome {
        Ok(o) => axum::Json(serde_json::to_value(o).unwrap_or_default()),
        Err(e) => axum::Json(serde_json::json!({ "accepted": false, "reason": e.to_string() })),
    }
}

const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"/>
<meta name="viewport" content="width=device-width, initial-scale=1"/>
<title>ce-fleet</title>
<style>
  :root { color-scheme: light dark; }
  body { font: 15px/1.5 system-ui, sans-serif; margin: 0; padding: 24px; max-width: 760px; }
  h1 { font-size: 20px; margin: 0 0 4px; }
  .sub { color: #888; margin: 0 0 24px; }
  h2 { font-size: 14px; text-transform: uppercase; letter-spacing: .04em; color: #888; margin: 28px 0 8px; }
  .card { border: 1px solid #8884; border-radius: 10px; padding: 14px 16px; margin: 8px 0; }
  .req { border-color: #e0a800; background: #e0a80018; }
  .row { display: flex; align-items: center; gap: 10px; flex-wrap: wrap; }
  .grow { flex: 1; min-width: 160px; }
  .name { font-weight: 600; }
  .mono { font-family: ui-monospace, monospace; font-size: 12px; color: #888; }
  .tag { font-size: 11px; background: #8882; border-radius: 5px; padding: 1px 6px; margin-right: 4px; }
  button { font: inherit; border: 0; border-radius: 8px; padding: 7px 14px; cursor: pointer; }
  .ok { background: #1a8917; color: #fff; }
  .no { background: #c0392b; color: #fff; }
  .dot { width: 8px; height: 8px; border-radius: 50%; display: inline-block; }
  .on { background: #1a8917; } .off { background: #aaa; }
  .empty { color: #999; font-style: italic; }
</style></head>
<body>
  <h1>ce-fleet</h1>
  <p class="sub">Pair your devices. Approve a request here to grant it access to this machine.</p>

  <h2>Pairing requests</h2>
  <div id="requests"></div>

  <h2>Nearby nodes</h2>
  <div id="nodes"></div>

<script>
async function decide(id, accept) {
  await fetch('/api/decide', { method:'POST', headers:{'content-type':'application/json'},
    body: JSON.stringify({ id, accept }) });
  refresh();
}
async function pairNode(nodeId, btn) {
  btn.disabled = true; btn.textContent = 'Waiting for approval...';
  try {
    const o = await (await fetch('/api/pair', { method:'POST', headers:{'content-type':'application/json'},
      body: JSON.stringify({ node_id: nodeId }) })).json();
    alert(o.accepted ? ('Paired. Saved as "' + o.alias + '".')
                     : ('Not paired: ' + (o.reason || 'declined')));
  } catch (e) { alert('Pair failed: ' + e); }
  btn.disabled = false; btn.textContent = 'Pair';
  refresh();
}
function esc(s){ return String(s).replace(/[&<>]/g, c=>({'&':'&amp;','<':'&lt;','>':'&gt;'}[c])); }
async function refresh() {
  try {
    const reqs = await (await fetch('/api/pending')).json();
    const rd = document.getElementById('requests');
    rd.innerHTML = reqs.length ? '' : '<p class="empty">No pending requests.</p>';
    for (const r of reqs) {
      const abil = r.abilities.map(a=>`<span class="tag">${esc(a)}</span>`).join('');
      const ttl = r.ttl_secs === 0 ? 'never expires' : `expires in ${Math.round(r.ttl_secs/86400)}d`;
      const roots = r.want_roots ? ' &middot; <b>requests owner (roots)</b>' : '';
      const el = document.createElement('div');
      el.className = 'card req';
      el.innerHTML = `<div class="row"><div class="grow">
          <div class="name">${esc(r.label)}</div>
          <div class="mono">${esc(r.from_node)}</div>
          <div style="margin-top:6px">${abil}</div>
          <div class="mono" style="margin-top:6px">${ttl}${roots}</div>
        </div>
        <button class="ok" onclick="decide(${r.id},true)">Accept</button>
        <button class="no" onclick="decide(${r.id},false)">Deny</button>
      </div>`;
      rd.appendChild(el);
    }
    const data = await (await fetch('/api/nodes')).json();
    const nd = document.getElementById('nodes');
    const list = data.nodes || [];
    nd.innerHTML = list.length ? '' : '<p class="empty">No nodes seen yet.</p>';
    const now = Math.floor(Date.now()/1000);
    for (const n of list) {
      const online = (now - n.last_seen_secs) < 120;
      const tags = (n.tags||[]).map(t=>`<span class="tag">${esc(t)}</span>`).join('');
      const el = document.createElement('div');
      el.className = 'card';
      const action = n.is_self ? ''
        : `<button class="ok" onclick="pairNode('${esc(n.node_id)}', this)">Pair</button>`;
      el.innerHTML = `<div class="row">
          <span class="dot ${online?'on':'off'}"></span>
          <div class="grow">
            <div class="name">${n.is_self?'this machine':esc(n.node_id.slice(0,12))} ${tags}</div>
            <div class="mono">${esc(n.node_id)}</div>
          </div>
          <div class="mono">${n.cpu_cores} cores &middot; ${Math.round(n.mem_mb/1024)} GB</div>
          ${action}
        </div>`;
      nd.appendChild(el);
    }
  } catch (e) { /* node not up yet; retry on next tick */ }
}
refresh();
setInterval(refresh, 2000);
</script>
</body></html>"#;
