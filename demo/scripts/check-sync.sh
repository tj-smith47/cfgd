#!/usr/bin/env bash
# Flag every demo GIF whose render inputs changed since the commit its take was
# recorded at (its line in demo/recorded.txt, copied by stamp.sh from the take),
# and every GIF whose stamp no take wrote: the GIF must have been committed
# after its stamped commit.
#
# The inputs are deliberately broad: the tape, the demo image and scripts, the
# chart and release images the cluster takes install, the workspace manifests
# and lockfile, and every file under crates/ except test code, changelogs and
# the test-fixtures crate (fixtures and snapshots a crate embeds count; a crate
# added later joins by existing). A dependency, version or chart bump therefore
# flags every GIF. A change that moves nothing is flagged too, and the answer to
# either is the same: re-record the GIF, which rewrites its stamp. There is no
# other way to clear a flag.
set -euo pipefail

cd "$(dirname "$0")/../.."

FILE=demo/recorded.txt
if [ ! -f "$FILE" ]; then
    echo "$FILE does not exist, so no GIF can be checked." >&2
    exit 1
fi

COMMON=(
    demo/Dockerfile demo/scripts .dockerignore crates Cargo.toml Cargo.lock
    chart Dockerfile.operator.release Dockerfile.csi.release
)
EXCLUDED=(
    ':(exclude)crates/cfgd-test-fixtures'
    ':(exclude,glob)**/CHANGELOG.md'
    ':(exclude,glob)**/tests.rs'
    ':(exclude,glob)**/tests/**/*.rs'
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
        echo "demo/$gif: recorded at $sha, a commit this clone does not have: a shallow clone, or the stamped commit was rewritten (rebase, reword, rebase-merge or squash-merge) after the take. Re-record the GIF."
        failed=1
        continue
    fi
    # A take commits its GIF after the commit it recorded, so a stamp that is not
    # a strict ancestor of the GIF's last commit was written by hand, not by a take.
    gif_at="$(git log -1 --format=%H -- "demo/$gif")"
    if [ -z "$gif_at" ] || [ "$gif_at" = "$sha" ] || ! git merge-base --is-ancestor "$sha" "$gif_at"; then
        echo "demo/$gif: stamped at $sha, but the GIF was last committed at ${gif_at:-nowhere}, not after that commit, so no take wrote the stamp. Re-record the GIF."
        failed=1
        continue
    fi
    changed="$(git diff --name-only "$sha" HEAD -- "demo/$tape" "${COMMON[@]}" "${EXCLUDED[@]}")"
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
