# Reads shell scripts the way bash finds their heredocs and prints one
# tab-separated record per event, in input order:
#
#   FILE      file
#   SH        file line raw          every line outside a body, comments included
#   CMD       file line cmd          every command outside a body, at its first line
#   OPEN      file line id delim quoted dash cmd
#   BODY      file line id raw
#   CLOSE     file line id [tail]    tail: any quotes and ) after the delimiter
#   UNCLOSED  file line delim        a heredoc still open at the end of its file
#
# id counts the heredocs of one file from 1. quoted is 1 when the delimiter is
# quoted (<<'W', <<"W", <<\W) and dash is 1 for <<-W. The cmd of OPEN is the
# command that opened the heredoc and the cmd of CMD is any command, both
# reduced as below, with backslash-continued lines joined. The last field of
# SH, CMD, BODY and OPEN is the rest of the record and may itself hold tabs.
#
# Every opener on a line queues a body, and the bodies follow in order. A
# terminator may be indented with tabs only after <<-, and may be followed by
# the quote that closes a `bash -c '...'`. A line of the delimiter followed by
# the `)` of a `$(...)` holding the heredoc is no terminator to bash: it ends
# the body where the substitution ends, warning "delimited by end-of-file", and
# the reader closes the body on that line too. Openers are looked for in the
# line as one pass over its characters reduces it: a '...' span is kept as
# written, \x is one unit, a # after whitespace ends the line, a ((...)) span
# is dropped, and a "..." span is dropped unless it follows << or is still
# open at the end of the line. Inside a "..." span, a $(...) is read like the
# top level, so the "<<W" in "$(echo "<<W")" opens nothing. Its ( and ) are
# counted without shell grammar, so a case pattern x) or a ((cmd) ) subshell
# on one line inside $(...) closes the span early and hides a later opener.
# A "$( still open at the end of a line, as in X="$(cmd <<W, carries on to
# the next line outside a heredoc body, so the )" after the terminator closes
# it and the CMD record starts at the line that opened it. One that ends the
# line inside a quote of its own is not carried, and the lines after it are
# each read on their own.
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
            if (j == 0) { j = N - pos; quote_open = 1 }
            out = out substr(S, pos, j + 1); pos += j + 1
        }
        else if (c == "#" && (pos == 1 || substr(S, pos - 1, 1) ~ /[[:space:]]/)) pos = N + 1
        else if (c == "\"") out = out dq(out)
        else if (substr(S, pos, 2) == "((") skip_arith()
        else if (nested && c == ")" && d-- == 0) { pos++; shut = 1; return out }
        else { if (c == "(") d++; out = out c; pos++ }
    }
    shut = 0
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
        else if (substr(S, pos, 2) == "$(") {
            pos += 2; keep = 1; quote_open = 0; red = red "$(" cmd(1) ")"
            if (!shut && !quote_open) held = 1
        }
        else { red = red c; pos++ }
    }
    quote_open = 1
    return substr(S, start, N)
}

function unclosed(   i) {
    for (i = qh; i <= qn; i++) print "UNCLOSED\t" file "\t" qline[i] "\t" q[i]
    qn = 0; qh = 1
}

BEGIN { qh = 1; OFS = "\t" }

FNR == 1 {
    if (qn >= qh) unclosed()
    qn = 0; qh = 1; cont = ""; resume = 0; file = FILENAME
    print "FILE\t" file
}

qn >= qh {
    if ($0 ~ ((qdash[qh] ? "^\t*" : "^") q[qh] "[\047\")]*$")) {
        tail = $0; sub(/^\t*/, "", tail)
        tail = substr(tail, length(q[qh]) + 1)
        print "CLOSE\t" file "\t" FNR "\t" qh (tail == "" ? "" : "\t" tail)
        qh++
    } else {
        print "BODY\t" file "\t" FNR "\t" qh "\t" $0
    }
    next
}

{ print "SH\t" file "\t" FNR "\t" $0 }

/^[[:space:]]*#/ { cont = ""; next }

{
    # A line resuming an open "$( is read behind the "$( that opened it.
    S = (resume ? "\"$(" : "") $0; N = length(S); pos = 1; held = 0
    rest = cmd(0)
    if (resume) rest = substr(rest, 4)
    if (cont == "") start = FNR
    logical = cont rest
    resume = held
    if ($0 ~ /\\$/ || held) {
        cont = logical
        sub(/\\$/, "", cont)
        cont = cont " "
    } else {
        cont = ""
        whole = logical
        sub(/[[:space:]]+$/, "", whole)
        if (whole != "") print "CMD\t" file "\t" start "\t" whole
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
