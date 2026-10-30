#!/usr/bin/env bash
# Signs SHA256SUMS with minisign and checks the signature right away.
# Usage: src/scripts/sign-release.sh <dir> <secret-key-file> <public-key-file>
# The password of the key (empty if it has none) comes from MINISIGN_PASSWORD, never from the command line.
set -euo pipefail

dir="${1:?usage: sign-release.sh <dir> <secret-key-file> <public-key-file>}"
secret="${2:?missing secret key file}"
public="${3:?missing public key file}"

cd "$dir"
rm -f SHA256SUMS.minisig
if [ -n "${MINISIGN_PASSWORD:-}" ]; then
  printf '%s\n' "$MINISIGN_PASSWORD" | minisign -S -s "$secret" -m SHA256SUMS -x SHA256SUMS.minisig
else
  minisign -S -W -s "$secret" -m SHA256SUMS -x SHA256SUMS.minisig
fi
minisign -V -p "$public" -m SHA256SUMS -x SHA256SUMS.minisig
