# Whole take plays at 1:1 (make-gif-flat.sh, no ramp) — this tape's claim is
# the CLI-authoring loop itself, so nothing in it is worth compressing away.
Output "demo/.out/author.mp4"

Require docker

Set Shell "bash"
# Same font as drift.tape — no LazyVim glyphs on screen here, but the shell
# prompt and cfgd's own output (icons, spinners) share the terminal.
Set FontFamily "FiraCode Nerd Font Mono"
Set FontSize 15
Set Width 1320
Set Height 1000
Set TypingSpeed 50ms
Set Padding 20
Set Framerate 50

# Everything up to the converged prompt is hidden: this tape's claim is
# authoring a profile from the CLI, not the install that gets a machine to a
# starting point. Recording resumes on an already-converged machine.
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
# this container can never converge: `jarvis` is a private repo with no SSH
# credentials baked into the demo image (cfgd correctly refuses it — that's
# not a bug, there's nothing to authenticate with), and `clift` installs `gum`
# via plain apt, which needs charm's apt repo configured and isn't present on
# a bare Ubuntu base. Both are unrelated to this tape's claim (authoring
# packages/aliases via the CLI), so they're stripped from the profile here,
# hidden, the same way the hidden setup already scopes convergence to nvim
# only rather than showing the full (irrelevant) profile install.
Type "cfgd profile update base --module -jarvis --module -clift"
Enter
Wait@30s /cfgd\$$/
Type "clear"
Enter
Sleep 500ms
Show

# Beat 1: render the schema/field docs for a resource path — proves you can
# discover what a profile accepts without opening docs or an editor.
Type "cfgd explain profile.spec.packages"
Enter
Wait@60s /cfgd\$$/
Sleep 500ms

# Beat 2: mutate the profile from the CLI — add a package and a shell alias
# in one command, no editor. `base` is the real profile name in the cloned
# config, verified via `cfgd profile list` during validation.
Type "cfgd profile update base --package brew:jq --alias k=kubectl"
Enter
Wait@60s /cfgd\$$/
Sleep 500ms

# Beat 3: preview what the update queued. The plan also carries nvim's full
# declared package/file/post-script set alongside the new jq install — nvim
# is already fully converged (verified via `brew list`/`dpkg -l` matching
# every pinned version), but `cfgd plan`'s package-phase rendering shows a
# module's whole declared set on every run rather than eliding satisfied
# packages the way it does for profile-level ones (`brew install jq` here
# correctly disappears once jq is actually installed — module packages don't
# get that same diff). Real, accurate, unedited cfgd output either way; the
# new profile:base package/alias-carrying work is the top of the Packages
# phase.
Type "cfgd plan"
Enter
Wait@60s /cfgd\$$/
Sleep 500ms

# Beat 4: converge. Everything nvim-owned no-ops (already installed/deployed,
# confirmed during validation); the only real work is the jq install this
# tape's beat 2 queued.
Type "cfgd apply --yes"
Enter
Wait@60s /cfgd\$$/
Sleep 500ms

# Beat 5: the payoff. The alias needs a re-sourced shell to reach this same
# session, so the visible proof is the package, not the alias invocation —
# faking `k get pods` here would be showing something that didn't happen.
Type "jq --version"
Enter
Wait@60s /cfgd\$$/
Sleep 500ms

# Closing hold — breathing room before the loop snaps back to the opening
# frame, matching drift.tape's closing beat.
Sleep 8s
