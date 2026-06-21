// Framework-free DOM rendering for the swarm console. Pure functions that take state and produce
// elements; main.ts owns the lifecycle and re-renders on Store changes. No emojis; monospace ids.

import type { NodeView, Role } from "./delegate.js";
import type { Store } from "./store.js";

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  attrs: Record<string, string> = {},
  children: (Node | string)[] = [],
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  for (const [k, v] of Object.entries(attrs)) {
    if (k === "class") node.className = v;
    else node.setAttribute(k, v);
  }
  for (const c of children) node.append(typeof c === "string" ? document.createTextNode(c) : c);
  return node;
}

function shortId(id: string): string {
  return id.length > 16 ? `${id.slice(0, 8)}…${id.slice(-6)}` : id;
}

function roleLabel(r: Role): string {
  return { worker: "WORKER", idle: "IDLE", router: "ROUTER", offline: "OFFLINE" }[r];
}

function fmtAge(secs: number): string {
  if (secs < 60) return `${secs}s`;
  if (secs < 3600) return `${Math.floor(secs / 60)}m`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h`;
  return `${Math.floor(secs / 86400)}d`;
}

/** The headline strip: fleet-wide counts + the enrollment funnel feel (grey -> green). */
export function renderHeadline(store: Store): HTMLElement {
  const r = store.state.rollup;
  const stat = (label: string, value: string | number, cls = "") =>
    el("div", { class: `stat ${cls}` }, [
      el("span", { class: "stat-value" }, [String(value)]),
      el("span", { class: "stat-label" }, [label]),
    ]);

  const delegates = Object.entries(store.state.delegateHealth);
  const healthy = delegates.filter(([, ok]) => ok).length;

  return el("section", { class: "headline" }, [
    stat("nodes", r.total),
    stat("workers", r.workers, "ok"),
    stat("idle", r.idle),
    stat("routers", r.routers),
    stat("offline", r.offline, r.offline > 0 ? "warn" : ""),
    stat("running jobs", r.running_jobs),
    stat("delegates", `${healthy}/${delegates.length || "?"}`, healthy ? "ok" : "warn"),
  ]);
}

/** The filter bar (text + role/tier/model facets). */
export function renderFilters(store: Store): HTMLElement {
  const f = store.state.filter;
  const { tiers, models } = store.facets();

  const text = el("input", {
    type: "search",
    placeholder: "filter node id / tag / tier / model",
    value: f.text,
    class: "filter-text",
  }) as HTMLInputElement;
  text.addEventListener("input", () => store.setFilter({ text: text.value }));

  const select = (
    current: string,
    options: string[],
    onChange: (v: string) => void,
    allLabel: string,
  ): HTMLSelectElement => {
    const sel = el("select", {}, [
      el("option", { value: "all" }, [allLabel]),
      ...options.map((o) => el("option", { value: o }, [o])),
    ]) as HTMLSelectElement;
    sel.value = current;
    sel.addEventListener("change", () => onChange(sel.value));
    return sel;
  };

  return el("section", { class: "filters" }, [
    text,
    select(f.role, ["worker", "idle", "router", "offline"], (v) => store.setFilter({ role: v as Role | "all" }), "all roles"),
    select(f.tier, tiers, (v) => store.setFilter({ tier: v }), "all tiers"),
    select(f.model, models, (v) => store.setFilter({ model: v }), "all models"),
  ]);
}

/** One node card in the swarm grid (torrent-peer-pane density). */
function renderNodeCard(n: NodeView): HTMLElement {
  const card = el("div", { class: `node node-${n.role}`, title: n.node_id }, [
    el("div", { class: "node-head" }, [
      el("span", { class: "node-dot" }, []),
      el("code", { class: "node-id" }, [shortId(n.node_id)]),
      el("span", { class: `role role-${n.role}` }, [roleLabel(n.role)]),
    ]),
    el("div", { class: "node-meta" }, [
      el("span", {}, [n.tier ?? "—"]),
      el("span", {}, [n.model ?? "no model"]),
    ]),
    el("div", { class: "node-meta" }, [
      el("span", {}, [`${n.cpu_cores} cores`]),
      el("span", {}, [`${Math.round(n.mem_mb / 1024)} GB`]),
      el("span", {}, [`${n.running_jobs} jobs`]),
    ]),
    el("div", { class: "node-foot" }, [
      el("span", {}, [`seen ${fmtAge(n.last_seen_secs)} ago`]),
      el("span", {}, [n.delivered_work != null ? `rep ${n.delivered_work}` : ""]),
    ]),
  ]);
  return card;
}

/** The swarm grid (the headline screen). */
export function renderSwarm(store: Store): HTMLElement {
  const nodes = store.filteredNodes();
  const grid = el("section", { class: "swarm-grid" });
  if (nodes.length === 0) {
    grid.append(
      el("div", { class: "empty" }, [
        store.state.rollup.total === 0
          ? "No nodes yet — they appear here grey→green as they enroll."
          : "No nodes match the current filter.",
      ]),
    );
    return grid;
  }
  // Workers first, then idle, routers, offline — most-interesting at the top.
  const order: Record<Role, number> = { worker: 0, idle: 1, router: 2, offline: 3 };
  for (const n of [...nodes].sort((a, b) => order[a.role] - order[b.role])) {
    grid.append(renderNodeCard(n));
  }
  return grid;
}

/** The status bar: last update, errors, the on-prem/LAN banner. */
export function renderStatusBar(store: Store): HTMLElement {
  const s = store.state;
  const when = s.lastUpdated ? new Date(s.lastUpdated).toLocaleTimeString() : "never";
  return el("footer", { class: "statusbar" }, [
    el("span", { class: "banner" }, ["On-prem · LAN-only · PHI stays on this network"]),
    el("span", {}, [`updated ${when}`]),
    s.lastError ? el("span", { class: "err" }, [s.lastError]) : el("span", {}, [""]),
  ]);
}
