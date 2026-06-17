# Changelog — cfgd-operator

## [Unreleased]

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

[Unreleased]: https://github.com/tj-smith47/cfgd/compare/operator-v0.5.0...HEAD
[0.5.0]: https://github.com/tj-smith47/cfgd/compare/operator-v0.4.0...operator-v0.5.0
