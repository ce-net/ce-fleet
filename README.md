# ce-fleet

The cross-platform **installer + fleet enrollment + admin swarm console** that rolls CE + ce-infer
onto a large mixed Windows/Linux/macOS fleet (e.g. a hospital's ~1500 machines) and watches them
join — Tailscale-admin meets torrent-swarm. An **app on CE**: it reuses `replicator`, `ce-cap`, and
the org capability root verbatim and adds **no node changes**.

Three parts:

| Part | Where | What it is |
|---|---|---|
| **Packaging** | `packaging/` | MSI (WiX) + deb/rpm (nfpm) + hardened systemd units + launchd plists + Ansible role. Bundles `ce`+`rdev`+`replicator`+`ce-infer-worker`+the llama.cpp engine, pins the org PUBLIC root key, installs LAN-only non-mining services, and a default-deny egress firewall. |
| **Enrollment + rollup service** | `enroll-service/` | A Rust app over `ce-rs` (`ce-fleet-enroll`): `POST /enroll` issues an audience-bound, one-time-nonce-checked working cap (Tailscale-authkey model on `ce-cap`); `GET /fleet/rollup` aggregates `atlas`+`status`+`history` across the delegate's subtree so the console scales past one node. |
| **Admin swarm console** | `console/` | Framework-free TypeScript + Vite app importing `@ce-net/sdk`. Live grid of every node as it enrolls (grey→green), health, role, tier, model, capability/TTL status, plus a Trust panel (generate tokens, view/request revocation). |

## Trust spine

One offline Ed25519 **org root** key (`ce-root`) signs everything; its **public** key is pinned in
every node's `<data_dir>/roots`. Fleet membership = "I honor chains rooted at `ce-root`."

- **Adding a node** = ship binaries + the root **pubkey** + an attenuated cap (first-boot enroll).
- **Removing a node** = expiry, on-chain `RevokeCapability` (subtree-killing), or root rotation.
- **IT/Intune/SCCM/Ansible own file placement + service lifecycle; the org root owns authorization.**
  A poisoned MSI/deb mirror grants **no** fleet authority — the root pubkey is a pin, and the working
  cap is issued by the delegate (rooted at the org key) and bound to the enrolling node's id.

The whole authorization model is `ce-cap` capability chains — the same `onward_abilities` /
`attenuate` / `delegate` discipline `replicator/src/main.rs` uses. Privilege can only ever shrink.

## Topology

```
1 org root (offline / HSM)
  └─ 3–5 regional delegates  (per site: run /enroll + /fleet/rollup + serve the console)
       └─ ~30–50 subnet seeds (enrolled via SCCM/Ansible; run `replicator seed --depth 2/3`)
            └─ leaves          (binaries + GGUF arrive in 2–3 LAN hops; leaf cap = sync only)
```

SCCM/Ansible is the **audited install-of-record + fallback**; `replicator` is the **fast O(log N)
content path** for binary/model fan-out over the LAN — driven from a seed node by
`packaging/linux/seed-fanout.sh` (Linux) or `packaging/windows/Seed-Fanout.ps1` (Windows). Both are
thin wrappers over the same cross-platform `replicator` binary.

## Air-gap (PHI never leaves the LAN)

Every package starts the node with `ce start --no-mine` and **no** `ce-net.com/bootstrap`, **no**
public relay, **no** internet DCUtR. The mesh is LAN-only (mDNS + static LAN multiaddrs). The second
half is a **default-deny egress firewall** (`packaging/linux/ce-fleet-egress.nft` on Linux; per-binary
outbound-block rules on Windows). A CI test (`enroll-service/tests/airgap.rs`) asserts no packaging
artifact dials a public address and that every node-start uses `--no-mine`. GGUF weights + the engine
arrive via the package or the internal blob LAN (`ce-pin`), never the internet.

## Quick start

### Run a delegate (per site)

```bash
# Hold an org-root-issued cap whose audience is this delegate (e.g. from `ce grant <delegate-id> ...`).
export CE_FLEET_DELEGATE_CAP=<hex-chain-token>
export CE_FLEET_BOOTSTRAP_SECRET=<short-ttl-tag-scoped-secret>

cargo run -p ce-fleet-enroll -- \
  --node http://127.0.0.1:8844 \
  --tag radiology \
  --abilities status,infer:chat,infer:summarize \
  --bind 0.0.0.0:8855
```

### Enroll a Windows machine (silent, machine-targeted — same line for GPO/SCCM/Intune)

```
msiexec /i ce-fleet.msi /qn /norestart ^
  CE_ROOT_KEY=<org-public-root-hex> ^
  CE_DELEGATE_URL=http://delegate.rad.hospital.lan:8855 ^
  CE_BOOTSTRAP_SECRET=<secret> ^
  CE_DATA_DIR="C:\ProgramData\ce"
```

### Enroll a Linux fleet (Ansible)

```yaml
- hosts: radiology
  become: true
  roles:
    - role: ce_fleet
  vars:
    ce_fleet_root_pubkey: "{{ org_root_pubkey }}"
    ce_fleet_bootstrap_secret: "{{ vault_ce_fleet_bootstrap_secret }}"  # no_log, from Vault
```

### Run the admin swarm console

```bash
cd console && npm install && npm run dev   # dev proxies /delegate -> :8855, /ce -> :8844
# or: npm run build  (static bundle served on-LAN behind SSO)
```

## Build / test

```bash
cargo build                # enroll-service (path deps: ../ce-rs, ../ce/crates/{ce-cap,ce-identity})
cargo test                 # enroll-service unit + air-gap packaging tests
cd console && npm run build # admin swarm console (typecheck + bundle)
```

The Rust enroll-service and the air-gap packaging tests build and pass on Linux, macOS, and Windows
(`cargo build` / `cargo test` are platform-clean: no Unix-only APIs, `std::env::temp_dir()` and a
`$XDG_DATA_HOME`/`%LOCALAPPDATA%`/`$HOME`-aware data dir, no hardcoded `/tmp` or `/`-joined paths).
CI (`.github/workflows/ci.yml`) runs a 3-OS matrix (`ubuntu-latest`, `macos-latest`, `windows-latest`,
`fail-fast: false`) for the Rust crate, an `ubuntu`+`windows` matrix for the TS console, shellcheck +
`nft -c` on Linux, and a Windows job that AST-parses every `packaging/windows/*.ps1`. The signed MSI
(`wix build`) and the `.deb`/`.rpm` (`nfpm package`) builds are documented as gated release jobs.

See `docs/` for the enrollment flow, the trust model, and the air-gap posture.
