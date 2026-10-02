# Reads shell scripts the way bash finds their heredocs and prints one
# tab-separated record per event, in input order:
#
#   FILE      file
#   SH        file line raw          every line outside a body, comments included
#   OPEN      file line id delim quoted dash cmd
#   BODY      file line id raw
#   CLOSE     file line id
#   UNCLOSED  file line delim        a heredoc still open at the end of its file
#
# id counts the heredocs of one file from 1. quoted is 1 when the delimiter is
# quoted (<<'W', <<"W", <<\W) and dash is 1 for <<-W. cmd is the command that
# opened the heredoc, reduced as below, with backslash-continued lines joined.
# The last field of SH, BODY and OPEN is the rest of the record and may itself
# hold tabs.
#
# Every opener on a line queues a body, and the bodies follow in order. A
# terminator may be indented with tabs only after <<-, and may be followed by
# the quote that closes a `bash -c '...'` or the `)` that closes a `$(...)`
# holding the heredoc. Openers are looked for in the line as one pass over its
# characters reduces it: a '...' span is kept as written, \x is one unit, a #
# after whitespace ends the line, a ((...)) span is dropped, and a "..." span
# is dropped unless it follows << or is still open at the end of the line.
# Inside a "..." span, a $(...) is read like the top level, so the "<<W" in
# "$(echo "<<W")" opens nothing. Its ( and ) are counted without shell grammar,
# so a case pattern x) or a ((cmd) ) subshell on one line inside $(...) closes
# the span early and hides a later opener.
#
# Usage: awk -f tests/e2e/common/heredocs.awk FILE...
# POSIX awk only: CI runners ship mawk.

function skip_arith(   d, c) {
    for (d = 0; pos <= N; pos++) {
        c = substr(S, pos, 1)
        if (c == "(") d++
        else if (c == ")" && --d == 0) { pos++; return }
    }
}

function cmd(nested,   out, d, c, j) {
    out = ""; d = 0
    while (pos <= N) {
        c = substr(S, pos, 1)
        if (c == "\\") { out = out substr(S, pos, 2); pos += 2 }
        else if (c == "\047") {
            j = index(substr(S, pos + 1), "\047")
            if (j == 0) j = N - pos
            out = out substr(S, pos, j + 1); pos += j + 1
        }
        else if (c == "#" && (pos == 1 || substr(S, pos - 1, 1) ~ /[[:space:]]/)) pos = N + 1
        else if (c == "\"") out = out dq(out)
        else if (substr(S, pos, 2) == "((") skip_arith()
        else if (nested && c == ")" && d-- == 0) { pos++; return out }
        else { if (c == "(") d++; out = out c; pos++ }
    }
    return out
}

function dq(before,   start, red, c, keep) {
    start = pos++; red = "\""; keep = 0
    while (pos <= N) {
        c = substr(S, pos, 1)
        if (c == "\\") { red = red substr(S, pos, 2); pos += 2 }
        else if (c == "\"") {
            pos++
            if (before ~ /<<-?[[:space:]]*$/) return substr(S, start, pos - start)
            return keep ? red "\"" : ""
        }
        else if (substr(S, pos, 3) == "$((") { pos++; skip_arith() }
        else if (substr(S, pos, 2) == "$(") { pos += 2; keep = 1; red = red "$(" cmd(1) ")" }
        else { red = red c; pos++ }
    }
    return substr(S, start, N)
}

function unclosed(   i) {
    for (i = qh; i <= qn; i++) print "UNCLOSED\t" file "\t" qline[i] "\t" q[i]
    qn = 0; qh = 1
}

BEGIN { qh = 1; OFS = "\t" }

FNR == 1 {
    if (qn >= qh) unclosed()
    qn = 0; qh = 1; cont = ""; file = FILENAME
    print "FILE\t" file
}

qn >= qh {
    if ($0 ~ ((qdash[qh] ? "^\t*" : "^") q[qh] "[\047\")]*$")) {
        print "CLOSE\t" file "\t" FNR "\t" qh
        qh++
    } else {
        print "BODY\t" file "\t" FNR "\t" qh "\t" $0
    }
    next
}

{ print "SH\t" file "\t" FNR "\t" $0 }

/^[[:space:]]*#/ { cont = ""; next }

{
    S = $0; N = length(S); pos = 1
    rest = cmd(0)
    logical = cont rest
    if ($0 ~ /\\$/) {
        cont = logical
        sub(/\\$/, "", cont)
        cont = cont " "
    } else {
        cont = ""
    }
    while (match(rest, /(^|[^<])<<-?[[:space:]]*\\?[\047"]?[A-Za-z_0-9][A-Za-z_0-9]*/)) {
        word = substr(rest, RSTART, RLENGTH)
        rest = substr(rest, RSTART + RLENGTH)
        qn++
        qline[qn] = FNR
        qdash[qn] = (word ~ /<<-/)
        sub(/^[^<]?<<-?[[:space:]]*/, "", word)
        quoted = (word ~ /^[\\\047"]/)
        sub(/^\\?[\047"]?/, "", word)
        q[qn] = word
        print "OPEN\t" file "\t" FNR "\t" qn "\t" word "\t" quoted "\t" qdash[qn] "\t" logical
    }
}

END { if (qn >= qh) unclosed() }
