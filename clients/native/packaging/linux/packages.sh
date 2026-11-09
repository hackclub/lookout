#!/usr/bin/env bash
# Build the existing lookout (deb/rpm) and lookout-bin (pacman) packages.
# Run on Ubuntu 24.04 of the desired architecture; needs dpkg-dev, rpm, zstd.
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
if [ "$build" = 1 ]; then (cd "$root" && cargo build --release -p lookout-native); fi
install -Dm755 "$root/target/release/lookout" "$stage/usr/bin/lookout"
strip "$stage/usr/bin/lookout"
install -Dm644 "$here/com.hackclub.lookout.desktop" "$stage/usr/share/applications/Lookout.desktop"
# Retain the Tauri icon name for existing pinned launchers.
install -Dm644 "$root/assets/app-icon.png" "$stage/usr/share/icons/hicolor/128x128/apps/lookout.png"
install -Dm644 "$root/assets/app-icon.png" "$stage/usr/share/icons/hicolor/128x128/apps/com.hackclub.lookout.png"
legacy="$repo/clients/desktop/src-tauri/linux"
install -Dm644 "$legacy/lookout-archive-keyring.asc" "$stage/usr/share/lookout/lookout-archive-keyring.asc"

# Resolve the ELF's actual linked dependencies, including ABI version floors.
mkdir -p "$work/debian"
printf 'Source: lookout\nMaintainer: Hack Club <team@hackclub.com>\n' > "$work/debian/control"
shlibs=$(cd "$work" && dpkg-shlibdeps -O -e"$stage/usr/bin/lookout")
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
Requires: desktop-file-utils, hicolor-icon-theme
Requires: glibc >= 2.39, vulkan-loader, libglvnd-egl, libglvnd-glx, glib-networking
Requires: pipewire-gstreamer, gstreamer1-plugins-base, gstreamer1-plugins-good, gstreamer1-plugins-bad-free
Recommends: gstreamer1-plugin-openh264, gstreamer1-libav
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
