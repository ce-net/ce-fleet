import { defineConfig } from "vite";

// ce-fleet admin swarm console — a pure-web SPA served on-LAN behind hospital SSO. It talks to a
// few regional DELEGATE rollup endpoints (/fleet/rollup, /fleet/token, /fleet/revoked) — never to
// 1500 nodes — plus a local CE node API (via @ce-net/sdk) for per-node drill-down. In dev we proxy
// `/delegate/*` to a delegate on :8855 and `/ce/*` to a node on :8844 so the browser is same-origin.
export default defineConfig({
  server: {
    port: 5190,
    proxy: {
      "/delegate": {
        target: "http://127.0.0.1:8855",
        changeOrigin: true,
        rewrite: (p) => p.replace(/^\/delegate/, ""),
      },
      "/ce": {
        target: "http://127.0.0.1:8844",
        changeOrigin: true,
        rewrite: (p) => p.replace(/^\/ce/, ""),
      },
    },
  },
  build: {
    target: "es2022",
    outDir: "dist",
    sourcemap: true,
  },
});
