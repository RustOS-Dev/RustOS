#!/bin/sh
# Fetch the test262 subset listed in tests/test262.list (pinned commit,
# sparse checkout) into target/test262/{harness,test}. The checkout is
# kept in the ports source cache.
#   tools/fetch-test262.sh [DEST]
set -e
ROOT="$(cd "$(dirname "$0")/.." && pwd)"
COMMIT=7ab7fafa0003f73fc85c1b95d88094d33f7eb8bd
CACHE="${RUSTOS_PORTS_CACHE:-$ROOT/target/ports/src}/test262"
DEST="${1:-$ROOT/target/test262}"
LIST="$ROOT/tests/test262.list"
DIRS="harness $(grep -v '^#' "$LIST" | sed 's|^|test/|' | tr '\n' ' ')"
if [ "$(git -C "$CACHE" rev-parse HEAD 2>/dev/null)" != "$COMMIT" ]; then
    rm -rf "$CACHE"
    git init -q "$CACHE"
    git -C "$CACHE" remote add origin https://github.com/tc39/test262
    git -C "$CACHE" sparse-checkout set --no-cone $DIRS
    git -C "$CACHE" fetch -q --depth 1 --filter=blob:none origin "$COMMIT"
    git -C "$CACHE" checkout -q FETCH_HEAD
else
    git -C "$CACHE" sparse-checkout set --no-cone $DIRS
fi
rm -rf "$DEST"
mkdir -p "$DEST"
for d in $DIRS; do
    mkdir -p "$DEST/$(dirname "$d")"
    cp -r "$CACHE/$d" "$DEST/$d"
done
cp "$ROOT/tests/test262.conf" "$DEST/rustos.conf"
cp "$ROOT/tests/test262_errors.txt" "$DEST/test262_errors.txt"
echo "test262: $(find "$DEST/test" -name '*.js' ! -name '*_FIXTURE.js' | wc -l) tests in $DEST"
