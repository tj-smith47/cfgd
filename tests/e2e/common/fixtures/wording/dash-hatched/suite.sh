# shellcheck shell=bash
if grep -q "Sources — none" <<<"$OUT"; then :; fi  # wording-ok: cfgd status prints this em dash and the case greps it verbatim
