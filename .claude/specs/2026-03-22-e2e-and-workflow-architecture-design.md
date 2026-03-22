# cfgd E2E & Workflow Architecture — Design Spec

**Date:** 2026-03-22
**Status:** Draft — updated with runner infrastructure decisions, pending Windows support completion
**Repo:** cfgd (tj-smith47) — runs-on tables for shelly-cli/shelly-go included for reference
**Depends on:** Runner Infrastructure Redesign (DONE), Windows support (IN PROGRESS)

## Goal

Three objectives, in priority order:

1. **Rework cfgd's E2E tests to not require KIND/Docker** — enabling them to run on ARC bare-mode runners (no Docker daemon available) by targeting the real k3s cluster via Tailscale. This is the hard problem that requires deep investigation. **Note:** Windows support is being added to cfgd by another session; E2E rework happens AFTER that lands.
2. **Update all workflow `runs-on` labels** across all 3 repos to target the correct runner (ARC bare-mode or GitHub-hosted) based on job requirements. There is no VM runner — Docker-dependent jobs use `ubuntu-latest`.
3. **Improve DRYness and local testability** across all 3 repos' workflows and build scripts.

## Part 1: cfgd E2E Test Architecture (primary — requires investigation)

### Problem

cfgd has 4 E2E test suites that all depend on KIND (Kubernetes-in-Docker):
- `e2e-cli.yml` — CLI exhaustive tests (one Docker job, one native job)
- `e2e-node.yml` — node agent tests (4 parallel suites: binary, helm, server, drift)
- `e2e-operator.yml` — operator tests (T01–T18)
- `e2e-full-stack.yml` — full stack integration tests (T01–T16)

All use `.github/actions/setup-e2e` which installs KIND, creates a cluster, builds Docker images (cfgd, cfgd-operator, cfgd-csi), loads them into KIND, deploys CRDs, and sets up a device gateway.

**KIND is being eliminated.** ARC runners operate in bare mode — steps execute directly in the runner container with no Docker daemon available. KIND requires Docker, making it incompatible with ARC runners. Instead, E2E tests should target the real k3s cluster (10.23.46.201) via Tailscale networking from GHA runners.

**Windows support note:** Windows support is actively being added to cfgd by another session. The E2E rework should happen AFTER Windows support lands to avoid conflicting changes.

### What needs investigation

**This spec intentionally does not prescribe a solution for the E2E rework.** The test architecture is deeply coupled to what cfgd's components actually need from a Kubernetes environment. A dedicated investigation must read the full E2E test code and answer:

#### 1. What does each E2E suite actually test?

For each of the 4 suites, catalog:
- What Kubernetes APIs/resources does it interact with? (CRDs, Pods, PVs, CSI, etc.)
- Does it need a real kubelet? (e.g., CSI driver mounting volumes to pods)
- Does it need real networking? (e.g., pod-to-pod communication, services)
- Does it need real scheduling? (e.g., node affinity, taints, topology)
- What specifically does it need from Docker vs what it needs from Kubernetes?
- Which tests are actually unit/integration tests masquerading as E2E?

#### 2. What are the replacement options per suite?

For each test's actual requirements, evaluate:
- **Real cluster (k3s) via Tailscale** (PREFERRED): test against the real k3s cluster (10.23.46.201 / k3s-master-1) in an isolated namespace. Full Kubernetes, no nesting. Requires RBAC scoping and namespace cleanup. Tailscale provides secure network access from GHA runners to the cluster API. This is the primary replacement for KIND.
- **envtest** (controller-runtime): API server + etcd only. Good for CRD/controller logic. No kubelet, no scheduling, no networking, no CSI.
- **Mock/fake clients**: for tests that only verify controller reconciliation logic.

**Eliminated options** (require Docker, which is not available on ARC bare-mode runners):
- ~~k3d~~: runs k3s in Docker — Docker dependency makes it incompatible with ARC.
- ~~Testcontainers with k3s~~: programmatic single-container cluster — same Docker dependency issue.

#### 3. What changes to test code are required?

For each suite:
- How coupled is the code to KIND specifically? (KIND API calls, kind-config.yaml, image loading via `kind load`)
- Can the tests accept any kubeconfig? Or are they hardcoded to KIND?
- What setup/teardown changes are needed?
- How much rewriting vs adapting?

#### 4. Local testing story

After rework, a developer should be able to:
- Run `task e2e` (or equivalent) locally
- Tests should work with whatever Kubernetes is available (local KIND, remote k3s, etc.)
- Docker image builds should be skippable if images are already in a registry
- No requirement for Docker if the test backend doesn't need it

#### 5. Migration strategy

- Can suites be migrated incrementally?
- Which suite is simplest to migrate first (smallest KIND dependency)?
- How do we validate coverage parity between old and new test architecture?
- What's the rollback plan if a reworked suite misses bugs the KIND version caught?

### Expected outcome

After investigation, produce a concrete implementation design that:
- Specifies the replacement for KIND per suite (may differ per suite)
- Includes the test code changes needed
- Includes updated `setup-e2e` action or its replacement
- Includes updated workflow files
- Preserves local testability

## Part 2: Workflow `runs-on` targeting (all repos)

Runner infrastructure is in place (ARC bare mode). All workflows need updated `runs-on` labels. The runner landscape is:
- **`arc-*`** (ARC bare mode): Steps execute directly in the runner container. Custom image with Rust, Go, protoc, sccache pre-installed. **No Docker daemon available.**
- **`ubuntu-latest`** (GitHub-hosted): For jobs that require Docker (image builds, KIND until E2E rework). Performance difference doesn't matter for image builds.
- **`macos-*` / `windows-latest`** (GitHub-hosted): For platform-specific builds. Unchanged.

### cfgd

| Workflow | Job | New `runs-on` | Notes |
|----------|-----|---------------|-------|
| ci.yml | fmt, clippy, test, audit | `arc-cfgd` | ARC bare mode, no Docker needed |
| auto-tag.yml | tag | `arc-cfgd` | ARC bare mode, lightweight |
| e2e-cli.yml | exhaustive-docker | `ubuntu-latest` (until E2E rework) | Docker required, no Docker on ARC |
| e2e-cli.yml | exhaustive-native | `arc-cfgd` | ARC bare mode, no Docker |
| e2e-node.yml | all jobs | `ubuntu-latest` (until E2E rework), then `arc-cfgd` with Tailscale | KIND→real k3s cluster via Tailscale |
| e2e-operator.yml | all jobs | `ubuntu-latest` (until E2E rework), then `arc-cfgd` with Tailscale | KIND→real k3s cluster via Tailscale |
| e2e-full-stack.yml | all jobs | `ubuntu-latest` (until E2E rework), then `arc-cfgd` with Tailscale | KIND→real k3s cluster via Tailscale |
| release.yml | linux builds | `arc-cfgd` | ARC bare mode, Rust cross-compilation (no Docker needed) |
| release.yml | macos builds | `macos-latest` / `macos-14` | Unchanged |
| release.yml | docker*, crossplane | `ubuntu-latest` | Docker builds require Docker daemon (GitHub-hosted) |
| release.yml | checksums, changelog, release | `arc-cfgd` | ARC bare mode, no Docker |
| release.yml | helm-chart, cargo-publish, homebrew, krew, olm-bundle | `arc-cfgd` | ARC bare mode, no Docker |

### shelly-cli

| Workflow | Job | New `runs-on` | Notes |
|----------|-----|---------------|-------|
| ci.yml | lint, test, build, audit, security | `arc-shelly-cli` | ARC bare mode, no Docker |
| auto-tag.yml | tag | `arc-shelly-cli` | ARC bare mode, lightweight |
| docs.yml | all jobs | `arc-shelly-cli` | ARC bare mode, Hugo, no Docker |
| release.yml | build (linux amd64) | `arc-shelly-cli` | ARC bare mode, CGO + native build |
| release.yml | build (linux arm64) | `ubuntu-24.04-arm` | Unchanged (native ARM) |
| release.yml | build (macos) | `macos-*` | Unchanged |
| release.yml | build (windows) | `windows-latest` | Unchanged |
| release.yml | docker | `ubuntu-latest` | Docker multi-arch requires Docker daemon (GitHub-hosted) |
| release.yml | release, homebrew | `arc-shelly-cli` | ARC bare mode, no Docker |

### shelly-go

| Workflow | Job | New `runs-on` | Notes |
|----------|-----|---------------|-------|
| ci.yml | build, lint, security | `arc-shelly-go` | ARC bare mode, no Docker |
| ci.yml | test (linux) | `arc-shelly-go` | ARC bare mode, no Docker |
| ci.yml | test (macos) | `macos-latest` | Unchanged |
| ci.yml | test (windows) | `windows-latest` | Unchanged |
| auto-tag.yml | tag | `arc-shelly-go` | ARC bare mode, lightweight |
| release.yml | goreleaser | `arc-shelly-go` | ARC bare mode, no Docker |

## Part 3: DRY consolidation and local testing (cfgd only)

**shelly-cli and shelly-go DRY work is DONE** — coverage badge scripts extracted, `make ci` targets added.

### Coverage badge logic (cfgd)

Extract inline coverage badge logic from cfgd's CI workflow to a script (e.g., `.claude/scripts/update-coverage-badge.sh`). cfgd uses cargo-tarpaulin, so the parsing differs from the Go repos.

### Local testing interface (cfgd)

`task check` already runs fmt, clippy, test, audit. Add `task e2e` for running E2E suites locally (implementation depends on E2E rework outcome).

**Principle:** Workflows should call Taskfile targets where possible rather than inline shell. This ensures local and CI produce the same results.

## Implementation order

1. ~~Runner infrastructure (spec 1) — ARC bare mode~~ **DONE.** ARC runners deployed in bare mode with custom runner image (`registry.jarvispro.io/arc-runner:latest`) containing Rust, Go, protoc, sccache, etc. No VM runner — Docker-dependent jobs use GitHub-hosted `ubuntu-latest`.
2. Windows support for cfgd — **IN PROGRESS** (another session). E2E rework blocked on this completing.
3. Workflow `runs-on` changes (Part 2) — can be done incrementally. CI jobs can switch to `arc-*` immediately. E2E and Docker jobs use `ubuntu-latest` until E2E rework. shelly-cli/shelly-go runs-on tables included for reference.
4. ~~DRY and local testing improvements (Part 3)~~ — shelly-cli/shelly-go DONE. cfgd coverage badge + `task e2e` remain.
5. cfgd E2E investigation and rework (Part 1) — the big effort, done after Windows support lands. Will replace KIND with real k3s cluster testing via Tailscale.
