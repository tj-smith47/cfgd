#!/usr/bin/env bash
# Checks the e2e image tag map in helpers.sh without a cluster: each image's
# reference resolves to IMAGE_TAG unless its own override is set, and no suite
# composes a first-party image reference outside the helpers.
#
# Usage: tests/e2e/common/test-image-tags.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
e2e_root="$(dirname "$here")"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT

registry="registry.test"
images=(cfgd cfgd-operator cfgd-csi function-cfgd)
failures=0

# Each resolution sources helpers.sh in a fresh shell, so an override is seen
# the way a suite sees it: set in the environment before the helpers load.
# shellcheck disable=SC2016 # the inner script expands its own positional args
resolve() {
    local image="$1"; shift
    env -u CFGD_IMAGE_TAG -u OPERATOR_IMAGE_TAG -u CSI_IMAGE_TAG -u FUNCTION_IMAGE_TAG \
        REGISTRY="$registry" IMAGE_TAG=base CLI_SCRATCH="$scratch" "$@" \
        bash -c 'source "$1/common/helpers.sh"; e2e_image "$2"' _ "$e2e_root" "$image"
}

expect() {
    local label="$1" image="$2" want="$3"; shift 3
    local got
    got="$(resolve "$image" "$@" 2>&1)" || got="(failed: $got)"
    if [ "$got" = "$want" ]; then
        echo "PASS  $label: $image -> $got"
    else
        echo "FAIL  $label: $image -> $got (want $want)"
        failures=$((failures + 1))
    fi
}

for image in "${images[@]}"; do
    expect "default" "$image" "$registry/$image:base"
done

# One override at a time: its own image moves, the other three keep IMAGE_TAG.
declare -A override_of=(
    [cfgd]=CFGD_IMAGE_TAG
    [cfgd-operator]=OPERATOR_IMAGE_TAG
    [cfgd-csi]=CSI_IMAGE_TAG
    [function-cfgd]=FUNCTION_IMAGE_TAG
)
for target in "${images[@]}"; do
    var="${override_of[$target]}"
    for image in "${images[@]}"; do
        if [ "$image" = "$target" ]; then want="$registry/$image:pinned"; else want="$registry/$image:base"; fi
        expect "$var only" "$image" "$want" "$var=pinned"
    done
done

released=(CFGD_IMAGE_TAG=0.11.0 OPERATOR_IMAGE_TAG=0.9.0 CSI_IMAGE_TAG=0.7.2)
expect "released set" cfgd "$registry/cfgd:0.11.0" "${released[@]}"
expect "released set" cfgd-operator "$registry/cfgd-operator:0.9.0" "${released[@]}"
expect "released set" cfgd-csi "$registry/cfgd-csi:0.7.2" "${released[@]}"
expect "released set" function-cfgd "$registry/function-cfgd:base" "${released[@]}"

if resolve not-an-image >/dev/null 2>&1; then
    echo "FAIL  unknown image: resolved instead of failing"
    failures=$((failures + 1))
else
    echo "PASS  unknown image: refused"
fi

# A reference composed by hand would ignore its override, so the helpers are
# the only place a first-party image or IMAGE_TAG may be spelled.
stray="$(grep -rnE --include='*.sh' --include='*.yaml' \
    -e 'IMAGE_TAG|REGISTRY_PLACEHOLDER' \
    -e '\$\{?REGISTRY\}?/(cfgd|cfgd-operator|cfgd-csi|function-cfgd)([^-a-zA-Z0-9]|$)' \
    "$e2e_root" | grep -v -e '^[^:]*/common/helpers\.sh:' -e '^[^:]*/common/test-image-tags\.sh:' || true)"
if [ -n "$stray" ]; then
    echo "FAIL  image references composed outside common/helpers.sh:"
    printf '%s\n' "$stray"
    failures=$((failures + 1))
else
    echo "PASS  every first-party image reference goes through common/helpers.sh"
fi

if [ "$failures" -ne 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "all checks passed"
