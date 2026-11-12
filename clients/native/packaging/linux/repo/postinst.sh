#!/bin/sh
# Adds Lookout's apt repo so `apt upgrade` picks up new versions. The in-app
# updater is deliberately off on Linux, so this is how Linux users get updates.
set -e

REPO_URL="https://pkg.lookout.hackclub.com/deb"
PACKAGED_KEY="/usr/share/lookout/lookout-archive-keyring.asc"
# Copied out rather than shipped at this path directly: if dpkg owned it,
# `apt remove` would delete it and leave a sources file apt can't verify.
KEYRING="/usr/share/keyrings/lookout-archive-keyring.asc"
SOURCES="/etc/apt/sources.list.d/lookout.sources"
DEFAULTS="/etc/default/lookout"
MARKER="/var/lib/lookout/repo-added"
# The binary used to be installed as lookout-desktop; keep that name working
# for anyone whose scripts, launchers or muscle memory still use it.
LEGACY_ALIAS="/usr/bin/lookout-desktop"

repo_add_once=true
if [ -r "$DEFAULTS" ]; then
  . "$DEFAULTS"
fi

enroll() {
  case "$repo_add_once" in
    false|no|0|"") return 0 ;;
  esac

  # Local builds ship a placeholder instead of the key; a Signed-By pointing
  # at that would break apt update.
  if ! grep -q "BEGIN PGP PUBLIC KEY BLOCK" "$PACKAGED_KEY" 2>/dev/null; then
    echo "lookout: no repository key in package -- skipping apt enrollment" >&2
    return 0
  fi

  # Added once, so deleting the sources file makes it stay deleted.
  if [ -e "$MARKER" ]; then
    return 0
  fi

  install -d -m 0755 /usr/share/keyrings
  install -m 0644 "$PACKAGED_KEY" "$KEYRING"

  install -d -m 0755 /etc/apt/sources.list.d
  cat > "$SOURCES" <<EOF
# Added by the lookout package. Delete it to stop receiving updates; it will
# not come back. To opt out before installing, put repo_add_once=false in
# /etc/default/lookout.
Types: deb
URIs: $REPO_URL
Suites: ./
Signed-By: $KEYRING
EOF
  chmod 0644 "$SOURCES"

  install -d -m 0755 /var/lib/lookout
  : > "$MARKER"

  echo "lookout: added $REPO_URL to apt sources"
}

alias_legacy_name() {
  [ -x /usr/bin/lookout ] || return 0
  [ -e "$LEGACY_ALIAS" ] && [ ! -L "$LEGACY_ALIAS" ] && return 0
  ln -sf lookout "$LEGACY_ALIAS"
}

case "$1" in
  configure)
    # The `|| true` suppresses errexit inside enroll(); a postinst that exits
    # non-zero leaves the package half-configured.
    enroll || true
    alias_legacy_name || true
    ;;
esac

exit 0
