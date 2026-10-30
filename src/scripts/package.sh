#!/usr/bin/env bash
# Builds the release ZIP and SHA256SUMS from an already compiled `target/release`.
# Usage: src/scripts/package.sh <version> [out-dir]   (default out-dir: dist/ at the repo root)
set -euo pipefail

version="${1:?usage: package.sh <version> [out-dir]}"
root="$(cd "$(dirname "$0")/../.." && pwd)"
out="${2:-$root/dist}"
release="$root/src/target/release"
name="crisol-$version-windows-x64"

stage="$(mktemp -d)"
trap 'rm -rf "$stage"' EXIT

for exe in crisol crisold crisol-shim crisol-elevate; do
  [ -f "$release/$exe.exe" ] || { echo "missing $release/$exe.exe: run cargo build --release first" >&2; exit 1; }
  cp "$release/$exe.exe" "$stage/"
done
cp "$root/LICENSE-MIT" "$root/LICENSE-APACHE" "$root/THIRD-PARTY-NOTICES.md" "$stage/"
cp "$root/src/scripts/release/LEEME.txt" "$stage/"

# The licenses of the Rust dependencies compiled into the programs (src/about.toml fails on a license
# that has not been reviewed). CRISOL_LICENSES_FILE lets a caller without cargo-about supply the file.
if [ -n "${CRISOL_LICENSES_FILE:-}" ]; then
  cp "$CRISOL_LICENSES_FILE" "$stage/LICENSES-RUST.md"
else
  (cd "$root/src" && cargo about generate about.hbs -o "$(cygpath -w "$stage")\LICENSES-RUST.md")
fi
[ -s "$stage/LICENSES-RUST.md" ] || { echo "LICENSES-RUST.md is missing or empty" >&2; exit 1; }

mkdir -p "$out"
rm -f "$out/$name.zip" "$out/SHA256SUMS" "$out/SHA256SUMS.minisig"

# PowerShell 7 writes forward slashes in the entries; the ZIP is flat on purpose, so the winget
# manifest can name the programs without a version in the path.
zip_win="$(cygpath -w "$out/$name.zip")"
stage_win="$(cygpath -w "$stage")"
pwsh -NoProfile -Command "Compress-Archive -Path '$stage_win\*' -DestinationPath '$zip_win' -CompressionLevel Optimal"

# Two spaces and no path, so `sha256sum -c` works from the folder that holds the files.
(cd "$out" && sha256sum "$name.zip" > SHA256SUMS)
echo "==> $out/$name.zip"
cat "$out/SHA256SUMS"
