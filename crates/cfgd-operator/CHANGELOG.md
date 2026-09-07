# Changelog — cfgd-operator

## [Unreleased]

## [0.8.0] - 2026-09-07

### Features

* 85d1fca9b11a gate an env var or alias to named platforms with the same platforms: list a module and a package take, and concatenate the PATH declarations that survive on a host instead of keeping only the last ([@tj-smith47](https://github.com/tj-smith47))
* 4b8857cdc476 verify a Module artifact with cosign before reporting it verified, add operator.extraEnv and agent.extraEnv to the chart, and stop re-reading a registry that answered nothing on every reconcile ([@tj-smith47](https://github.com/tj-smith47))

---
### Bug Fixes

* 6c9a58101035 scan test code for session-narrative comments, mint declared_modules_dir, and widen the themed-arrow walk ([@tj-smith47](https://github.com/tj-smith47))
* af9dd3475ddf hold module push, pull, build and image pack under their own title section, report a Module signature as one word in a SIGNATURE column, open kubectl cfgd status with its context and namespace and measure the pods carrying modules, fill a Module PLATFORMS column from the artifact it names, and prove the exec bit survives push and CSI unpack ([@tj-smith47](https://github.com/tj-smith47))
* 9cf2f0f1a79d mint every system drift identity through the one composer and floor the fleet-field pin's reader walk ([@tj-smith47](https://github.com/tj-smith47))
* f222241b9aab price a --phase plan footer over the phases it lists, report kubectl's own failure instead of a broken pipe when it exits before reading its manifest, and settle backup and restore rows through one classifier ([@tj-smith47](https://github.com/tj-smith47))
* 1becbd6fa298 qualify a fleet drift row's field with its configurator so two configurators sharing a key stop colliding into one DriftAlert row ([@tj-smith47](https://github.com/tj-smith47))
* 410cf6bfe936 say nothing to apply with the pending count instead of up to date while a decision is withheld, annotate a decision item a higher layer outranks, close every result line in one sentence shape, count pending decisions in the section title, spell the decide hint once with backticked commands, retire a successful clone silently, name the Sources section and source owner one way, humanize Last Sync, add Last Sync and Signed columns to source list, and record a Module artifact attestation types beside its platforms ([@tj-smith47](https://github.com/tj-smith47))
* e465f6bb0e0e state each surface's real drift coverage so the gateway, doctor and compliance reports stop implying checks they never ran ([@tj-smith47](https://github.com/tj-smith47))
* 222939af17f7 walk the whole fleet drift surface in its pin and state the dashboard cards' system-only coverage ([@tj-smith47](https://github.com/tj-smith47))
* ffb8749be2aa resolve every intra-doc link the published crates render, so a rustdoc page no longer drops a pointer to a private helper or swallows a placeholder as an HTML tag ([@tj-smith47](https://github.com/tj-smith47))
* 7a349cb1d2fb resolve rustdoc link and cfg-visibility findings from the --all-features doc gate ([@tj-smith47](https://github.com/tj-smith47))
* f2723523fac1 reshape a device-supplied id into the Kubernetes name and labels a DriftAlert becomes ([@tj-smith47](https://github.com/tj-smith47))
* 1c6a2c73b07c rule out request-side serialization before the DriftAlert retry loop so an in-loop SerdeError is provably a response ([@tj-smith47](https://github.com/tj-smith47))
* 4a1f12acd614 treat a DriftAlert create whose 2xx body cannot be parsed as the success it is, instead of retrying into a 409 ([@tj-smith47](https://github.com/tj-smith47))
* 59fad17f13f4 a deleted ConfigPolicy stops exporting its devices_compliant series and clears its verdict from machines relabelled out of its selector, resetting each from a live read ([@tj-smith47](https://github.com/tj-smith47))
* 09f47ee217c9 apply the leader lease under the operator's own field manager ([@tj-smith47](https://github.com/tj-smith47))
* 7dcea16a4ebb count a no-op MachineConfig reconcile as a success so the liveness metric keeps advancing on a steady machine ([@tj-smith47](https://github.com/tj-smith47))
* 214d35703951 count deletion passes on the reconciliation liveness metrics so a deletion-heavy period does not read as a dead controller ([@tj-smith47](https://github.com/tj-smith47))
* 87e4f48ca05e drop the unreachable unsigned-policy gate from Module availability, and prove the withheld verdict under a verifier that would have accepted the artifact ([@tj-smith47](https://github.com/tj-smith47))
* 0837fe28b90e finalize ClusterConfigPolicy so devices_compliant never leaks a series ([@tj-smith47](https://github.com/tj-smith47))
* 32210c2b1d9a leave the Module PLATFORMS column empty instead of printing [] when no platform is known, and stamp status.observedGeneration so a verdict names the spec it describes ([@tj-smith47](https://github.com/tj-smith47))
* 46e89a6c604c pin the observed lease version on a forced takeover and treat a create conflict as not-acquired ([@tj-smith47](https://github.com/tj-smith47))
* f3db20529c36 replace the gateway reader-vs-writer timing test with a deterministic held-transaction observable so CI cannot flake on a loaded runner ([@tj-smith47](https://github.com/tj-smith47))
* aad37f3c26b2 report the exact ClusterConfigPolicy violator count instead of the capped one, so a fleet-wide violation is not understated ([@tj-smith47](https://github.com/tj-smith47))
* c1d876dabaa8 reset the Compliant verdict on a deleted ConfigPolicy's machines instead of leaving a judgement no policy makes ([@tj-smith47](https://github.com/tj-smith47))
* a35ebc3a43f2 stop a DriftAlert status patch from deleting the machine Reconciled, ModulesResolved and Compliant conditions ([@tj-smith47](https://github.com/tj-smith47))
* d05da1488b40 stop the MachineConfig controller republishing a Compliant verdict it did not reach, which had it and the policy controller rewriting each other at watch speed ([@tj-smith47](https://github.com/tj-smith47))
* fdb725647e37 fold every tracing event before it reaches a terminal ([@tj-smith47](https://github.com/tj-smith47))
* 56752544339b remove a stale hash-refresh walk's backward blind spot, extend the "we"/self-citation sweep repo-wide with an audit.sh gate, and pin every module-cache hand-join and themed-arrow-into-json seam ([@tj-smith47](https://github.com/tj-smith47))
* adee01e580c0 read an env default and await a shutdown request through one shared helper each, and widen the duplicate-function audit to generic and pub(crate) definitions ([@tj-smith47](https://github.com/tj-smith47))
* c441c6a95b62 add and remove a finalizer through one shared pair, so three controllers stop carrying their own copy of the same patch ([@tj-smith47](https://github.com/tj-smith47))

---
### Performance

* d03bb1ad40ac serve every cross-resource read from a reflector store, and write status only when it changes ([@tj-smith47](https://github.com/tj-smith47))
* d208be38e393 write MachineConfig status only when the reconcile observed a change, watch namespaces by metadata alone, and thread the compliance verdict instead of recomputing it ([@tj-smith47](https://github.com/tj-smith47))

## [0.7.0] - 2026-08-16

### Features

* f88d99604c8a phase-first apply tree with kind:name owner groups, concurrent per-manager installs, and live rows that settle in place (#105) ([@tj-smith47](https://github.com/tj-smith47))

## [0.5.1] - 2026-07-20

### Bug Fixes

* 00e9d4409128 force rustls ring backend so the FreeBSD release binary builds ([@tj-smith47](https://github.com/tj-smith47))

---
### Others

* f1c282a0d707 revert stranded v0.6.0 stamp for clean re-cut ([@tj-smith47](https://github.com/tj-smith47))

## [0.5.0] - 2026-06-17

### Features

* cd48345c10e4 clap argv root — --version/--help answer instantly, reject unknown args ([@tj-smith47](https://github.com/tj-smith47))
* 54d227ec9272 observable desired-config pushes — generation + lastPushedAt ([@tj-smith47](https://github.com/tj-smith47))
* 1ea5c9deb8d7 CRD validation via shared cfgd-crd fns (CLI+webhook converged) ([@tj-smith47](https://github.com/tj-smith47))

---
### Bug Fixes

* 82cdbafea745 emit SLSA v1 predicate body for cosign attest, not a full statement ([@tj-smith47](https://github.com/tj-smith47))
* 5897a0d70043 config-push size limit returns the actionable 400, not a generic 413 ([@tj-smith47](https://github.com/tj-smith47))
* 33e596af8a82 gateway fresh-bootstrap migration logs dup column at DEBUG, not WARN ([@tj-smith47](https://github.com/tj-smith47))
* 6387aabc2808 standalone device-gateway mode (DEVICE_GATEWAY_STANDALONE), no cluster required ([@tj-smith47](https://github.com/tj-smith47))

---
### Others

* 213f0db8b468 per-crate changelogs via anodizer tag; retire git-cliff ([@tj-smith47](https://github.com/tj-smith47))
* ac2efacbcbc3 extract cfgd-crd crate (types + validate) from operator ([@tj-smith47](https://github.com/tj-smith47))
* d7d3bf6bc720 gen_crds render_all + file-writing, sourced from cfgd-crd ([@tj-smith47](https://github.com/tj-smith47))

[Unreleased]: https://github.com/tj-smith47/cfgd/compare/operator-v0.8.0...HEAD
[0.8.0]: https://github.com/tj-smith47/cfgd/compare/operator-v0.7.0...operator-v0.8.0
[0.7.0]: https://github.com/tj-smith47/cfgd/compare/operator-v0.5.1...operator-v0.7.0
[0.5.1]: https://github.com/tj-smith47/cfgd/compare/operator-v0.5.0...operator-v0.5.1
[0.5.0]: https://github.com/tj-smith47/cfgd/compare/operator-v0.4.0...operator-v0.5.0
