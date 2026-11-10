# Lookout on Linux

## Updates come from your package manager

The in-app updater is off on Linux on purpose — `latest.json` lists macOS and
Windows only, so `check()` never finds anything. A package installed by a
package manager should be updated by that package manager, not by the app
reaching for `pkexec`.

So each package enrols the machine in the right repository as it installs, the
way Chrome's does, and normal system updates carry you forward. Installing from
the releases page is all you need to do.

If a newer version is out and you haven't taken it, the app says so and shows
the exact command for however you installed — it detects that rather than
guessing.

## One line

```bash
curl -fsSL https://hack.club/lookout.sh | sh
```

(`hack.club/lookout.sh` redirects to `lookout.hackclub.com/install.sh`; the
source is [`packages/server/public/install.sh`](../packages/server/public/install.sh).)

Works on apt, dnf, zypper and pacman. It prints what it will do and waits for
confirmation; `-y` skips the prompt, and it refuses to run unattended without
it rather than assuming consent.

It is also the update path for anyone without a repository. Run on a machine
that already has Lookout, it says which version is installed, adds the
repository, updates in place and reports `Updated Lookout from X to Y` (or that
it was already the newest). Running it twice changes nothing. The app suggests
exactly this command when it finds a newer version and no repository to get it
from, which is the case for everyone who installed 0.3.11 or earlier: packages
only started enrolling a repository after that.

Before changing anything it refuses, with the reason, on systems Lookout can't
run on: glibc older than 2.39 (Ubuntu 22.04, Debian 12, Mint 21), musl
(Alpine), image-based systems (Fedora Atomic, Bazzite, SteamOS, Ubuntu Core)
and NixOS, where it points at the community package in nixpkgs instead.

The `Linux installer` workflow runs it in clean containers (fresh installs,
an update from 0.3.9, a second run and the refusals) on every change to it, after
every repository publish, and weekly.

## Debian and Ubuntu

```bash
sudo apt install ./Lookout_linux-*_amd64.deb
```

`apt install ./file.deb` rather than `dpkg -i` — Lookout depends on several
GStreamer packages for capture and encoding, and only apt will pull them in.

That writes `/etc/apt/sources.list.d/lookout.sources`, and every later version
arrives through `apt upgrade`.

### Adding the repository by hand

```bash
sudo install -d /usr/share/keyrings
sudo curl -fsSL -o /usr/share/keyrings/lookout-archive-keyring.asc \
  https://pkg.lookout.hackclub.com/lookout-archive-keyring.asc
```

Check the key before trusting it:

```bash
gpg --show-keys --with-fingerprint /usr/share/keyrings/lookout-archive-keyring.asc
```

```bash
sudo tee /etc/apt/sources.list.d/lookout.sources >/dev/null <<'EOF'
Types: deb
URIs: https://pkg.lookout.hackclub.com/deb
Suites: ./
Signed-By: /usr/share/keyrings/lookout-archive-keyring.asc
EOF
sudo apt update && sudo apt install lookout
```

Requires Ubuntu 24.04+ or Debian 13+ — the package declares `libc6 (>= 2.39)`,
which is the glibc it is built against, so an older release refuses it cleanly
instead of installing something that cannot start.

## Fedora and RHEL

```bash
sudo dnf install ./Lookout_linux-*_x86_64.rpm
```

That writes `/etc/yum.repos.d/lookout.repo`, and `dnf upgrade` handles the
rest. By hand:

```bash
sudo rpm --import https://pkg.lookout.hackclub.com/lookout-archive-keyring.asc
sudo tee /etc/yum.repos.d/lookout.repo >/dev/null <<'EOF'
[lookout]
name=Lookout
baseurl=https://pkg.lookout.hackclub.com/rpm
enabled=1
gpgcheck=1
repo_gpgcheck=1
gpgkey=https://pkg.lookout.hackclub.com/lookout-archive-keyring.asc
EOF
sudo dnf install lookout
```

## openSUSE

```bash
sudo zypper install --allow-unsigned-rpm ./Lookout_linux-*_x86_64.rpm
```

The same rpm as Fedora's. Its dependencies are capabilities both distributions
provide (`libvulkan.so.1()(64bit)`, `gstreamer1(element-vp9enc)()(64bit)`, …)
rather than package names, which differ (`vulkan-loader` vs `libvulkan1`,
`pipewire-gstreamer` vs `gstreamer-plugin-pipewire`). Its post-install script
writes `/etc/zypp/repos.d/lookout.repo` when zypper is present, since zypper
never reads `/etc/yum.repos.d`, and `zypper update` handles the rest. Tumbleweed
and Leap 16 qualify; Leap 15's glibc is too old. By hand:

```bash
sudo install -d /etc/pki/rpm-gpg
sudo curl -fsSL -o /etc/pki/rpm-gpg/RPM-GPG-KEY-lookout \
  https://pkg.lookout.hackclub.com/lookout-archive-keyring.asc
sudo rpm --import /etc/pki/rpm-gpg/RPM-GPG-KEY-lookout
sudo tee /etc/zypp/repos.d/lookout.repo >/dev/null <<'EOF'
[lookout]
name=Lookout
baseurl=https://pkg.lookout.hackclub.com/rpm
enabled=1
gpgcheck=1
repo_gpgcheck=1
gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-lookout
type=rpm-md
autorefresh=1
pkg_gpgcheck=1
EOF
sudo zypper refresh lookout && sudo zypper install lookout
```

## Arch

Arch is the one that isn't self-enrolling: a package editing `pacman.conf` is
considered rude in a way a Debian postinst isn't, so the repository is added by
hand once.

```bash
curl -fsSL https://pkg.lookout.hackclub.com/lookout-archive-keyring.asc \
  | sudo pacman-key --add -
sudo pacman-key --lsign-key 1A6D22590D34B83E2D0B3C8DF4806E984D9DA365

sudo tee -a /etc/pacman.conf >/dev/null <<'EOF'

[lookout]
Server = https://pkg.lookout.hackclub.com/arch/$arch
EOF

sudo pacman -Syu lookout-bin
```

After that `pacman -Syu` updates it with everything else.

There is no AUR package. It would be the more idiomatic route, but registration
is closed to new accounts, so the repository is what Arch gets.

## The command is `lookout`

`lookout-desktop` also works — the packages symlink the old name to the new one
so existing scripts and launchers keep working.

## Opting out of the repository

Delete the file and it stays deleted; the package will not put it back:

```bash
sudo rm /etc/apt/sources.list.d/lookout.sources   # or /etc/yum.repos.d/ or /etc/zypp/repos.d/lookout.repo
```

To opt out *before* installing:

```bash
echo 'repo_add_once=false' | sudo tee /etc/default/lookout
```

Purging removes the repository file, the key, and the marker.

## Maintaining the repositories

`pkg.lookout.hackclub.com` is one GitHub Pages site holding both:

| Path | What |
| --- | --- |
| `/deb` | flat apt repository — `Packages` and `InRelease` at that level, packages under `pool/` |
| `/rpm` | dnf repository — `repodata/` plus the signed rpms |
| `/arch/$arch` | pacman repository — `lookout.db` plus the signed packages, split by architecture |
| `/lookout-archive-keyring.asc` | the public key for both |

The `packages` workflow builds both from the GitHub release assets and deploys
them together. Pages allows one custom domain and replaces the whole site per
deploy, which is why they share a host and cannot be published independently.

It can be run by hand at any time (`workflow_dispatch`), so bringing a
repository up or repairing an index does not need a tag. Both are rebuilt from
scratch each run, from releases rather than from whatever is currently
deployed, so a wiped site heals on the next run. The last five releases are
kept, which bounds the site and keeps `apt install lookout=<version>` working —
and means an index someone cached before a release still points at a package
that exists.

### Signing

One key, generated once by [`scripts/apt-keygen.sh`](../scripts/apt-keygen.sh),
signs both repositories. Its public half is committed at
`clients/desktop/src-tauri/linux/lookout-archive-keyring.asc` on purpose: it
ships inside the packages as the trusted key, and having it in git is what lets
a locally built package enrol correctly.

dnf verifies signatures on the *packages*, not just the metadata, so the
workflow re-signs each rpm with `rpmsign` before indexing — they come off the
releases unsigned — and then reads each signature back with only the public
half, because `rpmsign` reports success on things dnf later rejects.

pacman wants a detached *binary* signature beside each package, not an armored
one, and `SigLevel = Required` turns an unverifiable signature into a hard
failure on every upgrade rather than a warning. So those are read back with the
public half too.

Rotating the key is disruptive: an enrolled machine has the old public half
pinned on disk and only learns a new one from a newer package, which it would
have to fetch from the repository it can no longer verify. Protect the private
key rather than rotating it.

### Secrets

| Secret | Used by |
| --- | --- |
| `APT_GPG_PRIVATE_KEY` | signing `InRelease`, `repomd.xml`, and the rpms |
| `APT_GPG_PASSPHRASE` | the same |

### One-time setup

1. Settings → Pages → Source: **GitHub Actions**; custom domain
   `pkg.lookout.hackclub.com`, Enforce HTTPS once the certificate issues.
2. DNS: `CNAME pkg.lookout → hackclub.github.io`.

Pages sets its own `Cache-Control` (10 minutes) and it can't be overridden.
That's fine here because a deploy purges the CDN and swaps the site atomically,
so an index and the packages it references always go live together.

### Guards

Three, because each failure is silent and each one freezes Linux users on one
version:

- a tagged Linux build fails if the shipped keyring is still a placeholder,
  which would ship a package that installs fine and never updates;
- the publish refuses if the key inside a package isn't the key signing the
  index, which would turn every `apt update` into `NO_PUBKEY`;
- the clean-Ubuntu check asserts the binary is `/usr/bin/lookout` with the
  alias beside it, since `mainBinaryName` failing to apply breaks nothing at
  build time — it just ships a package whose command isn't the documented one.
