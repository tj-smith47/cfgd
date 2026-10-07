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
