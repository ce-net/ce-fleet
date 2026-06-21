// Typed client for the ce-fleet DELEGATE service (the rollup + trust endpoints). The shapes mirror
// the Rust `enroll-service` exactly (rollup::SubtreeRollup / NodeView, service::GenTokenResp). The
// console hits a few delegates, never 1500 nodes — this is the scaling spine.

/** A node's inferred role for the swarm grid. Mirrors `rollup::Role` (serde snake_case). */
export type Role = "worker" | "idle" | "router" | "offline";

/** One node as the console renders it. Mirrors `rollup::NodeView`. */
export interface NodeView {
  node_id: string;
  cpu_cores: number;
  mem_mb: number;
  running_jobs: number;
  last_seen_secs: number;
  tags: string[];
  tier: string | null;
  model: string | null;
  role: Role;
  delivered_work: number | null;
}

/** Fleet-wide rollup over one delegate's subtree. Mirrors `rollup::SubtreeRollup`. */
export interface SubtreeRollup {
  total: number;
  workers: number;
  idle: number;
  routers: number;
  offline: number;
  running_jobs: number;
  nodes: NodeView[];
}

/** A generated enrollment token + its scope. Mirrors `service::GenTokenResp`. */
export interface GeneratedToken {
  token: string;
  abilities: string[];
  tag: string | null;
  not_after: number;
  nonce: string;
}

export interface RevokedEntry {
  issuer: string;
  nonce: number;
}

/** A configured delegate endpoint (one per region/site). `base` may be a dev proxy path. */
export interface DelegateRef {
  /** Display name, e.g. "Radiology". */
  label: string;
  /** Base URL or proxy path, e.g. "/delegate" (dev) or "https://delegate.rad.hospital.lan:8855". */
  base: string;
}

async function getJson<T>(url: string, init?: RequestInit): Promise<T> {
  const res = await fetch(url, init);
  if (!res.ok) {
    let detail = "";
    try {
      detail = JSON.stringify(await res.json());
    } catch {
      detail = res.statusText;
    }
    throw new Error(`${url} -> ${res.status}: ${detail}`);
  }
  return (await res.json()) as T;
}

/** Read the subtree rollup from one delegate. Token-free (a read panel). */
export async function fetchRollup(d: DelegateRef): Promise<SubtreeRollup> {
  return getJson<SubtreeRollup>(`${d.base}/fleet/rollup`);
}

/** Liveness of a delegate (so the console can show which delegates are reachable). */
export async function delegateHealthy(d: DelegateRef): Promise<boolean> {
  try {
    const res = await fetch(`${d.base}/healthz`);
    return res.ok;
  } catch {
    return false;
  }
}

/** Generate an enrollment token (admin Trust panel). Capability-gated server-side. */
export async function generateToken(
  d: DelegateRef,
  req: {
    node_id?: string;
    abilities: string[];
    tag?: string;
    ttl_secs: number;
  },
): Promise<GeneratedToken> {
  return getJson<GeneratedToken>(`${d.base}/fleet/token`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify(req),
  });
}

/** The on-chain revoked capability set as the delegate sees it. */
export async function fetchRevoked(d: DelegateRef): Promise<RevokedEntry[]> {
  const r = await getJson<{ revoked: RevokedEntry[] }>(`${d.base}/fleet/revoked`);
  return r.revoked;
}

/** Request a revoke instruction (the org root signs/submits the on-chain tx; root is offline). */
export async function requestRevoke(
  d: DelegateRef,
  issuer: string,
  nonce: number,
): Promise<{ status: string; instruction: string; note: string }> {
  return getJson(`${d.base}/fleet/revoke`, {
    method: "POST",
    headers: { "Content-Type": "application/json" },
    body: JSON.stringify({ issuer, nonce }),
  });
}

/** Merge rollups from multiple delegates into one fleet-wide view, de-duplicating by node id. */
export function mergeRollups(parts: SubtreeRollup[]): SubtreeRollup {
  const byId = new Map<string, NodeView>();
  for (const part of parts) {
    for (const n of part.nodes) {
      // Prefer the freshest (smallest last_seen_secs) record when a node appears under two delegates.
      const prev = byId.get(n.node_id);
      if (!prev || n.last_seen_secs < prev.last_seen_secs) byId.set(n.node_id, n);
    }
  }
  const nodes = [...byId.values()];
  const count = (r: Role) => nodes.filter((n) => n.role === r).length;
  return {
    total: nodes.length,
    workers: count("worker"),
    idle: count("idle"),
    routers: count("router"),
    offline: count("offline"),
    running_jobs: nodes.reduce((s, n) => s + (n.role === "worker" ? n.running_jobs : 0), 0),
    nodes,
  };
}
