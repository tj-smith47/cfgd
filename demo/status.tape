# Whole take plays at 1:1 (make-gif-flat.sh, no ramp) — this tape's claim is
# the status surfaces themselves, so nothing in it is worth compressing away.
Output "demo/.out/status/"

Require docker

Set Shell "bash"
# Same font as drift.tape — no LazyVim glyphs on screen here, but cfgd's own
# verdict glyphs and the shell prompt share the terminal.
Set FontFamily "FiraCode Nerd Font Mono"
Set FontSize 15
Set Width 1320
# 1320x1200 at font size 15 measures 125x60 (measured with `tput cols`/`tput
# lines` inside a take at exactly these three values, not derived). The tallest
# single render below is `cfgd status -o wide` at 37 lines, and every render
# that tall is preceded by a clear, so the grid only has to hold one of them
# plus the line that typed it.
Set Height 1200
Set TypingSpeed 50ms
Set Padding 20
Set Framerate 50

# Everything up to the converged prompt is hidden: this tape's claim is what
# cfgd can TELL you about a machine, not the install that gets a machine to a
# starting point — that is init.tape's job.
Hide
Type "clear"
Enter

Type "docker run --rm -it --hostname jarvispro cfgd-demo:latest bash -l"
Enter
Wait@2m /cfgd\$$/
Sleep 1s

Type "cfgd init --from tj-smith47/cfgd-config --apply-module nvim --theme dracula --yes"
Enter
# Same ceiling as init.tape's hidden install — see that tape's comment for why
# 60m covers even a worst-case provision.
Wait@60m /cfgd\$$/
Type "source ~/.cfgd.env"
Enter
Wait@30s /cfgd\$$/

# The nvim module leaves this container with two npms (brew's node and apt's
# npm), and sourcing ~/.cfgd.env put brew's first on PATH — the same residue
# drift.tape's hidden setup clears, and for the same reason: every scan below
# would otherwise report package drift on a take whose opening beat claims a
# converged machine. `--phase packages` keeps the module's postApply hooks out
# of it.
Type "cfgd apply --module nvim --phase packages --yes"
Enter
Wait@30m /cfgd\$$/

# The real tj-smith47/cfgd-config `base` profile also declares two modules this
# container can never converge (jarvis: private repo, no SSH credentials baked
# in; clift: needs charm's apt repo, not present on a bare Ubuntu base) —
# stripped here for the same reason drift.tape strips them: both are unrelated
# to this tape's claim, and every Component Health row below would carry them
# as noise.
Type "cfgd profile update base --module -jarvis --module -clift"
Enter
Wait@30s /cfgd\$$/
Type "clear"
Enter
Sleep 500ms
Show
Sleep 1s

# Beat 1: the whole machine in one screen — the run that put it there (Last
# Apply), a verdict per owner (Component Health), and the inventory those
# verdicts count (Managed Resources). No clear of its own: the one the hidden
# setup ended on is still the top of the screen.
Type "cfgd status"
Enter
Wait@60s /cfgd\$$/
Sleep 5s

# Beat 2: sabotage a real module-managed file from outside cfgd. The file is
# deployed via Symlink, and GNU sed -i writes a new file and renames it over
# the link, so the target becomes a regular file holding different bytes —
# genuine drift. (A `>` redirect would write THROUGH the link onto cfgd's own
# module source, and source and target would still match.) No Wait: the
# command is local and instant, and no clear: one line is not a tall render.
Type "sed -i 's/indent_width = 2/indent_width = 4/' ~/.config/nvim/stylua.toml"
Enter
Sleep 2s

# Beat 3: the live scan finds it and records it. The finding nests under the
# owner whose verdict it changed rather than opening a section of its own.
# Cleared first: the render is taller than half the frame.
Type "clear"
Enter
Type "cfgd status --scan"
Enter
Wait@60s /cfgd\$$/
Sleep 5s

# Beat 4: the same dashboard with no --scan — the fast read, off the record
# the scan just wrote, touching neither the filesystem nor a package manager.
# Same nested finding, same owner row.
Type "clear"
Enter
Type "cfgd status"
Enter
Wait@60s /cfgd\$$/
Sleep 5s

# Beat 5: heal it. `--phase files` scopes to the phase that owns the drifted
# file — the nvim module's postApply hooks run unconditionally on every apply
# and would otherwise bury the heal under minutes of unrelated reinstall
# noise. The deploy row names every file it writes and the method each one
# lands by. No clear: thirteen lines is not a tall render.
Type "cfgd apply --module nvim --phase files --yes"
Enter
Wait@60s /cfgd\$$/
Sleep 4s

# Beat 6: converged again, at full granularity — `-o wide` blows the file
# aggregate into one row per deployed file and fills the Method column for
# every row that has one. `--scan` because a verdict is only ever as fresh as
# the check behind it: beat 5 healed the file, but the record beat 4 read was
# written by beat 3's scan and still holds that finding until something looks
# again. Cleared first: the render is taller than half the frame.
Type "clear"
Enter
Type "cfgd status -o wide --scan"
Enter
Wait@60s /cfgd\$$/
Sleep 6s

# Closing hold — the last beat's own hold already covers reading it; this is
# only the breath before the loop snaps back.
Sleep 2s
