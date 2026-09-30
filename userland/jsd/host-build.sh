#!/bin/sh
# Build jsd for the host, for the protocol tests in crates/jsproto:
#   host-build.sh QUICKJS_SRC OUT_DIR
# QUICKJS_SRC is the quickjs directory of the port's source (see
# ports/quickjs/build.sh); the binary is OUT_DIR/jsd.
set -e
Q="$1"; OUT="$2"
HERE="$(cd "$(dirname "$0")" && pwd)"
mkdir -p "$OUT"
for f in quickjs libregexp libunicode dtoa; do
    [ "$OUT/$f.o" -nt "$Q/$f.c" ] || cc -O1 -g -D_GNU_SOURCE -w -c "$Q/$f.c" -o "$OUT/$f.o"
done
python3 - "$HERE/lib" "$OUT/jsd_lib.h" <<'PY'
import os, sys
lib, out = sys.argv[1], sys.argv[2]
data = b"".join(open(os.path.join(lib, f), "rb").read() + b"\n" for f in sorted(os.listdir(lib)) if f.endswith(".js"))
with open(out, "w") as o:
    o.write("static const char jsd_lib[] = {\n")
    for i in range(0, len(data), 24):
        o.write(",".join(str(b) for b in data[i:i + 24]) + ",\n")
    o.write("0};\n")
PY
cc -O1 -g -D_GNU_SOURCE -I"$Q" -I"$OUT" -o "$OUT/jsd" "$HERE/jsd.c" \
    "$OUT/quickjs.o" "$OUT/libregexp.o" "$OUT/libunicode.o" "$OUT/dtoa.o" -lm -lpthread
