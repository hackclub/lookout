#!/usr/bin/env bash
# Build Lookout's System Settings pane (swift/lookout-prefpane) as a
# universal Lookout.prefPane. bundle.sh puts it in
# Lookout.app/Contents/Library/PreferencePanes, and the app copies it to
# ~/Library/PreferencePanes at launch (src/platform/pref_pane.rs).
#
#   packaging/macos/prefpane.sh --out <dir> [--icon <icns>] [--sign <identity>]
#
# --sign takes a codesign identity, or "-" (the default) for ad hoc. A real
# identity signs with the hardened runtime and a timestamp, for notarization.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"            # clients/native
src="$root/swift/lookout-prefpane"
out=""
icon=""
identity="-"

while [ $# -gt 0 ]; do
  case "$1" in
    --out) out="$2"; shift 2 ;;
    --icon) icon="$2"; shift 2 ;;
    --sign) identity="$2"; shift 2 ;;
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
[ -n "$out" ] || { echo "prefpane.sh: --out is required" >&2; exit 2; }

version="$("$root/packaging/version.sh")"
bundle_version="${version%%-*}"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

echo "==> Building Lookout.prefPane $version"
for arch in arm64 x86_64; do
  xcrun swiftc -O -whole-module-optimization -swift-version 5 \
    -module-name LookoutPrefPane -emit-library \
    -target "$arch-apple-macos10.15" \
    -framework AppKit -framework PreferencePanes \
    -o "$work/Lookout-$arch" "$src"/*.swift
done

pane="$work/Lookout.prefPane"
mkdir -p "$pane/Contents/MacOS" "$pane/Contents/Resources"
lipo -create -output "$pane/Contents/MacOS/Lookout" "$work/Lookout-arm64" "$work/Lookout-x86_64"
sed -e "s/@VERSION@/$version/g" -e "s/@BUNDLE_VERSION@/$bundle_version/g" "$src/Info.plist" > "$pane/Contents/Info.plist"
plutil -lint "$pane/Contents/Info.plist"
# The Settings icons, as template PDFs drawn from the app's own SVGs (the
# names match `Icon` in Kit.swift).
python3 "$here/svg2pdf.py" "$pane/Contents/Resources/icons" \
  $(for n in funnel-fill link-bold wrench-fill lk-power lk-play info-fill caret-right-bold check-bold; do
      echo "$root/assets/icons/$n.svg"; done)
if [ -n "$icon" ]; then
  cp "$icon" "$pane/Contents/Resources/icon.icns"
fi

if [ "$identity" = - ]; then
  codesign --force --sign - "$pane"
else
  codesign --force --timestamp --options runtime --sign "$identity" "$pane"
fi
codesign --verify --strict --verbose=2 "$pane"

mkdir -p "$out"
rm -rf "$out/Lookout.prefPane"
ditto "$pane" "$out/Lookout.prefPane"
echo "==> $out/Lookout.prefPane"
