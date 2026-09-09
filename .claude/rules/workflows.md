---
paths:
  - ".github/**"
---

# cfgd GitHub Actions conventions — `.github/**`

The universal step-shape rule (every step leads with `name:`, blank line
between steps, naming style) lives in the user-level
`~/.claude/rules/github-actions.md` and is enforced by the
`post-edit-workflow.sh` hook. This file covers only cfgd-specific
single-source-of-truth wiring.

## SSOT map (keep intact)

| Pin | Single source | Consumers |
|---|---|---|
| anodizer version | repo variable `ANODIZER_VERSION` (`gh variable set`) | release, ci, nightly, determinism-shards (all via `env: ${{ vars.ANODIZER_VERSION }}`) |
| release publisher skip (operator override, empty default) | repo variable `RELEASE_SKIP_PUBLISHERS` | release.yml publish-trio → publish-crate.yml `skip:` input (appended to the mandatory `cargo` skip) — set to skip publishers a prior partial run already submitted; clear it once the release completes |
| protoc version | `.github/actions/setup-protoc` input default | setup-rust composite, release/nightly/determinism-shards (call bare, no `version:`) |
| crossplane version + sha256 | `.github/actions/setup-crossplane` input defaults | release (function/push jobs), e2e-setup; `tests/e2e/setup-cluster.sh` fallback mirrors it for local runs |
| cosign version | `COSIGN_VERSION` env in e2e.yml | both cosign-installer steps |
| MSRV | `rust-version` in root Cargo.toml | ci.yml msrv job reads it with sed |

## Job wiring invariants

- crates.io publishing for ALL crates runs once, in the dispatched
  `publish-oidc.yml` (`--publishers cargo`, topo-ordered — see the OIDC bullet
  below); the crd/core libraries have no other publish target, so they have no
  `publish-crate.yml` leg at all. The binary trio's `publish-crate.yml` calls
  are a matrix (`--skip cargo`, rollback left false) covering only their
  binary distribution — and they run FIRST, ahead of `dispatch-oidc`:
  `publish-trio`'s `github-release` publisher creates the release + binary
  assets that cargo's verify-release gate requires for a binary crate (its
  binstall `pkg_url` must resolve, never 404), and crates.io is append-only
  while the release + tags are deletable, so the irreversible cargo leg goes
  LAST. `publish-trio` needs only `[tag, determinism-check]`; `dispatch-oidc`
  needs `[tag, determinism-check, publish-trio]` and gates on `publish-trio`
  not-failed (a SKIPPED trio — library-only release — still publishes the
  libraries, which have no gate). `dispatch-oidc` rolls the tags + the trio's
  GitHub releases back on cargo failure; trio failures go to the `rollback-trio`
  job. The two rollbacks are mutually exclusive: a trio failure skips
  `dispatch-oidc`, and `dispatch-oidc`'s in-job rollback runs only after every
  trio leg has settled, so neither races the other. helm/crossplane/olm gate on
  BOTH `publish-trio` and `dispatch-oidc` success (cargo is no longer transitive
  via trio). crates.io dep ordering (`cfgd-crd → cfgd-core → trio`) is
  load-bearing and enforced INSIDE anodizer's workspace topo-sort, not the job
  graph.
- Determinism lanes come from the tag job's `det_matrix` output: trio
  crates shard across all three OSes, library crates linux-only (via
  determinism-shards' `os-labels` input). Publish legs restore their
  crate's `dist-<crate>-*` artifact — there is no inline preserve-dist.
- Trio rollback is the dedicated `rollback-trio` job, never a per-leg
  step (fail-fast off means legs run concurrently).
- `permissions:` read-only at workflow level; publish jobs elevate to the
  full write set, image-push jobs to `packages: write` only. The preflight
  job also carries `id-token: write` — not to publish, but because the
  runtime only injects `ACTIONS_ID_TOKEN_REQUEST_URL/TOKEN` into jobs that
  can mint OIDC tokens, and anodizer's secret preflight validates those on
  behalf of the MCP-registry publisher.
- crates.io Trusted Publishing (`.anodizer.yaml` cargo `auth: oidc`) runs in a
  DEDICATED `publish-oidc.yml` (`on: workflow_dispatch`), never in `release.yml`
  or the reusable `publish-crate.yml`: crates.io TP rejects the `workflow_run`
  event those fire on ("does not support the workflow_run event trigger" — the
  OIDC `event_name` claim is fixed per trigger and checked before any
  workflow-filename match), and `workflow_dispatch` is on its accepted list.
  `release.yml`'s `dispatch-oidc` job fires `publish-oidc.yml` via the
  `dispatch-and-wait` composite (a reusable `workflow_call` can't be used — it
  would re-inherit the caller's `workflow_run` event and re-taint the claim),
  polls it to a verdict, and rolls the tags + the trio's GitHub releases back on
  cargo failure. anodizer topo-sorts the workspace, so one `--publishers cargo`
  call publishes all five crates in `cfgd-crd → cfgd-core → trio` dependency
  order — the trio's
  `publish-crate.yml` legs run `--skip cargo` (the exact complement). The
  Trusted-Publisher configs on crates.io therefore name `publish-oidc.yml` (the
  file that runs cargo publish), NOT `release.yml`.
- Deferred-branch release topology (anodizer >= v0.16.0, uniform-local
  `tag`): the tag step runs `tag --changelog --push-tags-only` (tags only —
  the bump commit is reachable ONLY via the tags until publish completes),
  and the `advance-master` job fast-forwards master post-publish
  (`gh api PATCH`, `force=false`, GH_PAT). Its `if:` is the drift-proof
  collapsed form `!cancelled() && needs.tag.result == 'success' &&
  !contains(needs.*.result, 'failure') && !contains(needs.*.result,
  'cancelled')` — semantically "tag succeeded AND no needed job failed or
  was cancelled; skips allowed", with the `needs.*` sweeps automatically
  gating any job later added to the needs list. Never weaken it to a
  per-leg `!= 'failure'` enumeration, and keep EVERY publish leg in the
  job's `needs:` — a leg absent from needs is invisible to the gate. A
  failed release must advance neither master nor a release. The tag job
  also carries a pre-tag stranded-bump guard (highest `v[0-9]*` tag must
  be an ancestor of the release ref, else fail with the
  `git push origin <tag-sha>:refs/heads/master` reconcile command) —
  keep it before the anodizer tag step.
- Preflight's bump-message guard breaks the advance-master→CI→Release
  self-retrigger loop (GH_PAT pushes DO retrigger CI — deliberately, for
  master-badge health); don't loosen it.
- Nightly is sharded per-OS via anodizer split/merge (`partial.by: os` in
  `.anodizer.yaml`): three `build` shards (ubuntu/macos/windows, same runner
  labels as determinism-shards, `auto-install: 'true'`, fail-fast off) each
  run `release --nightly --split` and upload
  `nightly-dist-<shard>` with `include-hidden-files: true`; the ubuntu
  `publish` leg downloads all shards (`merge-multiple: true`) and runs
  `release --nightly --merge`. Publish/sign secrets
  (gpg/apk keys, CLOUDSMITH/SMTP/SNAPCRAFT/GPG_FINGERPRINT) live ONLY on the
  merge leg; split legs get GH_PAT alone. Never collapse nightly back to a
  single ubuntu job — darwin targets cannot zig-link without a macOS SDK.
- The `test-freebsd` job in ci.yml is a `vmactions/freebsd-vm` guest (no
  GitHub-hosted FreeBSD runner exists), pinned to the same release as the
  acceptance VM. It runs `task test:ci` like every other test leg — the
  FreeBSD scope decision lives in the Taskfile, not the workflow: `test:ci`
  detects FreeBSD via `uname -s` (no `RUNNER_OS` inside the guest) and scopes
  to `-p cfgd-core -p cfgd`, because cfgd-csi/cfgd-operator are k8s
  server-side with no FreeBSD surface (same rationale as the Windows branch).
  The toolchain is `rustup-init` not pkg `rust` (guarantees `>= MSRV`, mirrors
  the VM); `task`/`nextest`/`npm` come from pkg; no protoc (neither in-scope
  crate compiles protos). The `run:` block opens on `set -e`: it holds three
  commands now, the guest script's shell flags are the action's rather than
  GitHub's, and without the abort a failing `task test:ci` is followed by a
  passing build and the leg reports green on red tests.
- After `task test:ci` that same guest builds `--bin cfgd` and runs
  `task test:freebsd:npm-prefix`, the real-host proof of the unprivileged npm
  global-prefix fallback documented in `docs/packages.md`: the unit pins
  inject both elevation and the write-probe, so only a real non-root user
  against the real `www/npm` (configured prefix `/usr/local`, root-owned) can
  observe cfgd fall back to `$HOME/.npm-global` and pass `--prefix`.
  **That step runs as root and mutates the guest**: it installs `www/npm`,
  and it creates and `pw userdel -r`s the `cfgdnpm` user, home included. It
  belongs only on a disposable guest, which is why the FreeBSD-only decision
  is a Taskfile `uname -s` branch like `test:ci`'s and never a leg of
  `task ci`; the script refuses a non-root caller, a non-FreeBSD host, and a
  target user whose uid or home says it is somebody real. `npm` is installed
  in `prepare` rather than by the script because `IGNORE_OSVERSION` is not
  exported into the `run:` shell. The guest gets `mem: 10240`: rustc compiling cfgd-core's
  test crate was SIGKILLed on the default allotment (run 34063783806), and
  the 16 GB runner can spare it. `task test:freebsd` runs the same leg
  locally against the accept VM (start-if-stopped, poll, sync, `task test:ci`).
- The `test-thread-model` job in ci.yml runs `task test:threads` — plain
  `cargo test --test-threads=16`, not nextest. It is not redundant with the
  `test` job: nextest runs one process per test, so each test gets its own
  `environ` and its own copy of every `static`, and a race on process-global
  state shared BETWEEN tests (PATH mutation vs `command_path` resolution, the
  `console` crate's global colour flags) cannot be observed there at all.
  ubuntu-only is deliberate — the race class is not OS-specific, so a matrix
  buys nothing.
- The `audit` job in ci.yml ends with `anodizer check version-files`, the
  same check `task ci` runs as `version-files:check`. It exists because a
  release that fails AFTER `anodizer tag` never lands its bump on master, and
  every later old→new `version_files` sweep then finds nothing to replace —
  the literal freezes (v0.8.0 left `chart/cfgd/Chart.yaml` and four docs at
  0.7.0 through two more releases). Keep it in the audit job, not the
  release workflow: the point is to fail a PR, before anything is tagged.
  Its sibling, right after it in the same job, is `task chart:tags:check`,
  which renders the chart with every first-party image enabled and holds
  each `cfgd*` tag to the release contract — the chart's three components
  version independently (agent = `appVersion`, operator/csi = their own
  crate versions), so literals agreeing with each other prove nothing, and
  the published 0.9.0 chart resolved an operator tag nobody had pushed. The
  agent pin is swept by anodizer's `version_files` at tag time, so it must
  name the released version and exist on ghcr. The operator and CSI pins are
  kept by hand (anodizer refuses one file enrolled by crates with different
  bumps), so on a release branch each must equal the version `anodizer tag
  --dry-run` predicts for its crate — the release cut from that very commit
  publishes it, which is why the guard as an existence check could never
  pass a pin bump (run 34063783806) — and off a release branch a pin may run
  ahead of the released version, which the release branch's own run then
  checks exactly. Both guards are registry/anodizer questions rather than
  Rust ones; they sit in the one job that holds the tools they need
  (`task`, anodizer on PATH from the action step, docker, helm, yq, jq),
  and that job checks out with `fetch-depth: 0` because the prediction
  walks the tags.
- The `rustdoc` job runs `task doc` (`cargo doc --workspace --no-deps
  --document-private-items --all-features` under `RUSTDOCFLAGS="-D warnings"`,
  the flag spelled once as the Taskfile's `RUSTDOC_DENY_WARNINGS` var) as its
  only step, in the first wave beside `fmt` and `clippy`: it is the longest
  single step in the workflow, and inside the `clippy` job it queued the
  schema/CRD/chart drift guards behind it. Its checkout+toolchain setup is
  paid twice on purpose; the doc leg's `--all-features` pulls in
  `test-helpers`, which `cargo clippy --workspace --all-targets` does not
  build, so the two legs never shared a build cache even in one job.
  `--all-features` is load-bearing, not decoration:
  `cfgd-core` is the only crate in the workspace with a non-default feature (`test-helpers`),
  and without it the gate never compiles `test_helpers.rs` or the
  `EnvHostProbeOverride` seam at all, so a broken link inside either one
  passes clean locally and in CI alike. Denying warnings turns every rustdoc
  lint (broken intra-doc link, private-item link from a public item, bare
  URL, unclosed HTML tag, redundant explicit link target) into a build
  failure; fix the reference, never `#[allow(rustdoc::…)]` and never a `///`
  demoted to `//`. The one standing exception is a clap-derive `///` whose
  placeholder syntax is user-facing help text (`cli/mod.rs`'s four
  `<manager>`-style arg docs on `ProfileCreateArgs`/`ProfileUpdateArgs`/
  `ModuleCreateArgs`/`ModuleUpdateArgs`, rendered verbatim in `cfgd --help`):
  escaping there would leak backslashes into help output, so those four carry
  `#[allow(rustdoc::invalid_html_tags)]`. Everywhere else, prefer a backtick
  code span over a backslash escape for a literal that looks like an HTML
  tag — it resolves the same lint and reads cleaner in the source. The gate
  lives in this job and in `task push`, which runs it ahead of the push and
  blocks on a failure; no other local target (`task ci`, `task check`, `task
  lint`, the `task commit` chain) chains to `task doc`, and the task itself
  refuses to start with under 10 GB available (`_check:mem-headroom`). The
  two rustdocs peak near 10 GB each; on the 12 GB dev host that is the
  kernel OOM-killing the largest process on the box, whichever session owns
  it (41 kills between 2026-09-04 and 2026-09-09, every one a rustdoc or the
  rust-analyzer beside it). A broken link is a merge blocker, not a commit
  blocker — CI refuses it before it lands.
- Self-hosted runner labels for actionlint live in `.github/actionlint.yaml`.
- Any job that `uses: ./.github/actions/...` MUST have a checkout step
  before it (the local action file only exists on the runner after
  checkout), and the checkout must precede any `download-artifact` step
  (checkout's git-clean deletes files already in the workspace).
- Reruns of a failed run execute the workflow file FROZEN at its original
  dispatch — a workflow fix never reaches an existing run. If a push-leg
  defect strands built artifacts, recover with a temporary dispatch
  workflow that downloads the run's artifacts and republishes them (the
  v0.5.0 recipe, `backfill-xpkg.yml`, lives in git history — added and
  removed around the v0.5.0 crossplane backfill), then delete it: a
  standing copy of push steps drifts from release.yml.
