/**
 * The session token is the whole auth model, and it arrives in the URL.
 * Both facts are worth one small, tested module.
 */

/** Session tokens are 32 random bytes, hex-encoded. */
const TOKEN_PATTERN = /^[a-f0-9]{64}$/i;

export function isValidToken(value: string | null | undefined): value is string {
  return typeof value === "string" && TOKEN_PATTERN.test(value);
}

export interface UrlOptions {
  token: string | null;
  /** `?edit=false` drops the "Edit & save" option from the stop dialog, for
   *  programs that don't want users trimming their own recordings. The
   *  dialog itself still appears: stopping stays a confirmed action either
   *  way. */
  editing: boolean;
  /** `?app=` — the program that sent the user here. Rides along into
   *  client telemetry so a session's captures are attributable to the
   *  program, the same way the web client does it. */
  appName?: string;
}

export function readUrlOptions(search: string): UrlOptions {
  const params = new URLSearchParams(search);
  const token = params.get("token");
  const appName = params.get("app")?.trim();
  return {
    token: isValidToken(token) ? token : null,
    editing: params.get("edit") !== "false",
    ...(appName ? { appName } : {}),
  };
}

/**
 * The deep link that hands a session to the installed desktop app.
 * Must stay byte-identical to what the desktop app parses — see
 * `handle_deep_link_urls` in clients/desktop/src-tauri/src/lib.rs.
 */
export function desktopHandoffUrl(token: string): string {
  return `lookout://session/?token=${encodeURIComponent(token)}`;
}
