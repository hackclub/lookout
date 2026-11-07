/**
 * Loading the shared recorder (WebAssembly) in the browser — once per
 * page — and constructing recorders on it.
 */
import type { LookoutHost, LookoutRecorder as WasmRecorder } from "@lookout/wasm";

export type { WasmRecorder };

type WasmModule = typeof import("@lookout/wasm");

let loading: Promise<WasmModule> | null = null;

/** How the module's bytes are found. Default: next to the module's own
 *  JavaScript (the bundler copies the `.wasm` as an asset). Tests pass the
 *  bytes themselves. */
export type WasmInput = BufferSource | WebAssembly.Module | URL | string;

let inputOverride: WasmInput | null = null;

/** Point the loader at the module bytes (tests, or a host that serves the
 *  `.wasm` from its own path). Call before the first `loadRecorderModule`. */
export function setWasmInput(input: WasmInput | null): void {
  inputOverride = input;
}

/** The initialized module. Idempotent. */
export function loadRecorderModule(): Promise<WasmModule> {
  if (!loading) {
    loading = (async () => {
      const mod = await import("@lookout/wasm");
      await mod.default(inputOverride ? { module_or_path: inputOverride } : undefined);
      return mod;
    })();
    loading.catch(() => {
      loading = null;
    });
  }
  return loading;
}

/** Timing applied to every recorder created from here on — simulations
 *  scale the pipeline down with it. `null` restores the real timing. */
let timingOverride: Record<string, number | number[]> | null = null;

export function setRecorderTiming(timing: Record<string, number | number[]> | null): void {
  timingOverride = timing;
}

/** A recorder on the loaded module. */
export async function createRecorder(host: LookoutHost): Promise<WasmRecorder> {
  const mod = await loadRecorderModule();
  const recorder = new mod.LookoutRecorder(host);
  if (timingOverride) recorder.setTiming(timingOverride);
  return recorder;
}
