#!/usr/bin/env bash
# Build Lookout Native for macOS and package it:
#
#   <out>/Lookout-native_<version>_darwin-<arch>.app.tar.gz   updater payload (tar of Lookout.app/)
#   <out>/Lookout-native_<version>_darwin-<arch>.dmg          first install
#
# Usage (from anywhere, on a Mac with Xcode; the build links the Swift
# package swift/lookout-tray through swift-rs):
#
#   packaging/macos/bundle.sh [--target aarch64-apple-darwin|x86_64-apple-darwin]
#                             [--out <dir>] [--no-build]
#
# Signing is driven by the same environment the Tauri release uses, and all
# of it is optional. Without it the app is ad-hoc signed and not notarized,
# which is fine for a local run.
#
#   APPLE_CERTIFICATE           base64 .p12 (Developer ID Application), imported
#   APPLE_CERTIFICATE_PASSWORD  into a throwaway keychain for this run
#   APPLE_SIGNING_IDENTITY      e.g. "Developer ID Application: Hack Club (TEAMID)"
#   APPLE_ID, APPLE_PASSWORD, APPLE_TEAM_ID   notarize + staple (needs an identity)
#
# Nothing but the executable goes in Contents/MacOS: video is AVFoundation,
# camera AVCaptureSession, the recorder VideoToolbox, and the Swift tray is
# linked statically against the OS Swift runtime (/usr/lib/swift). The script
# fails if the binary ever links a dylib outside /usr/lib or /System, so a new
# dependency can't silently ship a bundle that only runs on the build machine.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"            # clients/native
repo="$(cd "$root/../.." && pwd)"
target="$(rustc -vV | sed -n 's/^host: //p')"
out="$root/target/bundle"
build=1

while [ $# -gt 0 ]; do
  case "$1" in
    --target) target="$2"; shift 2 ;;
    --out) out="$2"; shift 2 ;;
    --no-build) build=0; shift ;;
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

case "$target" in
  aarch64-apple-darwin) arch=aarch64 ;;
  x86_64-apple-darwin) arch=x86_64 ;;
  *) echo "bundle.sh: $target is not a macOS target" >&2; exit 2 ;;
esac

version="$("$root/packaging/version.sh")"
bundle_version="${version%%-*}"
name="Lookout-native_${version}_darwin-${arch}"
identity="${APPLE_SIGNING_IDENTITY:--}"
mkdir -p "$out"
out="$(cd "$out" && pwd)"
work="$(mktemp -d)"
keychain=""
cleanup() {
  if [ -n "$keychain" ]; then security delete-keychain "$keychain" 2>/dev/null || true; fi
  rm -rf "$work"
}
trap cleanup EXIT

echo "==> Lookout Native $version for $target"

if [ "$build" = 1 ]; then
  (cd "$root" && MACOSX_DEPLOYMENT_TARGET=10.15 cargo build --release -p lookout-native --target "$target")
fi
bin="$root/target/$target/release/lookout"
[ -x "$bin" ] || { echo "bundle.sh: $bin not found (build first, or drop --no-build)" >&2; exit 1; }

echo "==> Checking linked libraries"
otool -L "$bin"
foreign=$(otool -L "$bin" | tail -n +2 | awk '{print $1}' | grep -vE '^(/usr/lib/|/System/Library/)' || true)
if [ -n "$foreign" ]; then
  echo "bundle.sh: the binary links libraries macOS doesn't ship; bundle them into Contents/Frameworks first:" >&2
  echo "$foreign" >&2
  exit 1
fi

echo "==> Assembling Lookout.app"
app="$work/Lookout.app"
mkdir -p "$app/Contents/MacOS" "$app/Contents/Resources"
cp "$bin" "$app/Contents/MacOS/lookout"
sed -e "s/@VERSION@/$version/g" -e "s/@BUNDLE_VERSION@/$bundle_version/g" "$here/Info.plist" > "$app/Contents/Info.plist"
plutil -lint "$app/Contents/Info.plist"
printf 'APPL????' > "$app/Contents/PkgInfo"

# Same artwork as assets/app-icon.png (a 128px copy of it); the Tauri icon set
# has every size up to 1024 already. Rebuild from the 512px PNG otherwise.
icns="$repo/clients/desktop/src-tauri/icons/icon.icns"
if [ -f "$icns" ]; then
  cp "$icns" "$app/Contents/Resources/icon.icns"
else
  src="$repo/clients/desktop/src-tauri/icons/icon.png"
  [ -f "$src" ] || src="$root/assets/app-icon.png"
  set_dir="$work/icon.iconset"
  mkdir -p "$set_dir"
  for s in 16 32 128 256 512; do
    sips -z "$s" "$s" "$src" --out "$set_dir/icon_${s}x${s}.png" >/dev/null
    sips -z $((s * 2)) $((s * 2)) "$src" --out "$set_dir/icon_${s}x${s}@2x.png" >/dev/null
  done
  iconutil -c icns "$set_dir" -o "$app/Contents/Resources/icon.icns"
fi

if [ -n "${APPLE_CERTIFICATE:-}" ]; then
  echo "==> Importing the signing certificate"
  keychain="$work/lookout-signing.keychain-db"
  kcpass="$(openssl rand -hex 24)"
  security create-keychain -p "$kcpass" "$keychain"
  security set-keychain-settings -lut 21600 "$keychain"
  security unlock-keychain -p "$kcpass" "$keychain"
  printf '%s' "$APPLE_CERTIFICATE" | base64 --decode > "$work/cert.p12"
  security import "$work/cert.p12" -k "$keychain" -P "${APPLE_CERTIFICATE_PASSWORD:-}" -T /usr/bin/codesign
  rm -f "$work/cert.p12"
  security set-key-partition-list -S apple-tool:,apple:,codesign: -s -k "$kcpass" "$keychain" >/dev/null
  # Search the throwaway keychain first, keeping the existing ones.
  existing=()
  while IFS= read -r k; do existing+=("$(echo "$k" | sed -e 's/^ *"//' -e 's/"$//')"); done < <(security list-keychains -d user)
  security list-keychains -d user -s "$keychain" "${existing[@]}"
  security find-identity -v -p codesigning "$keychain"
fi

echo "==> Signing ($([ "$identity" = - ] && echo ad-hoc || echo "$identity"))"
if [ "$identity" = - ]; then
  codesign --force --sign - --entitlements "$here/Entitlements.plist" "$app"
else
  codesign --force --timestamp --options runtime --entitlements "$here/Entitlements.plist" --sign "$identity" "$app"
fi
codesign --verify --deep --strict --verbose=2 "$app"

notarize() { # <file>
  echo "==> Notarizing $(basename "$1")"
  local res id
  res=$(xcrun notarytool submit "$1" --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" \
    --team-id "$APPLE_TEAM_ID" --wait --timeout 45m --output-format json) || true
  echo "$res"
  id=$(echo "$res" | sed -n 's/.*"id" *: *"\([^"]*\)".*/\1/p' | head -1)
  if ! echo "$res" | grep -q '"status" *: *"Accepted"'; then
    if [ -n "$id" ]; then
      xcrun notarytool log "$id" --apple-id "$APPLE_ID" --password "$APPLE_PASSWORD" --team-id "$APPLE_TEAM_ID" || true
    fi
    echo "bundle.sh: notarization of $(basename "$1") was not accepted" >&2
    exit 1
  fi
}

notarizing=0
if [ "$identity" != - ] && [ -n "${APPLE_ID:-}" ] && [ -n "${APPLE_PASSWORD:-}" ] && [ -n "${APPLE_TEAM_ID:-}" ]; then
  notarizing=1
  ditto -c -k --keepParent "$app" "$work/Lookout.zip"
  notarize "$work/Lookout.zip"
  xcrun stapler staple "$app"
  xcrun stapler validate "$app"
  spctl --assess --type execute --verbose=2 "$app"
else
  echo "==> Not notarizing (needs APPLE_SIGNING_IDENTITY, APPLE_ID, APPLE_PASSWORD, APPLE_TEAM_ID)"
fi

echo "==> Writing $name.app.tar.gz"
# The updater (updater/platform.rs unpack_app) strips the first component,
# so every entry must sit under Lookout.app/. COPYFILE_DISABLE keeps
# AppleDouble ._ files out.
COPYFILE_DISABLE=1 tar -C "$work" -czf "$out/$name.app.tar.gz" Lookout.app
bad=$(tar -tzf "$out/$name.app.tar.gz" | grep -v '^Lookout\.app/' || true)
[ -z "$bad" ] || { echo "bundle.sh: unexpected entries in the tarball: $bad" >&2; exit 1; }

echo "==> Writing $name.dmg"
stage="$work/dmg"
mkdir -p "$stage"
ditto "$app" "$stage/Lookout.app"
ln -s /Applications "$stage/Applications"
hdiutil create -volname Lookout -srcfolder "$stage" -fs HFS+ -format UDZO -ov "$out/$name.dmg"
if [ "$identity" != - ]; then
  codesign --force --timestamp --sign "$identity" "$out/$name.dmg"
fi
if [ "$notarizing" = 1 ]; then
  notarize "$out/$name.dmg"
  xcrun stapler staple "$out/$name.dmg"
fi

ls -la "$out/$name".*
