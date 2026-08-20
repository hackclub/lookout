import { defineConfig } from "vitest/config";

export default defineConfig({
  test: {
    // `@lookout/react` reaches @squircle-js/react, whose published dist uses
    // CommonJS `module` from a file its package.json declares as ESM. Node
    // refuses it outright; inlining hands it to Vite's transform, which
    // resolves the interop. Nothing here is stubbed out — the real modules
    // load, so a test importing more of the shared package still gets it.
    server: { deps: { inline: [/@squircle-js/, /@lookout\/react/] } },
  },
});
