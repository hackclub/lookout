# Tauri → native upgrade verification

Verified locally on 2026-11-08. This is not approval for an automatic migration
on every platform. Nothing was published during this audit.

## Verified

- The published v0.3.11 Debian and RPM artifacts both identify their package as
  `lookout`, matching the new native packages. Pacman retains `lookout-bin`.
- An isolated Ubuntu 24.04 filesystem accepted the actual published Tauri
  v0.3.11 .deb followed by the locally generated native v0.4.0 .deb. The old
  binary was `lookout-desktop`; the replacement provides `lookout` and the
  legacy alias. Repository enrollment and the desktop URL handler worked.
  Removing native removed enrollment and retained a sentinel secrets file.
  **This transaction deliberately bypassed dependencies**: the local native
  binary was built on Ubuntu 26.04 and needs its newer glibc. This proves
  package replacement/maintainer scripts, not Ubuntu 24.04 runtime support.
- A separate Ubuntu 26.04 base filesystem resolved all declared dependencies
  for that local native .deb. After extracting those dependencies, every
  linked library resolved. GStreamer found x264enc, avdec_h264, vp9enc,
  pipewiresrc, and souphttpsrc; synthetic video encoded to H.264/MP4 and
  decoded successfully. This does not test a real screen/camera/portal.
- All 44 native unit tests passed with `--no-default-features`, including
  installer argument parsing and four SQLite migration tests. A default
  feature test run was attempted but this machine lacks libsoup/WebKitGTK
  development pkg-config files. CI has those build dependencies.
- The Linux packages were generated locally in all three formats and their
  names, architecture metadata, desktop handler and enrollment hooks checked.
- The native updater library and manifest/signing tool passed 37 tests,
  including manifest generation/signing verification.
- Workflow YAML parses, shell scripts pass syntax checks, and `git diff
  --check` passes. Native unit tests are now part of each release build.

## Confirmed migration defect fixed

The WebKit importer previously scanned raw SQLite bytes for keys and guessed
where each value ended. A real SQLite fixture showed it appending unrelated
page bytes to the configured server URL; deleted keys also remained visible
to that scanner. SQLite overflow pages and committed WAL records were not
handled either.

The importer now uses bundled SQLite through rusqlite, opens the source
read-only, and selects live `ItemTable` records. Regression tests cover exact
large/Unicode values, replacement/deletion, invalid UTF-16, and committed WAL
data. Original webview storage remains in place. Read errors now propagate and leave
the import incomplete so it can retry on the next launch. Existing native
profiles already marked imported by an older native build are not automatically
re-imported.

## Remaining release blockers and gaps

- **macOS/Windows automatic delivery is not switched to native.** The release
  workflow still generates `latest.json` from Tauri artifacts. Native artifacts
  use the separate `update.json` manifest, which existing Tauri apps do not
  consume. Publishing both sets does not migrate those users automatically.
- **Windows settings migration is not reliable enough to certify.** It still
  scans raw LevelDB files rather than reading live database records. Table
  compression, tombstones, record boundaries, and string encodings require
  proper handling and real WebView2 fixtures.
- Native Windows only builds x86_64, while Tauri currently also ships arm64.
  An explicit delivery/support decision is needed for existing arm64 installs.
- macOS and Windows still need actual signed Tauri → native upgrade tests:
  launch/relaunch, login and settings, existing recordings, protocol handlers,
  permissions, rollback, and a subsequent native update. Matching app IDs,
  signing teams and file paths alone do not prove those behaviors.
- Ubuntu 24.04 runtime installation, Fedora RPM installation, Arch installation,
  and Linux arm64 remain CI checks, not locally completed runtime checks.
  Linux desktop/Wayland capture and session restoration still need desktop
  testing. RPM installation dependencies differ by distribution; the added
  Fedora check does not establish openSUSE compatibility.
- A repository upgrade must have a version higher than the installed package.
  Replacing release assets with native builds of the same version will not
  normally cause package managers to upgrade existing installations.

The local logs and downloaded fixtures are under `/tmp/lookout-upgrade-audit`;
those are temporary artifacts, not committed release inputs.
