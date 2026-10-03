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
f() { echo "ERROR: in a one-line function body with its own redirect" >&2; }
case "$x" in
    *) echo "ERROR: in a default case arm with its own redirect" >&2; exit 1 ;;
esac
command echo "ERROR: after command" >&2
time -p echo "ERROR: after time" >&2
case "$x" in *) echo "ERROR: in a one-line case closed by a redirected esac" ;; esac >&2
if true; then echo "ERROR: in a one-line if closed by a redirected fi"; fi >&2
while false; do echo "ERROR: in a one-line loop closed by a redirected done"; done >&2
( echo "ERROR: in a one-line subshell closed by a redirected paren"; exit 1 ) >&2
if true; then
    echo "ERROR: in an if redirected at fi"
fi >&2
for x in a; do
    echo "ERROR: in a loop redirected at done"
done >&2
case "$x" in
    *)
        echo "ERROR: in a case redirected at esac"
        ;;
esac >&2
(
    echo "ERROR: in a subshell redirected at its paren"
) >&2
echo "ERROR: to /dev/stderr" >/dev/stderr
echo "ERROR: appended to /dev/stderr" >> /dev/stderr
h2() {
    if true; then
        echo "ERROR: in an if inside a redirected function"
    fi
} >&2
if true; then
    echo "ERROR: before an else, in an if redirected at fi"
else
    :
fi >&2
case "$x" in
a)
    echo "ERROR: in a first case arm, patterns level with esac, redirected at esac"
    ;;
b)
    ;;
esac >&2
{
    echo "ERROR: in a redirected group with a string continued at column 0"
    msg="one
two"
} >&2
{
    echo "ERROR: in a redirected group with a command continued at column 0"
    printf '%s\n' \
--flag
} >&2
