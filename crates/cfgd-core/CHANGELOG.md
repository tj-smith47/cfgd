# Changelog — cfgd-core

## [Unreleased]

## [0.6.0] - 2026-07-20

### Features

* 4be6be2f83c7 write profiles in canonical bundle form; dual-read every profile surface ([@tj-smith47](https://github.com/tj-smith47))
* e61d837dacb3 profile bundle layout — dual-read profiles/<name>/profile.yaml ([@tj-smith47](https://github.com/tj-smith47))
* 7d0aa6282200 add 'profile migrate' + ambiguity-tolerant scan in doctor ([@tj-smith47](https://github.com/tj-smith47))
* 0a1b8c73f92f emit SchemaStore modelines from every scaffolder ([@tj-smith47](https://github.com/tj-smith47))

---
### Bug Fixes

* f015070811d1 AmbiguousProfile names every coexisting form ([@tj-smith47](https://github.com/tj-smith47))
* d32f7b5efe05 drop misleading migrate hint from AmbiguousProfile error ([@tj-smith47](https://github.com/tj-smith47))
* d4629d3ab68e harden leading-comment capture against BOM and read failures ([@tj-smith47](https://github.com/tj-smith47))
* eb059aba0e42 preserve leading YAML comments across CLI rewrites ([@tj-smith47](https://github.com/tj-smith47))
* c28e50ca2243 guard test-home across compliance-tick spawn_blocking ([@tj-smith47](https://github.com/tj-smith47))
* 9a0d769b71b4 propagate the test-home override across git sync thread hops ([@tj-smith47](https://github.com/tj-smith47))
* 42cf1b6ff7bd resolve home before spawn_blocking in reconcile ([@tj-smith47](https://github.com/tj-smith47))
* 3cad906f0fc6 route every spawn_blocking through the test-home-preserving wrapper ([@tj-smith47](https://github.com/tj-smith47))
* 5ac7cca8d2f6 scope windows service import privately to avoid ambiguous-import-visibilities ([@tj-smith47](https://github.com/tj-smith47))
* 7b38c49f8b3c correct releasing runbook and drop hand-written changelog entry ([@tj-smith47](https://github.com/tj-smith47))
* ea2092e1fb69 teach explain, AI tools, and SchemaStore globs the profile bundle form ([@tj-smith47](https://github.com/tj-smith47))
* dd7b115d5082 non-interactive prompt refusal names the real cause ([@tj-smith47](https://github.com/tj-smith47))
* 80121382f26f FreeBSD pkg version-constraint + versioned-name convergence ([@tj-smith47](https://github.com/tj-smith47))
* 620405cb328d bootstrap via POSIX sh, not bash ([@tj-smith47](https://github.com/tj-smith47))
* 6dd8dd923677 make every profile reader accept both bundle and flat forms ([@tj-smith47](https://github.com/tj-smith47))
* 42272cdadc48 scope ambiguity fail-closed to the ambiguous profile itself ([@tj-smith47](https://github.com/tj-smith47))
* 7d350d347475 clarify interpreter-spawn error; portable cfg(unix) tests ([@tj-smith47](https://github.com/tj-smith47))
* 84a938f37f36 inject POSIX '.' source line and migrate legacy 'source' ([@tj-smith47](https://github.com/tj-smith47))
* 5f446f6ff0ab omit inert environment.d file on FreeBSD ([@tj-smith47](https://github.com/tj-smith47))
* 85b30911ad53 use canonical http draft-07 $schema URI ([@tj-smith47](https://github.com/tj-smith47))
* 222f7c1464a0 honor the test-home override in user-scope state resolution ([@tj-smith47](https://github.com/tj-smith47))
* 0acc303a175b anchor detached-checkout git spawns to the test tempdir ([@tj-smith47](https://github.com/tj-smith47))
* 68953952fc16 accept any canonical-repo workflow as the cosign signer ([@tj-smith47](https://github.com/tj-smith47))
* 0a1a570eac82 anchor cosign ref pins; restrict tag alternative to version tags ([@tj-smith47](https://github.com/tj-smith47))
* 3de7afd6bf27 pin cosign identity to the three signing workflows ([@tj-smith47](https://github.com/tj-smith47))

---
### Performance

* 31f1079a57f1 stop re-parsing profiles during resolve and scanning ([@tj-smith47](https://github.com/tj-smith47))

---
### Others

* f1c282a0d707 revert stranded v0.6.0 stamp for clean re-cut ([@tj-smith47](https://github.com/tj-smith47))
* 4d3c83ae112c scaffold-write helper, ensure_parent_dir, drop dead is_yaml_ext ([@tj-smith47](https://github.com/tj-smith47))
* 01e05dd1c3db split inline test modules into sibling tests.rs files ([@tj-smith47](https://github.com/tj-smith47))
* 3974b1926669 restore >=93% line floor with error-path and conversion tests ([@tj-smith47](https://github.com/tj-smith47))
* ccaef554088b serialize version-cache tests against CFGD_CACHE_DIR leak ([@tj-smith47](https://github.com/tj-smith47))

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

[Unreleased]: https://github.com/tj-smith47/cfgd/compare/core-v0.6.0...HEAD
[0.6.0]: https://github.com/tj-smith47/cfgd/compare/core-v0.5.0...core-v0.6.0
[0.5.0]: https://github.com/tj-smith47/cfgd/compare/core-v0.4.0...core-v0.5.0
