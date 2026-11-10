#!/bin/sh
# The rpm counterpart of postinst.sh: adds Lookout's repository so
# `dnf upgrade` (Fedora, RHEL) or `zypper update` (openSUSE) picks up new
# versions. One rpm serves both, but they read repositories from different
# directories, so it is written for whichever of them is installed.
#
# rpm scriptlet arguments are counts, not verbs: %post gets 1 on a fresh
# install and 2 on an upgrade, so >= 1 covers both.

set -e

REPO_URL="https://pkg.lookout.hackclub.com/rpm"
PACKAGED_KEY="/usr/share/lookout/lookout-archive-keyring.asc"
# Copied out rather than shipped at this path directly: if rpm owned it,
# erasing the package would take the key with it while the .repo file
# remained, and every later dnf call would fail on a missing gpgkey.
KEYRING="/etc/pki/rpm-gpg/RPM-GPG-KEY-lookout"
REPO_FILE="/etc/yum.repos.d/lookout.repo"
ZYPP_FILE="/etc/zypp/repos.d/lookout.repo"
DEFAULTS="/etc/default/lookout"
MARKER="/var/lib/lookout/repo-added"
# The binary used to be installed as lookout-desktop; keep that name working.
LEGACY_ALIAS="/usr/bin/lookout-desktop"

repo_add_once=true
if [ -r "$DEFAULTS" ]; then
  . "$DEFAULTS"
fi

# $1: the file; $2: extra lines for the manager that reads it. The same text
# install.sh writes, so whichever runs second changes nothing.
#
# A heredoc rather than printf: packages.sh pastes this file into the rpm
# spec as the post-install section, where rpm expands percent signs as macros.
write_repo() {
  install -d -m 0755 "$(dirname "$1")"
  cat > "$1" <<EOF
# Added by the lookout package. Delete it to stop receiving updates; it will
# not come back. To opt out before installing, put repo_add_once=false in
# /etc/default/lookout.
[lookout]
name=Lookout
baseurl=$REPO_URL
enabled=1
gpgcheck=1
repo_gpgcheck=1
gpgkey=file://$KEYRING
EOF
  if [ -n "$2" ]; then
    echo "$2" >> "$1"
  fi
  chmod 0644 "$1"
}

enroll() {
  case "$repo_add_once" in
    false|no|0|"") return 0 ;;
  esac

  # Local builds ship a placeholder instead of the key; a gpgkey pointing at
  # that would break every dnf transaction.
  if ! grep -q "BEGIN PGP PUBLIC KEY BLOCK" "$PACKAGED_KEY" 2>/dev/null; then
    echo "lookout: no repository key in package -- skipping repository enrollment" >&2
    return 0
  fi

  # Added once, so deleting the .repo file makes it stay deleted.
  if [ -e "$MARKER" ]; then
    return 0
  fi

  install -d -m 0755 /etc/pki/rpm-gpg
  install -m 0644 "$PACKAGED_KEY" "$KEYRING"
  rpm --import "$KEYRING" 2>/dev/null || true

  wrote=''
  if command -v zypper >/dev/null 2>&1; then
    write_repo "$ZYPP_FILE" "type=rpm-md
autorefresh=1
pkg_gpgcheck=1"
    wrote="$wrote zypper"
  fi
  # With neither manager (rpm alone), the dnf location: it is where every
  # other rpm-md tool looks as well.
  if command -v dnf >/dev/null 2>&1 || [ -z "$wrote" ]; then
    write_repo "$REPO_FILE" ""
    wrote="$wrote dnf"
  fi

  install -d -m 0755 /var/lib/lookout
  : > "$MARKER"

  echo "lookout: added $REPO_URL for$wrote"
}

alias_legacy_name() {
  [ -x /usr/bin/lookout ] || return 0
  [ -e "$LEGACY_ALIAS" ] && [ ! -L "$LEGACY_ALIAS" ] && return 0
  ln -sf lookout "$LEGACY_ALIAS"
}

if [ "$1" -ge 1 ] 2>/dev/null; then
  # The `|| true` suppresses errexit inside enroll(); a failing scriptlet
  # leaves the transaction looking broken.
  enroll || true
  alias_legacy_name || true
fi

exit 0
