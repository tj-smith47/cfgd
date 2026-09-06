# Whole take plays at 1:1 (make-gif-flat.sh, no ramp) — this tape's claim is
# the catch-and-heal loop itself, so nothing in it is worth compressing away.
#
# Dwell rule, applied to every visible beat below: a hold is 1.5s plus 0.1s per
# line the beat printed, clamped to [2s, 6s]; the one payoff beat (beat 5, the
# heal) gets +1.5s on top; a beat whose output is a single line holds 2s; a
# typed line that prints nothing holds 1s. A reader needs time in proportion to
# what landed on screen, so each Sleep below carries the line count it was
# computed from, measured off a real render of that verb rather than guessed.
# The closing hold reads nothing and stays 2s.
Output "demo/.out/drift/"

Require docker

Set Shell "bash"
# Same font as init.tape — no LazyVim glyphs on screen here, but the shell
# prompt and cfgd's own output (icons, spinners) share the terminal.
Set FontFamily "FiraCode Nerd Font Mono"
Set FontSize 15
Set Width 1320
# 1320x820 at font size 15 measures 125x41. The grid is sized to the tallest
# single render below — the closing `cfgd status -o wide`, 37 rows including
# the line that typed it — plus four rows of headroom, because both
# machine-wide renders are preceded by a clear and so never have to share the
# frame with what came before them. The beats between the two clears do
# accumulate past 41 rows and the earlier ones scroll away; each one has
# already had its own hold by then, and every beat's own output is whole on
# screen while it is the beat being read. A taller grid only pads the closing
# frame with empty background.
Set Height 820
Set TypingSpeed 50ms
Set Padding 20
Set Framerate 50

# Everything up to the converged prompt is hidden: this tape's claim is drift
# detection and healing, not the install that gets a machine to a starting
# point — that is init.tape's job. Recording resumes on an already-converged
# machine, with the nvim module already applied.
Hide
Type "clear"
Enter

Type "docker run --rm -it --hostname jarvispro cfgd-demo:latest bash -l"
Enter
Wait@2m /cfgd\$$/
Sleep 1s

Type "cfgd init --from tj-smith47/cfgd-config --apply-module nvim --theme dracula --yes"
Enter
# Same ceiling as init.tape's hidden install — see that tape's comment for
# why 60m covers even a worst-case provision.
Wait@60m /cfgd\$$/
Type "source ~/.cfgd.env"
Enter
Wait@30s /cfgd\$$/

# The nvim module leaves this container with two npms (brew's node and apt's
# npm). The hidden init installed neovim/yarn through apt's, and sourcing
# ~/.cfgd.env put brew's first on PATH, so every visible scan below would
# otherwise report both packages `not installed` under the npm the shell now
# uses — package drift this tape is not about, on a take whose opening beat
# claims a converged machine. Re-running the package phase alone installs
# them through brew's npm; `--phase packages` keeps the module's postApply
# hooks out of it.
Type "cfgd apply --module nvim --phase packages --yes"
Enter
Wait@30m /cfgd\$$/

# The real tj-smith47/cfgd-config `base` profile also declares two modules
# this container can never converge (jarvis: private repo, no SSH credentials
# baked in; clift: needs charm's apt repo, not present on a bare Ubuntu
# base) — stripped here for the same reason author.tape/backup.tape strip
# them: both are unrelated to this tape's claim (drift detection and
# healing), and `cfgd diff --module nvim`'s Env phase resolves the whole
# active profile, so an unstripped jarvis/clift would surface as noise
# alongside the nvim drift this tape is actually about.
Type "cfgd profile update base --module -jarvis --module -clift"
Enter
Wait@30s /cfgd\$$/
Type "clear"
Enter
Sleep 500ms
Show
Sleep 1s

# Beat 1: a converged machine. `--scan` runs the live scan (content +
# package), not the fast recorded-only dashboard `status` gives by default —
# the same distinction `cfgd diff`/`verify` already draw, and the only way
# this command can claim "no drift" rather than just "nothing recorded yet".
# The live scan genuinely walks 28 packages plus every deployed file, so it
# costs real wall time (measured ~3s in the demo container) — every beat
# below waits on the returning prompt rather than guessing a fixed Sleep,
# because a flat Sleep shorter than that cost queues the NEXT beat's
# keystrokes into the terminal before this one finishes, garbling the
# recording.
Type "cfgd status --module nvim --scan"
Enter
Wait@60s /cfgd\$$/
# 13 lines → 2.8s
Sleep 2800ms

# Beat 2: sabotage a real module-managed file from outside cfgd. The file is
# deployed via Symlink, and GNU sed -i writes a new file and renames it over
# the link, so the target becomes a regular file holding different bytes —
# genuine drift. (A `>` redirect would write THROUGH the link onto cfgd's
# own module source, and source and target would still match.) No Wait
# here: the command is local and instant.
Type "sed -i 's/indent_width = 2/indent_width = 4/' ~/.config/nvim/stylua.toml"
Enter
# prints nothing → 1s
Sleep 1s

# Beat 3: the same live scan now reports it.
Type "cfgd status --module nvim --scan"
Enter
Wait@60s /cfgd\$$/
# 14 lines → 2.9s
Sleep 2900ms

# Beat 3b: the whole machine, off the record the scan just wrote — no --scan,
# so nothing is re-read from the filesystem or a package manager, and the
# finding still nests under the owner whose verdict it changed. Cleared first:
# a machine-wide dashboard is taller than half the frame.
Type "clear"
Enter
Type "cfgd status"
Enter
Wait@60s /cfgd\$$/
# 31 lines → 4.6s
Sleep 4600ms

# Beat 4: the hunk. cfgd diff always runs the live scan — no --scan needed.
Type "cfgd diff --module nvim"
Enter
Wait@60s /cfgd\$$/
# 15 lines → 3.0s
Sleep 3s

# Beat 5: heal it. `--phase files` scopes to the phase that owns the drifted
# file — the nvim module's postApply hooks (plugin sync, go tool installs)
# run unconditionally on every apply and would otherwise bury the heal under
# minutes of unrelated reinstall noise that has nothing to do with the file
# this beat is about.
Type "cfgd apply --module nvim --phase files --yes"
Enter
Wait@60s /cfgd\$$/
# 10 lines → 2.5s, +1.5s as this tape's payoff beat
Sleep 4s

# Beat 6: clean again.
Type "cfgd status --module nvim --scan"
Enter
Wait@60s /cfgd\$$/
# 13 lines → 2.8s
Sleep 2800ms

# Beat 7: the healed machine at full granularity — `-o wide` blows the file
# aggregate into one row per deployed file and fills the Method column for
# every row that has one. No `--scan`: the apply that healed the file resolved
# that file's own drift row on its way out, so the fast read off the record is
# already the converged truth. Cleared first, same reason as beat 3b.
Type "clear"
Enter
Type "cfgd status -o wide"
Enter
Wait@60s /cfgd\$$/
# 37 lines → 5.2s
Sleep 5200ms

# Closing hold — the last beat's own hold already covers reading it; this is
# only the breath before the loop snaps back.
Sleep 2s
