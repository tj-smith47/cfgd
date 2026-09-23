# Demo GIFs

Each GIF here is recorded from the tape beside it by a `task demo*` target (`task demo` for
`cfgd-demo.gif`, `task demo:<tape>` for the rest).

- `recorded.txt` names, for every GIF, its tape and the commit it was recorded at. The encode step
  (`scripts/stamp.sh`) rewrites the line; nothing else should.
- `task demo:check` (and the `demo-sync` CI job on every PR) runs `scripts/check-sync.sh`, which
  flags a GIF when its tape, `Dockerfile`, `scripts/`, or the non-test source of a crate the take
  runs changed since that commit (the `k8s` and `connect` takes also count the operator, the CSI
  driver and `chart/`).
- The only way to clear a flag is to re-record that GIF, which stamps it at the new commit.
