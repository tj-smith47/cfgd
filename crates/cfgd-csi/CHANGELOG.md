# Changelog — cfgd-csi

## [Unreleased]

## [0.8.0] - 2026-10-06

### Features

* 0346357ce37b read the driver name from CSI_DRIVER_NAME, defaulting to csi.cfgd.io ([@tj-smith47](https://github.com/tj-smith47))

---
### Bug Fixes

* ab9385126612 update-check, gateway CORS and CSI allow-list env names join the one CFGD_* name module ([@tj-smith47](https://github.com/tj-smith47))
* 871a692d9e3f count a cache hit on the publish path too, where inline ephemeral volumes are mounted ([@tj-smith47](https://github.com/tj-smith47))
* 3c3ec5f6bb1b count a cache hit once per mount: at stage when staged, at publish when inline ([@tj-smith47](https://github.com/tj-smith47))
* dc272fb8a8cc document CSI_DRIVER_NAME and read it only when a pod carries modules ([@tj-smith47](https://github.com/tj-smith47))
* 8c33e1329cbe drop the trailing period prometheus-client already adds to the cache-hits help ([@tj-smith47](https://github.com/tj-smith47))
* 17ed8806c140 module pull takes its platform out of an OCI index, and every module push joins the tag ([@tj-smith47](https://github.com/tj-smith47))
* 5f46e7eee779 module sign and pull checks name the digest at the tag, and --platform is checked at parse ([@tj-smith47](https://github.com/tj-smith47))
* c90ced00760c register counters without a _total suffix so samples render it once ([@tj-smith47](https://github.com/tj-smith47))

## [0.7.2] - 2026-09-16

### Bug Fixes

* 88780e29d7e2 refuse a socket path whose ancestors another account can swap, roster the dispatcher installer wrapper so its callers are demanded the group, and name the chmod walk roots ([@tj-smith47](https://github.com/tj-smith47))

## [0.7.1] - 2026-09-07

### Bug Fixes

* 6c9a58101035 scan test code for session-narrative comments, mint declared_modules_dir, and widen the themed-arrow walk ([@tj-smith47](https://github.com/tj-smith47))
* af9dd3475ddf hold module push, pull, build and image pack under their own title section, report a Module signature as one word in a SIGNATURE column, open kubectl cfgd status with its context and namespace and measure the pods carrying modules, fill a Module PLATFORMS column from the artifact it names, and prove the exec bit survives push and CSI unpack ([@tj-smith47](https://github.com/tj-smith47))
* 2b702ea6c796 escape the C1 range in the JSON log line, so a U+009B cannot repaint kubectl logs ([@tj-smith47](https://github.com/tj-smith47))
* fdb725647e37 fold every tracing event before it reaches a terminal ([@tj-smith47](https://github.com/tj-smith47))
* 56752544339b remove a stale hash-refresh walk's backward blind spot, extend the "we"/self-citation sweep repo-wide with an audit.sh gate, and pin every module-cache hand-join and themed-arrow-into-json seam ([@tj-smith47](https://github.com/tj-smith47))
* adee01e580c0 read an env default and await a shutdown request through one shared helper each, and widen the duplicate-function audit to generic and pub(crate) definitions ([@tj-smith47](https://github.com/tj-smith47))

## [0.7.0] - 2026-08-16

### Features

* f88d99604c8a phase-first apply tree with kind:name owner groups, concurrent per-manager installs, and live rows that settle in place (#105) ([@tj-smith47](https://github.com/tj-smith47))

## [0.5.0] - 2026-06-17

### Others

* f1ab9eb25ba2 drop now-derivable anodizer config (auto-derived from Cargo.toml) (TJ Smith)

[Unreleased]: https://github.com/tj-smith47/cfgd/compare/csi-v0.8.0...HEAD
[0.8.0]: https://github.com/tj-smith47/cfgd/compare/csi-v0.7.2...csi-v0.8.0
[0.7.2]: https://github.com/tj-smith47/cfgd/compare/csi-v0.7.1...csi-v0.7.2
[0.7.1]: https://github.com/tj-smith47/cfgd/compare/csi-v0.7.0...csi-v0.7.1
[0.7.0]: https://github.com/tj-smith47/cfgd/compare/csi-v0.5.0...csi-v0.7.0
[0.5.0]: https://github.com/tj-smith47/cfgd/compare/csi-v0.4.0...csi-v0.5.0
