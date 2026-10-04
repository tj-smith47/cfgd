# shellcheck shell=bash disable=SC2006,SC2016,SC2034,SC2091,SC2194,SC2216  # each line is a spelling the walk reads and nothing runs
# One way bash runs sleep per line; the walk names every line. test-waits.sh
# counts the lines below the marker and expects one SLEEP for each.
# spellings:
x=`sleep 1`
\sleep 1
/bin/sleep 1
/usr/bin/sleep 1
timeout 5 sleep 1
env sleep 1
nice sleep 1
if sleep 1; then :; fi
while sleep 1; do break; done
until sleep 1; do :; done
case a in a) sleep 1 ;; esac
echo 1 | xargs sleep
bash -c 'sleep 1'
sh -c "sleep 1"
kubectl exec p -- sleep 2
exec_in_pod sleep 2
