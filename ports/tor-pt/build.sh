#!/bin/sh
# Tor pluggable transports for bridges: lyrebird (obfs4, meek, webtunnel)
# and the Snowflake client, plus Snowflake's default bridge lines for
# edex-tor-bridges. Go programs, built static without cgo (raw Linux
# system calls, no libc); needs the host's Go 1.22+ and its module proxy.
# Called by tools/install-port.sh with: SRC_DIR BUILD_DIR DEST_DIR
set -e
LYREBIRD=0.6.1
SNOWFLAKE=2.11.0
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
. "$ROOT/tools/port-lib.sh"
SRC="$1"; BUILD="$2"; DEST="$3"
command -v go >/dev/null || { echo "tor-pt: Go is not installed" >&2; exit 1; }
PT=https://gitlab.torproject.org/tpo/anti-censorship/pluggable-transports
fetch $PT/lyrebird/-/archive/lyrebird-$LYREBIRD/lyrebird-lyrebird-$LYREBIRD.tar.gz \
    387f3ea59024523b698ec6d714a6c7e259561d5fb85e9bde466d40b671b3af71 "$SRC/lyrebird-$LYREBIRD.tar.gz"
fetch $PT/snowflake/-/archive/v$SNOWFLAKE/snowflake-v$SNOWFLAKE.tar.gz \
    1362a8d7e848beea63bf4d7e6b5541df92f2859b83daaf4260afef131556ac57 "$SRC/snowflake-$SNOWFLAKE.tar.gz"
rm -rf "$BUILD/lyrebird-lyrebird-$LYREBIRD" "$BUILD/snowflake-v$SNOWFLAKE"
tar -xzf "$SRC/lyrebird-$LYREBIRD.tar.gz" -C "$BUILD"
tar -xzf "$SRC/snowflake-$SNOWFLAKE.tar.gz" -C "$BUILD"
export CGO_ENABLED=0 GOOS=linux GOARCH=amd64 GOAMD64=v1 GOFLAGS=-trimpath
export GOPATH="$BUILD/gopath" GOCACHE="$BUILD/gocache"
cd "$BUILD/lyrebird-lyrebird-$LYREBIRD"
go build -ldflags="-s -w -X main.lyrebirdVersion=$LYREBIRD" -o "$DEST/bin/lyrebird" ./cmd/lyrebird
cd "$BUILD/snowflake-v$SNOWFLAKE"
go build -ldflags="-s -w" -o "$DEST/bin/snowflake-client" ./client
mkdir -p "$DEST/share/tor"
grep '^Bridge snowflake ' client/torrc > "$DEST/share/tor/snowflake-bridge.conf"
install -m 644 LICENSE "$DEST/share/tor/LICENSE.snowflake"
install -m 644 "$BUILD/lyrebird-lyrebird-$LYREBIRD/LICENSE" "$DEST/share/tor/LICENSE.lyrebird"
