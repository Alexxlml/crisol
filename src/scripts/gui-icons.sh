#!/usr/bin/env bash
# Regenerates the GUI icons from assets/brand/ (ADR-0027). Needs `resvg` (cargo install resvg
# --locked) and Python with Pillow (for the .ico). The results are versioned: run this only
# when the SVGs change.
set -euo pipefail
root="$(cd "$(dirname "$0")/../.." && pwd)"
out="$root/src/crisol-gui/icons"
mkdir -p "$out/tray"

# Tray: 32 px PNG, one per state and taskbar theme (Windows scales it down at 100 %).
for svg in "$root"/assets/brand/tray/*.svg; do
  name="$(basename "$svg" .svg)"
  resvg -w 32 -h 32 "$svg" "$out/tray/$name.png"
done

# Application: window, Start menu and .exe come from the symbol on its dark square, which
# holds on a light Start menu or Explorer too (app-icon.svg).
for size in 32 128 256 512; do
  resvg -w "$size" -h "$size" "$root/assets/brand/app-icon.svg" "$out/icon-$size.png"
done
cp "$out/icon-512.png" "$out/icon.png"
python - "$out" <<'PY'
import sys
from PIL import Image
out = sys.argv[1]
Image.open(f"{out}/icon-256.png").save(
    f"{out}/icon.ico", sizes=[(16, 16), (24, 24), (32, 32), (48, 48), (64, 64), (128, 128), (256, 256)]
)
PY
rm "$out/icon-512.png"
echo "icons written to $out"
