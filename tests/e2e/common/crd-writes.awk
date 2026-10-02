# Reads heredocs.awk records and prints one tab-separated line per command
# that can write a CustomResourceDefinition, tag first:
#
#   KUBECTL   file:line cmd   a kubectl apply/create/replace/patch/delete/edit/
#                             label/annotate/set whose lines name a CRD (crd,
#                             crds, customresourcedefinition, $CRD_YAML,
#                             cfgd-gen-crds, a crds.yaml path); --local and
#                             --dry-run calls write nothing and are skipped
#   HELM      file:line cmd   helm install, or helm upgrade --install/-i,
#                             without --skip-crds: Helm creates each CRD in
#                             the chart's crds/ that the cluster lacks
#   HEREDOC   file:line cmd   a heredoc holding kind: CustomResourceDefinition,
#                             whatever reads it: a CRD written to a file can be
#                             applied later by a path that does not name it
#
# cmd is the command as heredocs.awk reduced it, for HEREDOC the one that
# opened the heredoc, with each run of whitespace made one space so that an
# entry keyed on it survives reindenting.
#
# A command is read from its CMD record, where heredocs.awk has joined its
# continued lines and dropped its comments and most double-quoted text, so a
# message that quotes a command matches nothing. Whether a kubectl command
# names a CRD is read from its raw lines, where a quoted "$CRD_YAML" survives.
# The line printed is the command's first.
#
# Usage: awk -f heredocs.awk FILE... | awk -f crd-writes.awk

BEGIN { FS = "\t" }

function squeeze(s) {
    gsub(/[[:space:]]+/, " ", s)
    sub(/^ /, "", s)
    sub(/ $/, "", s)
    return s
}

$1 == "FILE" { start = ""; next }

$1 == "SH" {
    raw = $4
    for (i = 5; i <= NF; i++) raw = raw "\t" $i
    if (start == "") start = $3
    from[$2, $3] = start
    text[$2, start] = text[$2, start] raw "\n"
    if (raw !~ /\\$/) start = ""
    next
}

$1 == "CMD" {
    cmd = $4
    for (i = 5; i <= NF; i++) cmd = cmd "\t" $i
    cmd = squeeze(cmd)
    if (cmd ~ /(^|[^[:alnum:]_-])kubectl[[:space:]]([^|;&]*[[:space:]])?(apply|create|replace|patch|delete|edit|label|annotate|set)([[:space:]]|$)/ &&
        cmd !~ /--local|--dry-run/ && tolower(text[$2, from[$2, $3]]) ~ /crd|customresourcedefinition/)
        print "KUBECTL\t" $2 ":" $3 "\t" cmd
    if (cmd ~ /(^|[^[:alnum:]_-])helm[[:space:]]+(install[[:space:]]|upgrade([[:space:]].*)?[[:space:]](--install|-i)([[:space:]]|$))/ &&
        cmd !~ /--skip-crds/)
        print "HELM\t" $2 ":" $3 "\t" cmd
    next
}

$1 == "OPEN" {
    cmd = $8
    for (i = 9; i <= NF; i++) cmd = cmd "\t" $i
    cmd = squeeze(cmd)
    opener[$2, $4] = cmd
    opened[$2, $4] = $3
    next
}

$1 == "BODY" && !seen[$2, $4] && $5 ~ /^[[:space:]]*kind:[[:space:]]*CustomResourceDefinition[[:space:]]*$/ {
    seen[$2, $4] = 1
    print "HEREDOC\t" $2 ":" opened[$2, $4] "\t" opener[$2, $4]
}
