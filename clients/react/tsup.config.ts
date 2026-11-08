import { defineConfig } from "tsup";
import { readFileSync } from "node:fs";

const pkg = JSON.parse(readFileSync("./package.json", "utf8"));

export default defineConfig({
  entry: ["src/index.ts"],
  format: ["esm", "cjs"],
  // tsup 8 injects `baseUrl: "."` into its dts compile, which TypeScript 6
  // rejects as deprecated. We never set baseUrl ourselves; drop this once
  // tsup stops injecting it.
  dts: { compilerOptions: { ignoreDeprecations: "6.0" } },
  splitting: false,
  sourcemap: true,
  clean: true,
  // The wasm package stays external: its JS locates the .wasm next to
  // itself with import.meta.url, which the consumer's bundler resolves.
  external: ["react", "react-dom", "@lookout/wasm"],
  treeshake: true,
  injectStyle: true,
  define: {
    __LOOKOUT_VERSION__: JSON.stringify(pkg.version),
  },
});
