# Whole take plays at 1:1 (make-gif-flat.sh, no ramp) — this tape's claim is
# the source-trust boundary itself, so nothing in it is worth compressing away.
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

# Everything up to the converged prompt is hidden: this tape's claim is the
# subscription trust boundary (a source demanding signed commits, refused
# unsigned and accepted signed), not the install that gets a machine to a
# starting point — that is init.tape's job. Recording resumes on an
# already-converged machine, with the nvim module already applied.
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
# both are unrelated to this tape's claim (the source-trust boundary), so
# showing cfgd correctly refuse them would just be noise.
Type "cfgd profile update base --module -jarvis --module -clift"
Enter
Wait@30s /cfgd\$$/

# Fixture: a small local "team" ConfigSource — a git repo carrying a
# cfgd-source.yaml manifest and a `team` profile (one env var, one npm
# package), the same shape the acceptance suite's S7.2 fixtures use. Its
# first commit is deliberately left UNSIGNED — the demo's refusal beat needs
# a real unsigned HEAD to reject, not a staged error. Setup only, never shown.
Type "mkdir -p ~/team-config/profiles && printf '%s\n' 'apiVersion: cfgd.io/v1alpha1' 'kind: ConfigSource' 'metadata:' '  name: team' '  version: 1.0.0' '  description: Team base config' 'spec:' '  provides:' '    profiles:' '      - team' > ~/team-config/cfgd-source.yaml && printf '%s\n' 'apiVersion: cfgd.io/v1alpha1' 'kind: Profile' 'metadata:' '  name: team' 'spec:' '  modules: []' '  env:' '    - name: EDITOR' '      value: vim' '  packages:' '    npm:' '      - lodash' > ~/team-config/profiles/team.yaml && (cd ~/team-config && git init -q && git config user.email team@jarvispro.io && git config user.name 'Team Config' && git add -A && git commit -q -m 'initial team config')"
Enter
Wait@30s /cfgd\$$/

# A real gpg key, generated in-container, so the "fix" beat later signs with
# a real signature cfgd's own `git log --format=%G?` check has to verify —
# never a staged/faked signed-status string. `%no-protection` skips the
# passphrase prompt, appropriate for a throwaway demo key. Configuring the
# fixture repo to sign with it is setup, not the claim being demonstrated.
Type "mkdir -p ~/.gnupg && chmod 700 ~/.gnupg && printf '%s\n' '%no-protection' 'Key-Type: eddsa' 'Key-Curve: ed25519' 'Subkey-Type: eddsa' 'Subkey-Curve: ed25519' 'Name-Real: Team Config' 'Name-Email: team@jarvispro.io' 'Expire-Date: 0' > ~/gpgkey.conf && gpg --batch --generate-key ~/gpgkey.conf && rm -f ~/gpgkey.conf"
Enter
Wait@30s /cfgd\$$/
Type "(cd ~/team-config && git config user.signingkey $(gpg --list-secret-keys --keyid-format=long | awk '/^sec/{print $2}' | cut -d/ -f2) && git config commit.gpgsign true)"
Enter
Wait@10s /cfgd\$$/

# Subscribe to the fixture over file:// — CFGD_ALLOW_LOCAL_SOURCES is the
# proven acceptance pattern for a local-git fixture (see cfgd-acceptance's
# S7.2), exported hidden so it never appears on camera: a real subscriber
# would never need it for an actual remote source. `source add` has no flag
# for `subscription.requireSignedCommits`, so the demand itself is a direct,
# hidden edit of the written cfgd.yaml (docs/sources.md "Demanding signed
# commits" — no CLI verb exists to set it).
Type "export CFGD_ALLOW_LOCAL_SOURCES=1 && cfgd source add file://$HOME/team-config --name team --branch master --profile team --yes && sed -i 's/requireSignedCommits: false/requireSignedCommits: true/' ~/.config/cfgd/cfgd.yaml"
Enter
Wait@10s /cfgd\$$/
Type "clear"
Enter
Sleep 500ms
Show

# Beat 1: the demand. `cfgd source show team` renders no subscription/policy
# field for requireSignedCommits (verified live — SourceShowOutput carries
# no such field), so the honest, short beat is the written YAML itself:
# the sources block of cfgd.yaml, sliced between its own `sources:` and
# `theme:` keys so the demand sits alone on screen.
Type "awk '/^  sources:/{p=1} /^  theme:/{p=0} p' ~/.config/cfgd/cfgd.yaml"
Enter
Wait@10s /cfgd\$$/
Sleep 500ms

# Beat 2: the demo's core. `cfgd sync` really fetches the fixture, really
# runs `git log --format=%G?` against its HEAD, and really refuses — the
# unsigned-HEAD message below is cfgd's own, unedited. Held for 3s (longer
# than every other beat's 500ms) because the refusal line is the whole
# claim and must be legible on its own before the screen clears for the fix.
Type "cfgd sync"
Enter
Wait@10s /cfgd\$$/
Sleep 3s

# Beat 3: the fix. A brand-new signed commit (never an amend — cfgd's fetch
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
Sleep 500ms

# Beat 4: the payoff. `cfgd source list` is the concise, honest render of a
# healthy subscription — `cfgd status` also proves it but buries the source
# row under the module's full managed-resource table, which has nothing to
# do with this tape's claim.
Type "cfgd source list"
Enter
Wait@10s /cfgd\$$/
Sleep 500ms

# Closing hold — breathing room before the loop snaps back to the opening
# frame, matching drift.tape/author.tape/backup.tape's closing beat.
Sleep 8s
