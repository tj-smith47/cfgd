# Changelog — cfgd

## [Unreleased]

## [0.6.1] - 2026-07-21

### Bug Fixes

* 0bf799c7b2b5 bump the cargo group across 1 directory with 8 updates ([@tj-smith47](https://github.com/tj-smith47))

## [0.6.0] - 2026-07-20

### Features

* 4be6be2f83c7 write profiles in canonical bundle form; dual-read every profile surface ([@tj-smith47](https://github.com/tj-smith47))
* 7d0aa6282200 add 'profile migrate' + ambiguity-tolerant scan in doctor ([@tj-smith47](https://github.com/tj-smith47))
* 0a1b8c73f92f emit SchemaStore modelines from every scaffolder ([@tj-smith47](https://github.com/tj-smith47))

---
### Bug Fixes

* 5f491cf8a240 unify profile not-found mapping and gather delete confirmations up front ([@tj-smith47](https://github.com/tj-smith47))
* f015070811d1 AmbiguousProfile names every coexisting form ([@tj-smith47](https://github.com/tj-smith47))
* d4629d3ab68e harden leading-comment capture against BOM and read failures ([@tj-smith47](https://github.com/tj-smith47))
* eb059aba0e42 preserve leading YAML comments across CLI rewrites ([@tj-smith47](https://github.com/tj-smith47))
* 00e9d4409128 force rustls ring backend so the FreeBSD release binary builds ([@tj-smith47](https://github.com/tj-smith47))
* 7b38c49f8b3c correct releasing runbook and drop hand-written changelog entry ([@tj-smith47](https://github.com/tj-smith47))
* ea2092e1fb69 teach explain, AI tools, and SchemaStore globs the profile bundle form ([@tj-smith47](https://github.com/tj-smith47))
* 81ac02bf3f7c exit non-zero on a failed verdict ([@tj-smith47](https://github.com/tj-smith47))
* 9f0827a80e5c fail on missing config at an explicit path ([@tj-smith47](https://github.com/tj-smith47))
* 48f4edd7e7f8 name --config-dir in the explicit-missing detail; pin yaml shape ([@tj-smith47](https://github.com/tj-smith47))
* 64b9bc3518df dry-run reports plan failures in its exit code ([@tj-smith47](https://github.com/tj-smith47))
* 80121382f26f FreeBSD pkg version-constraint + versioned-name convergence ([@tj-smith47](https://github.com/tj-smith47))
* 620405cb328d bootstrap via POSIX sh, not bash ([@tj-smith47](https://github.com/tj-smith47))
* 9deea882b85a query pkg version via rquery, not the unsynced search index ([@tj-smith47](https://github.com/tj-smith47))
* 6dd8dd923677 make every profile reader accept both bundle and flat forms ([@tj-smith47](https://github.com/tj-smith47))
* 78a00d4c5a3d posix-fold the profile create JSON path payload ([@tj-smith47](https://github.com/tj-smith47))
* 42272cdadc48 scope ambiguity fail-closed to the ambiguous profile itself ([@tj-smith47](https://github.com/tj-smith47))
* 2e238285911e surface unparseable manifests and name divergence in scans ([@tj-smith47](https://github.com/tj-smith47))
* 84a938f37f36 inject POSIX '.' source line and migrate legacy 'source' ([@tj-smith47](https://github.com/tj-smith47))
* 88785e97f76f detect bundle-form profile changes and escape grep metachars ([@tj-smith47](https://github.com/tj-smith47))
* b2f94775b24a pin generated grep behavior to real GNU grep and clarify dialect ([@tj-smith47](https://github.com/tj-smith47))
* f8e06c328f28 trigger generated release on canonical profile bundles ([@tj-smith47](https://github.com/tj-smith47))
* 7de899795f89 validate scanned names, detect output-key collisions, tighten change greps ([@tj-smith47](https://github.com/tj-smith47))

---
### Performance

* 31f1079a57f1 stop re-parsing profiles during resolve and scanning ([@tj-smith47](https://github.com/tj-smith47))

---
### Others

* e948559e084b bump the cargo group across 1 directory with 8 updates ([@tj-smith47](https://github.com/tj-smith47))
* f1c282a0d707 revert stranded v0.6.0 stamp for clean re-cut ([@tj-smith47](https://github.com/tj-smith47))
* f64ff8632b47 give cfgd a README + rendering icon on crates.io ([@tj-smith47](https://github.com/tj-smith47))
* 26f0ea446672 add release runbook with SchemaStore-merge done-definition ([@tj-smith47](https://github.com/tj-smith47))
* 83b65a4ddbac consolidate best-effort workflow regeneration into one helper ([@tj-smith47](https://github.com/tj-smith47))
* 4d3c83ae112c scaffold-write helper, ensure_parent_dir, drop dead is_yaml_ext ([@tj-smith47](https://github.com/tj-smith47))
* 01e05dd1c3db split inline test modules into sibling tests.rs files ([@tj-smith47](https://github.com/tj-smith47))
* 68cea5bfbb5a dependabot cargo group bump (e948559e) to complete v0.6.0 ([@tj-smith47](https://github.com/tj-smith47))
* c5dbe921c6ca make package-resolution tests hermetic across native managers ([@tj-smith47](https://github.com/tj-smith47))
* 4cb00480294a make plain cargo test deterministic for PATH-reading tests ([@tj-smith47](https://github.com/tj-smith47))
* 3974b1926669 restore >=93% line floor with error-path and conversion tests ([@tj-smith47](https://github.com/tj-smith47))
* ab7bc5fe13f4 rename apply-profile miss test to match exit-code siblings ([@tj-smith47](https://github.com/tj-smith47))
* 9135c503f50d make go-bootstrap assertion track its prerequisite ([@tj-smith47](https://github.com/tj-smith47))
* ecc6c8dd873c make diff_matching hermetic across OSes ([@tj-smith47](https://github.com/tj-smith47))
* 5fd8ee72e755 give global_scope human-render its own home to stay hermetic ([@tj-smith47](https://github.com/tj-smith47))

## [0.5.0] - 2026-06-17

### Features

* 7822dc009da9 add 'cfgd man' subcommand and build out release dogfooding (TJ Smith)
* a5918e305254 include module resources and check file content (TJ Smith)
* 38b9c3cb933d add FileManager::content_drift so core can check file content (TJ Smith)
* 354753abb891 make diff, status -e, and verify see module packages and system (TJ Smith)
* 95d9b5be6f23 broaden spec.env to full user-level reach with envScope knob (TJ Smith)
* ba9eb443a347 fire module-level onDrift scripts in the daemon drift path (TJ Smith)
* e18bd5e031cd module-level platforms gating on ModuleSpec (TJ Smith)
* bc2e00321c2f permissions on module file entries + shared octal-mode parser (TJ Smith)
* 995b106347e8 persist custom-manager uninstall so orphaned packages still prune (TJ Smith)
* bff4adba68d1 state-tracked declarative package removal (TJ Smith)
* 6fd034471375 expose action target paths in plan/apply -o json (TJ Smith)
* 9a5c37655038 declarative idempotency guards onlyIf/unless/creates (TJ Smith)
* 1f679bf413a3 interactive lifecycle scripts (TTY-or-skip-with-warn) (TJ Smith)
* 40f48d60d89c nested git keys + warn on unknown system keys (TJ Smith)

---
### Bug Fixes

* be0617aff574 exit nonzero (code 7) on partial or total apply failure (TJ Smith)
* b62ccdf3881e warn instead of claiming up-to-date when a filter excludes pending work (TJ Smith)
* 64805e189d90 mcp examples help, CFGD_YES boolish env, content-aware drift in status/verify -e (TJ Smith)
* c93dbcbd6bac point first-run users at `cfgd init` on missing config (TJ Smith)
* e0f2253dc1a8 route every CLI error through one central renderer (TJ Smith)
* b6e62ec97041 --config <dir> infers the discovery config file (TJ Smith)
* 82578bc5ea2f macOS LaunchAgent publishes spec.env via launchctl setenv (TJ Smith)
* a5b33f8d8891 macOS system environment uses a true system LaunchDaemon (TJ Smith)
* 6024e1037f2d probe real write access instead of mode bits in ensure_target_writable (TJ Smith)
* dc8b956f6a29 exit non-zero when the git prerequisite is missing (TJ Smith)
* 52ea0699e008 start daemon on --install-daemon, honor --name on clone, consistent HOME-unset handling (TJ Smith)
* f6afdb752460 omit null description + correct parse-vs-not-found error code (TJ Smith)
* 9a13d1adf5da repair nix manager on nix 2.20+ (JSON list + remove by element name) (TJ Smith)
* ff70b99a7b7e use `dnf list --installed` flag for dnf5 compatibility (TJ Smith)
* da0f956a2391 tilde-expand ageKey + secret targets; systemd unitFile config-dir resolution, 0644, honest skip (TJ Smith)
* 9da6d5687e45 unify state DB filename + typed no-config exit (exit 3, names path) (TJ Smith)
* ef23370cc833 content-aware module-file drift + truthful status -e display (TJ Smith)

---
### Others

* b85d1bcbd3ab fix clippy --all-targets needless-borrow and attribute lints (TJ Smith)
* 78a623147447 correct upgrade --help to keyless cosign model (TJ Smith)
* 8c8cd17b6238 docs/test: use canonical `cfgd completion` (keep completions alias + coverage) (TJ Smith)
* 1d38d7c8735f drop audit tag, first-person, and stale comment in drift code (TJ Smith)
* 018a7ff7b259 migrate upgrade CLI test fixtures to split/keyless contract (TJ Smith)
* a397277605a9 serialize LOCALAPPDATA env tests to fix full-suite flake (TJ Smith)

[Unreleased]: https://github.com/tj-smith47/cfgd/compare/v0.6.1...HEAD
[0.6.1]: https://github.com/tj-smith47/cfgd/compare/v0.6.0...v0.6.1
[0.6.0]: https://github.com/tj-smith47/cfgd/compare/v0.5.0...v0.6.0
[0.5.0]: https://github.com/tj-smith47/cfgd/compare/v0.4.0...v0.5.0
