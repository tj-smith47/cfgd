# Changelog — cfgd-csi

## [Unreleased]

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

[Unreleased]: https://github.com/tj-smith47/cfgd/compare/csi-v0.7.1...HEAD
[0.7.1]: https://github.com/tj-smith47/cfgd/compare/csi-v0.7.0...csi-v0.7.1
[0.7.0]: https://github.com/tj-smith47/cfgd/compare/csi-v0.5.0...csi-v0.7.0
[0.5.0]: https://github.com/tj-smith47/cfgd/compare/csi-v0.4.0...csi-v0.5.0
