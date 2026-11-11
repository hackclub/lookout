#!/bin/bash
# Run in a clean Fedora, openSUSE or EL9 (AlmaLinux) container with /pkg and
# RPMFILE supplied by CI. One rpm serves them all, so all install it.
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
# Every symbol resolves, at the glibc version it was linked against, except
# WebKitGTK's: it is only recommended (weak dependencies are off above, so it
# isn't here), and the app loads it on demand (packaging/linux/packages.sh).
unresolved=$(ldd -r /usr/bin/lookout 2>&1 | grep -E 'undefined symbol|not found|not defined' \
  | grep -Ev 'undefined symbol: (webkit|jsc|soup)_' || true)
if [ -n "$unresolved" ]; then echo "$unresolved"; exit 1; fi
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
