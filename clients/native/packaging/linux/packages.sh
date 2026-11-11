#!/usr/bin/env bash
# Build the existing lookout (deb/rpm) and lookout-bin (pacman) packages.
# Run on Ubuntu 24.04 of the desired architecture; needs dpkg-dev, rpm, zstd,
# patchelf and binutils.
#
# One binary serves every distribution, down to glibc 2.34 (RHEL/AlmaLinux/
# Rocky 9), including ones without WebKitGTK 4.1: see "Optional WebKitGTK"
# below.
# Usage: packages.sh [--out DIR] [--no-build]
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
root="$(cd "$here/../.." && pwd)"
repo="$(cd "$root/../.." && pwd)"
out="$root/target/bundle"
build=1
while [ $# -gt 0 ]; do
  case "$1" in
    --out) out="$2"; shift 2 ;;
    --no-build) build=0; shift ;;
    *) echo "unknown argument: $1" >&2; exit 2 ;;
  esac
done
case "$(uname -m)" in
  x86_64) debarch=amd64; arch=x86_64 ;;
  aarch64) debarch=arm64; arch=aarch64 ;;
  *) echo 'unsupported architecture' >&2; exit 2 ;;
esac
version="$("$root/packaging/version.sh")"
# Debian sorts ~ before the final release; RPM separates the release field.
pkgversion="${version/-/~}"
mkdir -p "$out"
out="$(cd "$out" && pwd)"
work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
stage="$work/stage"
# Lazy binding: calls into shared libraries bind on first use, which is what
# lets WebKitGTK be loaded only when it's wanted (below). Rust's default full
# RELRO calls through the GOT and binds everything at startup; partial RELRO
# goes through the PLT, and -z lazy overrides the -z now Ubuntu's gcc adds.
if [ "$build" = 1 ]; then
  (cd "$root" && RUSTFLAGS="${RUSTFLAGS:-} -C relro-level=partial -C link-arg=-Wl,-z,lazy" cargo build --release -p lookout-native)
fi
bin="$stage/usr/bin/lookout"
install -Dm755 "$root/target/release/lookout" "$bin"
strip "$bin"
install -Dm644 "$here/com.hackclub.lookout.desktop" "$stage/usr/share/applications/Lookout.desktop"
# Retain the Tauri icon name for existing pinned launchers.
install -Dm644 "$root/assets/app-icon.png" "$stage/usr/share/icons/hicolor/128x128/apps/lookout.png"
install -Dm644 "$root/assets/app-icon.png" "$stage/usr/share/icons/hicolor/128x128/apps/com.hackclub.lookout.png"
legacy="$repo/clients/desktop/src-tauri/linux"
install -Dm644 "$legacy/lookout-archive-keyring.asc" "$stage/usr/share/lookout/lookout-archive-keyring.asc"

# Resolve the ELF's actual linked dependencies, including ABI version floors.
mkdir -p "$work/debian"
printf 'Source: lookout\nMaintainer: Hack Club <team@hackclub.com>\n' > "$work/debian/control"
# Before WebKitGTK is made optional below: Debian and Ubuntu always have it,
# so the .deb keeps depending on it.
shlibs=$(cd "$work" && dpkg-shlibdeps -O -e"$bin")

# --- Optional WebKitGTK, and the glibc floor --------------------------------
# WebKitGTK 4.1 (with JavaScriptCore and libsoup 3) only backs the Program
# Panel's embedded page, and RHEL 9 and its rebuilds ship none of them.
# Strip them from the binary's needed libraries so it starts without them;
# the app loads WebKitGTK globally when a panel opens and offers the browser
# when it can't (platform/webview.rs). That only works while every call into
# these libraries is bound lazily, through a PLT slot, so check that too.
webkit_libs='libwebkit2gtk-4.1.so.0 libjavascriptcoregtk-4.1.so.0 libsoup-3.0.so.0'
if readelf -d "$bin" | grep -Eq '\((FLAGS|FLAGS_1)\).*NOW'; then
  echo 'error: the binary binds every symbol at startup; build it as above (partial RELRO, -z lazy)' >&2
  exit 1
fi
eager=$(readelf -rW "$bin" | grep -E ' (webkit|jsc|soup)_[A-Za-z0-9_]+' | grep -v '_JUMP_SLOT ' || true)
if [ -n "$eager" ]; then
  printf 'error: these WebKitGTK references are bound at startup, so the binary would not start without it:\n%s\n' "$eager" >&2
  exit 1
fi
for lib in $webkit_libs; do patchelf --remove-needed "$lib" "$bin"; done
if readelf -d "$bin" | grep -Eq 'NEEDED.*(webkit2gtk|javascriptcoregtk|soup-3)'; then
  echo 'error: WebKitGTK is still a needed library' >&2
  exit 1
fi
# A symbol the binary can't do without, versioned for a glibc newer than 2.34,
# would stop it starting on RHEL 9. Weak ones (Rust's std probes for newer
# calls that way) resolve to nothing on an older glibc, and std copes.
too_new=$(objdump -T "$bin" | grep -E '\(GLIBC_2\.(3[5-9]|[4-9][0-9])\)' | grep -Ev '^[0-9a-f]+ +w ' || true)
if [ -n "$too_new" ]; then
  printf 'error: these need a glibc newer than 2.34 (see platform/glibc_compat.rs):\n%s\n' "$too_new" >&2
  exit 1
fi
depends="${shlibs#shlibs:Depends=}, libvulkan1, libegl1, libgl1, desktop-file-utils, hicolor-icon-theme, glib-networking, gstreamer1.0-plugins-base, gstreamer1.0-plugins-good, gstreamer1.0-plugins-bad, gstreamer1.0-plugins-ugly, gstreamer1.0-libav, gstreamer1.0-pipewire"
mkdir -p "$stage/DEBIAN"
cat > "$stage/DEBIAN/control" <<CONTROL
Package: lookout
Version: $pkgversion
Architecture: $debarch
Maintainer: Hack Club <team@hackclub.com>
Section: video
Priority: optional
Depends: $depends
Description: Lookout native desktop timelapse recorder
Homepage: https://github.com/hackclub/lookout
CONTROL
install -m755 "$legacy/postinst.sh" "$stage/DEBIAN/postinst"
install -m755 "$legacy/postrm.sh" "$stage/DEBIAN/postrm"
dpkg-deb --root-owner-group --build "$stage" "$out/Lookout_linux-${version}_${debarch}.deb"
rm -rf "$stage/DEBIAN"

# RPM automatically derives SONAME/ABI requirements from the same ELF.
mkdir -p "$work/rpm/SPECS"
cat > "$work/rpm/SPECS/lookout.spec" <<SPEC
Name: lookout
Version: $pkgversion
Release: 1
Summary: Lookout native desktop timelapse recorder
License: AGPL-3.0-or-later
URL: https://github.com/hackclub/lookout
Requires: desktop-file-utils, hicolor-icon-theme, glib-networking, glibc >= 2.34
# rpm reads the weak glibc references above as hard requirements; packages.sh
# has checked that every one newer than 2.34 is weak.
%global __requires_exclude ^lib(c|m)[.]so[.]6[(]GLIBC_2[.](3[5-9]|[4-9][0-9])[)]
# Optional (see above): installed wherever the distribution has it, so the
# Program Panel opens in the app; without it, it opens in the browser.
Recommends: libwebkit2gtk-4.1.so.0()(64bit)
# Capabilities rather than package names, so one rpm installs on Fedora/RHEL
# and openSUSE alike: both distributions name these packages differently
# (vulkan-loader/libvulkan1, pipewire-gstreamer/gstreamer-plugin-pipewire,
# gstreamer1-plugins-good/gstreamer-plugins-good, ...), but both generate the
# same soname and gstreamer1(element-*) provides. These are dlopened, so rpm
# can't find them in the ELF by itself.
Requires: libvulkan.so.1()(64bit), libEGL.so.1()(64bit), libGL.so.1()(64bit)
# capture
Requires: gstreamer1(element-pipewiresrc)()(64bit), gstreamer1(element-videoconvert)()(64bit)
Requires: gstreamer1(element-appsink)()(64bit), gstreamer1(element-appsrc)()(64bit)
# clips: the guaranteed VP9/WebM encoder, and the MP4 path for H.264 encoders
Requires: gstreamer1(element-vp9enc)()(64bit), gstreamer1(element-webmmux)()(64bit)
Requires: gstreamer1(element-mp4mux)()(64bit), gstreamer1(element-h264parse)()(64bit)
# playback
Requires: gstreamer1(element-playbin)()(64bit), gstreamer1(element-matroskademux)()(64bit)
# Better H.264 when available. libav has no element provides (its elements
# are generated at runtime), so it is named, either distribution's way.
Recommends: gstreamer1(element-openh264enc)()(64bit)
Recommends: (gstreamer1-plugin-libav or gstreamer-plugins-libav)
%description
Lookout native desktop timelapse recorder.
%install
mkdir -p %{buildroot}
cp -a "$stage/usr" %{buildroot}/
%post
SPEC
cat "$legacy/rpm-postinst.sh" >> "$work/rpm/SPECS/lookout.spec"
printf '\n%%postun\n' >> "$work/rpm/SPECS/lookout.spec"
cat "$legacy/rpm-postrm.sh" >> "$work/rpm/SPECS/lookout.spec"
printf '\n%%files\n/usr/bin/lookout\n/usr/share/applications/Lookout.desktop\n/usr/share/icons/hicolor/128x128/apps/*.png\n/usr/share/lookout\n' >> "$work/rpm/SPECS/lookout.spec"
rpmbuild --define "_topdir $work/rpm" --define '_build_id_links none' -bb "$work/rpm/SPECS/lookout.spec"
cp "$work"/rpm/RPMS/"$arch"/*.rpm "$out/Lookout_linux-${version}_${arch}.rpm"

# Keep the established Arch package identity. Enrollment is handled by install.sh.
cat > "$stage/.PKGINFO" <<PKGINFO
pkgname = lookout-bin
pkgver = ${version//-/_}-1
pkgdesc = Lookout native desktop timelapse recorder
url = https://github.com/hackclub/lookout
builddate = $(date +%s)
packager = Hack Club
size = $(du -sb "$stage/usr" | cut -f1)
arch = $arch
license = AGPL-3.0-or-later
provides = lookout
PKGINFO
for dep in desktop-file-utils hicolor-icon-theme glibc gcc-libs gtk3 webkit2gtk-4.1 libxkbcommon libxkbcommon-x11 libxcb libx11 libxrandr libxcursor libxi fontconfig freetype2 libpipewire mesa libglvnd vulkan-icd-loader glib-networking gstreamer gst-plugins-base gst-plugins-good gst-plugins-bad gst-plugins-ugly gst-libav gst-plugin-pipewire; do
  echo "depend = $dep" >> "$stage/.PKGINFO"
done
tar --zstd --owner=0 --group=0 -C "$stage" -cf "$out/Lookout_linux-${version}_${arch}.pkg.tar.zst" .PKGINFO usr
