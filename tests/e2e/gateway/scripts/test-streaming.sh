# shellcheck shell=bash
# Gateway SSE streaming test (GW-21).
# Sourced by run-all.sh — no shebang, no set, no source, no traps, no print_summary.

# =================================================================
# GW-21: SSE event stream
# =================================================================
begin_test "GW-21: SSE event stream"

if [ -z "${DEVICE_API_KEY:-}" ]; then
    skip_test "GW-21" "No device enrolled (GW-02 may have failed), so no drift report can raise an event"
else
    GW21_TMPFILE=$(mktemp "$GW_SCRATCH/gw21-sse.XXXXXX")

    curl -sN "$GW_URL/api/v1/events/stream" \
        -H "$(gw_admin_auth_header)" \
        -H "Accept: text/event-stream" \
        > "$GW21_TMPFILE" 2>/dev/null &
    GW21_PID=$!
    echo "  SSE listener started (PID $GW21_PID)"

    # The stream is a broadcast with no replay, so the listener must be
    # subscribed before the event is raised.
    sleep 2

    # A drift report is one of the calls that broadcasts a fleet event;
    # creating a token or reading state raises none.
    GW21_TRIGGER_CODE=$(curl -s -o /dev/null -w "%{http_code}" \
        -X POST "${GW_URL}/api/v1/devices/${GW_DEVICE_ID}/drift" \
        -H "Authorization: Bearer ${DEVICE_API_KEY}" \
        -H "Content-Type: application/json" \
        -d '{"details":[{"field":"packages.curl","expected":"8.5.0","actual":"8.4.0"}]}' \
        2>/dev/null || echo "000")
    echo "  Drift report HTTP status: $GW21_TRIGGER_CODE"

    GW21_TRIES=0
    while [ "$GW21_TRIES" -lt 20 ] && ! grep -qx 'event: drift' "$GW21_TMPFILE"; do
        sleep 0.5
        GW21_TRIES=$((GW21_TRIES + 1))
    done

    kill "$GW21_PID" 2>/dev/null || true
    wait "$GW21_PID" 2>/dev/null || true

    GW21_OUTPUT=$(cat "$GW21_TMPFILE" 2>/dev/null || echo "")
    rm -f "$GW21_TMPFILE"
    echo "  SSE output (first 500 chars):"
    echo "$GW21_OUTPUT" | head -c 500 | sed 's/^/    /'
    echo ""

    if [ "$GW21_TRIGGER_CODE" != "201" ]; then
        fail_test "GW-21" "Drift report returned HTTP $GW21_TRIGGER_CODE, expected 201, so no event was raised"
    elif ! grep -qx 'event: drift' <<<"$GW21_OUTPUT"; then
        fail_test "GW-21" "No 'event: drift' arrived on the stream within 10s of the drift report"
    elif ! grep -E '^data:' <<<"$GW21_OUTPUT" | grep -qF "\"deviceId\":\"${GW_DEVICE_ID}\""; then
        fail_test "GW-21" "The drift event's data does not name device ${GW_DEVICE_ID}"
    else
        pass_test "GW-21"
    fi
fi
