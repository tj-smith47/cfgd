echo "ERROR: plain"
if true; then echo "  ERROR: indented message"; fi
true || { echo "ERROR: one-line group without a redirect"; exit 1; }
true || {
    echo "ERROR: in a group without a redirect"
    exit 1
}
f() {
    if true; then
        echo "ERROR: in a function body without a redirect"
    fi
}
printf 'ERROR: %s\n' format
printf '%s\n' "ERROR: as an argument"
echo -e "ERROR: after an option"
echo "ERROR: continued" \
    "over two lines"
true && echo "ERROR: after &&"
echo "ERROR: before a redirected group"
{
    echo "detail"
} >&2
echo "ERROR: first, redirected" >&2; echo "ERROR: second, on stdout"
echo "detail" >&2 && echo "ERROR: after a redirected command"
{ echo "ERROR: in a group closed without a redirect"; } && echo ok >&2
echo 'ERROR: a quoted >&2 is text' "x >&2"
echo "ERROR: spread
over a quoted newline"
{ echo "ERROR: in a group whose inner group alone is redirected"; { echo "detail"; } >&2; }
f() { echo "ERROR: in a one-line function body"; }
function k { echo "ERROR: in a one-line function keyword body"; }
case "$x" in
    a) echo "ERROR: in a case arm" ;;
    *) echo "ERROR: in a default case arm"; exit 1 ;;
esac
command echo "ERROR: after command"
builtin printf 'ERROR: after builtin\n'
! echo "ERROR: after !"
time -p echo "ERROR: after time"
exec echo "ERROR: after exec"
LC_ALL=C echo "ERROR: after an assignment"
2>/dev/null echo "ERROR: after a redirect of fd 2"
if echo "ERROR: as a condition"; then :; fi
echo "ERROR: to fd twenty" >&20
if true; then echo "ERROR: in an if closed without a redirect"; fi
while false; do echo "ERROR: in a loop whose done sends fd 2 to fd 1"; done 2>&1
( echo "ERROR: in a one-line subshell without a redirect"; exit 1 )
g2() {
    if true; then
        echo "ERROR: in an if inside an unredirected function"
    fi
}
if true; then
    echo "ERROR: in an if that ends on a plain line, before a redirected group"
true; fi
{
    echo "detail"
} >&2
