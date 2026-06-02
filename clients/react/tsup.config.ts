import { defineConfig } from "tsup";
import { readFileSync } from "node:fs";

const pkg = JSON.parse(readFileSync("./package.json", "utf8"));

export default defineConfig({
  entry: ["src/index.ts"],
  format: ["esm", "cjs"],
  dts: true,
  splitting: false,
  sourcemap: false,
  clean: true,
  external: ["react", "react-dom"],
  treeshake: true,
  injectStyle: true,
  define: {
    __LOOKOUT_VERSION__: JSON.stringify(pkg.version),
  },
});
