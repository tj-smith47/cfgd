# Input for scan_subshell_conditions in test-pr-install.sh; nothing runs it. The check
# expects COND at lines 3, 11, 16, 24, 25, 27 and 29 and nowhere else.
if ! (E2E_NAMESPACE="$E2E_INSTALL_NS"; create_e2e_namespace; stop_heartbeat); then
    exit 1
fi
if (: < "/dev/tcp/127.0.0.1/$local_port") 2>/dev/null; then
    echo open
fi
    if (: < "/dev/tcp/127.0.0.1/$2") 2>/dev/null; then echo open; else echo closed; fi
# && inside the subshell, and its do on the next line.
while (kubectl get ns x && false)
do
    break
done
# A newline inside the subshell.
until ! (
    create_e2e_namespace
    stop_heartbeat
); do
    break
done
# An escaped quote, in double quotes and outside them, leaves no quote open.
if (c == "\"" || c == "x"); then :; fi
if ! (a; b); then :; fi
if (echo \" && true); then :; fi
# A ) inside quotes does not close the subshell.
if ! (echo ")"; true); then :; fi
# A brace group condition runs with set -e off too.
if ! { a; b; }; then :; fi
if { a; }; then :; fi
# A keyword line with no then or do is awk inside a quoted program.
awk '
    {
        if (a == 1 && b == 2) { y = 2; z = 3 }
    }
'
# A heredoc body is not shell.
cat <<'SH'
if ! (a; b); then :; fi
SH
