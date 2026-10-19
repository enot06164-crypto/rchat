import { defineConfig } from "vite";
import solid from "vite-plugin-solid";

// In dev the browser talks to the Vite server (5173); `/ws` is proxied to the
// Python gateway from bindings/python (stage H, `python3 -m rchat.gateway`).
// The same proxy is configured for `vite preview` so a production bundle can
// be exercised against a locally running gateway without extra tooling.
const wsProxy = {
  "/ws": {
    target: "ws://127.0.0.1:8080",
    ws: true,
  },
};

export default defineConfig({
  plugins: [solid()],
  server: {
    port: 5173,
    proxy: wsProxy,
  },
  preview: {
    port: 4173,
    proxy: wsProxy,
  },
  build: {
    target: "es2022",
    sourcemap: true,
  },
});
