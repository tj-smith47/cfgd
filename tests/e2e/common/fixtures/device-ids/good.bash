# shellcheck shell=bash
gateway_device GOOD_ID "e2e-good-${E2E_RUN_ID}"
gateway_device OTHER_ID "e2e-other-$(date +%s)-${E2E_RUN_ID}"
cfgd checkin --server-url "$URL" --device-id "$GOOD_ID" --no-color
cfgd checkin --device-id="${OTHER_ID}"
curl -d "{\"token\":\"t\",\"deviceId\":\"${GOOD_ID}\",\"os\":\"linux\"}" "$URL"
curl -d "{\"deviceId\": \"$OTHER_ID\"}" "$URL"
grep -qF "\"deviceId\":\"${GOOD_ID}\"" "$LOG"
jq -r '.deviceId // empty' <<<"$BODY"
# a comment naming --device-id literal-id is prose
relay_once() {
    curl -d "{\"token\":\"$1\",\"deviceId\":\"$2\"}" "$URL"
}
relay_post() {
    wait_until 30 2 "the relay to admit $2" relay_once "$@"
}
relay_post "$TOKEN" "$GOOD_ID" host "$BODY_FILE"
