# shellcheck shell=bash
# The one reader of the image tags chart/cfgd pins, shared by
# `task chart:tags:check` and the e2e setup's override gate.

# chart_image_pins <chart-dir>: print each first-party image the chart renders
# with every component on, one `ghcr.io/tj-smith47/<image>:<tag>` line apiece.
# The rendered reference is read so a tag values.yaml leaves empty resolves to
# appVersion the way the chart itself resolves it. Returns 1 when the chart does
# not render or renders no first-party image.
chart_image_pins() {
    local rendered refs
    rendered="$(helm template cfgd "${1:?chart directory}" --set agent.enabled=true --set csiDriver.enabled=true)" || return 1
    refs="$(sed -n 's/^ *image: *"\{0,1\}\(ghcr\.io\/tj-smith47\/cfgd[^" ]*\)"\{0,1\} *$/\1/p' <<<"$rendered" | sort -u)"
    [ -n "$refs" ] || return 1
    printf '%s\n' "$refs"
}
