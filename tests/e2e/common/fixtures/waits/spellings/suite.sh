# shellcheck shell=bash disable=SC1001,SC2006,SC2016,SC2034,SC2091,SC2093,SC2194,SC2216  # each line is a spelling the walk reads and nothing runs
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
nice -n 5 sleep 1
env FOO=1 sleep 1
FOO=1 sleep 1
timeout -s KILL 5 sleep 1
time -p sleep 1
command -p sleep 1
sudo sleep 1
stdbuf -oL sleep 1
coproc sleep 1
eval sleep 1
eval "sleep 1"
"sleep" 1
'sleep' 1
env -i PATH=/bin sleep 1
sudo -u root sleep 1
sudo -E sleep 1
echo 1 | xargs -n1 sleep
exec -a nm sleep 1
stdbuf -o L sleep 1
s\leep 1
\s\l\e\e\p 1
setsid sleep 1
setsid -w sleep 1
flock /x sleep 1
flock -n /x sleep 1
ionice -c3 sleep 1
ionice -c 3 sleep 1
timeout -k 5 10 sleep 1
nice -n -5 sleep 1
