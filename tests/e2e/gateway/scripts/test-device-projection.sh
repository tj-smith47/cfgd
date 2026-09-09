# Gateway device-projection tests (GW-32 through GW-36).
# Sourced by run-all.sh — no shebang, no set, no source, no traps, no print_summary.
#
# The only case family that drives a REAL cfgd binary against the gateway: every
# other gateway case speaks to the API with curl, so the halves the device owns
# — enrolling, recording a check-in's `backupSchedules` answer, re-arming the
# daemon's timer off it and rendering the result — were never exercised end to
# end on a cluster.

DP_ROOT="$GW_SCRATCH/device-projection"
DP_HOME="$DP_ROOT/home"
DP_CFG="$DP_ROOT/cfg"
DP_STATE="$DP_ROOT/state"
DP_DATA="$DP_ROOT/data"
DP_CONF="$DP_CFG/cfgd.yaml"
DP_PROFILE="$DP_CFG/profiles/e2e.yaml"
DP_UNIT="e2e-db"
DP_MC_NAME="e2e-mc-projection-${E2E_RUN_ID}"
DP_BP_NAME="e2e-bp-projection-${E2E_RUN_ID}"
DP_SELECTOR_VALUE="projection-${E2E_RUN_ID}"
# gethostname(2) is what the device reports as both its hostname and its
# device id, and `spec.hostname` is the only key the gateway resolves a
# MachineConfig by. GW-32 asserts the binary agrees with this reading.
DP_HOSTNAME="$(uname -n)"
DP_DAEMON_PID=""
DP_DAEMON_LOG="$DP_ROOT/daemon.log"

mkdir -p "$DP_HOME" "$DP_CFG/profiles" "$DP_STATE" "$DP_DATA"
echo "device-projection-fixture" > "$DP_DATA/db.sql"

# Every root the binary resolves is pinned inside the run's scratch tree: the
# runner dogfoods cfgd, and a device credential or state row written to the real
# HOME would be this suite editing the operator's own machine.
dp_cfgd() {
    env HOME="$DP_HOME" \
        XDG_CONFIG_HOME="$DP_HOME/.config" \
        XDG_STATE_HOME="$DP_HOME/.local/state" \
        XDG_CACHE_HOME="$DP_HOME/.cache" \
        CFGD_STATE_DIR="$DP_STATE" \
        CFGD_CACHE_DIR="$DP_HOME/.cache/cfgd" \
        CFGD_DAEMON_IPC_PATH="$DP_ROOT/cfgd.sock" \
        "$CFGD_BIN" --config "$DP_CONF" --color never "$@"
}

# One unit's field out of `cfgd backup list -o json`; "absent" for a key the
# payload omits, which is itself an assertable value (a cluster answer that
# changed nothing, and a unit pinned Local, both omit the effective slots).
dp_field() {
    dp_cfgd -o json backup list 2>/dev/null |
        jq -r --arg f "$1" '.[] | select(.name=="'"$DP_UNIT"'") | .[$f] // "absent"' 2>/dev/null
}

# Poll that field until it reads `want`. The suite's own waiting shape — a
# bounded retry inside the script, the way wait_for_pod waits on a pod — because
# what is being waited on is a controller requeue plus a daemon tick.
dp_wait_field() {
    local field="$1" want="$2" timeout="${3:-180}" got=""
    local deadline=$((SECONDS + timeout))
    while [ $SECONDS -lt $deadline ]; do
        got=$(dp_field "$field")
        if [ "$got" = "$want" ]; then
            echo "$got"
            return 0
        fi
        sleep 3
    done
    echo "$got"
    return 1
}

# Write the device's profile with the unit's declared cadence and a chosen
# `scheduleOwner`, so the pin GW-35 applies is the same authoring step a person
# would make.
dp_write_profile() {
    local owner="$1"
    cat > "$DP_PROFILE" <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: e2e
spec:
  backups:
    - name: ${DP_UNIT}
      source: ${DP_DATA}/db.sql
      schedule: "0 3 * * *"
      retention: 3
      scheduleOwner: ${owner}
EOF
}

cat > "$DP_CONF" <<EOF
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: device-projection-e2e
spec:
  profile: e2e
  origin:
    - type: Server
      url: "${GW_URL}"
  daemon:
    reconcile:
      interval: 10s
EOF
dp_write_profile Cluster

ensure_cfgd_binary

# =================================================================
# GW-32: A real cfgd binary enrolls with the gateway
# =================================================================
begin_test "GW-32: cfgd enroll stores a credential and registers the device"

DP_TOKEN=$(gw_create_bootstrap_token "dp-user")

if [ -z "$DP_TOKEN" ]; then
    skip_test "GW-32" "No bootstrap token available (admin token API may have failed)"
else
    GW32_PASS=true
    GW32_OUT=$(dp_cfgd -o json enroll --server-url "$GW_URL" --token "$DP_TOKEN" 2>&1)
    GW32_RC=$?
    echo "  enroll rc=$GW32_RC"
    echo "$GW32_OUT" | head -c 400 | sed 's/^/    /'
    echo ""

    GW32_CRED="$DP_STATE/device-credential.json"
    GW32_CRED_PRESENT=absent
    [ -f "$GW32_CRED" ] && GW32_CRED_PRESENT=present
    GW32_CRED_URL=$(jq -r '.serverUrl // empty' "$GW32_CRED" 2>/dev/null || echo "")
    GW32_DEVICE_ID=$(echo "$GW32_OUT" | jq -r '.deviceId // empty' 2>/dev/null || echo "")

    GW32_LISTED=$(curl -sf "$GW_URL/api/v1/devices/$DP_HOSTNAME" \
        -H "$(gw_admin_auth_header)" 2>/dev/null | jq -r '.hostname // empty' 2>/dev/null)
    GW32_LIST_RC=$?

    echo "  credential:      $GW32_CRED_PRESENT ($GW32_CRED)"
    echo "  credential url:  ${GW32_CRED_URL:-none}"
    echo "  reported id:     ${GW32_DEVICE_ID:-none} (uname -n: $DP_HOSTNAME)"
    echo "  gateway hostname: ${GW32_LISTED:-none} (rc=$GW32_LIST_RC)"

    assert_exit_code "$GW32_RC" 0 || GW32_PASS=false
    assert_equals "$GW32_CRED_PRESENT" "present" || GW32_PASS=false
    assert_equals "$GW32_CRED_URL" "$GW_URL" || GW32_PASS=false
    # The MachineConfig below is keyed on `uname -n`; if the binary ever reports
    # a different name the projection would silently never land, so the two
    # readings are pinned equal here rather than assumed.
    assert_equals "$GW32_DEVICE_ID" "$DP_HOSTNAME" || GW32_PASS=false
    assert_exit_code "$GW32_LIST_RC" 0 || GW32_PASS=false
    assert_equals "$GW32_LISTED" "$DP_HOSTNAME" || GW32_PASS=false

    if [ "$GW32_PASS" = true ]; then
        pass_test "GW-32"
    else
        fail_test "GW-32" "cfgd enroll did not leave a usable credential for $DP_HOSTNAME"
    fi
fi

# =================================================================
# GW-33: A BackupPolicy's cadence reaches the device through a check-in
# =================================================================
begin_test "GW-33: a check-in lands the cluster's backup schedule on the device"

if [ ! -f "$DP_STATE/device-credential.json" ]; then
    skip_test "GW-33" "Device not enrolled (GW-32 may have failed)"
else
    GW33_PASS=true

    GW33_MC_ERR=$(kubectl apply -f - 2>&1 >/dev/null <<EOF
apiVersion: cfgd.io/v1alpha1
kind: MachineConfig
metadata:
  name: ${DP_MC_NAME}
  namespace: ${E2E_NAMESPACE}
  labels:
    cfgd.io/projection: "${DP_SELECTOR_VALUE}"
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  hostname: "${DP_HOSTNAME}"
  profile: "e2e"
EOF
)
    GW33_MC_RC=$?

    GW33_BP_ERR=$(kubectl apply -f - 2>&1 >/dev/null <<EOF
apiVersion: cfgd.io/v1alpha1
kind: BackupPolicy
metadata:
  name: ${DP_BP_NAME}
  namespace: ${E2E_NAMESPACE}
  labels:
    ${E2E_RUN_LABEL_YAML}
    ${E2E_JOB_LABEL_YAML}
spec:
  selector:
    matchLabels:
      cfgd.io/projection: "${DP_SELECTOR_VALUE}"
  units:
    - name: ${DP_UNIT}
      schedule: "*/5 * * * *"
      retention: 7
EOF
)
    GW33_BP_RC=$?
    echo "  apply rc: mc=${GW33_MC_RC} bp=${GW33_BP_RC} ${GW33_MC_ERR}${GW33_BP_ERR}"

    # The gateway answers a check-in from the policy's STATUS, so the row for
    # this machine has to exist before the device asks; a bare sleep here is
    # what would make the assertion below flaky.
    GW33_ROW_SCHEDULE=$(wait_for_k8s_field backuppolicy "$DP_BP_NAME" "$E2E_NAMESPACE" \
        "{.status.units[?(@.hostname==\"${DP_HOSTNAME}\")].schedule}" "*/5 * * * *" 120)
    GW33_ROW_RC=$?
    GW33_ROW_OWNER=$(kubectl get backuppolicy "$DP_BP_NAME" -n "$E2E_NAMESPACE" \
        -o jsonpath="{.status.units[?(@.hostname==\"${DP_HOSTNAME}\")].owner}" 2>/dev/null || echo "")
    echo "  policy row: schedule='${GW33_ROW_SCHEDULE}' owner='${GW33_ROW_OWNER}' (rc=${GW33_ROW_RC})"

    GW33_CHECKIN=$(dp_cfgd checkin --server-url "$GW_URL" 2>&1)
    GW33_CHECKIN_RC=$?
    echo "  checkin rc=$GW33_CHECKIN_RC"
    printf '%s\n' "$GW33_CHECKIN" | strip_sgr | head -12 | sed 's/^/    /'

    GW33_OWNER=$(dp_field scheduleOwner)
    GW33_EFF_SCHEDULE=$(dp_field effectiveSchedule)
    GW33_EFF_RETENTION=$(dp_field effectiveRetention)
    GW33_DECLARED=$(dp_field schedule)
    GW33_HUMAN=$(dp_cfgd backup list 2>&1)

    echo "  scheduleOwner:      $GW33_OWNER"
    echo "  schedule:           $GW33_DECLARED"
    echo "  effectiveSchedule:  $GW33_EFF_SCHEDULE"
    echo "  effectiveRetention: $GW33_EFF_RETENTION"
    printf '%s\n' "$GW33_HUMAN" | strip_sgr | sed 's/^/    /'

    assert_exit_code "$GW33_MC_RC" 0 || GW33_PASS=false
    assert_exit_code "$GW33_BP_RC" 0 || GW33_PASS=false
    assert_exit_code "$GW33_ROW_RC" 0 || GW33_PASS=false
    assert_equals "$GW33_ROW_OWNER" "cluster" || GW33_PASS=false
    assert_exit_code "$GW33_CHECKIN_RC" 0 || GW33_PASS=false
    assert_equals "$GW33_OWNER" "cluster" || GW33_PASS=false
    assert_equals "$GW33_EFF_SCHEDULE" "*/5 * * * *" || GW33_PASS=false
    assert_equals "$GW33_EFF_RETENTION" "7" || GW33_PASS=false
    # The declared cadence survives the projection: both halves are what let a
    # reader see the machine asked for one thing and the cluster set another.
    assert_equals "$GW33_DECLARED" "0 3 * * *" || GW33_PASS=false
    assert_contains "$GW33_HUMAN" "projected" || GW33_PASS=false

    if [ "$GW33_PASS" = true ]; then
        pass_test "GW-33"
    else
        fail_test "GW-33" "The BackupPolicy's cadence did not reach the device"
    fi
fi

# =================================================================
# GW-34: The projection is a row in the device's own state store
# =================================================================
begin_test "GW-34: the check-in's answer is recorded in cluster_backup_schedules"

if ! command -v sqlite3 >/dev/null 2>&1; then
    skip_test "GW-34" "sqlite3 not available to read the device state store"
elif [ ! -f "$DP_STATE/state.db" ]; then
    skip_test "GW-34" "No device state store (GW-33 may have failed)"
else
    GW34_ROW=$(sqlite3 "$DP_STATE/state.db" \
        "SELECT name||'|'||schedule||'|'||retention FROM cluster_backup_schedules;" 2>&1)
    GW34_RC=$?
    echo "  cluster_backup_schedules: ${GW34_ROW:-none} (rc=${GW34_RC})"

    GW34_PASS=true
    assert_exit_code "$GW34_RC" 0 || GW34_PASS=false
    assert_equals "$GW34_ROW" "${DP_UNIT}|*/5 * * * *|7" || GW34_PASS=false

    if [ "$GW34_PASS" = true ]; then
        pass_test "GW-34"
    else
        fail_test "GW-34" "The recorded projection is not the one the gateway answered with"
    fi
fi

# =================================================================
# GW-35: The daemon takes a changed cadence and a local pin without a restart
# =================================================================
begin_test "GW-35: the daemon re-arms on a policy change and yields to a local pin"

if [ ! -f "$DP_STATE/device-credential.json" ]; then
    skip_test "GW-35" "Device not enrolled (GW-32 may have failed)"
else
    GW35_PASS=true
    rm -f "$DP_ROOT/cfgd.sock"
    dp_cfgd daemon > "$DP_DAEMON_LOG" 2>&1 &
    DP_DAEMON_PID=$!
    GW35_READY=timeout
    GW35_DEADLINE=$((SECONDS + 60))
    while [ $SECONDS -lt $GW35_DEADLINE ]; do
        if [ -S "$DP_ROOT/cfgd.sock" ]; then
            GW35_READY=ready
            break
        fi
        kill -0 "$DP_DAEMON_PID" 2>/dev/null || { GW35_READY=exited; break; }
        sleep 1
    done
    echo "  daemon pid=$DP_DAEMON_PID state=$GW35_READY"

    GW35_NEXT_BEFORE=$(dp_field nextRunAt)

    GW35_PATCH_ERR=$(kubectl patch backuppolicy "$DP_BP_NAME" -n "$E2E_NAMESPACE" \
        --type=merge \
        -p "{\"spec\":{\"units\":[{\"name\":\"${DP_UNIT}\",\"schedule\":\"*/7 * * * *\",\"retention\":7}]}}" \
        2>&1 >/dev/null)
    GW35_PATCH_RC=$?
    echo "  policy patch rc=${GW35_PATCH_RC} ${GW35_PATCH_ERR}"

    GW35_ROW_SCHEDULE=$(wait_for_k8s_field backuppolicy "$DP_BP_NAME" "$E2E_NAMESPACE" \
        "{.status.units[?(@.hostname==\"${DP_HOSTNAME}\")].schedule}" "*/7 * * * *" 120)
    GW35_ROW_RC=$?
    echo "  policy row schedule='${GW35_ROW_SCHEDULE}' (rc=${GW35_ROW_RC})"

    # Nothing here re-runs `cfgd checkin`: the only thing that can move the
    # device's recorded cadence now is the daemon's own tick.
    GW35_AFTER=$(dp_wait_field effectiveSchedule "*/7 * * * *" 180)
    GW35_AFTER_RC=$?
    GW35_NEXT_AFTER=$(dp_field nextRunAt)
    # `*/5` and `*/7` share a next occurrence for four minutes of every hour, so
    # "the stamp moved" is not a fact this test can hold at every wall clock.
    # What it can hold is that the re-armed stamp is one the NEW cadence
    # produces: a minute that is a multiple of seven.
    GW35_NEXT_MINUTE=$(printf '%s' "$GW35_NEXT_AFTER" | sed -n 's/^.*T[0-9][0-9]:\([0-9][0-9]\):.*$/\1/p')
    GW35_NEXT_MOD=none
    [ -n "$GW35_NEXT_MINUTE" ] && GW35_NEXT_MOD=$((10#$GW35_NEXT_MINUTE % 7))
    echo "  nextRunAt before=${GW35_NEXT_BEFORE} after=${GW35_NEXT_AFTER} (minute ${GW35_NEXT_MINUTE:-none} mod 7 = ${GW35_NEXT_MOD})"

    # The machine takes its unit back. The daemon re-reads the profile on its
    # next tick, reports `local`, the controller retires the policy's claim and
    # the following tick is answered with nothing to project.
    dp_write_profile Local
    GW35_PIN_RC=$?
    echo "  profile pinned Local (rc=${GW35_PIN_RC})"

    GW35_LOCAL_ROW=$(wait_for_k8s_field backuppolicy "$DP_BP_NAME" "$E2E_NAMESPACE" \
        "{.status.units[?(@.hostname==\"${DP_HOSTNAME}\")].owner}" "local" 180)
    GW35_LOCAL_ROW_RC=$?
    echo "  policy row owner='${GW35_LOCAL_ROW}' (rc=${GW35_LOCAL_ROW_RC})"

    GW35_LOCAL_EFF=$(dp_wait_field effectiveSchedule "absent" 180)
    GW35_LOCAL_EFF_RC=$?
    GW35_LOCAL_OWNER=$(dp_field scheduleOwner)
    GW35_LOCAL_HUMAN=$(dp_cfgd backup list 2>&1)
    echo "  effectiveSchedule=${GW35_LOCAL_EFF} scheduleOwner=${GW35_LOCAL_OWNER}"
    printf '%s\n' "$GW35_LOCAL_HUMAN" | strip_sgr | sed 's/^/    /'

    kill -TERM "$DP_DAEMON_PID" 2>/dev/null
    (sleep 5; kill -KILL "$DP_DAEMON_PID" 2>/dev/null || true) &
    GW35_WATCHDOG=$!
    wait "$DP_DAEMON_PID" 2>/dev/null
    kill -KILL "$GW35_WATCHDOG" 2>/dev/null || true
    wait "$GW35_WATCHDOG" 2>/dev/null || true
    GW35_STOPPED=running
    kill -0 "$DP_DAEMON_PID" 2>/dev/null || GW35_STOPPED=stopped
    DP_DAEMON_PID=""
    echo "  daemon after SIGTERM: $GW35_STOPPED"

    assert_equals "$GW35_READY" "ready" || GW35_PASS=false
    assert_exit_code "$GW35_PATCH_RC" 0 || GW35_PASS=false
    assert_exit_code "$GW35_ROW_RC" 0 || GW35_PASS=false
    assert_exit_code "$GW35_AFTER_RC" 0 || GW35_PASS=false
    assert_equals "$GW35_AFTER" "*/7 * * * *" || GW35_PASS=false
    assert_equals "$GW35_NEXT_MOD" "0" || GW35_PASS=false
    assert_exit_code "$GW35_PIN_RC" 0 || GW35_PASS=false
    assert_exit_code "$GW35_LOCAL_ROW_RC" 0 || GW35_PASS=false
    assert_exit_code "$GW35_LOCAL_EFF_RC" 0 || GW35_PASS=false
    assert_equals "$GW35_LOCAL_EFF" "absent" || GW35_PASS=false
    assert_equals "$GW35_LOCAL_OWNER" "local" || GW35_PASS=false
    assert_contains "$GW35_LOCAL_HUMAN" "local" || GW35_PASS=false
    assert_not_contains "$GW35_LOCAL_HUMAN" "projected" || GW35_PASS=false
    assert_equals "$GW35_STOPPED" "stopped" || GW35_PASS=false

    if [ "$GW35_PASS" = true ]; then
        pass_test "GW-35"
    else
        echo "  daemon log tail:"
        tail -30 "$DP_DAEMON_LOG" 2>/dev/null | strip_sgr | sed 's/^/    /'
        fail_test "GW-35" "The daemon did not follow the cluster's cadence, or did not yield to the local pin"
    fi
fi

# =================================================================
# GW-36: This run leaves nothing behind
# =================================================================
begin_test "GW-36: the device-projection objects and daemon are gone"

kubectl delete backuppolicy "$DP_BP_NAME" -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null
kubectl delete machineconfig "$DP_MC_NAME" -n "$E2E_NAMESPACE" --ignore-not-found 2>/dev/null

GW36_LEFT=$(kubectl get backuppolicy,machineconfig -n "$E2E_NAMESPACE" -o name 2>/dev/null |
    grep -F "projection-${E2E_RUN_ID}" || true)
GW36_DAEMON=gone
[ -n "${DP_DAEMON_PID:-}" ] && kill -0 "$DP_DAEMON_PID" 2>/dev/null && GW36_DAEMON=running
echo "  remaining objects: ${GW36_LEFT:-none}"
echo "  daemon: $GW36_DAEMON"

GW36_PASS=true
assert_equals "$GW36_LEFT" "" || GW36_PASS=false
assert_equals "$GW36_DAEMON" "gone" || GW36_PASS=false

if [ "$GW36_PASS" = true ]; then
    pass_test "GW-36"
else
    fail_test "GW-36" "This run left objects or a daemon behind"
fi
