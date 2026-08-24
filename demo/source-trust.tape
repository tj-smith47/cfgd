# Whole take plays at 1:1 (make-gif-flat.sh, no ramp) — this tape's claim is
# the subscription lifecycle and the source-trust boundary, so nothing in it
# is worth compressing away. Two acts: first the happy path (subscribe, pull
# an upstream change, review it, accept it, apply it), then the trust wall
# (the same source demanding signed commits, refused unsigned and accepted
# signed).
Output "demo/.out/source-trust.mp4"

Require docker

Set Shell "bash"
# Same font as drift.tape/author.tape/backup.tape — no LazyVim glyphs on
# screen here, but the shell prompt and cfgd's own output (icons, spinners)
# share the terminal.
Set FontFamily "FiraCode Nerd Font Mono"
Set FontSize 15
Set Width 1320
Set Height 1000
Set TypingSpeed 50ms
Set Padding 20
Set Framerate 50

# Everything up to the converged prompt is hidden: this tape's claim starts at
# `cfgd source add`, not at the install that gets a machine to a starting
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
# base) — stripped here for the same reason drift/author/backup.tape do:
# both are unrelated to this tape's claim (the subscription lifecycle), so
# showing cfgd correctly refuse them would just be noise.
Type "cfgd profile update base --module -jarvis --module -clift"
Enter
Wait@30s /cfgd\$$/

# The decision flow's opt-in is the machine's own reconcile policy
# (`spec.daemon.reconcile.autoApply` — docs/sources.md "Auto-apply policy"),
# NOT `source add --auto-apply`, which sets the per-source sync flag the
# daemon reads when deciding whether to apply a refreshed source. With the
# policy on, every incoming source item is classified (default tier: Notify)
# and queues as a pending decision that `cfgd plan`/`cfgd apply` withhold
# until answered. Set hidden: policy configuration is machine setup, not this
# tape's claim.
Type "cfgd config set daemon.reconcile.autoApply true"
Enter
Wait@10s /cfgd\$$/

# Fixture: a small local "team" ConfigSource — a git repo carrying a
# cfgd-source.yaml manifest and a `team` profile (env vars only: a package
# install is minutes of screen time about package managers, not about
# subscriptions), the same shape the acceptance suite's S7.2 fixtures use.
# Its first commit is deliberately left UNSIGNED — act two's refusal beat
# needs a real unsigned HEAD to reject, not a staged error. Setup only,
# never shown.
Type "mkdir -p ~/team-config/profiles && printf '%s\n' 'apiVersion: cfgd.io/v1alpha1' 'kind: ConfigSource' 'metadata:' '  name: team' '  version: 1.0.0' '  description: Team base config' 'spec:' '  provides:' '    profiles:' '      - team' > ~/team-config/cfgd-source.yaml && printf '%s\n' 'apiVersion: cfgd.io/v1alpha1' 'kind: Profile' 'metadata:' '  name: team' 'spec:' '  modules: []' '  env:' '    - name: EDITOR' '      value: vim' > ~/team-config/profiles/team.yaml && (cd ~/team-config && git init -q && git config user.email team@jarvispro.io && git config user.name 'Team Config' && git add -A && git commit -q -m 'initial team config')"
Enter
Wait@30s /cfgd\$$/

# A real gpg key, generated in-container, so act two's "fix" beat signs with
# a real signature cfgd's own `git log --format=%G?` check has to verify —
# never a staged/faked signed-status string. `%no-protection` skips the
# passphrase prompt, appropriate for a throwaway demo key. Configuring the
# fixture repo to sign with it is setup, not the claim being demonstrated;
# act one's upstream commit opts out explicitly so HEAD is still genuinely
# unsigned when the demand flips on.
Type "mkdir -p ~/.gnupg && chmod 700 ~/.gnupg && printf '%s\n' '%no-protection' 'Key-Type: eddsa' 'Key-Curve: ed25519' 'Subkey-Type: eddsa' 'Subkey-Curve: ed25519' 'Name-Real: Team Config' 'Name-Email: team@jarvispro.io' 'Expire-Date: 0' > ~/gpgkey.conf && gpg --batch --generate-key ~/gpgkey.conf && rm -f ~/gpgkey.conf"
Enter
Wait@30s /cfgd\$$/
Type "(cd ~/team-config && git config user.signingkey $(gpg --list-secret-keys --keyid-format=long | awk '/^sec/{print $2}' | cut -d/ -f2) && git config commit.gpgsign true)"
Enter
Wait@10s /cfgd\$$/

# CFGD_ALLOW_LOCAL_SOURCES is the proven acceptance pattern for a local-git
# fixture (see cfgd-acceptance's S7.2), exported hidden so it never appears
# on camera: a real subscriber would never need it for an actual remote
# source.
Type "export CFGD_ALLOW_LOCAL_SOURCES=1"
Enter
Wait@10s /cfgd\$$/
Type "clear"
Enter
Sleep 500ms
Show

# ── Act one: living with a subscription ────────────────────────────────────

# Beat 1: subscribe, on camera. The machine's reconcile policy (set in the
# hidden setup) classifies everything this source delivers at the default
# Notify tier, so incoming items queue as pending decisions instead of being
# silently absorbed.
Type "cfgd source add file://$HOME/team-config --name team --branch master --profile team --yes"
Enter
Wait@30s /cfgd\$$/
Sleep 4s

# Hidden: the team ships a change upstream — a second env var in the team
# profile. `-c commit.gpgsign=false` because the signing demand is act two's
# story; HEAD must still be unsigned when it flips on.
Hide
Type "printf '%s\n' 'apiVersion: cfgd.io/v1alpha1' 'kind: Profile' 'metadata:' '  name: team' 'spec:' '  modules: []' '  env:' '    - name: EDITOR' '      value: vim' '    - name: PAGER' '      value: less' > ~/team-config/profiles/team.yaml && (cd ~/team-config && git -c commit.gpgsign=false commit -qam 'team: default PAGER')"
Enter
Wait@10s /cfgd\$$/
Type "clear"
Enter
Sleep 500ms
Show

# Beat 2: the pull. `cfgd sync` fetches the source and reports the movement
# as `commit: <old> → <new>` — the subscription saw upstream change.
Type "cfgd sync"
Enter
Wait@30s /cfgd\$$/
Sleep 4s

# Beat 3: what arrived, before it touches the machine. `cfgd decide` lists
# every source item still awaiting an answer — the initial delivery's EDITOR
# and the pull's new PAGER both list, because nothing has been accepted yet
# and plan/apply withhold an unanswered item however it arrived.
Type "cfgd decide"
Enter
Wait@10s /cfgd\$$/
Sleep 5s

# Beat 4: accept the batch, then apply. Accepting records the decisions;
# apply is what actually moves the machine.
Type "cfgd decide accept --all"
Enter
Wait@10s /cfgd\$$/
Sleep 3s
Type "cfgd apply --yes"
Enter
Wait@60s /cfgd\$$/
Sleep 4s

# Beat 5: the subscription at rest — synced, healthy, applied.
Type "cfgd source list"
Enter
Wait@10s /cfgd\$$/
Sleep 4s

# ── Act two: the trust wall ────────────────────────────────────────────────

# Hidden scene cut: flip the subscription's signing demand on. `source add`
# has no flag for `subscription.requireSignedCommits`, so the demand is a
# direct edit of the written cfgd.yaml (docs/sources.md "Demanding signed
# commits" — no CLI verb exists to set it).
Hide
Type "sed -i 's/requireSignedCommits: false/requireSignedCommits: true/' ~/.config/cfgd/cfgd.yaml && clear"
Enter
Sleep 500ms
Show

# Beat 6: the demand, from the far side. HEAD is unsigned, so `cfgd source
# show team` cannot even load the manifest to render a policy — verification
# runs before that, and refuses. The refusal line names the demand on its
# own: "source requires signed commits". The successful policy render
# (Require Signed Commits: true) is beat 8b, once the fix lands and the same
# manifest actually resolves.
Type "cfgd source show team"
Enter
Wait@10s /cfgd\$$/
Sleep 4s

# Beat 7: the wall itself. `cfgd sync` really fetches the fixture, really
# runs `git log --format=%G?` against its HEAD, and really refuses — the
# unsigned-HEAD message below is cfgd's own, unedited. Held longer than the
# other beats because the refusal line is the whole claim and must be
# legible on its own before the screen clears for the fix.
Type "cfgd sync"
Enter
Wait@10s /cfgd\$$/
Sleep 6s

# Beat 8: the fix. A brand-new signed commit (never an amend — cfgd's fetch
# only fast-forwards the cache, and an amended HEAD is a diverged rewrite the
# fast-forward check silently leaves stale; a real second commit on top of
# the unsigned HEAD keeps the history linear). Hidden, since authoring a
# signed commit is normal git, not this tape's claim — `clear` before `Show`
# because VHS's `Hide` only pauses recording, it does not erase the
# terminal's own scrollback, so the hidden command would otherwise reappear
# the instant recording resumes.
Hide
Type "(cd ~/team-config && git commit --allow-empty -S -q -m 'sign HEAD')"
Enter
Wait@10s /cfgd\$$/
Type "clear"
Enter
Sleep 500ms
Show
Type "cfgd sync"
Enter
Wait@10s /cfgd\$$/
Sleep 4s

# Beat 8b: the render beat 6 couldn't reach. Same command, now that the
# manifest verifies — `source show` resolves the subscription policy
# directly, including `Require Signed Commits: true`, the field QP13 added
# and the reason this tape stopped slicing the written YAML with awk.
Type "cfgd source show team"
Enter
Wait@10s /cfgd\$$/
Sleep 4s

# Beat 9: the payoff — the arc closes where act one rested: a healthy
# subscription, now behind a signing demand it satisfies. `cfgd status` also
# proves it but buries the source row under the module's full
# managed-resource table, which has nothing to do with this tape's claim.
Type "cfgd source list"
Enter
Wait@10s /cfgd\$$/
Sleep 4s

# Closing hold — breathing room before the loop snaps back to the opening
# frame, matching drift.tape/author.tape/backup.tape's closing beat.
Sleep 8s
