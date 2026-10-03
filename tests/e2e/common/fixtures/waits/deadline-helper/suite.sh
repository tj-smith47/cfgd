# shellcheck shell=bash
# A suite script that waits through the helper.
# shellcheck source=/dev/null
source "$(dirname "$0")/common/helpers.sh"
wait_for_thing
one_liner
