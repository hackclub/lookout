/**
 * Device pairing with a program's backend, so the + menu can start a session
 * without the browser hop ("desktop instant start" in docs/integration.md).
 *
 * The shape mirrors an OAuth device grant with PKCE, kept deliberately tiny:
 *
 *   1. beginPairing(): generate a random verifier, open the program's pairUrl
 *      in the OS browser with sha256(verifier), a state nonce, and a device
 *      label. The program authenticates the user with whatever it already
 *      has (its own cookies) and redirects to lookout://pair?code=…&state=….
 *   2. completePairing(): match the callback's state to the pending pairing,
 *      POST {code, verifier} back to pairUrl, and store the returned
 *      deviceToken (Rust-side secret store) plus link metadata (localStorage).
 *   3. startLinkedSession(): POST startUrl with `Authorization: Bearer
 *      <deviceToken>`; the program's backend mints a Lookout session with its
 *      own API key and returns the session token. We verify the token against
 *      Lookout (right program, recordable status) before recording.
 *
 * Lookout's server carries none of this: no user model here, no vendored
 * program auth — just two registry URLs and this client-side dance. Every
 * failure path falls back to the browser flow (newSessionUrl), which stays
 * fully supported for programs that never implement pairing.
 *
 * Security posture:
 *  - The deep link carries a PKCE-bound single-use code, never the credential;
 *    anything squatting lookout:// receives something unredeemable.
 *  - The credential is origin-bound at pair time: it is only ever presented
 *    to the https origin the user consented to. A later registry edit
 *    pointing elsewhere invalidates the link instead of re-aiming the token.
 *  - pair/start URLs must be https (http only on localhost, for dev).
 */

import { invoke as rawInvoke } from "@tauri-apps/api/core";
import { invoke } from "./logger.js";
import { getApiBase } from "./serverConfig.js";
import { isValidToken } from "./utils.js";
import { isMacOS, isWindows } from "./platform.js";

export interface LinkableProgram {
  name: string;
  displayName?: string;
  newSessionUrl: string;
  iconUrl?: string | null;
  pairUrl?: string | null;
  startUrl?: string | null;
}

/** Non-secret link metadata, one entry per paired program (localStorage). */
export interface ProgramLink {
  program: string;
  /** Origin (scheme+host+port) of pairUrl at pair time; the credential is only ever sent here. */
  origin: string;
  pairUrl: string;
  startUrl: string;
  pairedAt: string; // ISO
  deviceLabel: string;
  // Presentation, copied from the registry at pair time so the settings list
  // can name and picture the program without a live registry — it still shows
  // correctly offline, and there's no flash of a placeholder glyph while a
  // fetch lands. A fresh registry read wins when one is available.
  displayName?: string;
  iconUrl?: string | null;
}

interface PendingPairing {
  program: string;
  state: string;
  verifier: string;
  origin: string;
  pairUrl: string;
  startUrl: string;
  createdAt: number; // epoch ms
  displayName?: string;
  iconUrl?: string | null;
}

const LINKS_KEY = "lookout-program-links";
const PENDING_KEY = "lookout-pending-pairing";
/** A consent page left open for longer than this is abandoned. */
const PENDING_TTL_MS = 10 * 60_000;
const EXCHANGE_TIMEOUT_MS = 15_000;
const START_TIMEOUT_MS = 15_000;
/** A missing consent page should fail fast into the browser flow, not hang the +. */
const CONSENT_PROBE_TIMEOUT_MS = 5_000;

// ---------------------------------------------------------------------------
// Pure helpers (exported for tests)
// ---------------------------------------------------------------------------

/**
 * SHA-256, dependency-free. Implemented here rather than via crypto.subtle
 * because SubtleCrypto is gated on "secure context", which differs across the
 * three webviews (tauri://, http://tauri.localhost, dev http://localhost) —
 * a pure function can't be undefined on one platform.
 */
export function sha256(bytes: Uint8Array): Uint8Array {
  const K = [
    0x428a2f98, 0x71374491, 0xb5c0fbcf, 0xe9b5dba5, 0x3956c25b, 0x59f111f1, 0x923f82a4, 0xab1c5ed5,
    0xd807aa98, 0x12835b01, 0x243185be, 0x550c7dc3, 0x72be5d74, 0x80deb1fe, 0x9bdc06a7, 0xc19bf174,
    0xe49b69c1, 0xefbe4786, 0x0fc19dc6, 0x240ca1cc, 0x2de92c6f, 0x4a7484aa, 0x5cb0a9dc, 0x76f988da,
    0x983e5152, 0xa831c66d, 0xb00327c8, 0xbf597fc7, 0xc6e00bf3, 0xd5a79147, 0x06ca6351, 0x14292967,
    0x27b70a85, 0x2e1b2138, 0x4d2c6dfc, 0x53380d13, 0x650a7354, 0x766a0abb, 0x81c2c92e, 0x92722c85,
    0xa2bfe8a1, 0xa81a664b, 0xc24b8b70, 0xc76c51a3, 0xd192e819, 0xd6990624, 0xf40e3585, 0x106aa070,
    0x19a4c116, 0x1e376c08, 0x2748774c, 0x34b0bcb5, 0x391c0cb3, 0x4ed8aa4a, 0x5b9cca4f, 0x682e6ff3,
    0x748f82ee, 0x78a5636f, 0x84c87814, 0x8cc70208, 0x90befffa, 0xa4506ceb, 0xbef9a3f7, 0xc67178f2,
  ];
  const H = [
    0x6a09e667, 0xbb67ae85, 0x3c6ef372, 0xa54ff53a, 0x510e527f, 0x9b05688c, 0x1f83d9ab, 0x5be0cd19,
  ];

  // Pad: message + 0x80 + zeros + 64-bit big-endian bit length.
  const bitLen = bytes.length * 8;
  const padded = new Uint8Array((((bytes.length + 8) >> 6) + 1) << 6);
  padded.set(bytes);
  padded[bytes.length] = 0x80;
  const dv = new DataView(padded.buffer);
  dv.setUint32(padded.length - 8, Math.floor(bitLen / 0x100000000));
  dv.setUint32(padded.length - 4, bitLen >>> 0);

  const w = new Array<number>(64);
  const rotr = (x: number, n: number) => (x >>> n) | (x << (32 - n));

  for (let i = 0; i < padded.length; i += 64) {
    for (let t = 0; t < 16; t++) w[t] = dv.getUint32(i + t * 4);
    for (let t = 16; t < 64; t++) {
      const s0 = rotr(w[t - 15], 7) ^ rotr(w[t - 15], 18) ^ (w[t - 15] >>> 3);
      const s1 = rotr(w[t - 2], 17) ^ rotr(w[t - 2], 19) ^ (w[t - 2] >>> 10);
      w[t] = (w[t - 16] + s0 + w[t - 7] + s1) >>> 0;
    }
    let [a, b, c, d, e, f, g, h] = H;
    for (let t = 0; t < 64; t++) {
      const S1 = rotr(e, 6) ^ rotr(e, 11) ^ rotr(e, 25);
      const ch = (e & f) ^ (~e & g);
      const t1 = (h + S1 + ch + K[t] + w[t]) >>> 0;
      const S0 = rotr(a, 2) ^ rotr(a, 13) ^ rotr(a, 22);
      const maj = (a & b) ^ (a & c) ^ (b & c);
      const t2 = (S0 + maj) >>> 0;
      h = g; g = f; f = e;
      e = (d + t1) >>> 0;
      d = c; c = b; b = a;
      a = (t1 + t2) >>> 0;
    }
    H[0] = (H[0] + a) >>> 0; H[1] = (H[1] + b) >>> 0;
    H[2] = (H[2] + c) >>> 0; H[3] = (H[3] + d) >>> 0;
    H[4] = (H[4] + e) >>> 0; H[5] = (H[5] + f) >>> 0;
    H[6] = (H[6] + g) >>> 0; H[7] = (H[7] + h) >>> 0;
  }

  const out = new Uint8Array(32);
  const odv = new DataView(out.buffer);
  for (let i = 0; i < 8; i++) odv.setUint32(i * 4, H[i]);
  return out;
}

export function base64url(bytes: Uint8Array): string {
  let bin = "";
  for (const b of bytes) bin += String.fromCharCode(b);
  return btoa(bin).replace(/\+/g, "-").replace(/\//g, "_").replace(/=+$/, "");
}

function randomBytes(n: number): Uint8Array {
  const out = new Uint8Array(n);
  crypto.getRandomValues(out);
  return out;
}

/**
 * A URL a device credential may be created against / presented to: https, or
 * http on localhost for development. Mirrors the server-side registry
 * validation — enforced again here because the registry is unauthenticated
 * data as far as this client is concerned.
 */
export function isAcceptableEndpoint(raw: string | null | undefined): raw is string {
  if (!raw) return false;
  let url: URL;
  try {
    url = new URL(raw);
  } catch {
    return false;
  }
  if (url.protocol === "https:") return true;
  return url.protocol === "http:" && (url.hostname === "localhost" || url.hostname === "127.0.0.1");
}

/** Whether a registry entry advertises the instant-start capability at all. */
export function isLinkable(p: LinkableProgram): boolean {
  return isAcceptableEndpoint(p.pairUrl) && isAcceptableEndpoint(p.startUrl);
}

/**
 * The registry can advertise a pair URL the program has not deployed yet.
 * A 404 or 405 means there is no consent page to open, so callers fall
 * back to newSessionUrl instead of showing the user that error. A login
 * redirect, a 400 for a probe without the pairing query, or a 200 means
 * the page is there and the browser hop should proceed.
 */
export function consentPageMissing(status: number): boolean {
  return status === 404 || status === 405;
}

export function buildPairPageUrl(
  pairUrl: string,
  params: { challenge: string; state: string; device: string },
): string {
  const url = new URL(pairUrl);
  url.searchParams.set("challenge", params.challenge);
  url.searchParams.set("state", params.state);
  url.searchParams.set("device", params.device);
  return url.toString();
}

/** Parses lookout://pair?code=…&state=…; null when the URL is anything else. */
export function parsePairCallback(raw: string): { code: string; state: string } | null {
  let url: URL;
  try {
    // Same normalization trick as extractToken: URL() in WebKit chokes on
    // custom-scheme authority parsing.
    url = new URL(raw.replace("lookout://", "https://lookout.local/"));
  } catch {
    return null;
  }
  const isPair =
    url.pathname === "/pair" || url.pathname === "/pair/" || url.pathname.startsWith("/pair/");
  if (!isPair) return null;
  const code = url.searchParams.get("code");
  const state = url.searchParams.get("state");
  // Codes are program-issued and opaque, but bound their size so a hostile
  // link can't stuff megabytes into storage/requests.
  if (!code || !state || code.length > 512 || state.length > 512) return null;
  return { code, state };
}

/**
 * fetch with a timeout via AbortController — AbortSignal.timeout() is newer
 * than some WebKitGTK builds the Linux app still runs on.
 */
async function fetchWithTimeout(
  url: string,
  init: RequestInit,
  timeoutMs: number,
): Promise<Response> {
  const controller = new AbortController();
  const timer = setTimeout(() => controller.abort(), timeoutMs);
  try {
    return await fetch(url, { ...init, signal: controller.signal });
  } finally {
    clearTimeout(timer);
  }
}

function deviceLabel(): string {
  const os = isMacOS ? "macOS" : isWindows ? "Windows" : "Linux";
  return `Lookout Desktop (${os})`;
}

// ---------------------------------------------------------------------------
// Link metadata (localStorage) + credential (Rust secret store)
// ---------------------------------------------------------------------------

function readLinks(): Record<string, ProgramLink> {
  try {
    const raw = localStorage.getItem(LINKS_KEY);
    if (!raw) return {};
    const parsed = JSON.parse(raw);
    return parsed && typeof parsed === "object" ? parsed : {};
  } catch {
    return {};
  }
}

function writeLinks(links: Record<string, ProgramLink>): void {
  localStorage.setItem(LINKS_KEY, JSON.stringify(links));
}

export function getLinkedPrograms(): ProgramLink[] {
  return Object.values(readLinks()).sort((a, b) => a.program.localeCompare(b.program));
}

export function getLink(program: string): ProgramLink | null {
  return readLinks()[program] ?? null;
}

const secretKey = (program: string) => `program-device-token:${program}`;

// ---------------------------------------------------------------------------
// Pairing
// ---------------------------------------------------------------------------

function readPending(): PendingPairing | null {
  try {
    const raw = localStorage.getItem(PENDING_KEY);
    if (!raw) return null;
    const p: PendingPairing = JSON.parse(raw);
    if (!p || typeof p.state !== "string") return null;
    if (Date.now() - p.createdAt > PENDING_TTL_MS) {
      localStorage.removeItem(PENDING_KEY);
      return null;
    }
    return p;
  } catch {
    return null;
  }
}

/**
 * Kick off pairing: persist the pending state (localStorage, so a cold-start
 * callback still completes after the app restarts) and open the program's
 * consent page in the OS browser. Resolution happens later, when the
 * lookout://pair deep link arrives and App routes it to completePairing().
 */
/**
 * GET the bare pair URL before we mint a verifier or open the browser.
 * A probe failure (offline, timeout) is not proof the page is missing, so
 * pairing continues and the browser hop can still succeed.
 */
async function assertConsentPageExists(pairUrl: string): Promise<void> {
  let res: Response;
  try {
    res = await fetchWithTimeout(pairUrl, { method: "GET" }, CONSENT_PROBE_TIMEOUT_MS);
  } catch (e) {
    console.warn("[pair] consent-page probe failed, opening it anyway:", e);
    return;
  }
  if (consentPageMissing(res.status)) {
    throw new Error(`consent page returned ${res.status}`);
  }
}

export async function beginPairing(program: LinkableProgram): Promise<void> {
  if (!isLinkable(program)) throw new Error("program does not support pairing");
  const pairUrl = program.pairUrl!;
  // Before any pending state is stored. Callers already fall back to
  // newSessionUrl when this throws — that is the create page Lapse actually ships.
  await assertConsentPageExists(pairUrl);
  const verifier = base64url(randomBytes(32));
  const state = base64url(randomBytes(16));
  const challenge = base64url(sha256(new TextEncoder().encode(verifier)));

  const pending: PendingPairing = {
    program: program.name,
    state,
    verifier,
    origin: new URL(pairUrl).origin,
    pairUrl,
    startUrl: program.startUrl!,
    createdAt: Date.now(),
    displayName: program.displayName,
    iconUrl: program.iconUrl ?? null,
  };
  // One pending pairing at a time: starting a new one abandons the old, whose
  // callback will no longer match any state.
  localStorage.setItem(PENDING_KEY, JSON.stringify(pending));

  const page = buildPairPageUrl(pairUrl, { challenge, state, device: deviceLabel() });
  console.log(`[pair] opening consent page for ${program.name}`);
  await invoke("open_external_url", { url: page });
}

/**
 * Complete a pairing from a lookout://pair callback. Returns the linked
 * program's name, or null when the callback matched no pending pairing
 * (stale, replayed, or forged — all safely ignorable).
 */
export async function completePairing(rawUrl: string): Promise<string | null> {
  const cb = parsePairCallback(rawUrl);
  if (!cb) return null;
  const pending = readPending();
  if (!pending || pending.state !== cb.state) {
    console.warn("[pair] callback matched no pending pairing — ignoring");
    return null;
  }
  // The code is single-use on the program side; clear the pending entry first
  // so a failed exchange can't be replayed against a half-cleared state.
  localStorage.removeItem(PENDING_KEY);

  const res = await fetchWithTimeout(
    pending.pairUrl,
    {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({ code: cb.code, verifier: pending.verifier }),
    },
    EXCHANGE_TIMEOUT_MS,
  );
  if (!res.ok) {
    throw new Error(`pairing exchange failed: HTTP ${res.status}`);
  }
  const data = await res.json().catch(() => ({}));
  const deviceToken = data?.deviceToken;
  if (typeof deviceToken !== "string" || !deviceToken || deviceToken.length > 4096) {
    throw new Error("pairing exchange returned no deviceToken");
  }

  // rawInvoke, not the logging wrapper: this argument is a bearer credential, and
  // the debug buffer it would land in is what users paste into support threads
  // (and what Sentry captures as a breadcrumb). The wrapper redacts this
  // command too — both, because the cost of being wrong here is the whole
  // credential.
  await rawInvoke("secret_set", { key: secretKey(pending.program), value: deviceToken });
  const links = readLinks();
  links[pending.program] = {
    program: pending.program,
    origin: pending.origin,
    pairUrl: pending.pairUrl,
    startUrl: pending.startUrl,
    pairedAt: new Date().toISOString(),
    deviceLabel: deviceLabel(),
    displayName: pending.displayName,
    iconUrl: pending.iconUrl ?? null,
  };
  writeLinks(links);
  console.log(`[pair] linked ${pending.program}`);
  return pending.program;
}

/**
 * Unlink a program: best-effort revoke on the program's backend (DELETE
 * pairUrl with the bearer token), then drop the credential and metadata
 * locally regardless — the user asked to unlink, and a dead program server
 * must not be able to hold the link hostage.
 */
export async function unlinkProgram(program: string): Promise<void> {
  const link = getLink(program);
  const key = secretKey(program);
  if (link) {
    try {
      const token = await invoke<string | null>("secret_get", { key });
      if (token) {
        await fetchWithTimeout(
          link.pairUrl,
          { method: "DELETE", headers: { Authorization: `Bearer ${token}` } },
          10_000,
        );
      }
    } catch (e) {
      console.warn(`[pair] revoke for ${program} failed (unlinking anyway):`, e);
    }
  }
  try {
    await invoke("secret_delete", { key });
  } catch (e) {
    console.error(`[pair] failed to delete credential for ${program}:`, e);
  }
  const links = readLinks();
  delete links[program];
  writeLinks(links);
  console.log(`[pair] unlinked ${program}`);
}

// ---------------------------------------------------------------------------
// Starting a session through an established link
// ---------------------------------------------------------------------------

export type StartFailure =
  /** Credential rejected (revoked/expired). The link has been dropped; re-pair. */
  | "unauthorized"
  /** Registry now points somewhere else than the user consented to. Link dropped; re-pair. */
  | "origin-changed"
  /** Anything else: network, 5xx, bad payload. Link kept; fall back to the browser. */
  | "unavailable";

export class StartLinkedError extends Error {
  constructor(public readonly reason: StartFailure, message: string) {
    super(message);
  }
}

/**
 * Ask the program's backend to mint a session, then verify the token against
 * Lookout before handing it back. Throws StartLinkedError; never returns an
 * unverified token.
 */
export async function startLinkedSession(program: LinkableProgram): Promise<string> {
  const link = getLink(program.name);
  if (!link) throw new StartLinkedError("unavailable", "not linked");

  // Origin binding: only present the credential to the origin consented to at
  // pair time. A registry row re-aimed at another host drops the link.
  if (
    !isAcceptableEndpoint(program.startUrl) ||
    new URL(program.startUrl).origin !== link.origin
  ) {
    console.warn(`[pair] ${program.name} start URL moved origins — dropping link`);
    await unlinkProgram(program.name);
    throw new StartLinkedError("origin-changed", "start URL origin changed since pairing");
  }

  const token = await invoke<string | null>("secret_get", { key: secretKey(program.name) });
  if (!token) {
    // Metadata without a credential (e.g. secrets file lost) — self-heal.
    const links = readLinks();
    delete links[program.name];
    writeLinks(links);
    throw new StartLinkedError("unauthorized", "credential missing");
  }

  let res: Response;
  try {
    res = await fetchWithTimeout(
      program.startUrl,
      {
        method: "POST",
        headers: {
          Authorization: `Bearer ${token}`,
          "Content-Type": "application/json",
        },
        body: JSON.stringify({}),
      },
      START_TIMEOUT_MS,
    );
  } catch (e) {
    throw new StartLinkedError("unavailable", `start request failed: ${e}`);
  }
  if (res.status === 401 || res.status === 403) {
    // Revoked on the program side (their device list, password change, …).
    console.warn(`[pair] ${program.name} rejected the device credential — unlinking`);
    await unlinkProgram(program.name);
    throw new StartLinkedError("unauthorized", `credential rejected: HTTP ${res.status}`);
  }
  if (!res.ok) {
    throw new StartLinkedError("unavailable", `start failed: HTTP ${res.status}`);
  }
  const data = await res.json().catch(() => ({}));
  const sessionToken = data?.sessionToken;
  if (typeof sessionToken !== "string" || !isValidToken(sessionToken)) {
    throw new StartLinkedError("unavailable", "start returned no valid sessionToken");
  }

  // Trust, then verify: the token must exist on OUR Lookout server, belong to
  // the program we asked, and be recordable. This closes the loop — a program
  // backend can't hand us someone else's token or a finished one.
  let check: Response;
  try {
    check = await fetchWithTimeout(
      `${getApiBase()}/api/sessions/${sessionToken}`,
      {},
      START_TIMEOUT_MS,
    );
  } catch (e) {
    throw new StartLinkedError("unavailable", `session verification failed: ${e}`);
  }
  if (!check.ok) {
    throw new StartLinkedError("unavailable", `session verification failed: HTTP ${check.status}`);
  }
  const session = await check.json().catch(() => ({}));
  if (session?.program !== program.name) {
    throw new StartLinkedError(
      "unavailable",
      `session belongs to "${session?.program ?? "unknown"}", expected "${program.name}"`,
    );
  }
  if (!["pending", "active", "paused"].includes(session?.status)) {
    throw new StartLinkedError("unavailable", `session is not recordable (${session?.status})`);
  }

  return sessionToken;
}
