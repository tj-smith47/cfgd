# Demo GIFs

Each GIF here is recorded from the tape beside it by a `task demo*` target (`task demo` for
`cfgd-demo.gif`, `task demo:<tape>` for the rest).

- `recorded.txt` names, for every GIF, its tape and the commit the take was recorded at.
  `scripts/record.sh` notes that commit beside the take's frames, and the encode step
  (`scripts/stamp.sh`) copies it into the GIF's line.
- `task demo:check` (and the `demo-sync` CI job on every PR) runs `scripts/check-sync.sh`. It
  fails a GIF in two cases:
  - anything that could move its render changed since that commit: its tape, `Dockerfile`,
    `scripts/` (other than `check-sync.sh` and `stamp.sh`, which run after a take), `.dockerignore`, `chart/`, the operator and CSI release Dockerfiles, the
    workspace `Cargo.toml` and `Cargo.lock`, or any file under `crates/` other than test code,
    changelogs and the `cfgd-test-fixtures` crate (fixtures a crate embeds count). A
    dependency or chart change flags every GIF. The release bump commit
    (`chore(release): bump ...`) is passed over: it changes version numbers only, and the demo
    builds everything from source;
  - the GIF was not committed after its stamped commit, which means no take wrote the stamp
    (a hand edit of `recorded.txt`, or a re-encode that produced the same GIF).
- The only way to clear a flag is to re-record that GIF and commit it.
