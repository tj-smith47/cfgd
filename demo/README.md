# Demo GIFs

Each GIF here is recorded from the tape beside it by a `task demo*` target (`task demo` for
`cfgd-demo.gif`, `task demo:<tape>` for the rest).

- `recorded.txt` names, for every GIF, its tape and the commit the take was recorded at.
  `scripts/record.sh` notes that commit beside the take's frames, and the encode step
  (`scripts/stamp.sh`) copies it into the GIF's line; nothing else writes the file.
- `task demo:check` (and the `demo-sync` CI job on every PR) runs `scripts/check-sync.sh`, which
  flags a GIF when anything that could move its render changed since that commit: its tape,
  `Dockerfile`, `scripts/`, the workspace `Cargo.toml` and `Cargo.lock`, or any non-test file
  under `crates/` (the `k8s` and `connect` takes also count `chart/` and the operator and CSI
  release Dockerfiles). A dependency or version bump flags every GIF.
- The only way to clear a flag is to re-record that GIF; re-encoding the old take keeps its old
  commit.
