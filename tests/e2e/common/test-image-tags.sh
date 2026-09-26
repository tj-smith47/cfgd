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

released=(CFGD_IMAGE_TAG=0.11.0 OPERATOR_IMAGE_TAG=0.9.0 CSI_IMAGE_TAG=0.7.2 FUNCTION_IMAGE_TAG=v0.11.0)
expect "released set" cfgd "$registry/cfgd:0.11.0" "${released[@]}"
expect "released set" cfgd-operator "$registry/cfgd-operator:0.9.0" "${released[@]}"
expect "released set" cfgd-csi "$registry/cfgd-csi:0.7.2" "${released[@]}"
expect "released set" function-cfgd "$registry/function-cfgd:v0.11.0" "${released[@]}"

# shellcheck disable=SC2016 # the inner script expands its own positional args
overridden() {
    local image="$1"; shift
    env -u CFGD_IMAGE_TAG -u OPERATOR_IMAGE_TAG -u CSI_IMAGE_TAG -u FUNCTION_IMAGE_TAG \
        REGISTRY="$registry" IMAGE_TAG=base CLI_SCRATCH="$scratch" "$@" \
        bash -c 'source "$1/common/helpers.sh"; e2e_image_overridden "$2"' _ "$e2e_root" "$image" 2>/dev/null
}

# Setup never builds an overridden image, so the predicate must be true exactly
# when a non-empty override is set; an empty one falls back to IMAGE_TAG.
for image in "${images[@]}"; do
    var="${override_of[$image]}"
    for arm in set unset empty; do
        case "$arm" in
            set) args=("$var=pinned"); want=true ;;
            unset) args=(); want=false ;;
            empty) args=("$var="); want=false ;;
        esac
        if overridden "$image" "${args[@]}"; then got=true; else got=false; fi
        if [ "$got" = "$want" ]; then
            echo "PASS  overridden, $var $arm: $image -> $got"
        else
            echo "FAIL  overridden, $var $arm: $image -> $got (want $want)"
            failures=$((failures + 1))
        fi
    done
done

# shellcheck disable=SC2016 # the inner script expands its own positional args
warning() {
    env -u CFGD_IMAGE_TAG -u OPERATOR_IMAGE_TAG -u CSI_IMAGE_TAG -u FUNCTION_IMAGE_TAG \
        REGISTRY="$registry" IMAGE_TAG=base CLI_SCRATCH="$scratch" "${@:2}" \
        bash -c 'source "$1/common/helpers.sh"; e2e_override_unused_warning "$2" "the tree at /deploy" deployment/x running:1' \
        _ "$e2e_root" "$1" 2>&1
}

# A component whose spec setup does not own must say when its override is
# ignored, naming the owner and what runs, and stay quiet with no override.
for image in "${images[@]}"; do
    var="${override_of[$image]}"
    want="  WARN: $var is set, but the tree at /deploy owns deployment/x, which runs running:1; the override does not reach it"
    got="$(warning "$image" "$var=pinned")"
    if [ "$got" = "$want" ]; then
        echo "PASS  unused-override warning, $var set: $image"
    else
        echo "FAIL  unused-override warning, $var set: $image -> '$got' (want '$want')"
        failures=$((failures + 1))
    fi
    got="$(warning "$image")"
    if [ -z "$got" ]; then
        echo "PASS  unused-override warning, $var unset: $image says nothing"
    else
        echo "FAIL  unused-override warning, $var unset: $image -> '$got' (want nothing)"
        failures=$((failures + 1))
    fi
done

if resolve not-an-image >/dev/null 2>&1; then
    echo "FAIL  unknown image: resolved instead of failing"
    failures=$((failures + 1))
else
    echo "PASS  unknown image: refused"
fi

# A reference composed by hand would ignore its override, so the helpers are
# the only place a first-party image or IMAGE_TAG may be spelled. grep exits 1
# for no match and 2 for an error; an error must fail the check, since an empty
# result would otherwise read as a clean tree.
scan_strays() {
    local out rc=0 line
    out="$(grep -rnE \
        --include='*.sh' --include='*.yaml' --include='*.yml' --include='*.tera' --include='*.json' \
        -e 'IMAGE_TAG|REGISTRY_PLACEHOLDER' \
        -e '\$\{?REGISTRY[^}/"]*\}?"?/(cfgd|cfgd-operator|cfgd-csi|function-cfgd)([^-a-zA-Z0-9]|$)' \
        "$1" 2>&1)" || rc=$?
    if [ "$rc" -gt 1 ]; then
        printf 'grep exited %s: %s\n' "$rc" "$out" >&2
        return 2
    fi
    while IFS= read -r line; do
        case "${line%%:*}" in
            "" | */common/helpers.sh | */common/test-image-tags.sh) ;;
            *) printf '%s\n' "$line" ;;
        esac
    done <<<"$out"
}

if stray="$(scan_strays "$e2e_root")"; then
    if [ -n "$stray" ]; then
        echo "FAIL  image references composed outside common/helpers.sh:"
        printf '%s\n' "$stray"
        failures=$((failures + 1))
    else
        echo "PASS  every first-party image reference goes through common/helpers.sh"
    fi
else
    echo "FAIL  the image reference scan could not read $e2e_root"
    failures=$((failures + 1))
fi

# Each spelling the scan exists to catch, one fixture file apiece, must be
# reported; an OCI module path that merely starts with cfgd must not be.
fixtures="$scratch/stray-fixtures"
mkdir -p "$fixtures"
# shellcheck disable=SC2016 # fixtures hold the literal text a script would contain
declare -A stray_fixture=(
    [braced.sh]='ref="${REGISTRY}/cfgd:x"'
    [bare.yml]='image: $REGISTRY/cfgd-operator:x'
    [quoted.sh]='ref="$REGISTRY"/cfgd-operator:x'
    [defaulted.tera]='image: ${REGISTRY:-r}/cfgd-csi:x'
    [line-end.yaml]='repository: $REGISTRY/function-cfgd'
    [tag.json]='{"tag": "$IMAGE_TAG"}'
    [placeholder.yaml]='image: REGISTRY_PLACEHOLDER/cfgd:x'
)
for name in "${!stray_fixture[@]}"; do
    printf '%s\n' "${stray_fixture[$name]}" > "$fixtures/$name"
done
# shellcheck disable=SC2016 # fixtures hold the literal text a script would contain
printf '%s\n' 'ref="${REGISTRY}/cfgd-e2e/module:v1"' > "$fixtures/module-artifact.sh"
if found="$(scan_strays "$fixtures")"; then
    for name in "${!stray_fixture[@]}"; do
        if grep -qF -- "$fixtures/$name:" <<<"$found"; then
            echo "PASS  scan reports $name"
        else
            echo "FAIL  scan misses $name: ${stray_fixture[$name]}"
            failures=$((failures + 1))
        fi
    done
    if grep -qF -- "$fixtures/module-artifact.sh:" <<<"$found"; then
        echo "FAIL  scan reports an OCI module artifact under \$REGISTRY/cfgd-e2e"
        failures=$((failures + 1))
    else
        echo "PASS  scan passes an OCI module artifact under \$REGISTRY/cfgd-e2e"
    fi
else
    echo "FAIL  the image reference scan could not read its fixtures"
    failures=$((failures + 1))
fi

if scan_strays "$scratch/no-such-dir" >/dev/null 2>&1; then
    echo "FAIL  scan of an unreadable path passed"
    failures=$((failures + 1))
else
    echo "PASS  scan of an unreadable path fails"
fi

if [ "$failures" -ne 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "all checks passed"
