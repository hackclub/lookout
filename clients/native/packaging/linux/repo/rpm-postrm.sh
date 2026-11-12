#!/bin/sh
# Undoes rpm-postinst.sh. %postun gets 0 only on the final erase -- during an
# upgrade it gets 1, and tearing the repository down mid-update would be wrong.

set -e

KEYRING="/etc/pki/rpm-gpg/RPM-GPG-KEY-lookout"
REPO_FILE="/etc/yum.repos.d/lookout.repo"
ZYPP_FILE="/etc/zypp/repos.d/lookout.repo"
MARKER="/var/lib/lookout/repo-added"
LEGACY_ALIAS="/usr/bin/lookout-desktop"

if [ "$1" = "0" ]; then
  # Marker goes too, or a later reinstall would never re-enroll.
  rm -f "$REPO_FILE" "$ZYPP_FILE" "$KEYRING" "$MARKER" || true
  if [ -L "$LEGACY_ALIAS" ]; then
    rm -f "$LEGACY_ALIAS"
  fi
  rmdir /var/lib/lookout 2>/dev/null || true
fi

exit 0
