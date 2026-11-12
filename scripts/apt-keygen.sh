#!/usr/bin/env bash
# Generates the signing key for pkg.lookout.hackclub.com, which signs both the
# apt and dnf repositories. Run once.
#
# Rotating later is painful: enrolled machines have the old public half pinned
# on disk and only learn a new one from a newer .deb, which they'd have to fetch
# from the repo they can no longer verify. Protect the private key instead.
set -euo pipefail

KEY_NAME="Lookout Archive Signing Key"
KEY_EMAIL="lookout@hackclub.com"
PUBKEY_DEST="$(cd "$(dirname "$0")/.." && pwd)/clients/native/packaging/linux/repo/lookout-archive-keyring.asc"

if grep -q "BEGIN PGP PUBLIC KEY BLOCK" "$PUBKEY_DEST" 2>/dev/null; then
  echo "error: $PUBKEY_DEST already holds a key -- delete it deliberately to rotate" >&2
  exit 1
fi

read -rsp "Passphrase for the new key (becomes APT_GPG_PASSPHRASE): " PASSPHRASE
echo
[ -n "$PASSPHRASE" ] || { echo "error: refusing to generate an unprotected key" >&2; exit 1; }

GNUPGHOME="$(mktemp -d)"
export GNUPGHOME
chmod 700 "$GNUPGHOME"
trap 'rm -rf "$GNUPGHOME"' EXIT

# RSA because whatever gpgv the user's distro ships has to verify it, and no
# expiry because an expired repo key breaks apt update on every enrolled
# machine, including the ones that needed an update to get a new key.
gpg --batch --quiet --gen-key <<EOF
Key-Type: RSA
Key-Length: 4096
Key-Usage: sign
Name-Real: $KEY_NAME
Name-Email: $KEY_EMAIL
Expire-Date: 0
Passphrase: $PASSPHRASE
%commit
EOF

FPR="$(gpg --batch --with-colons --list-keys "$KEY_EMAIL" | awk -F: '/^fpr:/ {print $10; exit}')"
gpg --batch --armor --export "$FPR" > "$PUBKEY_DEST"

echo "Public half written to $PUBKEY_DEST -- commit it."
echo
echo "Set these secrets now; the private key is printed once, then destroyed"
echo "with the temporary keyring."
echo
echo "APT_GPG_PASSPHRASE = (what you just typed)"
echo "APT_GPG_KEY_FPR    = $FPR"
echo
echo "APT_GPG_PRIVATE_KEY ="
gpg --batch --pinentry-mode loopback --passphrase "$PASSPHRASE" --armor \
    --export-secret-keys "$FPR"
