# Tier 3: OCI Build, Signing & Supply Chain Security

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add container-based module building, cosign signing at push time (static key + keyless), key management, and supply chain attestations to cfgd's OCI pipeline.

**Architecture:** Extend the existing sync OCI client (`cfgd-core/src/oci.rs`) with build, signing, and attestation capabilities. Shell out to `docker`/`podman` for container builds and `cosign` for signing/verification — consistent with how cfgd handles other external tools. Add new CLI subcommands under `cfgd module`. Three phases: build (Phase B), signing (Phase C), supply chain.

**Tech Stack:** Rust, ureq (sync HTTP), flate2/tar (archives), `docker`/`podman` (container builds), `cosign` (signing/verification), OCI Distribution Spec, in-toto attestation format.

**Code review:** Run `superpowers:code-reviewer` agent after each Task completes.

---

## File Map

| File | Action | Responsibility |
|------|--------|----------------|
| `crates/cfgd-core/src/oci.rs` | Modify | Add `build_module()`, `sign_artifact()`, `verify_signature()`, `attach_attestation()`, `verify_attestation()`, OCI index manifest for multi-platform |
| `crates/cfgd-core/src/errors/mod.rs` | Modify | Add `BuildError`, `SigningError`, `VerificationFailed`, `AttestationError` variants to `OciError` |
| `crates/cfgd/src/cli/mod.rs` | Modify | Add `Build`, `Keys` variants to `ModuleCommand` enum, add `--sign`/`--key` flags to `Push` |
| `crates/cfgd/src/cli/module.rs` | Modify | Add `cmd_module_build()`, `cmd_module_keys()`, extend `cmd_module_push()` with signing, extend `cmd_module_pull()` with attestation verification |
| `crates/cfgd-operator/src/controllers/mod.rs` | Modify | Enhance Module controller to use cosign verify instead of PEM-only check |
| `crates/cfgd-operator/src/crds/mod.rs` | Modify | Add attestation fields to `ModuleStatus`, add `keylessVerification` to `CosignSignature` |

---

## Task 1: OCI Pipeline Phase B — Module Build

Implements `cfgd module build`, multi-platform builds, Docker/Podman integration.

## Task 2: OCI Pipeline Phase C — Signing

Implements `cfgd module push --sign`, keyless signing, `cfgd module keys`.

## Task 3: Supply Chain Security

Implements SLSA provenance attestations, in-toto attestation support.
