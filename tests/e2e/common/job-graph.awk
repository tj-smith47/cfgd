# job-graph.awk reads a GitHub Actions workflow and prints one line per job:
#
#   <job id> TAB <needs, space separated> TAB <go-task targets its run: lines name, space separated>
#
# It reads the shapes e2e.yml writes: job ids at two spaces under `jobs:`,
# `needs: x`, `needs: [a, b]`, and a needs list continued over several lines
# in flow (`[` ... `]`) or block (`- a`) form. A line it cannot place inside a
# needs list it is reading ends the list.
function flush() {
    if (job != "") printf "%s\t%s\t%s\n", job, needs, tasks
    job = ""; needs = ""; tasks = ""; in_needs = 0; flow = 0
}
function add_needs(text,    n, parts, i, word) {
    gsub(/[][,]/, " ", text)
    n = split(text, parts, /[ \t]+/)
    for (i = 1; i <= n; i++) {
        word = parts[i]
        gsub(/["']/, "", word)
        if (word != "") needs = needs (needs == "" ? "" : " ") word
    }
}
/^jobs:[ \t]*$/ { in_jobs = 1; next }
in_jobs && /^[^ \t#]/ { flush(); in_jobs = 0; next }
!in_jobs { next }
/^  [A-Za-z0-9_-]+:[ \t]*$/ {
    flush()
    job = $0
    sub(/^  /, "", job); sub(/:[ \t]*$/, "", job)
    next
}
job == "" { next }
in_needs {
    line = $0
    sub(/#.*/, "", line)
    if (line ~ /^[ \t]*$/) next
    if (flow) {
        add_needs(line)
        if (line ~ /\]/) in_needs = 0
        next
    }
    if (line ~ /^[ \t]*\[/) {
        flow = 1
        add_needs(line)
        if (line ~ /\]/) in_needs = 0
        next
    }
    if (line ~ /^[ \t]*- /) {
        sub(/^[ \t]*- /, "", line)
        add_needs(line)
        next
    }
    in_needs = 0
}
/^    needs:/ {
    line = $0
    sub(/#.*/, "", line)
    sub(/^    needs:[ \t]*/, "", line)
    if (line == "") { in_needs = 1; flow = 0; next }
    add_needs(line)
    if (line ~ /^\[/ && line !~ /\]/) { in_needs = 1; flow = 1 }
    next
}
/^[ \t]+(- )?run:[ \t]*task[ \t]/ {
    line = $0
    sub(/^[ \t]+(- )?run:[ \t]*task[ \t]+/, "", line)
    sub(/[ \t].*/, "", line)
    tasks = tasks (tasks == "" ? "" : " ") line
}
END { flush() }
