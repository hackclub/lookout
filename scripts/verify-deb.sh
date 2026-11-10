#!/bin/bash
# Verifies a built .deb inside a clean Ubuntu container. Run by the "Verify the
# .deb installs" step in .github/workflows/release.yml, which mounts this file
# and the bundle directory, and passes DEB and PKG.
#
# Lives in a file rather than inline in the workflow because it used to be a
# single-quoted argument to `bash -c`, where one apostrophe in a comment ended
# the quote and spilled the rest into the outer shell.
set -eux

# archive.ubuntu.com crawls from GitHub's runners (3 min to update, ~700 kB/s
# for the ~220 MB of dependencies). The runners' own Azure mirror is local.
# arm64 images use ports.ubuntu.com, which this leaves alone.
sed -i -E 's#http://(archive|security)\.ubuntu\.com#http://azure.archive.ubuntu.com#g' \
  /etc/apt/sources.list.d/ubuntu.sources
apt-get update -qq
# Resolving and installing for real is the assertion: a Depends
# name that does not exist in the archive fails right here.
DEBIAN_FRONTEND=noninteractive apt-get install -y --no-install-recommends "/pkg/$DEB"
dpkg -s "$PKG" >/dev/null
# And the shipped executable must have all its shared libraries.
bin=$(dpkg -L "$PKG" | grep "^/usr/bin/" | head -1)
test -n "$bin"
links=$(ldd "$bin")
if echo "$links" | grep -qi "not found"; then echo "$links"; exit 1; fi

# mainBinaryName renames the cargo output, and nothing in the
# build fails if it silently doesn't -- you just get a package
# whose command is not the one the docs tell people to run.
test "$bin" = /usr/bin/lookout
command -v lookout >/dev/null
# postinst re-adds the old name for anyone still typing it.
test -L /usr/bin/lookout-desktop
command -v lookout-desktop >/dev/null
echo "binary OK: /usr/bin/lookout, with lookout-desktop aliased to it"

# The postinst adds pkg.lookout.hackclub.com, which is how Linux
# users get updates. Both failure modes here are silent: no
# enrollment at all, or a Signed-By pointing at a keyring the
# package never installed.
src=/etc/apt/sources.list.d/lookout.sources
if grep -q "BEGIN PGP PUBLIC KEY BLOCK" /usr/share/lookout/lookout-archive-keyring.asc; then
  test -f "$src"
  keyring=$(awk '/^Signed-By:/ {print $2}' "$src")
  test -n "$keyring"
  test -r "$keyring"
  grep -q "BEGIN PGP PUBLIC KEY BLOCK" "$keyring"
  # --print-uris parses the sources without fetching, so this
  # works before the bucket exists.
  apt-get update --print-uris | grep -q "pkg.lookout.hackclub.com"
  echo "enrollment OK: $src -> $keyring, and apt resolves it"
  DEBIAN_FRONTEND=noninteractive apt-get purge -y "$PKG" >/dev/null
  test ! -e "$src"
  test ! -e "$keyring"
  test ! -e /usr/bin/lookout-desktop
  echo "purge OK: the repository and the alias go with the package"
else
  test ! -e "$src"
  echo "unkeyed build correctly skipped enrollment"
fi