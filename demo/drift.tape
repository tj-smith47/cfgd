# Whole take plays at 1:1 (make-gif-flat.sh, no ramp) — this tape's claim is
# the catch-and-heal loop itself, so nothing in it is worth compressing away.
Output "demo/.out/drift/"

Require docker

Set Shell "bash"
# Same font as init.tape — no LazyVim glyphs on screen here, but the shell
# prompt and cfgd's own output (icons, spinners) share the terminal.
Set FontFamily "FiraCode Nerd Font Mono"
Set FontSize 15
Set Width 1320
Set Height 1500
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
Sleep 4s

# Beat 2: sabotage a real module-managed file from outside cfgd. The file is
# deployed via Symlink, and GNU sed -i writes a new file and renames it over
# the link, so the target becomes a regular file holding different bytes —
# genuine drift. (A `>` redirect would write THROUGH the link onto cfgd's
# own module source, and source and target would still match.) No Wait
# here: the command is local and instant.
Type "sed -i 's/indent_width = 2/indent_width = 4/' ~/.config/nvim/stylua.toml"
Enter
Sleep 2s

# Beat 3: the same live scan now reports it.
Type "cfgd status --module nvim --scan"
Enter
Wait@60s /cfgd\$$/
Sleep 4s

# Beat 4: the hunk. cfgd diff always runs the live scan — no --scan needed.
Type "cfgd diff --module nvim"
Enter
Wait@60s /cfgd\$$/
Sleep 4s

# Beat 5: heal it. `--phase files` scopes to the phase that owns the drifted
# file — the nvim module's postApply hooks (plugin sync, go tool installs)
# run unconditionally on every apply and would otherwise bury the heal under
# minutes of unrelated reinstall noise that has nothing to do with the file
# this beat is about.
Type "cfgd apply --module nvim --phase files --yes"
Enter
Wait@60s /cfgd\$$/
Sleep 4s

# Beat 6: clean again.
Type "cfgd status --module nvim --scan"
Enter
Wait@60s /cfgd\$$/
Sleep 4s

# Closing hold — the last beat's own hold already covers reading it; this is
# only the breath before the loop snaps back.
Sleep 2s
