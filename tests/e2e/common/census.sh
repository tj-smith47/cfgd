# shellcheck shell=bash
# The per-file floor every scan of the e2e scripts keeps: the scan lists its
# files, its awk writes the name of each file it reads to a log, and any listed
# file missing from the log fails the scan by name. A file awk could not open
# prints no breach, so without the floor it would read as a clean one.

# census_unread <scan> <listed> <read>: print `<scan>: <file> was listed and
# never read` on stderr for each line of <listed> absent from <read>, and
# return 1 when there is one. A readable empty file is exempt, since awk opens
# it without reading a line.
census_unread() {
    local scan="$1" listed="$2" read="$3" file missing=0
    while IFS= read -r file; do
        if [ -f "$file" ] && [ -r "$file" ] && [ ! -s "$file" ]; then continue; fi
        if ! grep -Fxq -- "$file" "$read" 2>/dev/null; then
            echo "$scan: $file was listed and never read" >&2
            missing=1
        fi
    done < "$listed"
    return "$missing"
}
