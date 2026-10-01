#!/bin/sh
# JetBrains Mono Nerd Font (the eDEX-DE terminal and UI font), regular and
# monospaced-glyph variants in four styles, with its licence.
# Called by tools/install-port.sh with: SRC_DIR BUILD_DIR DEST_DIR
set -e
VER=3.4.0
ROOT="$(cd "$(dirname "$0")/../.." && pwd)"
. "$ROOT/tools/port-lib.sh"
SRC="$1"; BUILD="$2"; DEST="$3"
fetch https://github.com/ryanoasis/nerd-fonts/releases/download/v$VER/JetBrainsMono.tar.xz \
    ef552a3e638f25125c6ad4c51176a6adcdce295ab1d2ffacf0db060caf8c1582 "$SRC/JetBrainsMono-$VER.tar.xz"
rm -rf "$BUILD/fonts"
mkdir -p "$BUILD/fonts"
tar -xJf "$SRC/JetBrainsMono-$VER.tar.xz" -C "$BUILD/fonts"
FONTS="$DEST/share/fonts/jetbrains-mono-nerd"
mkdir -p "$FONTS"
for family in JetBrainsMonoNerdFont JetBrainsMonoNerdFontMono; do
    for style in Regular Bold Italic BoldItalic; do
        install -m 644 "$BUILD/fonts/$family-$style.ttf" "$FONTS/"
    done
done
install -m 644 "$BUILD/fonts/OFL.txt" "$FONTS/LICENSE"
