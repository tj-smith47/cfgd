# Changelog — cfgd-crd

## [Unreleased]

## [0.6.0] - 2026-09-07

### Features

* 85d1fca9b11a gate an env var or alias to named platforms with the same platforms: list a module and a package take, and concatenate the PATH declarations that survive on a host instead of keeping only the last ([@tj-smith47](https://github.com/tj-smith47))
* 9a761437d7fe bound the nonCompliantMachines list a policy status enumerates, so a fleet-wide violation cannot outgrow etcd object limits ([@tj-smith47](https://github.com/tj-smith47))
* d8c2dedccc6b show a structure-only recursive tree with named field types, accepted value lists, a docs pointer per kind, and fuller field descriptions ([@tj-smith47](https://github.com/tj-smith47))
* bdec4ef6de45 show a Module's `Available` condition as a `kubectl get modules` column, so a module the operator withholds over its signature verdict no longer reads like a served one ([@tj-smith47](https://github.com/tj-smith47))
* 4b8857cdc476 verify a Module artifact with cosign before reporting it verified, add operator.extraEnv and agent.extraEnv to the chart, and stop re-reading a registry that answered nothing on every reconcile ([@tj-smith47](https://github.com/tj-smith47))

---
### Bug Fixes

* 45b26f988214 have an explain field name the siblings it relates to, instead of pointing at a rendered position the alphabetical sort does not put it in ([@tj-smith47](https://github.com/tj-smith47))
* af9dd3475ddf hold module push, pull, build and image pack under their own title section, report a Module signature as one word in a SIGNATURE column, open kubectl cfgd status with its context and namespace and measure the pods carrying modules, fill a Module PLATFORMS column from the artifact it names, and prove the exec bit survives push and CSI unpack ([@tj-smith47](https://github.com/tj-smith47))
* 1becbd6fa298 qualify a fleet drift row's field with its configurator so two configurators sharing a key stop colliding into one DriftAlert row ([@tj-smith47](https://github.com/tj-smith47))
* dffc1648776e read a Module with no signature verdict as `unknown` on `kubectl cfgd status` instead of deriving `unverified` from its declared key, and close every mutating `module` verb — `push`, `pull`, `build` included — on the next command through the one composer the `source` verbs use ([@tj-smith47](https://github.com/tj-smith47))
* e465f6bb0e0e state each surface's real drift coverage so the gateway, doctor and compliance reports stop implying checks they never ran ([@tj-smith47](https://github.com/tj-smith47))
* 32210c2b1d9a leave the Module PLATFORMS column empty instead of printing [] when no platform is known, and stamp status.observedGeneration so a verdict names the spec it describes ([@tj-smith47](https://github.com/tj-smith47))

---
### Performance

* d03bb1ad40ac serve every cross-resource read from a reflector store, and write status only when it changes ([@tj-smith47](https://github.com/tj-smith47))

## [0.5.0] - 2026-07-07

### Features

* 1ea5c9deb8d7 CRD validation via shared cfgd-crd fns (CLI+webhook converged) ([@tj-smith47](https://github.com/tj-smith47))

---
### Bug Fixes

* 4c2a47fdce5b align OCI-ref predicate with parser; widen agreement corpus ([@tj-smith47](https://github.com/tj-smith47))
* 1db280d1f6b5 eliminate non-structural single-source-of-truth violations, each guarded ([@tj-smith47](https://github.com/tj-smith47))

---
### Others

* 806313c5fedd bump the cargo group with 49 updates ([@dependabot[bot]](https://github.com/dependabot[bot]))
* b149ed67f961 align all crates to v0.5.0 for unified cut ([@tj-smith47](https://github.com/tj-smith47))
* f30a3b82769a rollback core-v0.5.0 [skip ci] (anodize-rollback)
* 473271ee36f4 rollback v0.5.0 [skip ci] (anodize-rollback)
* 53320c538876 rollback v0.5.0 [skip ci] (anodize-rollback)
* 8d2c5a028328 rollback v0.5.0 [skip ci] (anodize-rollback)
* a2fa04aa4e4e rollback v0.5.0 [skip ci] (anodize-rollback)
* ac2efacbcbc3 extract cfgd-crd crate (types + validate) from operator ([@tj-smith47](https://github.com/tj-smith47))

[Unreleased]: https://github.com/tj-smith47/cfgd/compare/crd-v0.6.0...HEAD
[0.6.0]: https://github.com/tj-smith47/cfgd/compare/crd-v0.5.0...crd-v0.6.0
[0.5.0]: https://github.com/tj-smith47/cfgd/releases/tag/crd-v0.5.0
