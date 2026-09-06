# Whole take plays at 1:1 (make-gif-flat.sh, no ramp) — this tape's claim is
# the authoring loop on an ALREADY-CONVERGED machine: discover the schema,
# change one thing, watch cfgd do exactly that one thing and then nothing.
#
# Dwell rule, applied to every visible beat below: a hold is 1.5s plus 0.1s per
# line the beat printed, clamped to [2s, 6s]; the one payoff beat (beat 6's
# `catn`, the line the authoring loop exists to land) gets +1.5s on top; a beat
# whose output is a single line holds 2s; a typed line that prints nothing
# holds 1s. A reader needs time in proportion to what landed on screen, so each
# Sleep below carries the line count it was computed from, measured off a real
# render of that verb rather than guessed. The closing hold reads nothing and
# stays 2s.
Output "demo/.out/author/"

Require docker

Set Shell "bash"
# Same font as drift.tape — cfgd's own output (icons, spinners) renders
# Nerd Font glyphs.
Set FontFamily "FiraCode Nerd Font Mono"
Set FontSize 15
Set Width 1320
# 1320x914 at font size 15 measures 125x46. The grid is sized to the tallest
# single render below — the closing `cfgd status -o wide`, 42 rows including
# the line that typed it, the plain dashboard's rows with the file aggregate
# blown into one row per deployed file — plus four rows of headroom. That beat
# is preceded by a clear, so it never shares the frame with the authoring loop
# above it; the loop's own beats scroll as they go, each whole on screen while
# it is the beat being read. A taller grid only pads the closing frame with
# empty background.
Set Height 914
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
# 31 lines → 4.6s
Sleep 4600ms

# Beat 1: render the schema/field docs for a resource path — proves you can
# discover what a profile accepts without opening docs or an editor.
Type "cfgd explain profile.spec.packages"
Enter
Wait@60s /cfgd\$$/
# 27 lines → 4.2s
Sleep 4200ms

# Beat 2: recurse into one manager key — brew is not just a list, it carries
# taps / formulae / casks / a Brewfile path, which is what beat 3 leans on.
Type "cfgd explain profile.spec.packages.brew"
Enter
Wait@60s /cfgd\$$/
# 14 lines → 2.9s
Sleep 2900ms

# Beat 2b: the same field, recursively, so every shape under `brew` and the
# field's own Docs row (a link on a terminal that opens one, the release-
# pinned URL everywhere else) land on one screen. A field path rather than the
# root kind: the row is anchored at the `spec.packages.brew` heading of the
# spec page, and `brew`'s subtree is short enough to hold.
Type "clear"
Enter
Type "cfgd explain profile.spec.packages.brew --recursive"
Enter
Wait@60s /cfgd\$$/
# 18 lines → 3.3s
Sleep 3300ms

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
# 8 lines → 2.3s
Sleep 2300ms

# Beat 4: preview what the update queued. Beat 3's delta appears with the
# brew index refresh it needs, and the session publish this box has no
# manager for is already marked skipped — everything nvim-owned is satisfied
# and elided, so no action on screen is older than beat 3.
Type "cfgd plan"
Enter
Wait@60s /cfgd\$$/
# 22 lines → 3.7s
Sleep 3700ms

# Beat 5: converge. The tap is added before the formula that may only exist
# inside it, then gum installs; nothing nvim-owned re-runs.
# 5m, not 60s: the tap clone and the brew index refresh are network round
# trips whose worst case is not this box's to control.
Type "cfgd apply --yes"
Enter
Wait@5m /cfgd\$$/
# 29 lines → 4.4s
Sleep 4400ms

# Beat 6: the payoff, both halves. This shell's copy of ~/.cfgd.env predates
# the write cfgd apply just made, so re-source it first — on its own line,
# never chained with `&&` onto the alias invocation below, since bash
# resolves an alias against what was already defined when it PARSED the
# line — then prove the formula runs and the exact alias beat 3 queued
# resolves against the very file that declared it.
Type "source ~/.cfgd.env"
Enter
Wait@10s /cfgd\$$/
# prints nothing → 1s
Sleep 1s
Type "gum --version"
Enter
Wait@10s /cfgd\$$/
# 1 line → 2s
Sleep 2s
Type "catn ~/.cfgd.env"
Enter
Wait@10s /cfgd\$$/
# The beat the whole authoring loop exists to land: 9 lines (8 numbered, the
# PATH line wrapping onto a second row) → 2.4s, +1.5s as the payoff beat.
Sleep 3900ms

# Beat 7: the idempotency shot — the same apply again, and cfgd says the
# whole machine is up to date in one line. Nothing re-installs, no module
# hook re-runs.
Type "cfgd apply --yes"
Enter
Wait@60s /cfgd\$$/
# 6 lines → 2.1s
Sleep 2100ms

# Beat 8: the same status as beat 0, now carrying what the loop authored —
# the tap, the formula and the alias under the profile's own owner, beside the
# module's rows — and at full granularity: `-o wide` blows the file aggregate
# into one row per deployed file and fills the Method column for every row that
# has one.
Type "clear"
Enter
Type "cfgd status -o wide"
Enter
Wait@60s /cfgd\$$/
# ~39 lines → 5.4s
Sleep 5400ms

# Closing hold — the last beat's own hold already covers reading it; this is
# only the breath before the loop snaps back.
Sleep 2s
