#!/bin/sh
# cfgd installer — used by the makeself self-extracting archive.
# Copies the cfgd binary to PREFIX/bin (default: /usr/local).
set -e

PREFIX="${PREFIX:-/usr/local}"
BINDIR="${PREFIX}/bin"

if [ ! -d "$BINDIR" ]; then
    mkdir -p "$BINDIR"
fi

if [ -f cfgd ]; then
    install -m 0755 cfgd "$BINDIR/cfgd"
    echo "Installed cfgd to $BINDIR/cfgd"
else
    echo "Error: cfgd binary not found in archive" >&2
    exit 1
fi
