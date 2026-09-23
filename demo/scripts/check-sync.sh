#!/usr/bin/env bash
# Flag every demo GIF whose render inputs changed since the commit it was
# recorded at (its line in demo/recorded.txt, written by stamp.sh when the GIF
# is encoded).
#
# The inputs are deliberately broad: any change to the tape, the image, the demo
# scripts or the non-test source of a crate the take runs counts, so a change
# that moves the rendered output cannot slip past. A change that moves nothing
# is flagged too, and the answer to either is the same: re-record the GIF, which
# rewrites its stamp. There is no other way to clear a flag.
set -euo pipefail

cd "$(dirname "$0")/../.."

FILE=demo/recorded.txt
if [ ! -f "$FILE" ]; then
    echo "$FILE does not exist, so no GIF can be checked." >&2
    exit 1
fi

COMMON=(demo/Dockerfile demo/scripts crates/cfgd/src crates/cfgd-core/src crates/cfgd-schema/src)
# The two takes that run against a cluster also render the operator, the CSI
# driver and the chart that installs them.
CLUSTER=(crates/cfgd-operator/src crates/cfgd-csi/src chart)
TESTS_EXCLUDED=(
    ':(exclude,glob)**/tests.rs'
    ':(exclude,glob)**/tests/**'
    ':(exclude,glob)**/test_helpers*'
    ':(exclude,glob)**/test_helpers*/**'
)

failed=0
checked=0
while read -r gif tape sha extra; do
    case "$gif" in '' | '#'*) continue ;; esac
    checked=$((checked + 1))
    if [ -z "$sha" ] || [ -n "$extra" ] || ! [[ "$sha" =~ ^[0-9a-f]{40}$ ]]; then
        echo "$FILE: the line for $gif is not \`<gif> <tape> <40-hex commit>\`."
        failed=1
        continue
    fi
    if [ ! -f "demo/$gif" ] || [ ! -f "demo/$tape" ]; then
        echo "$FILE: the line for $gif names a file demo/ does not hold (demo/$gif, demo/$tape)."
        failed=1
        continue
    fi
    # A shallow clone lacks the commit, and so does a stamp that never existed;
    # either way nothing can be said about the GIF, so it fails.
    if ! git cat-file -e "${sha}^{commit}" 2>/dev/null; then
        echo "demo/$gif: recorded at $sha, a commit this clone does not have (fetch the full history)."
        failed=1
        continue
    fi
    inputs=("demo/$tape" "${COMMON[@]}")
    case "$tape" in k8s.tape | connect.tape) inputs+=("${CLUSTER[@]}") ;; esac
    changed="$(git diff --name-only "$sha" HEAD -- "${inputs[@]}" "${TESTS_EXCLUDED[@]}")"
    if [ -n "$changed" ]; then
        count="$(printf '%s\n' "$changed" | wc -l)"
        echo "demo/$gif: recorded at $sha, $count render inputs changed since:"
        printf '%s\n' "$changed" | sed -n '1,10s/^/  /p'
        failed=1
    fi
done < "$FILE"

if [ "$checked" -eq 0 ]; then
    echo "$FILE holds no GIF line, so nothing was checked." >&2
    exit 1
fi
if [ "$failed" -ne 0 ]; then
    echo "Re-record each GIF named above with its \`task demo*\` target; the take rewrites its line in $FILE."
    exit 1
fi
echo "All $checked demo GIFs were recorded after their last render-input change."
