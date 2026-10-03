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
