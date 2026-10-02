#!/bin/sh
# Fetch the firmware listed in firmware/stock.list into
# target/firmware/<install-name> (kept between builds; set
# RUSTOS_FIRMWARE_CACHE to share a cache). Every file is checked against
# its SHA-256. Exits non-zero if any file could not be fetched.
set -e
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="${1:-$ROOT/target/firmware}"
CACHE="${RUSTOS_FIRMWARE_CACHE:-$ROOT/target/firmware-cache}"
LF="https://git.kernel.org/pub/scm/linux/kernel/git/firmware/linux-firmware.git/plain"
LF_TAG=20260916
REGDB="https://git.kernel.org/pub/scm/linux/kernel/git/wens/wireless-regdb.git/plain"
REGDB_TAG=master-2026-09-03
mkdir -p "$OUT" "$CACHE"
fail=0
grep -v '^#' "$ROOT/firmware/stock.list" | while read -r src name path sum; do
    [ -n "$src" ] || continue
    case "$src" in
        lf) url="$LF/$path?h=$LF_TAG" ;;
        regdb) url="$REGDB/$path?h=$REGDB_TAG" ;;
        *) echo "fetch-firmware: unknown source $src" >&2; exit 1 ;;
    esac
    cached="$CACHE/$(echo "$path" | tr / _)"
    if [ ! -f "$cached" ] || ! echo "$sum  $cached" | sha256sum -c --quiet - 2>/dev/null; then
        rm -f "$cached"
        curl -fsSL --retry 3 -o "$cached.part" "$url" || { echo "fetch-firmware: cannot download $path" >&2; exit 1; }
        mv "$cached.part" "$cached"
        echo "$sum  $cached" | sha256sum -c --quiet - || { echo "fetch-firmware: checksum mismatch: $path" >&2; rm -f "$cached"; exit 1; }
    fi
    mkdir -p "$OUT/$(dirname "$name")"
    cp "$cached" "$OUT/$name"
done
