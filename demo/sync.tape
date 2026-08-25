# Whole take plays at 1:1 (make-gif-flat.sh, no ramp) — this tape's claim is
# fleet sync: two machines initialized from one git config source, machine A
# commits a change to the nvim module, and machine B's daemon pulls and
# reconciles it while nobody touches B. The money shot is B's nvim gutter
# switching from absolute to relative line numbers on its second open.
Output "demo/.out/sync.mp4"

Require docker

Set Shell "bash"
# Same font as author.tape — the alpha dashboard beats render Nerd Font
# glyphs, and cfgd's own output (icons, tables) shares the terminal.
Set FontFamily "FiraCode Nerd Font Mono"
Set FontSize 15
Set Width 1320
Set Height 1000
Set TypingSpeed 50ms
Set Padding 20
Set Framerate 50

# Everything up to here is hidden and pre-staged by setup-sync-demo.sh: both
# containers were initialized from the same bare git hub (mounted at
# /srv/git/config.git in each), the nvim module applied, and `cfgd daemon run`
# started on each with autoPull sync every 5s. autoPush stays off — this
# tape's change leaves machine A by an explicit, on-camera git push. The
# host prompt is renamed so the hops between machines read as hops.
Hide
Type "export PS1='host$ '"
Enter
# Gate on machine B's daemon having finished its INITIAL reconcile: the tick
# loop is serial, the first reconcile on a cold machine blocks it for a
# minute-plus, and every sync tick queues behind it — a take recorded before
# it drains reaches beat 8 before the first pull can land. A second
# "running reconciliation check" line only appears once the first has
# finished, so two on screen means the loop is draining at its warm cadence.
Type "docker exec machine-b tail -f /home/tj/daemon.log"
Enter
Wait+Screen@240s /(?s)running reconciliation check.*running reconciliation check/
Ctrl+C
Type "clear"
Enter
Sleep 500ms
Show
Sleep 1s

# Beat 1: hop onto machine B — the machine nobody will touch. Its hostname
# is in the prompt, so every later frame says which machine is on camera.
Type "docker exec -it machine-b bash -l"
Enter
Wait@15s /machine-b:~\$$/
Sleep 2s

# Beat 2: B's daemon is alive and syncing — running, zero drift, and the
# `local` source (the machine's own clone of the shared config repo) active
# with a last-sync timestamp. This is the agent that will do all the work.
Type "cfgd daemon status"
Enter
Wait@30s /machine-b:~\$$/
Sleep 5s

# Beat 3: BEFORE — the module's own options file in B's nvim, opened at the
# line that declares the numbering (`opt.number = true`, line 52), with the
# absolute line numbers the shared config ships today: every gutter row
# counts from the top of the file.
Type "nvim +52 ~/.config/nvim/lua/config/options.lua"
Enter
Wait+Screen@20s /52 opt\.number = true/
Sleep 5s
Type ":qa"
Enter
Wait@15s /machine-b:~\$$/
# Long enough to read the returned prompt before the scene cut below wipes it.
Sleep 3s

# Hidden scene cut: nvim leaves VHS's terminal unable to scroll the primary
# screen (and can leave a stale scroll region) until a full reset — without
# this, output later in the take silently stops at the bottom row and the
# beat-8 log gate can never match. `printf '\033c'` is RIS: unlike `clear`,
# it also drops any scroll region the nvim session left behind.
Hide
Type "printf '\033c'; clear"
Enter
Sleep 300ms
Show
Sleep 1s

# Beat 4: leave B untouched and hop onto machine A.
Type "exit"
Enter
Wait@10s /host\$$/
Sleep 1s
Type "docker exec -it machine-a bash -l"
Enter
Wait@15s /machine-a:~\$$/
Sleep 2s

# Beat 5: the config dir IS a git clone of the shared source — turn on
# relative line numbers in the nvim module, right beneath the module's own
# `opt.number = true` in options.lua (inserted there rather than appended:
# the file ends in an early-returning notification block, so a tail append
# can be dead code), and read the diff back before it ships.
Type "cd .config/cfgd"
Enter
Wait@10s /cfgd\$$/
Sleep 1s
Type "sed -i '/^opt.number = true/a opt.relativenumber = true' modules/nvim/files/lua/config/options.lua"
Enter
Wait@10s /cfgd\$$/
Sleep 1s
Type "git diff"
Enter
Wait@10s /cfgd\$$/
Sleep 5s

# Beat 6: commit and push — the change leaves A for the shared source. This
# is the last manual action in the demo; everything after is the daemon's.
Type "git commit -am 'nvim: relative line numbers'"
Enter
Wait@10s /cfgd\$$/
Sleep 3s
Type "git push"
Enter
Wait@30s /cfgd\$$/
Sleep 4s

# Beat 7: back to machine B. Nothing is typed at it beyond looking — the
# 5s-interval autoPull daemon is what moves the change.
Type "exit"
Enter
Wait@10s /host\$$/
Sleep 1s
Type "docker exec -it machine-b bash -l"
Enter
Wait@15s /machine-b:~\$$/
Sleep 1s

# Hidden scene cut: fresh screen for the payoff act, so every line the
# daemon log shows fits on screen without scrolling (see the note above —
# same RIS reset, this shell is a fresh exec but the VHS terminal is not).
Hide
Type "printf '\033c'; clear"
Enter
Sleep 300ms
Show
Sleep 1s

# Beat 8: watch the daemon's log live until the pull lands on camera — it
# pulls, sees the module file change, and schedules a reconcile. The gate is
# the pull line itself, so the take can never claim a sync it didn't see.
Type "tail -f ~/daemon.log"
Enter
Wait+Screen@45s /pulled new changes from remote/
Sleep 4s
Ctrl+C
Wait@10s /machine-b:~\$$/
Sleep 1s

# Beat 9: A's commit is at the tip of B's clone — nobody ran git here.
Type "git -C ~/.config/cfgd --no-pager log -1 --format='%h %s'"
Enter
Wait+Screen@10s /relative line numbers/
Sleep 4s

# Beat 10: AFTER — the same file at the same line, reopened: the inserted
# option now sits directly beneath `opt.number = true`, and the gutter
# counts relative to the cursor (the new line reads 1, not 53). The synced
# module content is live through the deployed symlinks the moment the
# daemon's pull lands.
Type "nvim +52 ~/.config/nvim/lua/config/options.lua"
Enter
Wait+Screen@20s /1 opt\.relativenumber = true/
# Whenever upstream plugins have moved past the module's lazy-lock.json,
# lazy.nvim's update toast slides in ~2s after open and covers the tail of the
# inserted line for its 5s nvim-notify timeout. Sleep past its whole lifecycle,
# then re-gate on the full line: while the toast is up, `= true` is covered and
# the regex cannot match, so the dwell that follows is provably clean.
Sleep 9s
Wait+Screen@15s /1 opt\.relativenumber = true/
Sleep 5s
Type ":qa"
Enter
Wait@15s /machine-b:~\$$/
Sleep 1s

# Closing hold — breathing room before the loop snaps back to the opening
# frame, matching the set's closing beat.
Sleep 8s
