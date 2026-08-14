/**
 * Module-level "is a capture running right now" cell.
 *
 * The updater needs to know this because installing on Windows never returns:
 * the plugin hands the installer to ShellExecuteW and then calls
 * `std::process::exit(0)`. Doing that mid-session would kill the recording and
 * lose every screenshot that hadn't uploaded yet.
 *
 * The recorder lives several levels below the updater hook, so rather than
 * drilling `isCapturing` up through RecordPage into App just to gate one
 * branch, both sides touch this one cell.
 */
let capturing = false;

export function setCaptureActive(active: boolean): void {
  capturing = active;
}

export function isCaptureActive(): boolean {
  return capturing;
}
