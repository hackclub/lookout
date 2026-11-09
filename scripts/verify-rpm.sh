#!/bin/bash
# Run in a clean Fedora container with /pkg and RPMFILE supplied by CI.
set -euo pipefail
dnf install -y --setopt=install_weak_deps=False "/pkg/$RPMFILE"
rpm -q lookout
test -x /usr/bin/lookout
test -L /usr/bin/lookout-desktop
links=$(ldd /usr/bin/lookout)
if echo "$links" | grep -qi 'not found'; then echo "$links"; exit 1; fi
test -f /usr/share/applications/Lookout.desktop
grep -q 'Exec=lookout %u' /usr/share/applications/Lookout.desktop
if grep -q 'BEGIN PGP PUBLIC KEY BLOCK' /usr/share/lookout/lookout-archive-keyring.asc; then
  test -r /etc/pki/rpm-gpg/RPM-GPG-KEY-lookout
  test -f /etc/yum.repos.d/lookout.repo
fi
# Do not refresh the newly enrolled repository: this release isn't public yet.
rpm -e lookout
test ! -e /etc/yum.repos.d/lookout.repo
test ! -e /usr/bin/lookout-desktop
echo 'OK: RPM installs, links, enrolls and removes cleanly'
