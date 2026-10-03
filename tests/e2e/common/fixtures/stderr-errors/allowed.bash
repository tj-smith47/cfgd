echo "ERROR: redirected" >&2
echo "ERROR: fd 1 redirected" 1>&2
true || { echo "ERROR: in a one-line group"; exit 1; } >&2
true || { echo "ERROR: redirected inside a one-line group" >&2; exit 1; }
{
    echo "ERROR: in a redirected group"
    echo "  detail"
} >&2
g() {
    echo "ERROR: in a redirected function body"
} >&2
if true; then
    {
        echo "ERROR: nested in a redirected group"
    } >&2
fi
echo "ERROR: continued" \
    "and redirected on the last line" >&2
echo "no error here"
# echo "ERROR: in a comment"
# a comment; echo "ERROR: after a separator in a comment"
grep -q 'echo "ERROR' /dev/null
fail_test "X" "ERROR text in a failure reason"
cat <<EOT
echo "ERROR: in a heredoc body"
EOT
{
    echo "ERROR: in a group redirected from fd 1"
} 1>&2
echo "ERROR: a; b" >&2
echo 'ERROR: x || y' >&2
echo "ERROR: x & y | z" 1>&2; echo "detail" 2>&1
{ echo "ERROR: in a one-line group"; echo "detail"; } >&2
true || { { echo "ERROR: nested one-line groups"; }; exit 1; } >&2
h() { echo "ERROR: one-line function body"; } >&2
echo "ERROR: spread
over a quoted newline" >&2
