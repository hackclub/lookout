#!/usr/bin/env bash
# Install a locally built Lookout Native for the current user, including the
# lookout:// URL handler. Run from clients/native after `cargo build --release`.
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$here/../.."
bin="$root/target/release/lookout"
[ -x "$bin" ] || { echo "build first: cargo build --release" >&2; exit 1; }
install -Dm755 "$bin" "$HOME/.local/bin/lookout"
install -Dm644 "$here/com.hackclub.lookout.desktop" "$HOME/.local/share/applications/com.hackclub.lookout.desktop"
install -Dm644 "$root/assets/app-icon.png" "$HOME/.local/share/icons/hicolor/128x128/apps/com.hackclub.lookout.png"
update-desktop-database "$HOME/.local/share/applications" 2>/dev/null || true
xdg-mime default com.hackclub.lookout.desktop x-scheme-handler/lookout 2>/dev/null || true
echo "Installed. lookout:// links now open Lookout Native."
