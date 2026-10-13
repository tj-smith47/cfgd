#!/usr/bin/env bash
# Checks the e2e image tag map in helpers.sh without a cluster: each image's
# reference resolves to IMAGE_TAG unless its own override is set, an overridden
# image comes from RELEASE_REGISTRY when that is set, every push stays on
# REGISTRY, and no suite composes a registry reference or pushes outside the
# helpers.
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

# RELEASE_REGISTRY moves only an overridden image: a released image is pulled
# from where it was published, while an image setup builds stays on REGISTRY.
release="release.test/owner"
for image in "${images[@]}"; do
    expect "RELEASE_REGISTRY, overridden" "$image" "$release/$image:0.1.0" \
        "RELEASE_REGISTRY=$release" "${override_of[$image]}=0.1.0"
    expect "RELEASE_REGISTRY, not overridden" "$image" "$registry/$image:base" \
        "RELEASE_REGISTRY=$release"
    expect "RELEASE_REGISTRY empty, overridden" "$image" "$registry/$image:0.1.0" \
        "RELEASE_REGISTRY=" "${override_of[$image]}=0.1.0"
done
expect "released set from RELEASE_REGISTRY" cfgd-csi "$release/cfgd-csi:0.7.2" \
    "RELEASE_REGISTRY=$release" "${released[@]}"

# shellcheck disable=SC2016 # the inner script expands its own positional args
in_helpers() {
    env -u CFGD_IMAGE_TAG -u OPERATOR_IMAGE_TAG -u CSI_IMAGE_TAG -u FUNCTION_IMAGE_TAG \
        REGISTRY="$registry" IMAGE_TAG=base CLI_SCRATCH="$scratch" RELEASE_REGISTRY="$release" "${released[@]}" \
        PATH="$scratch/bin:$PATH" bash -c 'source "$1/common/helpers.sh"; shift; "$@"' _ "$e2e_root" "$@" 2>&1
}

check() {
    local label="$1" got="$2" want="$3"
    if [ "$got" = "$want" ]; then
        echo "PASS  $label"
    else
        echo "FAIL  $label: got '$got' (want '$want')"
        failures=$((failures + 1))
    fi
}

# A scratch artifact is written by the run, so it stays on REGISTRY even while a
# released set is pulled from RELEASE_REGISTRY.
check "scratch ref stays on REGISTRY under RELEASE_REGISTRY" \
    "$(in_helpers e2e_scratch_ref mod:v1)" "$registry/cfgd-e2e/mod:v1"

# e2e_push hands a ref under REGISTRY to the tool and refuses any other one,
# an overridden image's RELEASE_REGISTRY reference included, without running it.
mkdir -p "$scratch/bin"
# shellcheck disable=SC2016 # the stand-in expands its own $0 and $*
printf '#!/usr/bin/env bash\necho "$(basename "$0") $*" >>"%s/push.log"\n' "$scratch" >"$scratch/bin/docker"
cp "$scratch/bin/docker" "$scratch/bin/crossplane"
chmod +x "$scratch/bin/docker" "$scratch/bin/crossplane"
in_helpers e2e_push docker "$registry/cfgd:base" >/dev/null
in_helpers e2e_push xpkg "$registry/function-cfgd:base" -f pkg.xpkg >/dev/null
check "e2e_push runs the tool for refs under REGISTRY" "$(cat "$scratch/push.log")" \
    "docker push $registry/cfgd:base
crossplane xpkg push $registry/function-cfgd:base -f pkg.xpkg"
for ref in "$release/cfgd-operator:0.9.0" "$registry.evil/cfgd:x" "ghcr.io/other/cfgd:x"; do
    if out="$(in_helpers e2e_push docker "$ref")"; then
        check "e2e_push refuses $ref" "pushed: $out" "refused"
    else
        check "e2e_push refuses $ref" "$(grep -c 'refusing to push' <<<"$out")" 1
    fi
done
check "a refused push runs no tool" "$(wc -l <"$scratch/push.log")" 2

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
        REGISTRY="$registry" IMAGE_TAG=base CLI_SCRATCH="$scratch" "${@:3}" \
        bash -c 'source "$1/common/helpers.sh"; e2e_override_unused_warning "$2" "the tree at /deploy" deployment/x "$3"' \
        _ "$e2e_root" "$1" "$2" 2>&1
}

# A component whose spec setup does not own must say when its override is
# ignored, naming the owner and what runs, and stay quiet with no override or
# when the object already runs the overridden reference.
for image in "${images[@]}"; do
    var="${override_of[$image]}"
    want="  WARN: $var is set, but the tree at /deploy owns deployment/x, which runs running:1; the override does not reach it"
    got="$(warning "$image" running:1 "$var=pinned")"
    if [ "$got" = "$want" ]; then
        echo "PASS  unused-override warning, $var set: $image"
    else
        echo "FAIL  unused-override warning, $var set: $image -> '$got' (want '$want')"
        failures=$((failures + 1))
    fi
    got="$(warning "$image" running:1)"
    if [ -z "$got" ]; then
        echo "PASS  unused-override warning, $var unset: $image says nothing"
    else
        echo "FAIL  unused-override warning, $var unset: $image -> '$got' (want nothing)"
        failures=$((failures + 1))
    fi
    got="$(warning "$image" "$registry/$image:pinned" "$var=pinned")"
    if [ -z "$got" ]; then
        echo "PASS  unused-override warning, $var set and already running: $image says nothing"
    else
        echo "FAIL  unused-override warning, $var set and already running: $image -> '$got' (want nothing)"
        failures=$((failures + 1))
    fi
done

if resolve not-an-image >/dev/null 2>&1; then
    echo "FAIL  unknown image: resolved (want a refusal)"
    failures=$((failures + 1))
else
    echo "PASS  unknown image: refused"
fi

# A reference composed by hand would ignore its override or push past the
# REGISTRY guard, so the helpers are the only place IMAGE_TAG, RELEASE_REGISTRY,
# a path under REGISTRY or a docker or xpkg push may be spelled. grep exits 1
# for no match and 2 for an error; an error must fail the check, since an empty
# result would otherwise read as a clean tree.
scan_strays() {
    local out rc=0 line
    out="$(grep -rnE \
        --include='*.sh' --include='*.yaml' --include='*.yml' --include='*.tera' --include='*.json' \
        -e 'IMAGE_TAG|REGISTRY_PLACEHOLDER|RELEASE_REGISTRY' \
        -e '\$\{?REGISTRY[^}/"]*\}?"?/' \
        -e '(docker|xpkg) push' \
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
# reported; a scratch artifact composed through e2e_scratch_ref must not be.
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
    [module-artifact.sh]='ref="${REGISTRY}/cfgd-e2e/module:v1"'
    [release.sh]='echo "$RELEASE_REGISTRY"'
    [docker-push.sh]='docker push "$ref"'
    [xpkg-push.sh]='crossplane xpkg push "$ref" -f x.xpkg'
)
for name in "${!stray_fixture[@]}"; do
    printf '%s\n' "${stray_fixture[$name]}" > "$fixtures/$name"
done
# shellcheck disable=SC2016 # fixtures hold the literal text a script would contain
printf '%s\n' 'ref="$(e2e_scratch_ref "module:v1")"' > "$fixtures/scratch-ref.sh"
if found="$(scan_strays "$fixtures")"; then
    for name in "${!stray_fixture[@]}"; do
        if grep -qF -- "$fixtures/$name:" <<<"$found"; then
            echo "PASS  scan reports $name"
        else
            echo "FAIL  scan misses $name: ${stray_fixture[$name]}"
            failures=$((failures + 1))
        fi
    done
    if grep -qF -- "$fixtures/scratch-ref.sh:" <<<"$found"; then
        echo "FAIL  scan reports a scratch artifact composed through e2e_scratch_ref"
        failures=$((failures + 1))
    else
        echo "PASS  scan passes a scratch artifact composed through e2e_scratch_ref"
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
