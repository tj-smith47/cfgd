#!/usr/bin/env bash
# Record in demo/recorded.txt that demo/<gif> was just rendered from
# demo/<tape> as of HEAD, the commit `demo/scripts/check-sync.sh` diffs the
# GIF's render inputs from.
#
# The only writer of that file: every line is rewritten through the one
# format below and the lines are kept sorted by GIF, so two takes stamping
# different lines never disagree on the file's shape.
#
# Usage: stamp.sh <gif-basename> <tape-basename>
set -euo pipefail

cd "$(dirname "$0")/../.."

GIF="${1:?usage: stamp.sh <gif-basename> <tape-basename>}"
TAPE="${2:?usage: stamp.sh <gif-basename> <tape-basename>}"
FILE=demo/recorded.txt

for f in "demo/$GIF" "demo/$TAPE" "$FILE"; do
    if [ ! -f "$f" ]; then
        echo "$f does not exist, so there is nothing to stamp." >&2
        exit 1
    fi
done

# HEAD, not the working tree: an uncommitted change to a render input is
# committed after the take, so the check sees it as changed since the stamp and
# flags the GIF once more. That over-flag is the safe direction.
sha="$(git rev-parse HEAD)"

{
    printf '%-24s %-20s %s\n' '# gif' 'tape' 'commit-recorded-at'
    {
        awk -v g="$GIF" '!/^#/ && NF && $1 != g' "$FILE"
        printf '%s %s %s\n' "$GIF" "$TAPE" "$sha"
    } | LC_ALL=C sort | awk '{ printf "%-24s %-20s %s\n", $1, $2, $3 }'
} > "$FILE.tmp"
mv "$FILE.tmp" "$FILE"

echo "Stamped demo/$GIF as recorded from demo/$TAPE at $sha"
