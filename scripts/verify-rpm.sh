#!/bin/bash
# Run in a clean Fedora or openSUSE container with /pkg and RPMFILE supplied
# by CI. One rpm serves both, so both install it.
set -euo pipefail
if command -v zypper >/dev/null; then
  # The release's rpm isn't signed yet; the repository signs it on publish.
  zypper --non-interactive install --allow-unsigned-rpm --no-recommends "/pkg/$RPMFILE"
  repo=/etc/zypp/repos.d/lookout.repo
else
  dnf install -y --setopt=install_weak_deps=False "/pkg/$RPMFILE"
  repo=/etc/yum.repos.d/lookout.repo
fi
rpm -q lookout
test -x /usr/bin/lookout
test -L /usr/bin/lookout-desktop
links=$(ldd /usr/bin/lookout)
if echo "$links" | grep -qi 'not found'; then echo "$links"; exit 1; fi
test -f /usr/share/applications/Lookout.desktop
grep -q 'Exec=lookout %u' /usr/share/applications/Lookout.desktop
if grep -q 'BEGIN PGP PUBLIC KEY BLOCK' /usr/share/lookout/lookout-archive-keyring.asc; then
  test -r /etc/pki/rpm-gpg/RPM-GPG-KEY-lookout
  test -f "$repo"
fi
# Do not refresh the newly enrolled repository: this release isn't public yet.
rpm -e lookout
test ! -e "$repo"
test ! -e /usr/bin/lookout-desktop
echo "OK: RPM installs, links, enrolls ($repo) and removes cleanly"
