# shellcheck shell=bash
# Gateway SSE streaming test (GW-21).
# Sourced by run-all.sh: no shebang, no set, no source, no traps, no print_summary.

# =================================================================
# GW-21: SSE event stream
# =================================================================
begin_test "GW-21: SSE event stream"

if [ -z "${DEVICE_API_KEY:-}" ]; then
    skip_test "GW-21" "No device enrolled (GW-02 may have failed), so no drift report can raise an event"
else
    GW21_TMPFILE=$(mktemp "$GW_SCRATCH/gw21-sse.XXXXXX")
    GW21_HEADERS=$(mktemp "$GW_SCRATCH/gw21-sse-headers.XXXXXX")

    curl -sN -D "$GW21_HEADERS" "$GW_URL/api/v1/events/stream" \
        -H "$(gw_admin_auth_header)" \
        -H "Accept: text/event-stream" \
        > "$GW21_TMPFILE" 2>/dev/null &
    GW21_PID=$!
    echo "  SSE listener started (PID $GW21_PID)"

    # The stream is a broadcast with no replay, so the listener must be
    # subscribed before the event is raised. The handler subscribes before it
    # answers, so the response's status line means the subscription exists.
    wait_until 10 0.2 "the SSE stream's response headers" \
        grep -qE '^HTTP/[0-9.]+ 200' "$GW21_HEADERS" || true

    # A drift report is one of the calls that broadcasts a fleet event;
    # creating a token or reading state raises none.
    GW21_TRIGGER_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
        -X POST "${GW_URL}/api/v1/devices/${GW_DEVICE_ID}/drift" \
        -H "Authorization: Bearer ${DEVICE_API_KEY}" \
        -H "Content-Type: application/json" \
        -d '{"details":[{"field":"packages.curl","expected":"8.5.0","actual":"8.4.0"}]}' \
        2>/dev/null || echo "000")
    echo "  Drift report HTTP status: $GW21_TRIGGER_CODE"

    # The gateway is shared across runs, so another run's drift event can
    # arrive first; the wait is for this device's.
    wait_until 10 0.5 "an SSE event for device ${GW_DEVICE_ID}" \
        grep -qF "\"deviceId\":\"${GW_DEVICE_ID}\"" "$GW21_TMPFILE" || true

    kill "$GW21_PID" 2>/dev/null || true
    wait "$GW21_PID" 2>/dev/null || true

    GW21_OUTPUT=$(cat "$GW21_TMPFILE" 2>/dev/null || echo "")
    rm -f "$GW21_TMPFILE" "$GW21_HEADERS"
    echo "  SSE output (first 500 chars):"
    echo "$GW21_OUTPUT" | head -c 500 | sed 's/^/    /'
    echo ""

    if [ "$GW21_TRIGGER_CODE" != "201" ]; then
        fail_test "GW-21" "Drift report returned HTTP $GW21_TRIGGER_CODE, expected 201, so no event was raised"
    elif ! grep -E '^data:' <<<"$GW21_OUTPUT" | grep -qF "\"deviceId\":\"${GW_DEVICE_ID}\""; then
        fail_test "GW-21" "No event for device ${GW_DEVICE_ID} arrived on the stream within 10s of its drift report"
    elif ! grep -B1 -F "\"deviceId\":\"${GW_DEVICE_ID}\"" <<<"$GW21_OUTPUT" | grep -qx 'event: drift'; then
        fail_test "GW-21" "The event for device ${GW_DEVICE_ID} is not an 'event: drift'"
    else
        pass_test "GW-21"
    fi
fi
