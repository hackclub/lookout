#!/usr/bin/env bash
# Print Lookout Native's version (clients/native/Cargo.toml [workspace.package]),
# or stamp a new one before a release build:
#
#   packaging/version.sh            -> 0.4.0
#   packaging/version.sh --set 0.5.0-beta.1
#
# The app reports CARGO_PKG_VERSION to the updater, and the release's
# update.json carries the tag's version, so a release build must be stamped
# with the tag or every install would see itself as older/newer than it is.
# Cargo.lock's entries for the workspace crates are stamped too, so the
# release's `cargo test --locked` / `cargo build --locked` still accept it.
set -euo pipefail
root="$(cd "$(dirname "$0")/.." && pwd)"
manifest="$root/Cargo.toml"

if [ "${1:-}" = "--set" ]; then
  new="${2:?usage: version.sh --set <version>}"
  if ! [[ "$new" =~ ^[0-9]+\.[0-9]+\.[0-9]+(-[0-9A-Za-z.-]+)?$ ]]; then
    echo "version.sh: '$new' is not a semver version" >&2
    exit 1
  fi
  NEW="$new" perl -0pi -e 's/(\[workspace\.package\][^\[]*?\nversion\s*=\s*")[^"]*(")/${1}$ENV{NEW}${2}/s' "$manifest"
  # The crates that take the workspace version, by their [package] name.
  for crate in "$root"/crates/*/Cargo.toml; do
    grep -q '^version\.workspace *= *true' "$crate" || continue
    name=$(sed -n '/^\[package\]/,/^\[/s/^name *= *"\(.*\)"/\1/p' "$crate")
    NAME="$name" NEW="$new" perl -0pi -e 's/(\[\[package\]\]\nname = "\Q$ENV{NAME}\E"\nversion = ")[^"]*(")/${1}$ENV{NEW}${2}/' "$root/Cargo.lock"
  done
fi

v=$(sed -n '/^\[workspace\.package\]/,/^\[/s/^version *= *"\(.*\)"/\1/p' "$manifest")
[ -n "$v" ] || { echo "version.sh: no [workspace.package] version in $manifest" >&2; exit 1; }
if [ -n "${new:-}" ] && [ "$v" != "$new" ]; then
  echo "version.sh: stamping $new failed (still $v)" >&2
  exit 1
fi
echo "$v"
