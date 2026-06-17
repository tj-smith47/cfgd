# Changelog — cfgd-core

## [Unreleased]

## [0.5.0] - 2026-06-17

### Features

* a5918e305254 include module resources and check file content (TJ Smith)
* 07841742813d accept both list and struct forms for every package manager (TJ Smith)
* 38b9c3cb933d add FileManager::content_drift so core can check file content (TJ Smith)
* 354753abb891 make diff, status -e, and verify see module packages and system (TJ Smith)
* 95d9b5be6f23 broaden spec.env to full user-level reach with envScope knob (TJ Smith)
* ba9eb443a347 fire module-level onDrift scripts in the daemon drift path (TJ Smith)
* e18bd5e031cd module-level platforms gating on ModuleSpec (TJ Smith)
* bc2e00321c2f permissions on module file entries + shared octal-mode parser (TJ Smith)
* 70aef4d1f513 dedupe package installs across profile and module scopes (TJ Smith)
* 995b106347e8 persist custom-manager uninstall so orphaned packages still prune (TJ Smith)
* bff4adba68d1 state-tracked declarative package removal (TJ Smith)
* 9a5c37655038 declarative idempotency guards onlyIf/unless/creates (TJ Smith)
* 1f679bf413a3 interactive lifecycle scripts (TTY-or-skip-with-warn) (TJ Smith)
* 40f48d60d89c nested git keys + warn on unknown system keys (TJ Smith)

---
### Bug Fixes

* be0617aff574 exit nonzero (code 7) on partial or total apply failure (TJ Smith)
* 64805e189d90 mcp examples help, CFGD_YES boolish env, content-aware drift in status/verify -e (TJ Smith)
* b6e62ec97041 --config <dir> infers the discovery config file (TJ Smith)
* 82578bc5ea2f macOS LaunchAgent publishes spec.env via launchctl setenv (TJ Smith)
* a5b33f8d8891 macOS system environment uses a true system LaunchDaemon (TJ Smith)
* 52ea0699e008 start daemon on --install-daemon, honor --name on clone, consistent HOME-unset handling (TJ Smith)
* f6afdb752460 omit null description + correct parse-vs-not-found error code (TJ Smith)
* 24be2bc8736e apply module-contributed spec.system settings (TJ Smith)
* da0f956a2391 tilde-expand ageKey + secret targets; systemd unitFile config-dir resolution, 0644, honest skip (TJ Smith)
* 9da6d5687e45 unify state DB filename + typed no-config exit (exit 3, names path) (TJ Smith)
* 69a43156643e match anodizer release contract — keyless cosign + split sha256 (TJ Smith)
* ef23370cc833 content-aware module-file drift + truthful status -e display (TJ Smith)

---
### Others

* 6a9439d7de12 broaden clippy gate to --workspace --all-targets (TJ Smith)
* b85d1bcbd3ab fix clippy --all-targets needless-borrow and attribute lints (TJ Smith)
* fadddf42f591 correct direct-download asset names to keyless go-arch model (TJ Smith)
* 3ce6a1bb275e correct keyless wording + harden checksum validation (TJ Smith)
* bd7061f4b922 add effective-state source of truth for profile-modules merge (TJ Smith)
* b3a2b1383c79 drop orphaned OciError::SignatureRequired variant (TJ Smith)
* 1dc200e1e6d6 pin client to real release manifest (ground-truth contract test) (TJ Smith)

[Unreleased]: https://github.com/tj-smith47/cfgd/compare/core-v0.5.0...HEAD
[0.5.0]: https://github.com/tj-smith47/cfgd/compare/core-v0.4.0...core-v0.5.0
