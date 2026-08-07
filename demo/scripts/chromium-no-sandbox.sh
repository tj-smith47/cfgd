#!/usr/bin/env bash
# rod (the browser driver VHS records through) downloads its own Chromium on
# first run, and Chromium refuses to start as root without --no-sandbox — a flag
# VHS exposes no way to pass. Wrapping the downloaded binary in place is the only
# injection point.
set -euo pipefail

# Only root needs this. Wrapping an unprivileged user's Chromium would strip a
# sandbox it was perfectly able to use, for no gain.
if [ "$(id -u)" -ne 0 ]; then
    exit 0
fi

found=0
for chrome in "$HOME"/.cache/rod/browser/*/chrome; do
    [ -f "$chrome" ] || continue
    found=1
    # Re-wrapping an already-wrapped binary would make it exec itself.
    if head -c 2 "$chrome" | grep -q '#!'; then
        continue
    fi
    mv "$chrome" "$chrome.orig"
    printf '#!/bin/sh\nexec "%s" --no-sandbox "$@"\n' "$chrome.orig" >"$chrome"
    chmod +x "$chrome"
    echo "Wrapped: $chrome"
done

if [ "$found" = 0 ]; then
    echo "No rod Chromium downloaded yet — VHS will fetch one, then this needs to run again." >&2
fi
