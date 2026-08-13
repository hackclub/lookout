import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import { readFileSync } from "node:fs";

const pkg = JSON.parse(
  readFileSync(new URL("./package.json", import.meta.url), "utf8"),
);

export default defineConfig({
  // The app is mounted under a path, not at the origin root — the root is
  // the download landing page. Every emitted asset URL has to carry that
  // prefix, and the server serves this build from public/session/.
  base: "/session/",
  plugins: [react()],
  define: {
    __LOOKOUT_VERSION__: JSON.stringify(pkg.version),
  },
  server: {
    port: 5174,
    proxy: {
      "/api": "http://localhost:3001",
    },
  },
  build: {
    outDir: "dist",
  },
});
