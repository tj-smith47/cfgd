# Changelog — cfgd

## [Unreleased]

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
