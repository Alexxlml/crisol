#!/usr/bin/env bash
# Fills the winget manifest templates (packaging/winget/*.in) for a release and validates them.
# Usage: src/scripts/winget-manifest.sh <version> <SHA256SUMS file> [out-dir] [release-date]
#   version       without the leading v (0.1.0)
#   SHA256SUMS    the file of that release; its line for crisol-<version>-windows-x64.zip gives the hash
#   out-dir       default: dist/winget (the manifests go in <out-dir>/<version>/)
#   release-date  YYYY-MM-DD, default today
set -euo pipefail

version="${1:?usage: winget-manifest.sh <version> <SHA256SUMS> [out-dir] [date]}"
sums="${2:?missing the SHA256SUMS file}"
root="$(cd "$(dirname "$0")/../.." && pwd)"
out="${3:-$root/dist/winget}/$version"
date="${4:-$(date +%F)}"
zip="crisol-$version-windows-x64.zip"

sha="$(awk -v f="$zip" '{ n = $2; sub(/^\*/, "", n); if (n == f) print toupper($1) }' "$sums")"
[ -n "$sha" ] || { echo "no line for $zip in $sums" >&2; exit 1; }
printf '%s' "$sha" | grep -Eq '^[0-9A-F]{64}$' || { echo "bad SHA-256 for $zip: $sha" >&2; exit 1; }

mkdir -p "$out"
for template in "$root"/packaging/winget/*.in; do
  name="$(basename "${template%.in}")"
  sed -e "s/@VERSION@/$version/g" -e "s/@SHA256@/$sha/g" -e "s/@DATE@/$date/g" "$template" > "$out/$name"
done
echo "==> $out"
winget validate --manifest "$(cygpath -w "$out")"
