# Releasing

cfgd releases are cut entirely by CI. **Never run `git tag` or
`gh release create` by hand**: the Release workflow owns tag creation,
crate publishing, artifact signing, and the post-publish master advance.
A hand-cut tag desynchronizes the deferred-branch topology below and wedges
the next real release.

## How a release happens

The Release workflow (`.github/workflows/release.yml`) fires on a successful
CI run on `master` (`workflow_run`) or a manual `workflow_dispatch`. Jobs run
in this order:

| Job | What it does |
|---|---|
| `preflight` | Validates every publisher secret up front (`release --preflight-secrets`); the bump-message guard culls the self-retriggered run |
| `tag` | `anodizer tag --changelog --push-tags-only` — creates the bump commit + per-crate tags, pushes **only the tags** |
| `determinism-check` | Reproducible-build shards per released crate (binary trio on linux/macos/windows, library crates linux-only) |
| `publish-trio` | `cfgd` / `cfgd-csi` / `cfgd-operator` binary distributions in parallel. Runs before any crates.io publish: cargo's verify-release gate needs the GitHub releases + assets these legs create |
| `dispatch-oidc` | crates.io publish: one serial `publish-oidc.yml` dispatch per released crate, in dependency order; rolls back tags + the trio's GitHub releases on cargo failure |
| `rollback-trio` | Deletes this release's tags if any trio leg failed |
| `helm-chart`, `crossplane-function` + `crossplane-push`, `olm-bundle` | Chart, xpkg, and OLM bundle images to ghcr.io |
| `advance-master` | Fast-forwards `master` onto the bump commit (`gh api` PATCH, `force=false`) |

Deferred-branch topology: until `advance-master` runs, the bump commit is
reachable **only via the tags**; `master` does not move. A failed release
therefore advances neither `master` nor a release; the tags are rolled back
and the tree is untouched.

## Pre-release checklist

- [ ] All intended work is committed and pushed; nothing load-bearing sits
      unpushed in the working tree.
- [ ] Acceptance gates for the changes in this release are green on the real
      hosts they apply to (Linux, macOS, Windows): CI green alone does not
      clear OS-specific paths.
- [ ] `ANODIZER_VERSION` coupling: the repo variable
      (`gh variable set ANODIZER_VERSION -R tj-smith47/cfgd --body vX.Y.Z`)
      is live CI config shared by ci/nightly/release. When a workflow change
      and an anodizer version bump depend on each other, flip the variable
      **only in the same window as pushing the workflow change**. Safe
      order: push the workflow first, then flip the variable (an older
      anodizer fails loud on an unknown flag; a newer one under the old
      workflow can silently no-op the release).

## Done-definition

A release is **not done** when the workflow goes green. It is done when all
of the following hold:

- [ ] Every publish leg (`publish-trio`, `dispatch-oidc`, `helm-chart`,
      `crossplane-push`, `olm-bundle`) is `success` or legitimately
      `skipped` for a partial-workspace release.
- [ ] `advance-master` succeeded: `master` now points at the bump commit.
- [ ] Cosign verifies against a **downloaded** release asset (the release
      signs each `<archive>.sha256`, not the archive itself):

  ```sh
  cosign verify-blob \
    --bundle <archive>.sha256.cosign.bundle \
    --certificate-oidc-issuer https://token.actions.githubusercontent.com \
    --certificate-identity-regexp '^https://github\.com/tj-smith47/cfgd/\.github/workflows/(publish-crate\.ya?ml@refs/heads/master$|release\.ya?ml@refs/tags/v[0-9]|nightly\.ya?ml@refs/heads/master$)' \
    <archive>.sha256
  echo "$(cat <archive>.sha256)  <archive>" | sha256sum -c
  ```

- [ ] Any release-note items owed from the development cycle (behavior
      changes, upgrade caveats) are published on the GitHub release.

## Failure recovery

### Rerunning a failed run

Reruns execute the workflow file **frozen at its original dispatch**: a
workflow fix pushed afterward never reaches an existing run. If a push-leg
defect strands built artifacts, do not rerun-with-fix; use the backfill
recipe: a temporary dispatch workflow that downloads the run's artifacts and
republishes them (see `backfill-xpkg.yml` in git history around the v0.5.0
crossplane backfill), then delete it.

### Run cancelled after the tag job

The bump commit exists and is reachable only via the freshly-pushed tags;
`master` was never advanced. The next release then wedges on a
non-fast-forward tag push. Reconcile by advancing master onto the tagged
commit by hand:

```sh
git fetch origin
git push origin <tag-sha>:refs/heads/master
```

(The same command is what `advance-master` prints when its fast-forward
PATCH fails because master moved mid-release. In that case the release IS
published; do not re-cut.)

### Trio leg failure

`rollback-trio` runs once, after every trio leg settles (never inside a
leg), and deletes this release's tags. A cargo failure in `dispatch-oidc`
takes its own in-job rollback (tags plus the trio's GitHub releases); the
two never fire together, since a trio failure skips `dispatch-oidc`.
Already-published crates.io versions cannot be unpublished; the next
release bumps past them.
