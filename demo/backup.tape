# Whole take plays at 1:1 (make-gif-flat.sh, no ramp) — this tape's claim is
# the backup/tamper/restore loop itself, so nothing in it is worth compressing
# away.
Output "demo/.out/backup.mp4"

Require docker

Set Shell "bash"
# Same font as drift.tape/author.tape — no LazyVim glyphs on screen here, but
# the shell prompt and cfgd's own output (icons, spinners) share the terminal.
Set FontFamily "FiraCode Nerd Font Mono"
Set FontSize 15
Set Width 1320
Set Height 1000
Set TypingSpeed 50ms
Set Padding 20
Set Framerate 50

# Everything up to the converged prompt is hidden: this tape's claim is
# declarative backups, not the install that gets a machine to a starting
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
# Same ceiling as drift.tape's hidden install — see that tape's comment for
# why 60m covers even a worst-case provision.
Wait@60m /cfgd\$$/
Type "source ~/.cfgd.env"
Enter
Wait@30s /cfgd\$$/

# The real tj-smith47/cfgd-config `base` profile also declares two modules
# this container can never converge (jarvis: private repo, no SSH credentials
# baked in; clift: needs charm's apt repo, not present on a bare Ubuntu
# base) — stripped here for the same reason author.tape strips them: both are
# unrelated to this tape's claim (declarative backups), so showing cfgd
# correctly refuse them would just be noise.
Type "cfgd profile update base --module -jarvis --module -clift"
Enter
Wait@30s /cfgd\$$/

# The cloned config declares no spec.backups — there is no CLI verb to add
# one (cfgd profile update carries no --backup flag), so this is a direct,
# hidden edit of the profile YAML: a small legible data file, then a single
# backups entry naming it. Both edits are setup, never shown.
Type "printf '%s\n' '# Notes' '' '- Buy milk' '- Pay rent' '- Ship the demo' > ~/notes.md"
Enter
Wait@10s /cfgd\$$/
Type "sed -i 's/  backups: \[\]/  backups:\n  - name: notes\n    source: ~\/notes.md\n    retention: 5/' ~/.config/cfgd/profiles/base.yaml"
Enter
Wait@10s /cfgd\$$/
Type "clear"
Enter
Sleep 500ms
Show

# Beat 1: the declaration. cfgd backup list reads the recorded runs, so a
# never-run backup shows its source/retention with "never" in Last Run.
Type "cfgd backup list"
Enter
Wait@10s /cfgd\$$/
Sleep 500ms

# Beat 2: take the snapshot. The output names the stamped snapshot it wrote.
Type "cfgd backup run"
Enter
Wait@10s /cfgd\$$/
Sleep 500ms

# Beat 3: tamper from outside cfgd, then show the damage. Combined into one
# line — echo prints nothing itself, so the visible output is entirely cat's.
Type "echo 'oops' > ~/notes.md && cat ~/notes.md"
Enter
Wait@10s /cfgd\$$/
Sleep 500ms

# Beat 4: the snapshot inventory — what restore can put back.
Type "cfgd backup list notes --snapshots"
Enter
Wait@10s /cfgd\$$/
Sleep 500ms

# Beat 5: the payoff. --yes skips the confirmation prompt for a clean,
# deterministic recording (the CLI reference's own examples restore this
# way); cat proves the original content is back.
Type "cfgd backup restore notes --yes && cat ~/notes.md"
Enter
Wait@10s /cfgd\$$/
Sleep 500ms

# Closing hold — breathing room before the loop snaps back to the opening
# frame, matching drift.tape/author.tape's closing beat.
Sleep 8s
