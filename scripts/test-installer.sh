#!/bin/sh
# Exercise packages/server/public/install.sh inside a clean distribution
# container, against the live repository at pkg.lookout.hackclub.com.
# CI mounts the installer at /install.sh and this script at /test.sh:
#
#   docker run --rm -v "$PWD/packages/server/public/install.sh:/install.sh:ro" \
#     -v "$PWD/scripts/test-installer.sh:/test.sh:ro" -e SCENARIO=fresh fedora:43 sh /test.sh
#
# SCENARIO:
#   fresh    install on a machine that has never had Lookout, then run the
#            installer again and expect it to change nothing
#   update   install OLD_VERSION from its GitHub release first -- a release
#            from before packages enrolled a repository, which is where most
#            Linux users still are -- then expect the installer to bring it
#            up to the repository's newest version
#   refuse   a system Lookout can't run on: expect a clear refusal and no
#            repository left behind
set -eu

SCENARIO=${SCENARIO:-fresh}
OLD_VERSION=${OLD_VERSION:-0.3.9}
HOST=https://pkg.lookout.hackclub.com
REL=https://github.com/hackclub/lookout/releases/download

say() { printf '\n### %s\n' "$*"; }
fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }

# shellcheck disable=SC1091
. /etc/os-release
say "$PRETTY_NAME, scenario $SCENARIO"

# The bare minimum a real desktop already has: curl and CA certificates.
if command -v apt-get >/dev/null; then
  export DEBIAN_FRONTEND=noninteractive
  apt-get update -qq && apt-get install -y -qq curl ca-certificates procps >/dev/null
elif command -v dnf >/dev/null; then
  dnf install -y -q curl procps-ng >/dev/null
elif command -v zypper >/dev/null; then
  zypper --non-interactive -q install curl gzip >/dev/null
elif command -v pacman >/dev/null; then
  # -Syu even here: an image's packages plus fresh lists is a partial upgrade.
  pacman -Syu --noconfirm --needed curl >/dev/null
fi

if command -v dpkg-query >/dev/null; then MGR=apt
elif command -v dnf >/dev/null; then MGR=dnf
elif command -v zypper >/dev/null; then MGR=zypper
else MGR=pacman
fi

version() {
  case "$MGR" in
    apt) dpkg-query -W -f='${db:Status-Abbrev} ${Version}\n' lookout 2>/dev/null | awk '$1=="ii"{print $2}' ;;
    dnf|zypper) rpm -q lookout >/dev/null 2>&1 && rpm -q --qf '%{VERSION}\n' lookout | tail -1 ;;
    pacman) pacman -Q lookout-bin 2>/dev/null | awk '{sub(/-[0-9]+$/,"",$2); print $2}' ;;
  esac
}

# The newest version the repository advertises, read the way a client would.
newest() {
  case "$MGR" in
    apt) curl -fsSL "$HOST/deb/Packages" | awk '/^Version:/{print $2}' | sort -V | tail -1 ;;
    dnf|zypper)
      primary=$(curl -fsSL "$HOST/rpm/repodata/repomd.xml" | grep -o 'href="[^"]*-primary\.xml\.gz"' \
        | head -1 | sed 's/^href="//; s/"$//')
      curl -fsSL "$HOST/rpm/$primary" | gunzip | grep -o '<version [^>]*ver="[^"]*"' \
        | sed 's/.*ver="//; s/"//' | sort -V | tail -1 ;;
    pacman) curl -fsSL "$HOST/arch/$(uname -m)/lookout.db" | tar -tzf - | sed -n 's#^lookout-bin-\(.*\)-[0-9]*/$#\1#p' | sort -V | tail -1 ;;
  esac
}

repo_file() {
  case "$MGR" in
    apt) echo /etc/apt/sources.list.d/lookout.sources ;;
    dnf) echo /etc/yum.repos.d/lookout.repo ;;
    zypper) echo /etc/zypp/repos.d/lookout.repo ;;
    pacman) echo /etc/pacman.conf ;;
  esac
}

# tee hides the installer's exit status from `set -e`; keep it separately.
installer() {
  out=$(mktemp)
  # `|| r=$?`, or errexit would end the group before the status is saved.
  { r=0; sh /install.sh -y </dev/null 2>&1 || r=$?; echo "$r" > /tmp/rc; } | tee "$out"
  rc=$(cat /tmp/rc)
}

case "$SCENARIO" in
  refuse)
    installer
    [ "$rc" != 0 ] || fail "installer succeeded on a system it should refuse"
    grep -Eq "Nothing was changed|doesn't use glibc|isn't served by this installer|doesn't support that yet" "$out" \
      || fail "refusal didn't explain itself"
    for f in /etc/apt/sources.list.d/lookout.sources /etc/yum.repos.d/lookout.repo /etc/zypp/repos.d/lookout.repo; do
      [ ! -e "$f" ] || fail "refused, but left $f behind"
    done
    echo "OK: refused cleanly"
    exit 0 ;;

  update)
    say "Installing $OLD_VERSION from its release, the way people did before the repository"
    case "$MGR" in
      apt)
        curl -fsSLo /tmp/old.deb "$REL/v$OLD_VERSION/Lookout_linux-${OLD_VERSION}_$(dpkg --print-architecture).deb"
        apt-get install -y -qq /tmp/old.deb >/dev/null ;;
      dnf)
        curl -fsSLo /tmp/old.rpm "$REL/v$OLD_VERSION/Lookout_linux-${OLD_VERSION}_$(uname -m).rpm"
        dnf install -y -q /tmp/old.rpm >/dev/null ;;
      zypper)
        curl -fsSLo /tmp/old.rpm "$REL/v$OLD_VERSION/Lookout_linux-${OLD_VERSION}_$(uname -m).rpm"
        zypper --non-interactive install --allow-unsigned-rpm /tmp/old.rpm ;;
      pacman)
        curl -fsSLo /tmp/old.pkg.tar.zst "$REL/v$OLD_VERSION/lookout_linux-${OLD_VERSION}_$(uname -m).pkg.tar.zst"
        pacman -Syu --noconfirm >/dev/null
        pacman -U --noconfirm /tmp/old.pkg.tar.zst >/dev/null ;;
    esac
    [ "$(version)" = "$OLD_VERSION" ] || fail "couldn't set up $OLD_VERSION (got '$(version)')"
    ;;
esac

want=$(newest)
[ -n "$want" ] || fail "couldn't read the repository's newest version"
say "Repository's newest: $want; installed: $(version || true)"

say "Running the installer"
installer
if [ "$rc" != 0 ] && [ "$MGR" = zypper ] && grep -q "nothing provides" "$out"; then
  # The published rpm still carries Fedora-only package names (releases up
  # to 0.4.2). The installer's part -- enrolling, trusting the key, reading
  # signed metadata -- can still be checked.
  zypper --non-interactive search -s lookout | grep -q lookout || fail "repository didn't enroll"
  echo "SKIP: the published rpm predates openSUSE-compatible dependencies; enrollment verified"
  exit 0
fi
[ "$rc" = 0 ] || fail "installer exited $rc"

got=$(version)
[ "$got" = "$want" ] || fail "installed $got, but the repository's newest is $want"
[ -e "$(repo_file)" ] || fail "no repository at $(repo_file)"
command -v lookout >/dev/null || fail "no lookout on PATH"

case "$SCENARIO" in
  fresh) grep -q "Installed Lookout $want" "$out" || fail "didn't report the install" ;;
  update) grep -q "Updated Lookout from $OLD_VERSION to $want" "$out" || fail "didn't report the update" ;;
esac

say "Running it again"
before=$(md5sum "$(repo_file)")
installer
[ "$rc" = 0 ] || fail "second run exited $rc"
grep -q "Lookout $want is already the newest version" "$out" || fail "second run didn't recognise the install"
[ "$(md5sum "$(repo_file)")" = "$before" ] || fail "second run changed $(repo_file)"
[ "$MGR" != pacman ] || [ "$(grep -c '^\[lookout\]' /etc/pacman.conf)" = 1 ] || fail "pacman.conf has [lookout] twice"

# What the system's own updater will do from here on.
say "System update finds nothing more to do"
case "$MGR" in
  apt) apt-get update -qq && apt-get install -y -qq --only-upgrade lookout >/dev/null ;;
  dnf) dnf upgrade -y -q --refresh lookout ;;
  zypper) zypper --non-interactive update lookout ;;
  pacman) pacman -Syu --noconfirm >/dev/null ;;
esac
[ "$(version)" = "$want" ] || fail "system update moved lookout to $(version)"

echo "OK: $SCENARIO on $PRETTY_NAME ends at $want"
