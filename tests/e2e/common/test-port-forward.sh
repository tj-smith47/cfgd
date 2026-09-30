#!/usr/bin/env bash
# Checks the port-forward and scrape helpers in helpers.sh without a cluster:
# port_forward returns only once the local port accepts a connection and keeps
# kubectl's output for a failure, the scrape helpers keep a response's status,
# content type and body, no suite starts a port-forward of its own, and a
# runner installs its EXIT trap before a setup that starts one.
#
# Usage: tests/e2e/common/test-port-forward.sh
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
e2e_root="$(dirname "$here")"
scratch="$(mktemp -d)"
trap 'rm -rf "$scratch"' EXIT
failures=0

pass() { echo "PASS  $1"; }
fail() {
    echo "FAIL  $1"
    failures=$((failures + 1))
}

# A stand-in kubectl on PATH. FAKE_PF_MODE picks how its port-forward behaves:
# listen binds the local port, stubborn does too but ignores SIGTERM, exit
# fails the way a missing service does, and silent stays up without binding.
mkdir -p "$scratch/bin"
cat > "$scratch/bin/kubectl" <<'FAKE'
#!/usr/bin/env bash
local_port="${*: -1}"
local_port="${local_port%%:*}"
case "$FAKE_PF_MODE" in
    listen | stubborn)
        [ "$FAKE_PF_MODE" = listen ] || trap '' TERM
        echo "Forwarding from 127.0.0.1:$local_port -> 1"
        exec python3 -c '
import socket, sys
s = socket.socket()
s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
s.bind(("127.0.0.1", int(sys.argv[1])))
s.listen()
while True:
    s.accept()[0].close()
' "$local_port"
        ;;
    exit)
        echo 'error: services "cfgd-missing" not found' >&2
        exit 1
        ;;
    silent)
        echo "$$" > "$FAKE_PF_PIDFILE"
        exec sleep 60
        ;;
esac
FAKE
chmod +x "$scratch/bin/kubectl"

free_port() {
    python3 -c 'import socket; s = socket.socket(); s.bind(("127.0.0.1", 0)); print(s.getsockname()[1])'
}

# Each call sources helpers.sh in a fresh shell, the way a suite sees it.
in_helpers() {
    env PATH="$scratch/bin:$PATH" REGISTRY=registry.test CLI_SCRATCH="$scratch" "$@"
}

port="$(free_port)"
# shellcheck disable=SC2016 # the inner script expands its own positional args
out="$(in_helpers FAKE_PF_MODE=listen bash -c '
    source "$1/common/helpers.sh"
    pid="$(port_forward ns svc/cfgd-metrics "$2" 8443)" || { echo "returned $?"; exit 0; }
    if (: < "/dev/tcp/127.0.0.1/$2") 2>/dev/null; then echo open; else echo closed; fi
    stop_port_forward "$pid"
    if kill -0 "$pid" 2>/dev/null; then echo alive; else echo stopped; fi
' _ "$e2e_root" "$port" 2>&1)"
if [ "$out" = "$(printf 'open\nstopped')" ]; then
    pass "port_forward returns once the port is open, and stop_port_forward ends kubectl"
else
    fail "port_forward listen: got '$out' (want open, then stopped)"
fi

port="$(free_port)"
# shellcheck disable=SC2016 # the inner script expands its own positional args
out="$(in_helpers FAKE_PF_MODE=exit bash -c '
    source "$1/common/helpers.sh"
    if port_forward ns svc/cfgd-missing "$2" 80 >/dev/null; then echo returned-0; else echo returned-1; fi
' _ "$e2e_root" "$port" 2>&1)"
if grep -qx 'returned-1' <<<"$out" && grep -qF 'services "cfgd-missing" not found' <<<"$out"; then
    pass "port_forward fails and shows kubectl's error when kubectl exits first"
else
    fail "port_forward exit: got '$out' (want returned-1 and kubectl's error)"
fi

port="$(free_port)"
# shellcheck disable=SC2016 # the inner script expands its own positional args
out="$(in_helpers FAKE_PF_MODE=silent FAKE_PF_PIDFILE="$scratch/silent.pid" E2E_PORT_FORWARD_TRIES=2 bash -c '
    source "$1/common/helpers.sh"
    if port_forward ns pod/cfgd-x "$2" 80 >/dev/null; then echo returned-0; else echo returned-1; fi
    if kill -0 "$(cat "$3")" 2>/dev/null; then echo leaked; else echo reaped; fi
' _ "$e2e_root" "$port" "$scratch/silent.pid" 2>&1)"
if grep -qx 'returned-1' <<<"$out" && grep -qF 'did not accept a connection within 1s' <<<"$out" \
    && grep -qx 'reaped' <<<"$out"; then
    pass "port_forward times out after E2E_PORT_FORWARD_TRIES probes, says so and stops kubectl"
else
    fail "port_forward silent: got '$out' (want returned-1, the 1s timeout message, reaped)"
fi

port="$(free_port)"
# shellcheck disable=SC2016 # the inner script expands its own positional args
out="$(in_helpers FAKE_PF_MODE=stubborn bash -c '
    source "$1/common/helpers.sh"
    pid="$(port_forward ns svc/cfgd-metrics "$2" 8443)" || { echo "returned $?"; exit 0; }
    stop_port_forward "$pid"
    if kill -0 "$pid" 2>/dev/null; then echo alive; else echo stopped; fi
' _ "$e2e_root" "$port" 2>&1)"
if grep -q 'ignored SIGTERM, sending SIGKILL$' <<<"$out" && [ "$(tail -n 1 <<<"$out")" = stopped ]; then
    pass "stop_port_forward sends SIGKILL to a kubectl that ignores SIGTERM"
else
    fail "stop_port_forward stubborn: got '$out' (want the SIGKILL message, then stopped)"
fi

# shellcheck disable=SC2016 # the inner script expands its own positional args
out="$(in_helpers bash -c '
    source "$1/common/helpers.sh"
    if port_forward ns cfgd-server 1 1 >/dev/null; then echo returned-0; else echo returned-1; fi
' _ "$e2e_root" 2>&1)"
if grep -qx 'returned-1' <<<"$out" && grep -qF 'svc/<name> or pod/<name>' <<<"$out"; then
    pass "port_forward refuses a target without svc/ or pod/"
else
    fail "port_forward bare target: got '$out' (want a refusal)"
fi

port="$(free_port)"
# shellcheck disable=SC2016 # the inner script expands its own positional args
out="$(in_helpers bash -c '
    source "$1/common/helpers.sh"
    http_get_to_file "http://127.0.0.1:$2/metrics" "$3"
    [ -f "$3" ] && echo body-kept
    http_evidence 000 "" "$3"
' _ "$e2e_root" "$port" "$scratch/unreachable.txt" 2>&1)"
if [ "$(head -n 2 <<<"$out")" = "$(printf '000\nbody-kept')" ] \
    && grep -qE '^curl exit 7: curl: \(7\) ' <<<"$out" \
    && grep -qFx 'HTTP 000, content-type none, 0 body lines; first 15:' <<<"$out"; then
    pass "http_get_to_file reports 000, keeps an empty body, and http_evidence shows curl's error"
else
    fail "http_get_to_file unreachable: got '$out' (want 000, body-kept, curl exit 7 with its error)"
fi

seq 1 20 | sed 's/^/line /' > "$scratch/body.txt"
# shellcheck disable=SC2016 # the inner script expands its own positional args
out="$(in_helpers bash -c '
    source "$1/common/helpers.sh"
    http_evidence 200 "text/plain; version=0.0.4" "$2"
' _ "$e2e_root" "$scratch/body.txt" 2>&1)"
want="$(printf 'HTTP 200, content-type text/plain; version=0.0.4, 20 body lines; first 15:\n'; seq 1 15 | sed 's/^/    | line /')"
if [ "$out" = "$want" ]; then
    pass "http_evidence names the status, content type, line count and first 15 lines"
else
    fail "http_evidence: got '$out' (want '$want')"
fi

# A port-forward started outside port_forward goes back to a fixed sleep and a
# discarded stderr, so the helper is the only place one may start. The word is
# matched on its own so a command continued from the line before, or run
# through "$KUBECTL", is still caught. grep exits 1 for no match and 2 for an
# error; an error must fail the check.
scan_strays() {
    local out rc=0 line
    out="$(grep -rnE --include='*.sh' '(^|[[:space:]"])port-forward([[:space:]"]|$)' "$1" 2>&1)" || rc=$?
    if [ "$rc" -gt 1 ]; then
        printf 'grep exited %s: %s\n' "$rc" "$out" >&2
        return 2
    fi
    while IFS= read -r line; do
        case "$line" in
            "" | */common/helpers.sh:* | */common/test-port-forward.sh:*) continue ;;
        esac
        [[ "${line#*:*:}" =~ ^[[:space:]]*# ]] || printf '%s\n' "$line"
    done <<<"$out"
}

if stray="$(scan_strays "$e2e_root")"; then
    if [ -n "$stray" ]; then
        fail "port-forwards started outside common/helpers.sh:"
        printf '%s\n' "$stray"
    else
        pass "every port-forward goes through port_forward in common/helpers.sh"
    fi
else
    fail "the port-forward scan could not read $e2e_root"
fi

fixtures="$scratch/stray-fixtures"
mkdir -p "$fixtures"
printf '%s\n' 'kubectl port-forward -n ns svc/x 1:1 >/dev/null 2>&1 &' > "$fixtures/bare.sh"
# shellcheck disable=SC2016 # the fixture holds the literal text a script would contain
printf '%s\n' 'kubectl -n ns port-forward "pod/$P" 1:1 &' > "$fixtures/reordered.sh"
printf '%s\n' "kubectl -n ns \\" '    port-forward svc/x 1:1 &' > "$fixtures/continued.sh"
# shellcheck disable=SC2016 # the fixture holds the literal text a script would contain
printf '%s\n' '"$KUBECTL" port-forward svc/x 1:1 &' > "$fixtures/variable.sh"
printf '%s\n' '    # kubectl port-forward is started by the helper' > "$fixtures/comment.sh"
if found="$(scan_strays "$fixtures")"; then
    for name in bare.sh reordered.sh continued.sh variable.sh; do
        if grep -qF -- "$fixtures/$name:" <<<"$found"; then
            pass "scan reports $name"
        else
            fail "scan misses $name"
        fi
    done
    if grep -qF -- "$fixtures/comment.sh:" <<<"$found"; then
        fail "scan reports a comment that names kubectl port-forward"
    else
        pass "scan passes a comment that names kubectl port-forward"
    fi
else
    fail "the port-forward scan could not read its fixtures"
fi

# A setup that starts a port-forward can fail part way under set -e, so the
# run-all.sh that sources it must already have the EXIT trap that stops it.
for runner in "$e2e_root"/*/scripts/run-all.sh; do
    setup_line="$(grep -nE '^source .*/setup-[a-z-]+-env\.sh"$' "$runner" | head -n 1)" || continue
    setup="$(dirname "$runner")/$(basename "$(sed -E 's/.*\/(setup-[a-z-]+-env\.sh)"$/\1/' <<<"$setup_line")")"
    grep -q 'port_forward ' "$setup" || continue
    trap_at="$(grep -nE '^trap .* EXIT$' "$runner" | head -n 1 | cut -d: -f1)"
    if [ -n "$trap_at" ] && [ "$trap_at" -lt "${setup_line%%:*}" ]; then
        pass "${runner#"$e2e_root"/} installs its EXIT trap before sourcing $(basename "$setup")"
    else
        fail "${runner#"$e2e_root"/} sources $(basename "$setup"), which starts a port-forward, before its EXIT trap"
    fi
done

if scan_strays "$scratch/no-such-dir" >/dev/null 2>&1; then
    fail "scan of an unreadable path passed"
else
    pass "scan of an unreadable path fails"
fi

if [ "$failures" -ne 0 ]; then
    echo "$failures check(s) failed"
    exit 1
fi
echo "all checks passed"
