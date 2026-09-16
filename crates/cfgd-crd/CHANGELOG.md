# Changelog — cfgd-crd

## [Unreleased]

## [0.7.0] - 2026-09-16

### Features

* adb6be582758 add the BackupPolicy kind — a namespaced, selector-matched fleet backup schedule that overrides a named unit's cadence and never defines the unit itself ([@tj-smith47](https://github.com/tj-smith47))
* ef83f0e8d0f8 carry every local module field on the Module CRD — per-file strategy, patch, encryption and permissions, package minVersion and prefer, module platforms, aliases, system and the full hook set — and fold the rendered schema into a structural one ([@tj-smith47](https://github.com/tj-smith47))
* d89de00d4753 widen the Module CRD to the local kind — a package entry carries its per-manager `aliases`, its `deny` list and its own platform gates, every platform tag is validated by the rule the local parser refuses one by, and `spec.files` refuses an empty or duplicated server-side-apply key before the API server rejects the whole resource naming neither entry ([@tj-smith47](https://github.com/tj-smith47))
* e840ed29cca4 carry a device's package versions and backup schedule owners onto its MachineConfig status at check-in, and answer with the BackupPolicy schedules the cluster owns so a fleet cadence reaches the machine ([@tj-smith47](https://github.com/tj-smith47))
* 5b99bd406701 reconcile BackupPolicy — project a fleet schedule onto every selected machine and report, rather than silently override, a unit the machine pins locally ([@tj-smith47](https://github.com/tj-smith47))

---
### Bug Fixes

* cf041959f2e6 say the cluster projected a unit's cadence on every surface that names it, keep each listing helper under its own rustdoc, floor every source walk per file, and resolve only the two crates the sweep bumped ([@tj-smith47](https://github.com/tj-smith47))
* 6f63e9b9ca0d refuse a package name a Windows shim would read as a second command ([@tj-smith47](https://github.com/tj-smith47))
* ae260245a920 group an apply's onChange hooks under the thing that declared them, and fail a walk that cannot read a file it enumerated ([@tj-smith47](https://github.com/tj-smith47))
* ab6d25e549b1 name both claimants when two module file entries share a target, and word each refusal with the one subject its parser uses ([@tj-smith47](https://github.com/tj-smith47))
* 034cdf8ffa20 refuse a Module whose lifecycle hook has nothing to run ([@tj-smith47](https://github.com/tj-smith47))
* 83aae4f105c0 refuse a blank postApply command on a Module, and make the pod webhook read blank as absent ([@tj-smith47](https://github.com/tj-smith47))
* d2135a9ffde0 refuse a blank postApply in the words of the path it judges, and document the field as a path ([@tj-smith47](https://github.com/tj-smith47))
* 4c20ba8c166a require spec.units on a BackupPolicy in the schema itself, so the API server refuses a policy that schedules nothing before any webhook sees it ([@tj-smith47](https://github.com/tj-smith47))
* 97818ebfa541 validate a BackupPolicy unit's schedule and name by the grammar the machine parses, hoisted into cfgd-schema beside the local parser, and make the CRD completeness guard read every roster a kind must join ([@tj-smith47](https://github.com/tj-smith47))
* 55460404c7df let a check-in retire what the device stopped reporting by applying its status maps server-side, keep a lost gateway from wiping the cluster cadences, re-arm the daemon's backup timers when the projection changes, judge a version pin against every copy a machine holds, and send the daemon's periodic check-in as the device it is ([@tj-smith47](https://github.com/tj-smith47))
* 8c18e1a561bc inject a module into a pod on any Linux-family platform tag ([@tj-smith47](https://github.com/tj-smith47))
* 44f0633a91ee name a label selector by the matchLabels and matchExpressions the CRD spells, judge a rendered cap as the number it states, and list BackupPolicy among the kinds the operator ships ([@tj-smith47](https://github.com/tj-smith47))
* 550f8a49964d read a machine's schedule-owner pin through the one parser and never apply on an unreadable word, count the Applied condition by distinct units and machines, refuse a BackupPolicy that schedules nothing, and drop the accessor nothing calls ([@tj-smith47](https://github.com/tj-smith47))
* acb6904b048f answer a backup unit's duplicate-name and zero-retention refusals from one shared rule, so a profile and a BackupPolicy cannot disagree about the shape of a unit ([@tj-smith47](https://github.com/tj-smith47))
* ed834b401528 extract the shared config value types into a leaf cfgd-schema crate so the local parser and the CRD schema define a file's patch shape once ([@tj-smith47](https://github.com/tj-smith47))

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

[Unreleased]: https://github.com/tj-smith47/cfgd/compare/crd-v0.7.0...HEAD
[0.7.0]: https://github.com/tj-smith47/cfgd/compare/crd-v0.6.0...crd-v0.7.0
[0.6.0]: https://github.com/tj-smith47/cfgd/compare/crd-v0.5.0...crd-v0.6.0
[0.5.0]: https://github.com/tj-smith47/cfgd/releases/tag/crd-v0.5.0
