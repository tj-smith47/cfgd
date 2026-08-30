# Whole take plays at 1:1 (make-gif-flat.sh, no ramp) — this tape's claim is
# fleet sync: two machines initialized from one git config source, machine A
# commits a change to the nvim module, and machine B's daemon pulls it while
# nobody touches B: the module's files are deployed as symlinks into the
# clone, so the change is live the moment the pull lands, and the reconcile
# the pull triggers reports that nothing is left to do. The money shot is
# B's nvim gutter switching from absolute to relative line numbers on its
# second open.
Output "demo/.out/sync/"

Require docker

Set Shell "bash"
# Same font as author.tape — the alpha dashboard beats render Nerd Font
# glyphs, and cfgd's own output (icons, tables) shares the terminal.
Set FontFamily "FiraCode Nerd Font Mono"
Set FontSize 15
# The same width as every other tape, so the docs render one text scale. With
# the module's file tree open (it is part of what this demo deploys) the
# editor beats lose ~30 columns to it, and one context line plus the git
# hunk header clip mid-word; both editor beats clip identically, so the
# before/after comparison the beats exist for is untouched. Widening this
# tape alone would trade that for a demo at a different scale from its
# siblings.
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
# loop is serial, the interval pump sleeps a whole interval before its first
# send, so the first completion lands a little over one interval after start
# (the reconcile itself is a few seconds of that), and every sync tick queues
# behind it — a take recorded before
# it drains reaches beat 8 before the first pull can land. The daemon logs
# the completion of every reconcile it runs; the gate waits for the SECOND
# completion — the first only says the cold reconcile drained, the second
# says the loop has run a whole warm tick, so beat 2's Uptime reads as a
# machine that has been sitting there rather than one that just booted.
Type "docker exec machine-b tail -f /home/tj/daemon.log"
Enter
Wait+Screen@300s /(?s)reconcile: complete.*reconcile: complete/
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
# `local` source (the machine's own clone of the shared config repo) active,
# last synced moments ago. This is the agent that will do all the work.
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
# nvim switches to the alternate screen before it paints, which records as
# a black frame for as long as its start takes. The typed command stays on
# camera; from Enter the take stays hidden through the off-camera setup
# below and resumes on the settled editor.
Hide
Wait+Screen@20s /52 opt\.number = true/
# The module wraps long lines and repaints the cursor line on a hold; neither
# is what this beat is about (the gutter is). Off camera: unset wrap, clear
# the CursorHold autocmds, then wait out the plugin manager's start-up
# notices. The file tree stays open: it is part of the configuration this
# demo just deployed, and closing it would frame the payoff in an editor the
# module did not produce. The notifier is never replaced — noice watches it
# and reports a replacement louder than the notice it hid; hidden wall time
# costs the GIF nothing.
Type ":set nowrap | lua vim.api.nvim_clear_autocmds({ event = 'CursorHold' })"
Enter
Sleep 10s
Show
Wait+Screen@10s /52 opt\.number = true/
Sleep 5s
Type ":qa"
Enter
Wait@15s /machine-b:~\$$/

# Hidden scene cut: nvim leaves VHS's terminal unable to scroll the primary
# screen (and can leave a stale scroll region) until a full reset — without
# this, output later in the take silently stops at the bottom row and the
# beat-8 log gate can never match. `printf '\033c'` is RIS: unlike `clear`,
# it also drops any scroll region the nvim session left behind. The next
# beat's command is typed before the screen comes back, so no visible frame
# is a bare prompt.
Hide
Type "printf '\033c'; clear"
Enter
Sleep 300ms
# Beat 4: leave B untouched and hop onto machine A.
Type "exit"
Show
Sleep 1s
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
# can be dead code), and read the diff back before it ships. The path goes
# into a variable first so the edit itself fits one terminal row.
Type "cd .config/cfgd"
Enter
Wait@10s /cfgd\$$/
Sleep 1s
Type "f=modules/nvim/files/lua/config/options.lua"
Enter
Wait@10s /cfgd\$$/
Sleep 500ms
Type "sed -i '/^opt.number = true/a opt.relativenumber = true -- count from the cursor line' $f"
Enter
Wait@10s /cfgd\$$/
Sleep 1s
# The hunk header is filtered out and the pattern is spelled `@{2}`: FiraCode Nerd
# Font Mono shapes an adjacent literal `@@` into a garbled glyph under VHS's headless
# chromium (reproduced with a bare `echo '@@'`), in typed and output text alike.
Type "git diff | grep -vE '^@{2}'"
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

# Beat 8: watch the daemon's log live until the pull lands on camera — it
# pulls, reconciles because the source changed, and reports that reconcile
# complete. The gate is the pull line followed by the completion line, so
# the take can never claim a sync it didn't see through to its outcome.
Type "tail -f ~/daemon.log"
Show
Sleep 1s
Enter
Wait+Screen@60s /(?s)sync: pulled.*reconcile: complete/
Sleep 4s
Ctrl+C
Wait@10s /machine-b:~\$$/
Sleep 1s

# Beat 9: cfgd's own account of what just happened — the daemon still
# running, no drift, and the source synced moments ago.
Type "cfgd daemon status"
Enter
Wait@30s /machine-b:~\$$/
Sleep 4s

# Beat 10: A's commit is at the tip of B's clone — nobody ran git here.
# `--abbrev=12` so git names the commit the way the daemon log above does.
Type "git -C ~/.config/cfgd --no-pager log -1 --abbrev=12 --format='%h %s'"
Enter
Wait+Screen@10s /relative line numbers/
Sleep 4s

# Beat 11: AFTER — the same file at the same line, reopened: the inserted
# option now sits directly beneath `opt.number = true`, and the gutter
# counts relative to the cursor (the new line reads 1, not 53). The synced
# module content is live through the deployed symlinks the moment the
# daemon's pull lands.
Type "nvim +52 ~/.config/nvim/lua/config/options.lua"
Enter
# nvim switches to the alternate screen before it paints, which records as
# a black frame for as long as its start takes. The typed command stays on
# camera; from Enter the take stays hidden through the off-camera setup
# below and resumes on the settled editor.
Hide
Wait+Screen@20s /1 opt\.relativenumber = true/
# Same off-camera nowrap, CursorHold clear and notice wait-out as beat 3, so
# the before and after frames differ in the gutter alone.
Type ":set nowrap | lua vim.api.nvim_clear_autocmds({ event = 'CursorHold' })"
Enter
Sleep 10s
Show
Wait+Screen@10s /1 opt\.relativenumber = true/
Sleep 5s

# The take ends inside nvim, on the payoff, so the loop rests on the gutter
# rather than on scrollback. Closing hold — the last beat's own hold already
# covers reading it; this is only the breath before the loop snaps back.
Sleep 2s
