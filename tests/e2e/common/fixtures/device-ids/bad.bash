# shellcheck shell=bash
gateway_device GOOD_ID "e2e-good-${E2E_RUN_ID}"
PLAIN_ID="e2e-plain-$(date +%s)"
gateway_device REASSIGNED_ID "e2e-reassigned-${E2E_RUN_ID}"
REASSIGNED_ID="e2e-reassigned-again"
cfgd checkin --device-id literal-device
cfgd checkin --device-id "$PLAIN_ID"
cfgd checkin --device-id "$REASSIGNED_ID"
cfgd checkin --device-id "${GOOD_ID}-suffix"
curl -d "{\"deviceId\":\"e2e-literal\"}" "$URL"
curl -d "{\"deviceId\":\"$1\"}" "$URL"
relay_post "$TOKEN" e2e-literal host "$BODY_FILE"
relay_once "$TOKEN" "$PLAIN_ID" host "$BODY_FILE"
curl -X POST "$URL/api/v1/devices/e2e-literal-drift/drift"
cfgd checkin --server-url "$URL" --no-color
dp_cfgd enroll \
    --server-url "$URL" --token t
cfgd --config "$SERVER_CONF" daemon --no-color &
gateway_device READ_ID "e2e-read-${E2E_RUN_ID}"
read -r READ_ID < "$FILE"
gateway_device MAP_ID "e2e-map-${E2E_RUN_ID}"
mapfile -t MAP_ID < "$FILE"
gateway_device PRINTF_ID "e2e-printf-${E2E_RUN_ID}"
printf -v PRINTF_ID '%s' x
gateway_device FOR_ID "e2e-for-${E2E_RUN_ID}"
for FOR_ID in a b; do :; done
gateway_device LOCAL_ID "e2e-local-${E2E_RUN_ID}"
local LOCAL_ID
cfgd checkin --device-id "$READ_ID"
cfgd checkin --device-id "$MAP_ID"
cfgd checkin --device-id "$PRINTF_ID"
cfgd checkin --device-id "$FOR_ID"
cfgd checkin --device-id "$LOCAL_ID"
