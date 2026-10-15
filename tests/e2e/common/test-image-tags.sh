#!/usr/bin/env bash
# Checks the e2e image tag map in helpers.sh without a cluster: each image's
# reference resolves to IMAGE_TAG unless its own override is set, an overridden
# image comes from RELEASE_REGISTRY when that is set, every push stays on
# REGISTRY (the buildx cache included), setup refuses an override older than
# the tag chart/cfgd pins for its image, and no suite composes a registry
# reference or pushes outside the functions in helpers.sh that own it.
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
# function-cfgd is the exception: setup copies it into REGISTRY, since Crossplane
# installs one Function per package repository and ArgoCD's holds the release one.
for image in "${images[@]}"; do
    if [ "$image" = function-cfgd ]; then from="$registry"; else from="$release"; fi
    expect "RELEASE_REGISTRY set, image overridden" "$image" "$from/$image:0.1.0" \
        "RELEASE_REGISTRY=$release" "${override_of[$image]}=0.1.0"
    expect "RELEASE_REGISTRY set, image with no override" "$image" "$registry/$image:base" \
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

# Only an overridden function-cfgd under RELEASE_REGISTRY is copied, and the copy
# comes from its released reference.
for image in "${images[@]}"; do
    if in_helpers e2e_image_copied_from_release "$image" >/dev/null; then got=copied; else got=pulled; fi
    if [ "$image" = function-cfgd ]; then want=copied; else want=pulled; fi
    check "with RELEASE_REGISTRY and the released set, $image is $want" "$got" "$want"
done
check "function-cfgd is copied from its repository under RELEASE_REGISTRY" \
    "$(in_helpers e2e_image_repo --release function-cfgd)" "$release/function-cfgd"
# shellcheck disable=SC2016 # the inner script expands its own positional args
if env REGISTRY="$registry" RELEASE_REGISTRY= CLI_SCRATCH="$scratch" \
    bash -c 'source "$1/common/helpers.sh"; e2e_image_repo --release cfgd' _ "$e2e_root" >/dev/null 2>&1; then
    got=composed
else
    got=refused
fi
check "the release repository needs RELEASE_REGISTRY" "$got" refused

# The same command with and without FUNCTION_IMAGE_TAG, so a failure that has
# nothing to do with the override cannot pass for "not copied".
# shellcheck disable=SC2016 # the inner script expands its own positional args
copied_with() {
    if env -u CFGD_IMAGE_TAG -u OPERATOR_IMAGE_TAG -u CSI_IMAGE_TAG -u FUNCTION_IMAGE_TAG \
        REGISTRY="$registry" IMAGE_TAG=base CLI_SCRATCH="$scratch" RELEASE_REGISTRY="$release" "$@" \
        bash -c 'source "$1/common/helpers.sh"; e2e_image_copied_from_release function-cfgd' _ "$e2e_root" >/dev/null 2>&1; then
        echo copied
    else
        echo pulled
    fi
}
check "function-cfgd with an override is copied" "$(copied_with FUNCTION_IMAGE_TAG=v1)" copied
check "function-cfgd with no override is not copied" "$(copied_with)" pulled

# The buildx cache is written by the run, so its ref is a scratch artifact on
# REGISTRY for every image, a released one under RELEASE_REGISTRY included.
for image in "${images[@]}"; do
    check "buildcache refs for $image stay on REGISTRY" \
        "$(SCCACHE_GHA_ENABLED=true in_helpers buildcache_args "$image" | grep -o 'ref=[^,]*' | sort -u)" \
        "ref=$registry/cfgd-e2e/$image:buildcache"
done
check "buildcache flags are off without SCCACHE_GHA_ENABLED" \
    "$(SCCACHE_GHA_ENABLED='' in_helpers buildcache_args cfgd)" ""

# e2e_push hands a ref under REGISTRY to the tool and refuses any other one,
# an overridden image's RELEASE_REGISTRY reference included, without running it.
mkdir -p "$scratch/bin"
# shellcheck disable=SC2016 # the stand-in expands its own $0 and $*
printf '#!/usr/bin/env bash\necho "$(basename "$0") $*" >>"%s/push.log"\n' "$scratch" >"$scratch/bin/docker"
cp "$scratch/bin/docker" "$scratch/bin/crossplane"
chmod +x "$scratch/bin/docker" "$scratch/bin/crossplane"
in_helpers e2e_push docker "$registry/cfgd:base" >/dev/null
in_helpers e2e_push xpkg "$registry/function-cfgd:base" -f pkg.xpkg >/dev/null
in_helpers e2e_push copy "$registry/function-cfgd:v1" "$release/function-cfgd:v1" >/dev/null
check "e2e_push runs the tool for refs under REGISTRY" "$(cat "$scratch/push.log")" \
    "docker push $registry/cfgd:base
crossplane xpkg push $registry/function-cfgd:base -f pkg.xpkg
docker buildx imagetools create --tag $registry/function-cfgd:v1 $release/function-cfgd:v1"
for ref in "$release/cfgd-operator:0.9.0" "$registry.evil/cfgd:x" "ghcr.io/other/cfgd:x"; do
    if out="$(in_helpers e2e_push docker "$ref")"; then
        check "e2e_push refuses $ref" "pushed: $out" "refused"
    else
        check "e2e_push refuses $ref" "$(grep -c 'refusing to push' <<<"$out")" 1
    fi
done
check "a refused push runs no tool" "$(wc -l <"$scratch/push.log")" 3

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

if resolve not-an-image >/dev/null 2>&1; then
    echo "FAIL  unknown image: resolved (want a refusal)"
    failures=$((failures + 1))
else
    echo "PASS  unknown image: refused"
fi

# The override gate setup runs before any cluster write. Every image the chart
# pins comes from the chart itself, so an image the chart gains is checked
# without this list changing.
# shellcheck disable=SC2016 # the inner script expands its own positional args
gate() {
    local chart="$1"; shift
    env -u CFGD_IMAGE_TAG -u OPERATOR_IMAGE_TAG -u CSI_IMAGE_TAG -u FUNCTION_IMAGE_TAG \
        REGISTRY="$registry" IMAGE_TAG=base CLI_SCRATCH="$scratch" "$@" \
        bash -c 'source "$1/common/helpers.sh"; e2e_refuse_overrides_below_chart_pins "$2"' _ "$e2e_root" "$chart" 2>&1
}

# expect_gate <label> <chart> <want: pass|refuse> <message fragment> [VAR=tag...]
expect_gate() {
    local label="$1" chart="$2" want="$3" fragment="$4" out rc=0
    shift 4
    out="$(gate "$chart" "$@")" || rc=$?
    if [ "$want" = pass ] && [ "$rc" -eq 0 ] && [ -z "$out" ]; then
        echo "PASS  gate $label: passes"
    elif [ "$want" = refuse ] && [ "$rc" -eq 1 ] && grep -qF -- "$fragment" <<<"$out"; then
        echo "PASS  gate $label: refused"
    else
        echo "FAIL  gate $label: rc $rc, output '$out' (want $want, fragment '$fragment')"
        failures=$((failures + 1))
    fi
}

chart="$(dirname "$(dirname "$e2e_root")")/chart/cfgd"
# shellcheck disable=SC2016 # the inner script expands its own positional args
if ! pins="$(REGISTRY="$registry" CLI_SCRATCH="$scratch" bash -c 'source "$1/common/helpers.sh"; chart_image_pins "$2"' _ "$e2e_root" "$chart")"; then
    echo "FAIL  chart_image_pins could not read $chart"
    failures=$((failures + 1))
    pins=""
fi
for want_image in cfgd cfgd-operator cfgd-csi; do
    if grep -q "/$want_image:" <<<"$pins"; then
        echo "PASS  the chart pins $want_image"
    else
        echo "FAIL  the chart pin reader misses $want_image: '$pins'"
        failures=$((failures + 1))
    fi
done
expect_gate "with no override" "$chart" pass ""
for ref in $pins; do
    image="${ref##*/}"
    pin="${image##*:}"
    image="${image%%:*}"
    var="${override_of[$image]:-}"
    if [ -z "$var" ]; then
        echo "FAIL  the chart pins $image, which has no override in this check"
        failures=$((failures + 1))
        continue
    fi
    IFS=. read -r major minor patch <<<"${pin%%-*}"
    expect_gate "$var=0.0.1 under pin $pin" "$chart" refuse \
        "ERROR: $var=0.0.1 is older than the $image this chart requires ($pin, " "$var=0.0.1"
    expect_gate "$var=$pin-rc.1 under pin $pin" "$chart" refuse "$var=$pin-rc.1 is older" "$var=$pin-rc.1"
    expect_gate "$var at its pin" "$chart" pass "" "$var=$pin"
    expect_gate "$var at v-prefixed pin" "$chart" pass "" "$var=v$pin"
    expect_gate "$var a patch above its pin" "$chart" pass "" "$var=$major.$minor.$((patch + 1))"
    expect_gate "$var a major above its pin" "$chart" pass "" "$var=$((major + 1)).0.0"
    for loose in latest e2e-5c4978a35 pr-301 sha-deadbeef; do
        expect_gate "$var=$loose (not a semver)" "$chart" pass "" "$var=$loose"
    done
done
# The gate names each refused override.
for var in OPERATOR_IMAGE_TAG CSI_IMAGE_TAG; do
    expect_gate "two old overrides name $var" "$chart" refuse "$var=0.0.1 is older" \
        OPERATOR_IMAGE_TAG=0.0.1 CSI_IMAGE_TAG=0.0.1
done
# The image the chart does not pin has no floor.
expect_gate "FUNCTION_IMAGE_TAG=v0.0.1 (no chart pin)" "$chart" pass "" FUNCTION_IMAGE_TAG=v0.0.1

# A pin that cannot be read must refuse: a chart that does not render, and a
# pin that is not a semver.
expect_gate "chart that does not render" "$scratch/no-such-chart" refuse "could not read the image tags" \
    OPERATOR_IMAGE_TAG=0.10.0
cp -r "$chart" "$scratch/latest-pin-chart"
sed -i '/repository: ghcr.io\/tj-smith47\/cfgd-operator$/,/^ *tag:/ s/^\( *\)tag: "[^"]*"/\1tag: "latest"/' \
    "$scratch/latest-pin-chart/values.yaml"
expect_gate "non-semver pin" "$scratch/latest-pin-chart" refuse "pins cfgd-operator at 'latest', which is not a semver" \
    OPERATOR_IMAGE_TAG=0.10.0
expect_gate "non-semver pin of an image with no override" "$scratch/latest-pin-chart" pass ""

# The reader takes every image under the ghcr.io/tj-smith47 owner, whatever its
# name, and nothing under another owner.
probe_chart() {
    local dir="$scratch/$1"; shift
    cp -r "$chart" "$dir"
    printf '%s\n' "$@" >"$dir/templates/zz-probe.yaml"
    printf '%s\n' "$dir"
}
# shellcheck disable=SC2016 # the inner script expands its own positional args
pins_of() {
    REGISTRY="$registry" CLI_SCRATCH="$scratch" bash -c 'source "$1/common/helpers.sh"; chart_image_pins "$2"' \
        _ "$e2e_root" "$1" 2>&1
}
fn_chart="$(probe_chart fn-chart 'probe:' '  image: "ghcr.io/tj-smith47/function-cfgd:0.12.0"')"
check "the reader lists a first-party image outside the cfgd names" \
    "$(pins_of "$fn_chart" | grep -c '^ghcr.io/tj-smith47/function-cfgd:0.12.0$')" 1
expect_gate "FUNCTION_IMAGE_TAG=v0.0.1 under a function-cfgd pin" "$fn_chart" refuse \
    "FUNCTION_IMAGE_TAG=v0.0.1 is older than the function-cfgd this chart requires (0.12.0, " FUNCTION_IMAGE_TAG=v0.0.1
expect_gate "FUNCTION_IMAGE_TAG=v0.12.0 at a function-cfgd pin" "$fn_chart" pass "" FUNCTION_IMAGE_TAG=v0.12.0
other_chart="$(probe_chart other-chart 'probe:' '  image: "ghcr.io/other-owner/thing:9.9.9"' \
    '  sidecar: "docker.io/tj-smith47/thing:9.9.9"' '  more:' '    image: docker.io/tj-smith47/thing:9.9.9')"
check "the reader leaves out images of another owner" "$(pins_of "$other_chart" | grep -c 'thing')" 0
expect_gate "images of another owner are not gated" "$other_chart" pass "" OPERATOR_IMAGE_TAG=0.10.0
new_chart="$(probe_chart new-chart 'probe:' '  image: "ghcr.io/tj-smith47/newthing:1.0.0"')"
expect_gate "a first-party image with no override variable" "$new_chart" refuse \
    "pins newthing, which has no override variable"
# A component whose template no longer renders under the reader's flags would
# take its pin out of the check unseen.
unrendered_chart="$(probe_chart unrendered-chart)"
rm "$unrendered_chart/templates/csi-daemonset.yaml"
check "the reader refuses a values.yaml repository the chart does not render" \
    "$(pins_of "$unrendered_chart" | grep -c 'names ghcr.io/tj-smith47/cfgd-csi, which the chart does not render')" 1
expect_gate "a values.yaml repository the chart does not render" "$unrendered_chart" refuse \
    "could not read the image tags"

# semver_below's ordering, pre-releases and its refusal of non-semvers.
# shellcheck disable=SC2016 # the inner script expands its own positional args
below() {
    local rc=0
    REGISTRY="$registry" CLI_SCRATCH="$scratch" bash -c 'source "$1/common/helpers.sh"; semver_below "$2" "$3"' \
        _ "$e2e_root" "$1" "$2" || rc=$?
    echo "$rc"
}
for row in "0.9.9 0.10.0 0" "0.10.0 0.10.0 1" "0.10.1 0.10.0 1" "1.0.0 0.99.99 1" "v0.10.0 0.10.1 0" \
    "0.10.1-rc.1 0.10.1 0" "0.10.1 0.10.1-rc.1 1" "0.10.1-rc.9 0.10.1-rc.10 0" "0.10.1-rc.10 0.10.1-rc.9 1" \
    "0.08.0 0.8.1 0" "latest 0.10.0 2" "0.10.0 latest 2" "0.10 0.10.0 2"; do
    read -r a b want <<<"$row"
    check "semver_below $a $b" "$(below "$a" "$b")" "$want"
done

# A reference composed by hand would ignore its override or push past the
# REGISTRY guard, so the helpers are the only place IMAGE_TAG, RELEASE_REGISTRY,
# a path under REGISTRY, a push (docker, xpkg, an image copy, buildx --push or
# a --cache-to export) may be spelled. grep exits 1 for no match and 2 for an
# error; an error must fail the check, since an empty result would otherwise
# read as a clean tree.
scan_strays() {
    local out rc=0 line helpers
    out="$(grep -rnE \
        --include='*.sh' --include='*.yaml' --include='*.yml' --include='*.tera' --include='*.json' \
        -e 'IMAGE_TAG|REGISTRY_PLACEHOLDER|RELEASE_REGISTRY' \
        -e '\$\{?REGISTRY[^}/"]*\}?"?/' \
        -e '(docker|xpkg) push|imagetools create|--push([^-a-z]|$)|--cache-to|type=registry' \
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
    while IFS= read -r helpers; do
        scan_helpers "$helpers"
    done < <(find "$1" -path '*/common/helpers.sh' -type f)
}

# helpers.sh is judged one function at a time: each spelling is allowed only in
# the functions that own it, so a second composer or push added there is
# reported like one in a suite. `-` is the file's top level, where IMAGE_TAG
# gets its default. e2e_push may spell a path under REGISTRY only as the case
# pattern its guard matches refs against: a match composes no reference, while
# any other spelling of that path there would.
scan_helpers() {
    awk -v file="$1" '
        function allowed(owners,    n, i, a) {
            n = split(owners, a, " ")
            for (i = 1; i <= n; i++) if (a[i] == fn) return 1
            return 0
        }
        BEGIN { fn = "-" }
        /^[A-Za-z_][A-Za-z_0-9]*\(\) \{/ { fn = $1; sub(/\(\).*/, "", fn) }
        /^}/ { fn = "-"; next }
        /^[[:space:]]*#/ { next }
        {
            bad = 0
            if ($0 ~ /REGISTRY_PLACEHOLDER/) bad = 1
            if ($0 ~ /RELEASE_REGISTRY/ && !allowed("e2e_image_repo e2e_image_copied_from_release")) bad = 1
            if ($0 ~ /IMAGE_TAG/ && $0 !~ /RELEASE_REGISTRY/ && !allowed("- e2e_image_override_var e2e_image_tag")) bad = 1
            if ($0 ~ /[$][{]?REGISTRY[^}\/"]*[}]?"?\// && !allowed("e2e_image_repo e2e_scratch_ref") \
                && !(fn == "e2e_push" && $0 ~ /^[[:space:]]*"[$]REGISTRY"\/[*]\)/)) bad = 1
            if ($0 ~ /%s\// && !allowed("e2e_image_repo e2e_scratch_ref")) bad = 1
            if ($0 ~ /(docker|xpkg) push|imagetools create|--push([^-a-z]|$)/ && !allowed("e2e_push")) bad = 1
            if ($0 ~ /--cache-to|type=registry/ && !allowed("buildcache_args")) bad = 1
            if (bad) print file ":" NR ":" $0
        }
    ' "$1"
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
    ["braced.sh"]='ref="${REGISTRY}/cfgd:x"'
    ["bare.yml"]='image: $REGISTRY/cfgd-operator:x'
    ["quoted.sh"]='ref="$REGISTRY"/cfgd-operator:x'
    ["defaulted.tera"]='image: ${REGISTRY:-r}/cfgd-csi:x'
    ["line-end.yaml"]='repository: $REGISTRY/function-cfgd'
    ["tag.json"]='{"tag": "$IMAGE_TAG"}'
    ["placeholder.yaml"]='image: REGISTRY_PLACEHOLDER/cfgd:x'
    ["module-artifact.sh"]='ref="${REGISTRY}/cfgd-e2e/module:v1"'
    ["release.sh"]='echo "$RELEASE_REGISTRY"'
    ["docker-push.sh"]='docker push "$ref"'
    ["xpkg-push.sh"]='crossplane xpkg push "$ref" -f x.xpkg'
    ["copy.sh"]='docker buildx imagetools create --tag "$ref" "$src"'
    ["cache-to.sh"]='docker buildx build --cache-to "type=registry,ref=$ref" .'
    ["buildx-push.sh"]='docker buildx build --push -t "$ref" .'
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

# A helpers.sh is scanned per function: the composers and e2e_push keep their
# spellings, and the same spellings in any other function are reported.
mkdir -p "$fixtures/common"
# shellcheck disable=SC2016 # the fixture holds the literal text helpers.sh would contain
printf '%s\n' \
    'e2e_image_repo() {' \
    '    printf "%s/%s\n" "$RELEASE_REGISTRY" "$1"' \
    '}' \
    'e2e_push() {' \
    '    case "$2" in' \
    '        "$REGISTRY"/*) ;;' \
    '    esac' \
    '    docker push "$2" "$REGISTRY/extra:x"' \
    '}' \
    'buildcache_args() {' \
    '    echo "--cache-to" "type=registry,ref=$(e2e_scratch_ref x)"' \
    '}' \
    'rogue_repo() {' \
    '    printf "%s/%s\n" "$RELEASE_REGISTRY" "$1"' \
    '}' \
    'rogue_path() {' \
    '    echo "$REGISTRY/cfgd:x"' \
    '}' \
    'rogue_push() {' \
    '    docker push "$1"' \
    '}' \
    'rogue_cache() {' \
    '    echo "--cache-to type=registry,ref=$1"' \
    '}' >"$fixtures/common/helpers.sh"
if found="$(scan_helpers "$fixtures/common/helpers.sh")"; then
    check "helpers scan reports each rogue function, the path e2e_push composes, and no owner" \
        "$(cut -d: -f2 <<<"$found" | tr '\n' ' ')" "8 14 17 20 23 "
else
    check "helpers scan runs" failed ran
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
