#!/bin/sh
# Lookout installer.  curl -fsSL https://hack.club/lookout.sh | sh
#
# Adds the Lookout package repository for whichever package manager this
# machine uses, then installs from it, so future versions arrive with the rest
# of your system updates. Running it again on a machine that already has
# Lookout updates it to the newest version; it never installs a second copy.
#
# Everything runs from main(), called on the last line, so a download cut off
# halfway through runs nothing rather than half a script.
set -eu

HOST="https://pkg.lookout.hackclub.com"
RELEASES="https://github.com/hackclub/lookout/releases/latest"
KEY_FPR="1A6D22590D34B83E2D0B3C8DF4806E984D9DA365"
# Lookout is built against glibc 2.39 (Ubuntu 24.04); the packages declare it,
# but saying so up front beats a dependency error after the repo is added.
GLIBC_MIN_MAJOR=2
GLIBC_MIN_MINOR=39

# Where each manager's repository lives. The package's own post-install
# script writes the same files with the same contents, so whichever runs
# second is a no-op.
RPM_KEY=/etc/pki/rpm-gpg/RPM-GPG-KEY-lookout
APT_KEY=/usr/share/keyrings/lookout-archive-keyring.asc
APT_SOURCES=/etc/apt/sources.list.d/lookout.sources
DNF_REPO=/etc/yum.repos.d/lookout.repo
ZYPP_REPO=/etc/zypp/repos.d/lookout.repo

# The repository's public key, embedded rather than downloaded: the script is
# already the thing being trusted, so fetching the key separately could only
# add a second place for it to be swapped. CI checks this block against
# clients/desktop/src-tauri/linux/lookout-archive-keyring.asc.
write_key() {
  cat <<'KEY'
-----BEGIN PGP PUBLIC KEY BLOCK-----

mQINBGqNz84BEADJYOZLS6G/Dsv2Y/O4mJNp1b260rRrUD8GVgpWY3aQO606XKdW
+3MHjhxSr2oyGbFmbQs3+j0HfA6fT+QblafR8f+enyfa49qQE7PBFUhuE0aoRhUu
9eSFxAUwehjTspG7q7ZfhWb7PpVcsjeDv0L3jlqvtFse8omm8LcP2Jb7XTw4iYE6
izCW7QOujifmL/RFKfU6+8J5GFerucXM7NKvEQu5rMn7gDGkgl5rHTCtQvA58Ose
FgZTa/QKbLJO79P/Kj6o5LFN6IWd5cnRRSYBssY1zsIVif5j0BYPfl6+De7fqrHw
M/7ZFApQ/wN/9T1fNs5B/xy+aMOaMbjoh8znwlYOvgtNEtsQ/ybJyJLhClFhxljJ
zfupbt5gMYkmAWAQ9pc8V6JYLUP+WJPWqvkMCRIDme1sZgoT1tUCC+AE4az9rWqU
Y8uP+DpgKYonoSh5wWdgDBlEuEQA9D83+9N0SFB9N3bxn4dA02bxhQkKuLIFidPB
qDs3p/nEcW3zBOOCjkAofzgJvSgUAbKhekqYab1UnsTCBcwB+tYVFXT6Pr9+MBwS
glE0ZFORMr0ckLL8/wa90K4HGgFyMLS6rbCVLLr6hzMujYthpVTpC+sMbnx+BU7U
7L3jLnVe9hRuosAvQZUnLtPCwaHh2MXe7/0/ARUQGbOdo7LSi6Wi/cCdMQARAQAB
tDJMb29rb3V0IEFyY2hpdmUgU2lnbmluZyBLZXkgPGxvb2tvdXRAaGFja2NsdWIu
Y29tPokCTgQTAQoAOBYhBBptIlkNNLg+LQs8jfSAbphNnaNlBQJqjc/OAhsDBQsJ
CAcCBhUKCQgLAgQWAgMBAh4BAheAAAoJEPSAbphNnaNlqPIP/3c99E+nKkqZ9ASp
XVi0MabwYMbx7+r7Dw/Ku5vLhxj1Foeof7jdmt5ZenMJxUs5Gr6JilIAl4DRCSCJ
pqh8GMCev9B9fRKA1QjBLZJIL44wJXfivsxyEGbE+nH0mLpFSA5W/qAzwTPKJwpY
jjDt6rM76XgNHzVVW9OLch7ceYOZiEEBFNnnn25bQ2kxdvDq2CRBgUXjOTkp18Z2
5WBouWb3zpOq2YwsNsemDFgWVERQVO8Z50oktkArLIhZZlSfPGL7mAx1ssxEnYmw
gNs0y/9EbRtGya6USMxQXYEI704YRzChnGD3RPPSkxcUxRFGtlwRWHpi9qKralus
2PUKE24qVDWtiMI7fj/NnjPS7zomXweuSxwCc5xjTJpkTS08oNCIFFDsbZa4Ra6k
1iq6p3PdBEubyyvq77upsWd1KoEJLsnI4OzBhPqEalaKcGZ6dfiLDs3V0nuf7qjd
RKRvOhyIFhfEn7dbeUNyhL43nDfjrKiaHsX8aC+KQpa81FLTHlO1cpIfEGJFV+k3
Wl8OTufaogwHYnTwkmd+DVnOSXRic1feoeZj2w7wEsZuQ5OzwCdduBZc8/fxzJ2O
FaY8DmIEvhvyND+x5yOSMqDpc3593Nqk9/nwnZV/em97ZxKXed3Ti/vG1tEmIz4R
MEB6zP5bTUfOUTA/Vs5Ng5k5/yuX
=qyWa
-----END PGP PUBLIC KEY BLOCK-----
KEY
}

# ---------------------------------------------------------------------------

if [ -t 1 ] && [ -z "${NO_COLOR:-}" ]; then
  b=$(printf '\033[1m'); dim=$(printf '\033[2m'); r=$(printf '\033[0m')
else
  b=''; dim=''; r=''
fi

die() { printf '%s\n' "$*" >&2; exit 1; }
warn() { printf '%s    %s%s\n' "$dim" "$*" "$r" >&2; }
step() { printf '%s==>%s %s\n' "$b" "$r" "$1"; }
have() { command -v "$1" >/dev/null 2>&1; }
# Root-owned writes. `as_root tee` rather than redirections, which would run
# as the calling user.
as_root() { if [ -n "$SUDO" ]; then $SUDO "$@"; else "$@"; fi; }
put() { as_root tee "$1" >/dev/null; }

osr() {
  [ -r /etc/os-release ] || return 0
  sed -n "s/^$1=//p" /etc/os-release | tr -d "\"'" | head -1
}

usage() {
  cat <<EOF
Usage: install.sh [-y]

Installs Lookout from its package repository, or updates it if it is already
installed.

  -y, --yes   don't ask before making changes
  -h, --help  show this help
EOF
}

# --- what is this machine -------------------------------------------------

check_platform() {
  case "$(uname -s)" in
    Linux) ;;
    Darwin) die "This installer is for Linux. On macOS, download Lookout from $RELEASES" ;;
    *) die "Unsupported system: $(uname -s). See $RELEASES" ;;
  esac

  case "$(uname -m)" in
    x86_64|amd64|aarch64|arm64) ;;
    *) die "Lookout has no packages for $(uname -m) yet. See $RELEASES" ;;
  esac

  NAME=$(osr PRETTY_NAME); [ -n "$NAME" ] || NAME=$(osr NAME); [ -n "$NAME" ] || NAME="this system"

  # Immutable systems have a package manager that looks usable and isn't: the
  # root filesystem is read-only, or writes land in an overlay a reboot throws
  # away. Better to say so than to half-install and look like it worked.
  if [ -e /run/ostree-booted ]; then
    die "$NAME is an image-based (ostree) system, where packages are layered
with rpm-ostree rather than installed. Lookout doesn't support that yet.
See $RELEASES"
  fi
  case "$(osr ID)" in
    steamos)
      die "SteamOS keeps its root filesystem read-only, so an installed package
is wiped by the next system update. Lookout doesn't support that yet.
See $RELEASES" ;;
    ubuntu-core)
      die "Ubuntu Core installs snaps only, and Lookout has no snap yet.
See $RELEASES" ;;
    nixos)
      die "NixOS isn't served by this installer, but nixpkgs has a community-maintained
Lookout package (it can lag behind the latest release):
  nix-shell -p lookout                 # try it
  environment.systemPackages = [ pkgs.lookout ];   # keep it" ;;
  esac

  # Lookout links against glibc. musl systems (Alpine, Void's musl flavour,
  # Chimera) can't run it at all, and an older glibc can't start it.
  glibc=$(getconf GNU_LIBC_VERSION 2>/dev/null | awk '{print $2}') || glibc=''
  if [ -z "$glibc" ]; then
    die "$NAME doesn't use glibc (it looks like a musl system), which Lookout
needs. See $RELEASES"
  fi
  major=${glibc%%.*}
  minor=${glibc#*.}; minor=${minor%%.*}
  case "$major$minor" in *[!0-9]*|'') major=0; minor=0 ;; esac
  if [ "$major" -lt "$GLIBC_MIN_MAJOR" ] || { [ "$major" -eq "$GLIBC_MIN_MAJOR" ] && [ "$minor" -lt "$GLIBC_MIN_MINOR" ]; }; then
    die "Lookout needs glibc $GLIBC_MIN_MAJOR.$GLIBC_MIN_MINOR or newer, and $NAME has $glibc.
That means Ubuntu 24.04, Linux Mint 22, Debian 13, Fedora 40, RHEL 10,
openSUSE Tumbleweed or anything newer. Nothing was changed."
  fi
}

# The distribution says which manager is its own; the binaries alone can lie
# (Fedora ships a yum symlink, openSUSE can have dnf installed beside zypper,
# and apt has been ported to rpm distros). Probing binaries is the fallback.
pick_manager() {
  MGR=''
  for id in $(osr ID) $(osr ID_LIKE); do
    case "$id" in
      debian|ubuntu|linuxmint|pop|elementary|zorin|kali|raspbian|neon|deepin) m=apt ;;
      fedora|rhel|centos|rocky|almalinux|nobara|ultramarine|amzn|ol) m=dnf ;;
      opensuse*|suse|sles|sled) m=zypper ;;
      arch|archarm|manjaro|endeavouros|cachyos|garuda|artix) m=pacman ;;
      *) continue ;;
    esac
    case "$m" in
      apt) have apt-get || continue ;;
      *) have "$m" || continue ;;
    esac
    MGR=$m; break
  done

  if [ -z "$MGR" ]; then
    if   have apt-get; then MGR=apt
    elif have dnf;     then MGR=dnf
    elif have zypper;  then MGR=zypper
    elif have pacman;  then MGR=pacman
    else
      die "No supported package manager found (apt, dnf, zypper or pacman) on $NAME.
Download a package directly from $RELEASES"
    fi
  fi

  case "$MGR" in
    pacman) PKG=lookout-bin ;;
    *) PKG=lookout ;;
  esac

  # A 64-bit kernel can run a 32-bit userland (32-bit Raspberry Pi OS does),
  # and apt would then look for an armhf package that doesn't exist.
  if [ "$MGR" = apt ] && have dpkg; then
    case "$(dpkg --print-architecture)" in
      amd64|arm64) ;;
      *) die "This system's packages are $(dpkg --print-architecture), and Lookout is built for
amd64 and arm64 only. See $RELEASES" ;;
    esac
  fi
}

find_root() {
  if [ "$(id -u)" = 0 ]; then
    SUDO=''
  elif have sudo; then
    SUDO='sudo'
  elif have doas; then
    SUDO='doas'
  else
    die "This needs root, but neither sudo nor doas is installed. Re-run as root."
  fi
}

# The installed version, or nothing. Asked of the package database rather
# than the binary, so a copy unpacked by hand doesn't count.
installed_version() {
  case "$MGR" in
    apt)
      dpkg-query -W -f='${db:Status-Abbrev} ${Version}\n' lookout 2>/dev/null \
        | awk '$1 == "ii" { print $2 }' ;;
    dnf|zypper)
      if rpm -q lookout >/dev/null 2>&1; then
        rpm -q --qf '%{VERSION}\n' lookout | tail -1
      fi ;;
    pacman)
      pacman -Q lookout-bin 2>/dev/null | awk '{ sub(/-[0-9]+$/, "", $2); print $2 }' ;;
  esac
}

# --- consent --------------------------------------------------------------

confirm() {
  case "$MGR" in
    apt)    where="$APT_SOURCES" ;;
    dnf)    where="$DNF_REPO" ;;
    zypper) where="$ZYPP_REPO" ;;
    pacman) where="a [lookout] section in /etc/pacman.conf" ;;
  esac

  printf '\n%sLookout%s\n\n' "$b" "$r"
  if [ -n "$BEFORE" ]; then
    printf 'Lookout %s is installed. This will:\n' "$BEFORE"
  else
    printf 'This will:\n'
  fi
  printf '  • add %s to %s\n' "$HOST" "$where"
  printf '  • trust its signing key %s\n' "$KEY_FPR"
  if [ "$MGR" = pacman ]; then
    # pacman doesn't support refreshing the package lists without upgrading:
    # installing one package against new lists can break the rest.
    printf '  • upgrade your system packages, which pacman requires (pacman -Syu)\n'
  fi
  if [ -n "$BEFORE" ]; then
    printf '  • update %s to the newest version with %s\n' "$PKG" "$MGR"
  else
    printf '  • install %s with %s\n' "$PKG" "$MGR"
  fi
  printf '\n%sNothing else is changed. Uninstall with your package manager.%s\n\n' "$dim" "$r"

  [ "$ASSUME_YES" = 1 ] && return 0

  # Probed in a subshell: a failed `exec` redirection exits a non-interactive
  # shell outright, so testing it inline would skip the message below. And not
  # with `[ -r /dev/tty ]`, which passes under `curl | sh` even where the open
  # fails — leaving the reply empty, which is what pressing Enter also means.
  # Consent has to come from a read that actually happened.
  if ! (exec 3</dev/tty) 2>/dev/null; then
    die "Can't ask for confirmation without a terminal. Re-run with -y:
  curl -fsSL https://hack.club/lookout.sh | sh -s -- -y"
  fi
  printf 'Continue? [Y/n] '
  read -r reply < /dev/tty || reply=n
  case "$reply" in
    ''|y|Y|yes|Yes|YES) ;;
    *) echo "Cancelled."; exit 0 ;;
  esac
  echo
  INTERACTIVE=1
}

# --- doing it -------------------------------------------------------------

# Pinned against the fingerprint above where gpg is around to check, which
# guards this file against a bad edit rather than against the network.
check_key() {
  have gpg || return 0
  got=$(gpg --batch --show-keys --with-colons "$1" 2>/dev/null | awk -F: '/^fpr:/ { print $10; exit }') || got=''
  # Empty: a gpg too old for --show-keys, which says nothing either way.
  [ -z "$got" ] || [ "$got" = "$KEY_FPR" ] || die "The embedded signing key is $got, expected $KEY_FPR. Not continuing."
}

# The header matches what the packages' post-install scripts write.
repo_header() {
  cat <<'EOF'
# Added by the lookout package. Delete it to stop receiving updates; it will
# not come back. To opt out before installing, put repo_add_once=false in
# /etc/default/lookout.
EOF
}

do_apt() {
  step "Adding the apt repository"
  as_root install -d -m 0755 /usr/share/keyrings /etc/apt/sources.list.d
  as_root install -m 0644 "$key" "$APT_KEY"
  { repo_header; printf 'Types: deb\nURIs: %s/deb\nSuites: ./\nSigned-By: %s\n' "$HOST" "$APT_KEY"; } | put "$APT_SOURCES"
  as_root chmod 0644 "$APT_SOURCES"

  step "Refreshing package lists"
  # Someone else's broken repository shouldn't stop this one; if it matters,
  # the install below says so.
  as_root apt-get update </dev/null || warn "apt-get update reported problems with a repository; carrying on"

  step "Installing"
  # `install` also upgrades a package that is already there.
  as_root env DEBIAN_FRONTEND=noninteractive apt-get install -y "$PKG" </dev/null
}

write_rpm_key() {
  as_root install -d -m 0755 /etc/pki/rpm-gpg
  as_root install -m 0644 "$key" "$RPM_KEY"
  as_root rpm --import "$RPM_KEY"
}

rpm_repo() {
  # $1: extra lines for the manager reading it.
  repo_header
  printf '[lookout]\nname=Lookout\nbaseurl=%s/rpm\nenabled=1\ngpgcheck=1\nrepo_gpgcheck=1\ngpgkey=file://%s\n%s' \
    "$HOST" "$RPM_KEY" "$1"
}

do_dnf() {
  step "Adding the dnf repository"
  write_rpm_key
  as_root install -d -m 0755 /etc/yum.repos.d
  rpm_repo '' | put "$DNF_REPO"
  as_root chmod 0644 "$DNF_REPO"

  # --refresh: dnf otherwise trusts cached metadata for up to two days and
  # wouldn't see a release made since.
  if [ -n "$BEFORE" ]; then
    step "Updating"
    as_root dnf upgrade -y --refresh "$PKG" </dev/null
  else
    step "Installing"
    as_root dnf install -y --refresh "$PKG" </dev/null
  fi
}

do_zypper() {
  step "Adding the zypper repository"
  write_rpm_key
  as_root install -d -m 0755 /etc/zypp/repos.d
  rpm_repo "$(printf 'type=rpm-md\nautorefresh=1\npkg_gpgcheck=1')
" | put "$ZYPP_REPO"
  as_root chmod 0644 "$ZYPP_REPO"
  # Earlier versions of this script wrote the repository where only dnf
  # looks. Without dnf it is just clutter, so tidy it if it is ours.
  if ! have dnf && [ -f "$DNF_REPO" ] && grep -q "$HOST" "$DNF_REPO"; then
    as_root rm -f "$DNF_REPO"
  fi

  step "Refreshing the repository"
  as_root zypper --non-interactive refresh lookout </dev/null

  if [ -n "$BEFORE" ]; then
    step "Updating"
    as_root zypper --non-interactive update "$PKG" </dev/null
  else
    step "Installing"
    as_root zypper --non-interactive install "$PKG" </dev/null
  fi
}

do_pacman() {
  step "Trusting the key"
  # A fresh install (or a container) may never have set up pacman's keyring.
  [ -d /etc/pacman.d/gnupg ] || as_root pacman-key --init
  as_root pacman-key --add "$key"
  as_root pacman-key --lsign-key "$KEY_FPR"

  step "Adding the repository"
  # Appending twice would make pacman complain on every operation.
  if grep -q '^\[lookout\]' /etc/pacman.conf; then
    printf '%s    already in /etc/pacman.conf%s\n' "$dim" "$r"
  else
    # shellcheck disable=SC2016 # $arch is pacman's variable, not ours.
    printf '\n[lookout]\nServer = %s/arch/$arch\n' "$HOST" | as_root tee -a /etc/pacman.conf >/dev/null
  fi

  if [ -n "$BEFORE" ]; then verb=updating; else verb=installing; fi
  step "Upgrading the system and $verb Lookout"
  # -Syu, never -Sy <pkg>: Arch doesn't support partial upgrades. Where there
  # is a terminal and no -y, pacman shows its own transaction and asks.
  if [ "$INTERACTIVE" = 1 ]; then
    as_root pacman -Syu --needed "$PKG" </dev/tty
  else
    as_root pacman -Syu --needed --noconfirm "$PKG" </dev/null
  fi
}

# --- after ----------------------------------------------------------------

report() {
  AFTER=$(installed_version)
  [ -n "$AFTER" ] || die "The package manager finished, but $PKG isn't installed. Scroll up for its error."

  echo
  if [ -z "$BEFORE" ]; then
    printf '%sInstalled Lookout %s.%s Run %slookout%s, or find it in your applications menu.\n' "$b" "$AFTER" "$r" "$b" "$r"
  elif [ "$BEFORE" = "$AFTER" ]; then
    printf '%sLookout %s is already the newest version.%s\n' "$b" "$AFTER" "$r"
  else
    printf '%sUpdated Lookout from %s to %s.%s\n' "$b" "$BEFORE" "$AFTER" "$r"
  fi

  case "$MGR" in
    apt)    up='sudo apt update && sudo apt upgrade' ;;
    dnf)    up='sudo dnf upgrade' ;;
    zypper) up='sudo zypper update' ;;
    pacman) up='sudo pacman -Syu' ;;
  esac
  printf 'Future versions arrive with your system updates (%s%s%s).\n' "$b" "$up" "$r"

  # The old version keeps running until it is restarted.
  if [ -n "$BEFORE" ] && [ "$BEFORE" != "$AFTER" ] && have pgrep \
     && { pgrep -x lookout >/dev/null 2>&1 || pgrep -x lookout-desktop >/dev/null 2>&1; }; then
    printf '%sLookout is still running the old version. Quit it from the tray icon and open it again.%s\n' "$b" "$r"
  fi
  echo
}

main() {
  ASSUME_YES=0
  INTERACTIVE=0
  for arg in "$@"; do
    case "$arg" in
      -y|--yes) ASSUME_YES=1 ;;
      -h|--help) usage; exit 0 ;;
      *) usage >&2; die "unknown option: $arg" ;;
    esac
  done

  check_platform
  pick_manager
  find_root
  BEFORE=$(installed_version)
  confirm

  # Ask for the password once, up front, rather than between steps.
  if [ "$SUDO" = sudo ]; then sudo -v; fi

  key=$(mktemp)
  trap 'rm -f "$key"' EXIT
  write_key > "$key"
  check_key "$key"

  case "$MGR" in
    apt) do_apt ;;
    dnf) do_dnf ;;
    zypper) do_zypper ;;
    pacman) do_pacman ;;
  esac

  report
}

main "$@"
