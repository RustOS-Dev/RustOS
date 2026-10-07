# Helpers for ports/*/build.sh.

# fetch URL SHA256 FILE: download once and verify.
fetch() {
    if [ ! -f "$3" ]; then
        curl -fsSL -o "$3.part" "$1"
        mv "$3.part" "$3"
    fi
    echo "$2  $3" | sha256sum -c --quiet - || { echo "checksum mismatch: $3" >&2; rm -f "$3"; exit 1; }
}

# fetch_git URL TAG COMMIT DIR: shallow clone of TAG into DIR (for hosts
# whose release tarballs are not reachable), checked against COMMIT.
fetch_git() {
    if [ ! -d "$4/.git" ]; then
        rm -rf "$4.part"
        git clone -q --depth 1 -b "$2" "$1" "$4.part" 2>/dev/null
        mv "$4.part" "$4"
    fi
    [ "$(git -C "$4" rev-parse HEAD)" = "$3" ] || { echo "commit mismatch: $4" >&2; exit 1; }
}
