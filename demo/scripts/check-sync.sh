#!/usr/bin/env bash
# Flag every demo GIF whose render inputs changed since the commit it was
# recorded at (its line in demo/recorded.txt, written by stamp.sh when the GIF
# is encoded).
#
# The inputs are deliberately broad: any change to the tape, the image, the demo
# scripts, the workspace manifests and lockfile, or any non-test file of any
# crate counts (a crate added later joins by existing), so a change that moves
# the rendered output cannot slip past. A dependency or version bump therefore
# flags every GIF. A change that moves nothing
# is flagged too, and the answer to either is the same: re-record the GIF, which
# rewrites its stamp. There is no other way to clear a flag.
set -euo pipefail

cd "$(dirname "$0")/../.."

FILE=demo/recorded.txt
if [ ! -f "$FILE" ]; then
    echo "$FILE does not exist, so no GIF can be checked." >&2
    exit 1
fi

COMMON=(demo/Dockerfile demo/scripts crates Cargo.toml Cargo.lock)
# The two takes that run against a cluster also render the operator and CSI
# images they install and the chart that installs them.
CLUSTER=(chart Dockerfile.operator.release Dockerfile.csi.release)
EXCLUDED=(
    ':(exclude)crates/cfgd-test-fixtures'
    ':(exclude,glob)**/CHANGELOG.md'
    ':(exclude,glob)**/tests.rs'
    ':(exclude,glob)**/tests/**'
    ':(exclude,glob)**/test_helpers*'
    ':(exclude,glob)**/test_helpers*/**'
)

failed=0
checked=0
# `read` fails on a last line with no newline after filling the fields, so the
# second test is what keeps that line from being dropped unchecked.
while read -r gif tape sha extra || [ -n "$gif" ]; do
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
    # Nothing can be said about a GIF whose commit is gone, so it fails.
    if ! git cat-file -e "${sha}^{commit}" 2>/dev/null; then
        echo "demo/$gif: recorded at $sha, a commit this clone does not have: a shallow clone, or the stamped commit was rewritten (rebase, reword or rebase-merge) after the take. Re-record the GIF."
        failed=1
        continue
    fi
    inputs=("demo/$tape" "${COMMON[@]}")
    case "$tape" in k8s.tape | connect.tape) inputs+=("${CLUSTER[@]}") ;; esac
    changed="$(git diff --name-only "$sha" HEAD -- "${inputs[@]}" "${EXCLUDED[@]}")"
    if [ -n "$changed" ]; then
        count="$(grep -c '' <<<"$changed")"
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
