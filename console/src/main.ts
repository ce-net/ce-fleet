// ce-fleet admin swarm console — entry point. Wires the Store (polling delegate rollups) to two
// tabs: Swarm (the live node grid) and Trust (enrollment tokens + revocation). Per-node drill-down
// uses @ce-net/sdk against the local node API. Framework-free; re-renders on Store change.

import { CeClient } from "@ce-net/sdk";
import type { DelegateRef } from "./delegate.js";
import { Store } from "./store.js";
import { renderFilters, renderHeadline, renderStatusBar, renderSwarm } from "./render.js";
import { renderTrustPanel } from "./trust.js";

// Delegate endpoints. In production these are injected (a few regional delegates); in dev we point
// at the Vite proxy `/delegate`. A `window.CE_FLEET_DELEGATES` global lets the served bundle be
// configured on-LAN without a rebuild.
declare global {
  interface Window {
    CE_FLEET_DELEGATES?: DelegateRef[];
    CE_FLEET_NODE_URL?: string;
  }
}

const delegates: DelegateRef[] =
  window.CE_FLEET_DELEGATES && window.CE_FLEET_DELEGATES.length > 0
    ? window.CE_FLEET_DELEGATES
    : [{ label: "local-delegate", base: "/delegate" }];

// The CE node client (per-node drill-down: atlas + history). Served same-origin behind SSO in prod;
// dev proxy maps `/ce` -> node :8844. Held for the drill-down view (wired below).
const node = new CeClient({ baseUrl: window.CE_FLEET_NODE_URL ?? "/ce" });

const store = new Store(delegates);

type Tab = "swarm" | "trust";
let activeTab: Tab = "swarm";

const root = document.getElementById("app");
if (!root) throw new Error("missing #app root");

function header(): HTMLElement {
  const h = document.createElement("header");
  h.className = "app-header";
  const title = document.createElement("h1");
  title.textContent = "CE Fleet — Swarm Console";
  const tabs = document.createElement("nav");
  tabs.className = "tabs";
  for (const t of ["swarm", "trust"] as Tab[]) {
    const b = document.createElement("button");
    b.textContent = t === "swarm" ? "Swarm" : "Trust";
    b.className = `tab ${activeTab === t ? "active" : ""}`;
    b.addEventListener("click", () => {
      activeTab = t;
      render();
    });
    tabs.append(b);
  }
  h.append(title, tabs);
  return h;
}

function render(): void {
  if (!root) return;
  root.replaceChildren();
  root.setAttribute("aria-busy", "false");
  root.append(header(), renderHeadline(store));

  if (activeTab === "swarm") {
    root.append(renderFilters(store), renderSwarm(store));
  } else {
    const firstDelegate = delegates[0];
    if (firstDelegate) root.append(renderTrustPanel(store, firstDelegate));
  }
  root.append(renderStatusBar(store));
}

// Drill-down helper: fetch one node's history via @ce-net/sdk (used by the swarm grid on click).
// Exposed on window so a future per-node modal can call it without re-importing the SDK.
window.addEventListener("ce-fleet:drilldown", (ev) => {
  const id = (ev as CustomEvent<string>).detail;
  void node
    .history(id)
    .then((h) => console.info("[ce-fleet] node history", id, h))
    .catch((e: unknown) => console.warn("[ce-fleet] history fetch failed", e));
});

store.subscribe(render);
store.start();
render();
