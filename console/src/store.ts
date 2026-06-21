// The reactive Store — single source of truth, owns polling across the configured delegates and
// notifies subscribers on change. Same shape as ce-host's Store (a tiny observable, no framework).
// The browser polls a few DELEGATE rollups, not 1500 nodes (the scaling architecture).

import {
  delegateHealthy,
  fetchRollup,
  fetchRevoked,
  mergeRollups,
  type DelegateRef,
  type NodeView,
  type RevokedEntry,
  type Role,
  type SubtreeRollup,
} from "./delegate.js";

export type Listener = () => void;

/** A filter over the swarm grid (Tailscale-machine-list style facets). */
export interface SwarmFilter {
  text: string; // matches node id / hostname-ish tags
  role: Role | "all";
  tier: string | "all";
  model: string | "all";
}

export interface FleetState {
  /** Reachability per delegate (by label). */
  delegateHealth: Record<string, boolean>;
  /** The merged, de-duplicated fleet-wide rollup across all delegates. */
  rollup: SubtreeRollup;
  /** On-chain revoked capability set (Trust panel). */
  revoked: RevokedEntry[];
  /** Last successful refresh (unix ms), or 0. */
  lastUpdated: number;
  /** Last error message, if the most recent refresh failed (kept for the status bar). */
  lastError: string | null;
  filter: SwarmFilter;
}

const EMPTY_ROLLUP: SubtreeRollup = {
  total: 0,
  workers: 0,
  idle: 0,
  routers: 0,
  offline: 0,
  running_jobs: 0,
  nodes: [],
};

export class Store {
  private listeners = new Set<Listener>();
  private timer: ReturnType<typeof setInterval> | null = null;
  state: FleetState;

  constructor(
    private readonly delegates: DelegateRef[],
    private readonly pollMs = 5000,
  ) {
    this.state = {
      delegateHealth: {},
      rollup: EMPTY_ROLLUP,
      revoked: [],
      lastUpdated: 0,
      lastError: null,
      filter: { text: "", role: "all", tier: "all", model: "all" },
    };
  }

  subscribe(fn: Listener): () => void {
    this.listeners.add(fn);
    return () => this.listeners.delete(fn);
  }

  private emit(): void {
    for (const fn of this.listeners) fn();
  }

  setFilter(patch: Partial<SwarmFilter>): void {
    this.state.filter = { ...this.state.filter, ...patch };
    this.emit();
  }

  /** The grid rows after applying the current filter. */
  filteredNodes(): NodeView[] {
    const f = this.state.filter;
    const text = f.text.trim().toLowerCase();
    return this.state.rollup.nodes.filter((n) => {
      if (f.role !== "all" && n.role !== f.role) return false;
      if (f.tier !== "all" && (n.tier ?? "") !== f.tier) return false;
      if (f.model !== "all" && (n.model ?? "") !== f.model) return false;
      if (text) {
        const hay = `${n.node_id} ${n.tags.join(" ")} ${n.tier ?? ""} ${n.model ?? ""}`.toLowerCase();
        if (!hay.includes(text)) return false;
      }
      return true;
    });
  }

  /** Distinct tier / model values for the filter dropdowns. */
  facets(): { tiers: string[]; models: string[] } {
    const tiers = new Set<string>();
    const models = new Set<string>();
    for (const n of this.state.rollup.nodes) {
      if (n.tier) tiers.add(n.tier);
      if (n.model) models.add(n.model);
    }
    return { tiers: [...tiers].sort(), models: [...models].sort() };
  }

  /** One refresh cycle: health-check + rollup each delegate, merge, fetch revoked from the first. */
  async refresh(): Promise<void> {
    const health: Record<string, boolean> = {};
    const parts: SubtreeRollup[] = [];
    const errors: string[] = [];

    await Promise.all(
      this.delegates.map(async (d) => {
        health[d.label] = await delegateHealthy(d);
        try {
          parts.push(await fetchRollup(d));
        } catch (e) {
          errors.push(`${d.label}: ${(e as Error).message}`);
        }
      }),
    );

    // Revoked set: read from the first healthy delegate (it is fleet-wide on-chain data).
    let revoked: RevokedEntry[] = this.state.revoked;
    const firstHealthy = this.delegates.find((d) => health[d.label]);
    if (firstHealthy) {
      try {
        revoked = await fetchRevoked(firstHealthy);
      } catch {
        /* keep previous on a transient failure */
      }
    }

    this.state.delegateHealth = health;
    this.state.rollup = parts.length ? mergeRollups(parts) : this.state.rollup;
    this.state.revoked = revoked;
    this.state.lastError = errors.length ? errors.join(" | ") : null;
    if (parts.length) this.state.lastUpdated = Date.now();
    this.emit();
  }

  start(): void {
    if (this.timer) return;
    void this.refresh();
    this.timer = setInterval(() => void this.refresh(), this.pollMs);
  }

  stop(): void {
    if (this.timer) {
      clearInterval(this.timer);
      this.timer = null;
    }
  }
}
