#!/usr/bin/env bash
# Build Lookout Native as an AppImage:
#
#   <out>/Lookout-native_<version>_linux-x86_64.AppImage
#
# Usage: packaging/linux/appimage.sh [--out <dir>] [--no-build]
#
# Meant to run on Ubuntu 24.04 (what CI uses): the AppImage needs at least the
# build host's glibc, so build on the oldest distro you want to support.
# Build dependencies (apt) are the list in .github/workflows/release.yml's
# native-build job; on top of the -dev packages the GStreamer plugin packages
# must be installed, because the plugins are copied from the build host.
#
# Bundled: GTK 3 + WebKitGTK 4.1 (with its helper processes), libsoup 3,
#   glib-networking's TLS module, GStreamer core + the plugins listed below
#   (x264, libav, VP9, mp4/webm muxers, v4l2, souphttpsrc, ...) and everything
#   they link, except the libraries below.
# Taken from the host (must be installed there; every desktop distro has them):
#   glibc, libGL/libEGL/libgbm/libdrm/libvulkan + the GPU drivers (wgpu dlopens
#   Vulkan, falling back to GL), libwayland-*, libasound, libva (VA-API
#   drivers must match it), and libpipewire-0.3 (it loads the host's own
#   PipeWire modules, which must match the running daemon; screen capture on
#   Wayland and the pipewiresrc plugin use it).
#
# The tools (linuxdeploy + its GTK plugin, appimagetool, the static type-2
# runtime, so the AppImage needs no libfuse2) are downloaded once into
# $LOOKOUT_APPIMAGE_TOOLS (default target/appimage-tools) and checked against
# the pinned hashes. They run with APPIMAGE_EXTRACT_AND_RUN=1, so no FUSE is
# needed to build (containers, WSL).
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"            # clients/native
repo="$(cd "$root/../.." && pwd)"
out="$root/target/bundle"
build=1

while [ $# -gt 0 ]; do
  case "$1" in
    --out) out="$2"; shift 2 ;;
    --no-build) build=0; shift ;;
    -h|--help) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 0 ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done

[ "$(uname -m)" = x86_64 ] || { echo "appimage.sh: only x86_64 is packaged" >&2; exit 2; }
multiarch=x86_64-linux-gnu
libdir="/usr/lib/$multiarch"

version="$("$root/packaging/version.sh")"
name="Lookout-native_${version}_linux-x86_64"
tools="${LOOKOUT_APPIMAGE_TOOLS:-$root/target/appimage-tools}"
mkdir -p "$out" "$tools"
out="$(cd "$out" && pwd)"
tools="$(cd "$tools" && pwd)"
work="$root/target/appimage-work"
appdir="$work/AppDir"
rm -rf "$work"
mkdir -p "$appdir"

export APPIMAGE_EXTRACT_AND_RUN=1
export PATH="$tools:$PATH"

# ── tools ────────────────────────────────────────────────────────────────
fetch() { # <file> <url> <sha256>
  local f="$tools/$1"
  if [ -f "$f" ] && echo "$3  $f" | sha256sum -c --status; then chmod +x "$f"; return; fi
  echo "==> Downloading $1"
  curl -fsSL --retry 3 -o "$f.part" "$2"
  if ! echo "$3  $f.part" | sha256sum -c --status; then
    echo "appimage.sh: $1 does not match its pinned sha256 (got $(sha256sum "$f.part" | cut -d' ' -f1))" >&2
    rm -f "$f.part"
    exit 1
  fi
  mv "$f.part" "$f"
  chmod +x "$f"
}
fetch linuxdeploy-x86_64.AppImage \
  https://github.com/linuxdeploy/linuxdeploy/releases/download/1-alpha-20251107-1/linuxdeploy-x86_64.AppImage \
  c20cd71e3a4e3b80c3483cef793cda3f4e990aca14014d23c544ca3ce1270b4d
fetch linuxdeploy-plugin-gtk.sh \
  https://raw.githubusercontent.com/linuxdeploy/linuxdeploy-plugin-gtk/7a3fbc31a9e5075073ff8790f26effbac5f84453/linuxdeploy-plugin-gtk.sh \
  b0f4cbc684a0103a9651f0955b635eaea0096b3a66c0f5a2c2aa337960375171
fetch appimagetool-x86_64.AppImage \
  https://github.com/AppImage/appimagetool/releases/download/1.9.1/appimagetool-x86_64.AppImage \
  ed4ce84f0d9caff66f50bcca6ff6f35aae54ce8135408b3fa33abfc3cb384eb0
fetch runtime-x86_64 \
  https://github.com/AppImage/type2-runtime/releases/download/20251108/runtime-x86_64 \
  2fca8b443c92510f1483a883f60061ad09b46b978b2631c807cd873a47ec260d

# ── build ────────────────────────────────────────────────────────────────
echo "==> Lookout Native $version AppImage"
if [ "$build" = 1 ]; then
  # Line tables are embedded in a Linux ELF (not split out like a .pdb/.dSYM).
  # linuxdeploy strips the AppImage's copy; the one left in target/ is what
  # CI uploads to Sentry so native crash reports get function names and lines.
  (cd "$root" && cargo build --release -p lookout-native)
fi
bin="$root/target/release/lookout"
[ -x "$bin" ] || { echo "appimage.sh: $bin not found (build first, or drop --no-build)" >&2; exit 1; }

# ── AppDir ───────────────────────────────────────────────────────────────
install -Dm755 "$bin" "$appdir/usr/bin/lookout"
desktop="$appdir/usr/share/applications/com.hackclub.lookout.desktop"
install -Dm644 "$here/com.hackclub.lookout.desktop" "$desktop"
echo "X-AppImage-Version=$version" >> "$desktop"
icon="$root/assets/app-icon-512.png"   # assets/app-icon.png is its 128px copy
size=512
if [ ! -f "$icon" ]; then icon="$root/assets/app-icon.png"; size=128; fi
install -Dm644 "$icon" "$appdir/usr/share/icons/hicolor/${size}x${size}/apps/com.hackclub.lookout.png"

# WebKitGTK's helper processes. libwebkit2gtk has their directory compiled in
# (/usr/lib/<multiarch>/webkit2gtk-4.1); it is patched below to the same-length
# relative ././/lib/…, and the AppRun hook cds to $APPDIR/usr so it resolves
# inside the AppImage (the approach Tauri's AppImages use).
wk="$libdir/webkit2gtk-4.1"
[ -d "$wk" ] || { echo "appimage.sh: $wk missing (apt install libwebkit2gtk-4.1-dev)" >&2; exit 1; }
wk_dst="$appdir/usr/lib/$multiarch/webkit2gtk-4.1"
mkdir -p "$wk_dst"
for f in "$wk"/WebKit*Process "$wk"/injected-bundle; do cp -a "$f" "$wk_dst/"; done

# glib-networking's TLS backend: libsoup (WebKit, souphttpsrc) needs it for https.
mkdir -p "$appdir/usr/lib/gio/modules"
cp "$libdir"/gio/modules/libgio{gnutls,openssl}.so "$appdir/usr/lib/gio/modules/" 2>/dev/null || true
ls "$appdir"/usr/lib/gio/modules/*.so >/dev/null 2>&1 \
  || { echo "appimage.sh: no GIO TLS module in $libdir/gio/modules (apt install glib-networking)" >&2; exit 1; }

# GStreamer plugins. "a|b" takes the first that exists (1.22+ merged
# videoconvert + videoscale). Required: what the recorder, camera, Wayland
# capture, player and filmstrip build. Optional: hardware paths and the audio
# chain WebKit's <video> may want.
gst_src="$libdir/gstreamer-1.0"
gst_dst="$appdir/usr/lib/gstreamer-1.0"
mkdir -p "$gst_dst"
required=(coreelements app playback typefindfunctions "videoconvertscale|videoconvert" isomp4 matroska
  videoparsersbad jpeg video4linux2 autodetect x264 libav vpx soup pipewire)
optional=(videoscale videorate videofilter openh264 va audioconvert audioresample volume pulseaudio
  opus vorbis ogg wavparse id3demux audioparsers)
# LOOKOUT_APPIMAGE_SKIP_PLUGINS="libav ..." downgrades required plugins to
# optional, for a local test build on a machine without them. Never in CI.
skip=" ${LOOKOUT_APPIMAGE_SKIP_PLUGINS:-} "
copy_plugin() { # <name|alt...> <required 0/1>
  local alt
  if [[ "$skip" == *" $1 "* ]]; then set -- "$1" 0; fi
  IFS='|' read -ra alts <<< "$1"
  for alt in "${alts[@]}"; do
    if [ -f "$gst_src/libgst$alt.so" ]; then cp "$gst_src/libgst$alt.so" "$gst_dst/"; return 0; fi
  done
  if [ "$2" = 1 ]; then
    echo "appimage.sh: GStreamer plugin libgst${1//|/ or libgst}.so not found in $gst_src" >&2
    echo "  apt install gstreamer1.0-plugins-{base,good,bad,ugly} gstreamer1.0-libav gstreamer1.0-pipewire" >&2
    exit 1
  fi
  echo "    (optional plugin $1 not installed, skipped)"
}
for p in "${required[@]}"; do copy_plugin "$p" 1; done
for p in "${optional[@]}"; do copy_plugin "$p" 0; done
scanner_rel="lib/gstreamer1.0/gstreamer-1.0"
install -Dm755 "$libdir/gstreamer1.0/gstreamer-1.0/gst-plugin-scanner" "$appdir/usr/$scanner_rel/gst-plugin-scanner"

mkdir -p "$appdir/apprun-hooks"
cat > "$appdir/apprun-hooks/lookout.sh" <<EOF
# Lookout Native AppRun hook (packaging/linux/appimage.sh).
# libwebkit2gtk looks for its helpers at ././/lib/$multiarch/webkit2gtk-4.1.
cd "\$APPDIR/usr" || exit 1
# Only the bundled GStreamer plugins and GIO modules: host ones are built
# against other library versions.
export GST_PLUGIN_SYSTEM_PATH_1_0="\$APPDIR/usr/lib/gstreamer-1.0"
unset GST_PLUGIN_PATH_1_0 GST_PLUGIN_PATH
export GST_PLUGIN_SCANNER_1_0="\$APPDIR/usr/$scanner_rel/gst-plugin-scanner"
export GST_REGISTRY_1_0="\${XDG_CACHE_HOME:-\$HOME/.cache}/com.hackclub.lookout/gstreamer-1.0-registry.bin"
export GIO_MODULE_DIR="\$APPDIR/usr/lib/gio/modules"
EOF

# ── linuxdeploy: copy every linked library, set rpaths, GTK runtime data ─
echo "==> linuxdeploy"
excludes=()
for lib in 'libpipewire-0.3.so*' 'libva.so*' 'libva-*.so*' 'libvulkan.so*' 'libwayland-*.so*' \
           'libdrm.so*' 'libgbm.so*' 'libasound.so*'; do
  excludes+=(--exclude-library "$lib")
done
DEPLOY_GTK_VERSION=3 linuxdeploy-x86_64.AppImage \
  --appdir "$appdir" \
  --executable "$appdir/usr/bin/lookout" \
  --desktop-file "$desktop" \
  --icon-file "$appdir/usr/share/icons/hicolor/${size}x${size}/apps/com.hackclub.lookout.png" \
  --deploy-deps-only "$gst_dst" \
  --deploy-deps-only "$appdir/usr/$scanner_rel" \
  --deploy-deps-only "$appdir/usr/lib/gio/modules" \
  --deploy-deps-only "$wk_dst" \
  --deploy-deps-only "$wk_dst/injected-bundle" \
  "${excludes[@]}" \
  --plugin gtk

# The GTK plugin's hook forces GDK_BACKEND=x11. Lookout picks GTK's backend
# itself (the panel webview renders offscreen on Wayland), and with x11 forced
# GTK finds no allowed backend and the panel can't load. Keep the user's own
# GDK_BACKEND instead.
gtk_hook="$appdir/apprun-hooks/linuxdeploy-plugin-gtk.sh"
[ -f "$gtk_hook" ] || { echo "appimage.sh: linuxdeploy wrote no GTK AppRun hook" >&2; exit 1; }
sed -i '/^export GDK_BACKEND=/d' "$gtk_hook"
if grep -q 'GDK_BACKEND=' "$gtk_hook"; then echo "appimage.sh: couldn't drop GDK_BACKEND from $gtk_hook" >&2; exit 1; fi

lib_wk="$appdir/usr/lib/libwebkit2gtk-4.1.so.0"
[ -f "$lib_wk" ] || { echo "appimage.sh: linuxdeploy didn't bundle libwebkit2gtk-4.1.so.0" >&2; exit 1; }
needle="/usr/lib/$multiarch/webkit2gtk-4.1"
grep -qaF "$needle" "$lib_wk" || { echo "appimage.sh: $needle not found in libwebkit2gtk; the helper-path patch needs updating" >&2; exit 1; }
NEEDLE="$needle" REPL="././/lib/$multiarch/webkit2gtk-4.1" perl -pi -e 's/\Q$ENV{NEEDLE}\E/$ENV{REPL}/g' "$lib_wk"
if grep -qaF "$needle" "$lib_wk"; then echo "appimage.sh: patching libwebkit2gtk failed" >&2; exit 1; fi
[ -e "$appdir/AppRun" ] || { echo "appimage.sh: linuxdeploy wrote no AppRun" >&2; exit 1; }

# Every ELF we ship must resolve; log what still comes from the host, which
# must be only the libraries the header lists (plus glibc and the X11/GL set
# AppImage's excludelist leaves out).
echo "==> Libraries taken from the host"
# (GTK's immodules / pixbuf loaders are skipped: they are dlopened into a
# process that already has the bundled GTK loaded, so they carry no rpath.)
host_libs=$(find "$appdir/usr" -type f \( -name '*.so*' -o -perm -u+x \) \
    -not -path '*/gtk-3.0/*' -not -path '*/gdk-pixbuf-2.0/*' | while read -r f; do
  [ "$(head -c4 "$f" | tail -c3)" = ELF ] || continue
  env -u LD_LIBRARY_PATH ldd "$f" 2>/dev/null \
    | awk -v a="$appdir" '/not found/ {print "MISSING " $1; next} /=>/ && index($3, a) != 1 {print $1}'
done | sort -u)
echo "$host_libs" | tr '\n' ' '; echo
if echo "$host_libs" | grep -q '^MISSING'; then
  echo "appimage.sh: unresolved libraries:" >&2
  echo "$host_libs" | grep '^MISSING' >&2
  exit 1
fi

# ── AppImage ─────────────────────────────────────────────────────────────
echo "==> appimagetool"
rm -f "$out/$name.AppImage"
ARCH=x86_64 VERSION="$version" appimagetool-x86_64.AppImage --no-appstream \
  --runtime-file "$tools/runtime-x86_64" "$appdir" "$out/$name.AppImage"
chmod +x "$out/$name.AppImage"
ls -la "$out/$name.AppImage"
