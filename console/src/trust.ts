// The Trust / enrollment panel — generate enrollment tokens (with a copyable token + a QR for
// kiosk enroll), view the on-chain revoked set, and request a revoke instruction. Mutations are
// capability-gated server-side (the delegate only issues caps it can attenuate from its org-root
// chain); the console just drives the endpoints.

import {
  generateToken,
  requestRevoke,
  type DelegateRef,
  type GeneratedToken,
} from "./delegate.js";
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

function ttlCountdown(notAfter: number): string {
  if (notAfter === 0) return "never";
  const secs = notAfter - Math.floor(Date.now() / 1000);
  if (secs <= 0) return "expired";
  if (secs < 3600) return `${Math.floor(secs / 60)}m left`;
  if (secs < 86400) return `${Math.floor(secs / 3600)}h left`;
  return `${Math.floor(secs / 86400)}d left`;
}

/** Build the Trust panel for a chosen delegate. `delegate` is where tokens are minted/revoked. */
export function renderTrustPanel(store: Store, delegate: DelegateRef): HTMLElement {
  const panel = el("section", { class: "trust-panel" }, [
    el("h2", {}, ["Trust & enrollment"]),
    el("p", { class: "muted" }, [
      `Tokens minted by delegate "${delegate.label}", rooted at the offline org key. Removing a node = expiry or on-chain RevokeCapability (subtree-killing).`,
    ]),
  ]);

  // --- Generate enrollment token ---
  const form = el("form", { class: "token-form" });
  const tagInput = el("input", { type: "text", placeholder: "tag (e.g. radiology)", value: "radiology" }) as HTMLInputElement;
  const abilitiesInput = el("input", {
    type: "text",
    placeholder: "abilities (comma-separated)",
    value: "status,infer:chat",
  }) as HTMLInputElement;
  const ttlInput = el("input", { type: "number", min: "60", value: "7200", title: "TTL seconds" }) as HTMLInputElement;
  const nodeInput = el("input", { type: "text", placeholder: "bind to node id (optional — reusable if blank)" }) as HTMLInputElement;
  const out = el("div", { class: "token-out" });

  const submit = el("button", { type: "submit", class: "btn" }, ["Generate enrollment token"]);
  form.append(
    el("label", {}, ["Tag", tagInput]),
    el("label", {}, ["Abilities", abilitiesInput]),
    el("label", {}, ["TTL (s)", ttlInput]),
    el("label", {}, ["Node (optional)", nodeInput]),
    submit,
  );

  form.addEventListener("submit", (e) => {
    e.preventDefault();
    out.replaceChildren(el("span", { class: "muted" }, ["minting…"]));
    const req: { node_id?: string; abilities: string[]; tag?: string; ttl_secs: number } = {
      abilities: abilitiesInput.value
        .split(",")
        .map((s) => s.trim())
        .filter(Boolean),
      ttl_secs: Number(ttlInput.value) || 7200,
    };
    const tag = tagInput.value.trim();
    if (tag) req.tag = tag;
    const nid = nodeInput.value.trim();
    if (nid) req.node_id = nid;

    generateToken(delegate, req)
      .then((t) => out.replaceChildren(renderTokenResult(t)))
      .catch((err: Error) => out.replaceChildren(el("span", { class: "err" }, [err.message])));
  });

  panel.append(el("h3", {}, ["Generate token"]), form, out);

  // --- Revoked set ---
  const revokedBox = el("div", { class: "revoked-box" });
  const rev = store.state.revoked;
  if (rev.length === 0) {
    revokedBox.append(el("p", { class: "muted" }, ["No revoked capabilities."]));
  } else {
    for (const r of rev) {
      const row = el("div", { class: "revoked-row" }, [
        el("code", {}, [`${r.issuer.slice(0, 12)}… #${r.nonce}`]),
        el("span", { class: "tag-revoked" }, ["revoked"]),
      ]);
      revokedBox.append(row);
    }
  }
  panel.append(el("h3", {}, ["On-chain revoked"]), revokedBox);

  // --- Request revoke ---
  const revokeForm = el("form", { class: "revoke-form" });
  const issuerInput = el("input", { type: "text", placeholder: "issuer node id (hex)" }) as HTMLInputElement;
  const nonceInput = el("input", { type: "number", min: "0", placeholder: "nonce" }) as HTMLInputElement;
  const revokeOut = el("div", { class: "token-out" });
  const revokeBtn = el("button", { type: "submit", class: "btn danger" }, ["Request revoke"]);
  revokeForm.append(
    el("label", {}, ["Issuer", issuerInput]),
    el("label", {}, ["Nonce", nonceInput]),
    revokeBtn,
  );
  revokeForm.addEventListener("submit", (e) => {
    e.preventDefault();
    revokeOut.replaceChildren(el("span", { class: "muted" }, ["requesting…"]));
    requestRevoke(delegate, issuerInput.value.trim(), Number(nonceInput.value) || 0)
      .then((r) =>
        revokeOut.replaceChildren(
          el("div", {}, [
            el("p", {}, [r.note]),
            el("code", { class: "instruction" }, [r.instruction]),
          ]),
        ),
      )
      .catch((err: Error) => revokeOut.replaceChildren(el("span", { class: "err" }, [err.message])));
  });
  panel.append(el("h3", {}, ["Revoke a node"]), revokeForm, revokeOut);

  return panel;
}

function renderTokenResult(t: GeneratedToken): HTMLElement {
  const box = el("div", { class: "token-result" }, [
    el("div", { class: "kv" }, [el("span", {}, ["scope"]), el("span", {}, [`${t.abilities.join(", ")}${t.tag ? ` · tag=${t.tag}` : ""}`])]),
    el("div", { class: "kv" }, [el("span", {}, ["ttl"]), el("span", {}, [ttlCountdown(t.not_after)])]),
    el("div", { class: "kv" }, [el("span", {}, ["nonce"]), el("code", {}, [t.nonce])]),
  ]);
  const tokenField = el("textarea", { class: "token-field", readonly: "readonly", rows: "3" }) as HTMLTextAreaElement;
  tokenField.value = t.token;
  const copy = el("button", { type: "button", class: "btn" }, ["Copy token"]);
  copy.addEventListener("click", () => {
    void navigator.clipboard?.writeText(t.token);
    copy.textContent = "Copied";
    setTimeout(() => (copy.textContent = "Copy token"), 1500);
  });
  box.append(tokenField, copy, qrPlaceholder());
  return box;
}

// A QR placeholder. The real QR is rendered by a small QR lib in production (kept out of this
// dependency-light console build); for kiosk enroll the short-code text below is sufficient.
// TODO: render an actual QR (e.g. a vendored qrcode-svg) for kiosk camera enroll.
function qrPlaceholder(): HTMLElement {
  return el("div", { class: "qr-placeholder", title: "QR for kiosk enroll (rendered in production)" }, [
    el("span", { class: "muted" }, ["QR · scan to enroll a kiosk (rendered in production build)"]),
  ]);
}
