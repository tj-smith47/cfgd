# shellcheck shell=bash
# The one reader of the image tags chart/cfgd pins, shared by
# `task chart:tags:check` and the e2e setup's override gate.

# chart_image_pins <chart-dir>: print each first-party image (any repository
# under ghcr.io/tj-smith47) the chart renders with every component on, one
# `ghcr.io/tj-smith47/<image>:<tag>` line apiece. The rendered reference is read
# so a tag values.yaml leaves empty resolves to appVersion the way the chart
# itself resolves it, and every first-party `repository:` in values.yaml must be
# among them, since an image the render leaves out would go unchecked. Returns 1
# when the chart does not render, renders no first-party image, or leaves out a
# repository values.yaml names.
chart_image_pins() {
    local chart="${1:?chart directory}" rendered refs repos repo missing=0
    local first_party='ghcr\.io\/tj-smith47\/[^" ]*'
    rendered="$(helm template cfgd "$chart" --set agent.enabled=true --set csiDriver.enabled=true)" || return 1
    refs="$(sed -n "s/^ *image: *\"\{0,1\}\($first_party\)\"\{0,1\} *\$/\1/p" <<<"$rendered" | sort -u)"
    [ -n "$refs" ] || return 1
    repos="$(sed -n "s/^ *repository: *\"\{0,1\}\($first_party\)\"\{0,1\} *\$/\1/p" "$chart/values.yaml" | sort -u)" || return 1
    for repo in $repos; do
        if ! grep -q "^$repo:" <<<"$refs"; then
            echo "chart_image_pins: $chart/values.yaml names $repo, which the chart does not render with every component on" >&2
            missing=1
        fi
    done
    [ "$missing" -eq 0 ] || return 1
    printf '%s\n' "$refs"
}
