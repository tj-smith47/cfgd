#!/usr/bin/env bash
# Stand up (and tear down) the two-machine environment sync.tape records
# against: a bare git hub plus two containers ("machine-a", "machine-b")
# initialized from it, each running `cfgd daemon run` with autoPull sync.
#
# The recording itself shows none of this — the tape starts with both
# machines already converged, the same way author.tape hides its init.
#
# Phases (so a re-record can reset cheaply without re-provisioning):
#   hub      — clone tj-smith47/cfgd-config, pin the daemon/sync config and
#              the nvim-only profile, publish it as a bare repo the
#              containers reach at /srv/git/config.git
#   seed     — one full `cfgd init --from ... --apply-module nvim` in a
#              throwaway container, committed as cfgd-sync-machine:latest.
#              This is the slow phase (brew bootstrap + the nvim module's
#              whole provision) and it runs once, not once per machine.
#   machines — start machine-a and machine-b from the seed image and start
#              each one's daemon (log at ~/daemon.log)
#   reset    — rewind the hub to its recorded initial commit and recreate
#              the machines from the seed image: a clean slate for another
#              take without paying the seed provision again. The hub is
#              rewound, never rebuilt — the seed image's clone references
#              the original commit, and a rebuilt hub would orphan it.
#   down     — remove containers, the seed image, and the scratch tree
#
#   up       — hub + seed + machines
set -euo pipefail

SCRATCH="${CFGD_SYNC_DEMO_SCRATCH:-$HOME/.cache/cfgd-debug/sync-demo}"
HUB="$SCRATCH/hub"
WORK="$SCRATCH/work"
SEED_IMAGE="cfgd-sync-machine:latest"
BASE_IMAGE="cfgd-demo:latest"
MACHINES=(machine-a machine-b)

# Host-side git against the hub, run as the hub's own uid (1000) inside a
# throwaway container: the host user is root, and root's git refuses a repo
# owned by someone else ("dubious ownership") for exactly the right reasons.
hub_git() {
    docker run --rm -v "$HUB:/srv/git" "$BASE_IMAGE" \
        git --git-dir=/srv/git/config.git "$@"
}

phase_hub() {
    rm -rf "$WORK" "$HUB"
    mkdir -p "$SCRATCH"
    git clone --quiet https://github.com/tj-smith47/cfgd-config "$WORK"

    # The daemon/sync stanza is the demo's subject: both machines pull the
    # hub every 5s and auto-reconcile what arrives. autoPush stays off so
    # machine A's on-camera `git commit`/`git push` is the only writer —
    # with it on, A's own daemon races the typed commit and wins.
    cat > "$WORK/cfgd.yaml" <<'EOF'
apiVersion: cfgd.io/v1alpha1
kind: Config
metadata:
  name: cfgd-config
spec:
  profile: base
  theme: dracula
  daemon:
    enabled: true
    sync:
      autoPull: true
      interval: 5s
    reconcile:
      interval: 30s
      onChange: true
      autoApply: true
      driftPolicy: Auto
EOF

    # nvim only: jarvis needs SSH credentials no demo container holds and
    # clift needs charm's apt repo — the same strip author.tape performs.
    cat > "$WORK/profiles/base.yaml" <<'EOF'
apiVersion: cfgd.io/v1alpha1
kind: Profile
metadata:
  name: base
spec:
  modules:
    - nvim
EOF

    git -C "$WORK" commit --quiet -am "demo: nvim-only profile with 5s autoPull sync"
    git clone --quiet --bare "$WORK" "$HUB/config.git"
    git -C "$HUB/config.git" rev-parse HEAD > "$SCRATCH/hub-initial-commit"

    # Containers run as uid 1000 (tj) and machine A pushes into the hub.
    chown -R 1000:1000 "$HUB"
    echo "hub ready at $HUB/config.git ($(cat "$SCRATCH/hub-initial-commit"))"
}

phase_seed() {
    docker rm -f cfgd-sync-seed >/dev/null 2>&1 || true
    docker run -d --name cfgd-sync-seed -v "$HUB:/srv/git" "$BASE_IMAGE" sleep infinity
    docker exec cfgd-sync-seed git config --global user.name "TJ Smith"
    docker exec cfgd-sync-seed git config --global user.email "tj@jarvispro.io"
    # git's own commit/push output names the commit at the width cfgd's daemon
    # log and `short_commit` use, so the reader matches one spelling across
    # the machine hop instead of a 7-char and a 12-char one.
    docker exec cfgd-sync-seed git config --global core.abbrev 12
    docker exec cfgd-sync-seed bash -lc \
        'cfgd init --from /srv/git/config.git --apply-module nvim --yes'
    # init normalizes cfgd.yaml in place (origin/patches defaults serialized
    # back) and the nvim provision touches lazy-lock.json through the deployed
    # symlinks. Committing that residue here — and pushing it to the hub —
    # is what lets machine A's on-camera `git commit -am` contain exactly the
    # one file the demo edits, on an otherwise clean tree.
    docker exec cfgd-sync-seed bash -lc \
        'cd ~/.config/cfgd && if ! git diff --quiet; then git commit -qam "machine image: init-time normalization" && git push -q; fi'
    hub_git rev-parse HEAD > "$SCRATCH/hub-initial-commit"
    # TERM baked into the image because `docker exec -t` (how the tape enters a
    # machine) defaults TERM to plain xterm when the image defines none —
    # unlike the `docker run -it` the other tapes use, which forwards the
    # recording terminal's.
    docker commit --change 'WORKDIR /home/tj' --change 'CMD ["sleep","infinity"]' \
        --change 'ENV TERM=xterm-256color' cfgd-sync-seed "$SEED_IMAGE"
    docker rm -f cfgd-sync-seed
    echo "seed image ready: $SEED_IMAGE"
}

phase_machines() {
    for m in "${MACHINES[@]}"; do
        docker rm -f "$m" >/dev/null 2>&1 || true
        docker run -d --name "$m" --hostname "$m" -v "$HUB:/srv/git" "$SEED_IMAGE"
        # bash -lc is a non-interactive login shell, whose ~/.bashrc returns
        # before the injected `source ~/.cfgd.env` line runs — so the daemon
        # sources it explicitly, or its reconcile ticks would run without the
        # PATH the provision recorded.
        docker exec -d "$m" bash -lc '. ~/.cfgd.env && exec cfgd daemon run >> ~/daemon.log 2>&1'
    done
    echo "machines up: ${MACHINES[*]} (daemons running, log at ~/daemon.log)"
}

phase_reset() {
    for m in "${MACHINES[@]}"; do
        docker rm -f "$m" >/dev/null 2>&1 || true
    done
    hub_git update-ref refs/heads/master "$(cat "$SCRATCH/hub-initial-commit")"
    phase_machines
}

phase_down() {
    docker rm -f "${MACHINES[@]}" cfgd-sync-seed >/dev/null 2>&1 || true
    docker rmi "$SEED_IMAGE" >/dev/null 2>&1 || true
    rm -rf "$SCRATCH"
    echo "sync demo torn down"
}

case "${1:-}" in
    hub) phase_hub ;;
    seed) phase_seed ;;
    machines) phase_machines ;;
    reset) phase_reset ;;
    up) phase_hub; phase_seed; phase_machines ;;
    down) phase_down ;;
    *) echo "usage: setup-sync-demo.sh {up|hub|seed|machines|reset|down}" >&2; exit 1 ;;
esac
