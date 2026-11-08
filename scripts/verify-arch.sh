#!/bin/bash
# Verifies a built Arch package inside a clean archlinux container. Run by the
# "Verify the Arch package installs" step in .github/workflows/release.yml,
# which mounts this file and the bundle directory, and passes PKGFILE.
#
# The .PKGINFO is written by hand in the workflow, so nothing else checks that
# its depend names exist. One that doesn't (libsoup, which is only in the AUR,
# for libsoup3) made the package uninstallable for every Arch user, and the
# build was green the whole time.
set -eux

pacman -Syu --noconfirm --needed >/dev/null
# Resolving and installing for real is the assertion: a depend name that is
# not in the official repositories fails right here.
pacman -U --noconfirm "/pkg/$PKGFILE"
pacman -Qi lookout-bin >/dev/null

# And the shipped executable must have all its shared libraries.
test -x /usr/bin/lookout
ldd /usr/bin/lookout | (grep -i "not found" && exit 1 || true)
echo "OK: $PKGFILE installs from the official repositories and /usr/bin/lookout links"
