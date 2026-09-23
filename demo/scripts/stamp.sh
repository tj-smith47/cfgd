#!/usr/bin/env bash
# Copy into demo/recorded.txt the commit the take behind demo/<gif> was
# recorded at, the commit `demo/scripts/check-sync.sh` diffs the GIF's render
# inputs from.
#
# The commit comes from the take directory's `recorded-at`, which record.sh
# writes when the take is recorded, and never from the checkout this runs in: a
# re-encode of old frames after a later commit would otherwise claim frames the
# later commit never rendered, and a hand run would clear a flag without a take.
#
# The only writer of that file: every line is rewritten through the one
# format below and the lines are kept sorted by GIF, so two takes stamping
# different lines never disagree on the file's shape.
#
# Usage: stamp.sh <gif-basename> <tape-basename> <take-dir>
set -euo pipefail

cd "$(dirname "$0")/../.."

USAGE="usage: stamp.sh <gif-basename> <tape-basename> <take-dir>"
GIF="${1:?$USAGE}"
TAPE="${2:?$USAGE}"
TAKE="${3:?$USAGE}"
FILE=demo/recorded.txt

for f in "demo/$GIF" "demo/$TAPE" "$FILE"; do
    if [ ! -f "$f" ]; then
        echo "$f does not exist, so there is nothing to stamp." >&2
        exit 1
    fi
done

SIDECAR="${TAKE%/}/recorded-at"
if [ ! -f "$SIDECAR" ]; then
    echo "$SIDECAR does not exist: demo/$TAPE has no take recorded by demo/scripts/record.sh to stamp." >&2
    exit 1
fi
sha="$(cat "$SIDECAR")"
if ! [[ "$sha" =~ ^[0-9a-f]{40}$ ]]; then
    echo "$SIDECAR holds \`$sha\`, not a 40-hex commit, so demo/$TAPE's take cannot be stamped." >&2
    exit 1
fi

{
    printf '%-24s %-20s %s\n' '# gif' 'tape' 'commit-recorded-at'
    {
        awk -v g="$GIF" '!/^#/ && NF && $1 != g' "$FILE"
        printf '%s %s %s\n' "$GIF" "$TAPE" "$sha"
    } | LC_ALL=C sort | awk '{ printf "%-24s %-20s %s\n", $1, $2, $3 }'
} > "$FILE.tmp"
mv "$FILE.tmp" "$FILE"

echo "Stamped demo/$GIF as recorded from demo/$TAPE at $sha"
