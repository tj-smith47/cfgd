# Whole take plays at 1:1 (make-gif-flat.sh, no ramp) — this tape's claim is
# the authoring loop on an ALREADY-CONVERGED machine: discover the schema,
# change one thing, watch cfgd do exactly that one thing and then nothing.
Output "demo/.out/author/"

Require docker

Set Shell "bash"
# Same font as drift.tape — cfgd's own output (icons, spinners) renders
# Nerd Font glyphs.
Set FontFamily "FiraCode Nerd Font Mono"
Set FontSize 15
Set Width 1320
Set Height 1000
Set TypingSpeed 50ms
Set Padding 20
Set Framerate 50

# Everything up to the converged prompt is hidden: this tape starts where the
# hero demo ends. Recording resumes on an already-converged machine, which is
# what lets beats 7 and 8 say anything about idempotency.
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
# a bare Ubuntu base. Both are unrelated to this tape's claim, so they're
# stripped from the profile here, hidden, the same way the hidden setup
# already scopes convergence to nvim only.
Type "cfgd profile update base --module -jarvis --module -clift"
Enter
Wait@30s /cfgd\$$/
Type "clear"
Enter
Sleep 500ms
Show
Sleep 1s

# Beat 0: the starting point, on camera — converged machine, nvim module
# applied, no drift recorded. Without this frame beats 7/8 have no baseline.
Type "cfgd status"
Enter
Wait@60s /cfgd\$$/
Sleep 5s

# Beat 1: render the schema/field docs for a resource path — proves you can
# discover what a profile accepts without opening docs or an editor.
Type "cfgd explain profile.spec.packages"
Enter
Wait@60s /cfgd\$$/
Sleep 5s

# Beat 2: recurse into one manager key — brew is not just a list, it carries
# taps / formulae / casks / a Brewfile path, which is what beat 3 leans on.
Type "cfgd explain profile.spec.packages.brew"
Enter
Wait@60s /cfgd\$$/
Sleep 4s

# Beat 2b: the root kind, so the Docs row (a link on a terminal that opens
# one, the release-pinned URL everywhere else) lands on screen. A kind, not a
# field path — the row is a kind's; and not `--recursive`, whose 144 rows
# would scroll the Docs row off before the hold.
Type "clear"
Enter
Type "cfgd explain Config"
Enter
Wait@60s /cfgd\$$/
Sleep 5s

# Beat 3: mutate the profile from the CLI — a tap, a formula delivered
# through brew, and a shell alias, one command, no editor. The tap token
# spells the schema path beat 2 just rendered (`brew.taps`), so what is typed
# is what lands in the file. `catn` (not `k=kubectl`) because the demo image
# is bare Ubuntu with no kubectl on PATH — the alias has to be one beat 6 can
# actually run.
Type "cfgd profile update base \"
Enter
Type "    --package brew.taps:charmbracelet/tap \"
Enter
Type "    --package brew:gum \"
Enter
Type "    --alias catn='cat -n'"
Enter
Wait@60s /cfgd\$$/
Sleep 4s

# Beat 4: preview what the update queued. Beat 3's delta appears with the
# brew index refresh it needs, and the session publish this box has no
# manager for is already marked skipped — everything nvim-owned is satisfied
# and elided, so no action on screen is older than beat 3.
Type "cfgd plan"
Enter
Wait@60s /cfgd\$$/
Sleep 6s

# Beat 5: converge. The tap is added before the formula that may only exist
# inside it, then gum installs; nothing nvim-owned re-runs.
# 5m, not 60s: the tap clone and the brew index refresh are network round
# trips whose worst case is not this box's to control.
Type "cfgd apply --yes"
Enter
Wait@5m /cfgd\$$/
Sleep 6s

# Beat 6: the payoff, both halves. This shell's copy of ~/.cfgd.env predates
# the write cfgd apply just made, so re-source it first — on its own line,
# never chained with `&&` onto the alias invocation below, since bash
# resolves an alias against what was already defined when it PARSED the
# line — then prove the formula runs and the exact alias beat 3 queued
# resolves against the very file that declared it.
Type "source ~/.cfgd.env"
Enter
Wait@10s /cfgd\$$/
Sleep 2s
Type "gum --version"
Enter
Wait@10s /cfgd\$$/
Sleep 3s
Type "catn ~/.cfgd.env"
Enter
Wait@10s /cfgd\$$/
# The beat the whole authoring loop exists to land: held long enough to find
# the alias line in the numbered file.
Sleep 7s

# Beat 7: the idempotency shot — the same apply again, and cfgd says the
# whole machine is up to date in one line. Nothing re-installs, no module
# hook re-runs.
Type "cfgd apply --yes"
Enter
Wait@60s /cfgd\$$/
Sleep 5s

# Beat 8: the same status as beat 1, now carrying what the loop authored —
# the tap, the formula and the alias under the profile's own owner, beside
# the module's rows.
Type "clear"
Enter
Type "cfgd status"
Enter
Wait@60s /cfgd\$$/
Sleep 6s

# Closing hold — the last beat's own hold already covers reading it; this is
# only the breath before the loop snaps back.
Sleep 2s
