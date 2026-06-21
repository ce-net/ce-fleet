# Fleet enrollment, trust, and air-gap

This document describes how a machine joins the fleet, why it is safe, and how PHI is kept on the
LAN. It complements `ce/docs/capabilities.md` (the capability model) and `PLAN/09-hospital-inference.md`.

## 1. The trust spine

```
ce-root (offline Ed25519 key, HSM)
   │  PUBLIC key pinned in every node's <data_dir>/roots/ce-root.pub
   ▼
regional delegate  ── holds [ce-root → delegate] cap (audience = delegate)
   │  runs ce-fleet-enroll: POST /enroll, GET /fleet/rollup
   ▼
fleet node         ── receives [ce-root → delegate → node] working cap (audience = node)
```

A node honors a request iff the presented `ce-cap` chain roots at a key in its `roots/` (the org
root) and every link attenuates down to the node as audience. **File placement is not authority:**
the installer drops the org *public* key (a pin) and the binaries; the *working cap* is minted by
the delegate at enroll time and bound to the node's id. A poisoned binary mirror therefore grants no
fleet authority.

## 2. The one-click enroll flow

1. The package installs binaries + the root pubkey + service units, and starts the node with
   `ce start --no-mine` (LAN-only). The node gets an id and joins the LAN mesh (mDNS).
2. The first-boot `ce-enroll` oneshot (`ce-enroll.service` / the Windows `ce-enroll` task) runs:
   - reads the node id from the local API,
   - probes the inference tier (`ce-infer probe`),
   - generates a fresh **one-time nonce**,
   - `POST {node_id, hostname, os, tier, nonce, bootstrap_secret}` to the LAN delegate `/enroll`.
3. The delegate (`ce-fleet-enroll`):
   - checks the **bootstrap secret** (Tailscale-authkey bearer, constant-time-ish),
   - **burns the one-time nonce** (replay defense within the bootstrap TTL; see `nonce.rs`),
   - mints an **audience-bound working cap** attenuated from its org-root chain
     (abilities ⊆ delegate, expiry ≤ delegate, resource narrowed to the fleet `tag`; see `attenuate.rs`),
   - returns the working-cap token.
4. The node stores the working cap in its wallet, writes `enrolled`, and exits 0. The worker can
   now authorize inference. The node appears **LIVE** in the swarm console within seconds.

Zero clinician steps. For kiosks, the delegate's `/fleet/token` mints a short-code/QR enroll token
(the console's Trust panel surfaces it).

## 3. Reusable bootstrap caps (mass rollout)

A single short-TTL, tag-scoped **bootstrap secret** lets thousands of machines enroll. To stop a
leaked secret from being replayed forever, every enrollment carries a **one-time nonce** the delegate
burns on first use (`NonceLedger`). The nonce ledger expires entries so memory stays bounded during a
1500-node wave. HA across delegates (shared store / sticky routing) is an operator concern — a single
delegate handles a site's wave comfortably; the spec flags cross-delegate HA as a deployment caveat.

## 4. P2P propagation (replicator)

After ~1 seed per subnet is enrolled via SCCM/Ansible, do **not** push from one console to 1500
nodes. Each seed runs `replicator seed <targets> --depth 2/3` (see `packaging/linux/seed-fanout.sh`),
fanning binaries + the GGUF out as an attenuating tree — O(log N) depth, every hop strictly weaker,
leaves get `sync` only. SCCM/Ansible remains the audited install-of-record; replicator is the fast
content path. This is verbatim reuse of `replicator/src/main.rs`.

## 5. Air-gap posture

- **No phone-home:** every packaged service starts the node without `ce-net.com/bootstrap`, without a
  public relay, without internet DCUtR. Mesh is LAN-only (mDNS + static LAN multiaddrs).
- **Egress firewall:** `packaging/linux/ce-fleet-egress.nft` (Linux) and per-binary outbound-block
  rules (Windows) default-deny non-LAN outbound. PHI in the data plane physically cannot egress.
- **CI guard:** `enroll-service/tests/airgap.rs` asserts no packaging artifact contains a public
  address/bootstrap token and that every node-start uses `--no-mine`; it also asserts the egress
  ruleset ships and default-denies. A live-socket assertion is sketched (ignored) for the air-gap
  validation harness.
- **Weights:** GGUF models + the engine arrive via the package or the internal blob LAN (`ce-pin`),
  CID-verified on load — never the internet.

## 6. Removal / revocation

- **Expiry:** working caps are finite (default 30 days); a non-renewed node simply loses authority.
- **On-chain revoke:** `RevokeCapability` on any link's `(issuer, nonce)` kills that link and its
  whole subtree. The console's Trust panel surfaces the revoked set (`/fleet/revoked`) and a
  `/fleet/revoke` request that returns the exact instruction for the issuing key holder to sign
  (the org root is offline by design; the delegate cannot itself broadcast the tx — see the TODO in
  `service.rs`).
- **Root rotation:** re-pin a new org pubkey to every node's `roots/` and re-issue delegate chains.
