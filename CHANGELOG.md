# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **env**: Broaden spec.env to full user-level reach with envScope knob
- **plan**: Expose action target paths in plan/apply -o json
- **cli**: Add 'cfgd man' subcommand and build out release dogfooding
- **packaging**: Ship completions + man page in linux packages and cask
- **release**: Publish cfgd to AUR via aur_source (build-from-source)
- **packages**: State-tracked declarative package removal

### CI/CD

- **release**: Sign cosign artifacts keyless (Fulcio/OIDC) to match upgrade client
- **release**: Drop UPX packing (fixes DeepInstinct MALICIOUS false positive)
- Clear last run's annotations without dropping features
- **nightly**: Build anodizer from publisher-required-config branch
- **docker**: Nightly-aware image tags; drop vestigial docker_manifests

### Changed

- **errors**: Drop orphaned OciError::SignatureRequired variant
- **cli**: Drop audit tag, first-person, and stale comment in drift code

### Documentation

- **upgrade**: Correct keyless wording + harden checksum validation
- **installation**: Keyless signature verification + cfgd upgrade
- **cli**: Correct upgrade --help to keyless cosign model
- **installation**: Correct direct-download asset names to keyless go-arch model
- Align remaining release-asset references with go-arch/keyless contract
- Use canonical `cfgd completion` (keep completions alias + coverage)
- **env**: Add worked envScope example showing reach across contexts
- **system**: MacOS environment configurator is a system LaunchDaemon
- Add AUR (Arch Linux) install entry

### Fixed

- **upgrade**: Match anodizer release contract — keyless cosign + split sha256
- **install**: Install.sh uses go-arch asset names + per-artifact sha256 + keyless cosign
- **e2e**: Atomic lease acquire + label all e2e namespaces for janitor reaping
- **e2e**: Record_green_sha must merge-patch, not clobber
- **e2e**: Heartbeat active-run namespaces so the janitor never reaps a live run
- **build**: Name release artifacts from the CLI crate version + Go-arch
- **cli**: Point first-run users at `cfgd init` on missing config
- **init**: Exit non-zero when the git prerequisite is missing
- **apply**: Warn instead of claiming up-to-date when a filter excludes pending work
- **apply**: Exit nonzero (code 7) on partial or total apply failure
- **env**: MacOS LaunchAgent publishes spec.env via launchctl setenv
- **env**: MacOS system environment uses a true system LaunchDaemon
- **init**: Start daemon on --install-daemon, honor --name on clone, consistent HOME-unset handling
- **secrets,system**: Tilde-expand ageKey + secret targets; systemd unitFile config-dir resolution, 0644, honest skip
- **cli**: Mcp examples help, CFGD_YES boolish env, content-aware drift in status/verify -e
- **state,cli**: Unify state DB filename + typed no-config exit (exit 3, names path)
- **verify,status**: Content-aware module-file drift + truthful status -e display
- **config**: --config <dir> infers the discovery config file

### Miscellaneous

- **tests**: Fix clippy --all-targets needless-borrow and attribute lints

### Testing

- **upgrade**: Pin client to real release manifest (ground-truth contract test)
- **cli**: Migrate upgrade CLI test fixtures to split/keyless contract
- **e2e**: Path-scoped image-build skip + Lease-based setup serialization

### Build

- **ci**: Release cfgd-core first, then cfgd/cfgd-csi/cfgd-operator in parallel
- **ci**: Fix broken publish-trio per-leg gating + fragile downstream gates
- **release**: Partial.by goos→os (anodizer renamed the value)
- **task**: Add commit:quick lightweight commit target

## [0.4.0] - 2026-05-30

### Added

- **yaml**: Migrate to anodize Session B schema (publish.cargo, repository, formats plural)
- **config**: Comprehensive dogfooding sweep across workspaces
- **cfgd**: Drain known-bugs Active + dual-sink Windows daemon logging
- **output**: Printer prompt-response mock + use to cover profile/update restore loop
- **mcp**: Wire brontes as cfgd MCP server
- **release**: Dogfood MCP registry publish via brontes-powered cfgd mcp start
- **output_v2**: Add module skeleton (R1.T1)
- **output_v2**: Add Role enum (R1.T2)
- **output_v2**: Add Verbosity + OutputFormat (R1.T3)
- **output_v2**: Add Theme + 5 presets (R1.T4)
- **output_v2**: Add Component tree types (R1.T5)
- **output_v2**: Add renderer skeleton + glyph lookup (R1.T6)
- **output_v2**: Add Writer trait + blank-line state machine (R1.T07)
- **output_v2**: Add render_heading (R1.T8)
- **output_v2**: Add section open/close + collapse + empty_state (R1.T9)
- **output_v2**: Add render_kv + auto-batched KvBlock alignment (R1.T10)
- **output_v2**: Add render_bullet (R1.T11)
- **output_v2**: Add render_status with role/detail/duration/target (R1.T12)
- **output_v2**: Add render_hint, render_note, render_table (R1.T13)
- **output_v2**: Add Printer struct + top-level emit methods (R1.T14)
- **output_v2**: Add SectionGuard + Drop semantics (R1.T15)
- **output_v2**: Add StatusBuilder with chainable detail/duration/target (R1.T16)
- **output_v2**: Runtime check for orphan top-level emit during open section (R1.T17)
- **output_v2**: Add Spinner + ProgressBar (top-level + section-scoped) (R1.T18)
- **output_v2**: Add SectionGuard::run + Printer::run for live process output (R1.T19)
- **output_v2**: Add raw renderers (diff, syntax_highlight, data_line) (R1.T20)
- **output_v2**: Add prompts (kept verbatim from old API) (R1.T21)
- **output_v2**: Add Doc + SectionBuilder + StatusFields (R1.T22)
- **output_v2**: Add Doc renderer (Component tree → terminal) (R1.T23)
- **output_v2**: Add structured-output bridge (emit routes by OutputFormat) (R1.T24)
- **output_v2**: ThemeOverrides 18→17 fields with legacy-key warnings (R1.T25)
- **output_v2**: Feature-gate for_test* + add for_test_doc/DocCapture (R1.T26)
- **output_v2**: Wire DocCapture into emit() for buffered test snapshots (R1.T27)
- **output_v2**: Emit blank line between top-level groups (R1.T32-pre)
- **output_v2**: Kv_block follows heading at +1 depth (R1.T32-pre)
- **output_v2**: Buffer Status in sections to align trailing column (R1.T32-pre)
- **output_v2-F0**: Add _v2 helpers in cli/helpers.rs (R2.F0.T1)
- **output_v2-F0**: Add _v2 helpers in cli/plan_ops.rs (R2.F0.T2)
- **output_v2-F0**: Add _v2 helpers in cli/source/helpers.rs (R2.F0.T3)
- **output_v2-F1**: Migrate cfgd config show to Doc + emit (R2.F1.T1)
- **output_v2-F1**: Migrate cfgd module list+show to Doc + emit (R2.F1.T2)
- **output_v2-F1**: Migrate cfgd source show to Doc + emit (R2.F1.T3)
- **output_v2-F1**: Migrate cfgd status to Doc + emit (R2.F1.T4)
- **output_v2-F1**: Migrate cfgd profile show to Doc + emit (R2.F1.T5)
- **output_v2-F2**: Migrate cfgd init to Doc + emit (R2.F2.T1)
- **output_v2-F2**: Migrate cfgd compliance snapshot+export+history to Doc + emit (R2.F2.T2)
- **output_v2-F2**: Migrate cfgd compliance diff to Doc + emit (R2.F2.T3)
- **output_v2-F2**: Migrate cfgd doctor to Doc + emit (R2.F2.T4)
- **output_v2-F2**: Migrate cfgd verify to Doc + emit (R2.F2.T5)
- **output_v2-F2**: Migrate cfgd decide to Doc + emit (R2.F2.T6)
- **output_v2-F2**: Migrate cfgd explain to Doc + emit (R2.F2.T7)
- **output_v2-F2**: Migrate cfgd enroll to Doc + emit (R2.F2.T8 — close audit gap)
- **output_v2-F3**: Migrate cfgd apply to Doc + emit (R2.F3.T1)
- **output_v2-F3**: Migrate cfgd plan to Doc + emit (R2.F3.T2)
- **output_v2-F3**: Migrate cfgd sync to Doc + emit (R2.F3.T3)
- **output_v2-F3**: Migrate cfgd pull to Doc + emit (R2.F3.T4)
- **output_v2-F3**: Migrate cfgd diff to Doc + emit (R2.F3.T5)
- **output_v2-F3**: Migrate cfgd rollback to Doc + emit (R2.F3.T6)
- **output_v2-F3**: Migrate cfgd checkin to Doc + emit (R2.F3.T7)
- **output_v2-F3**: Migrate cfgd log to Doc + emit (R2.F3.T8)
- **output_v2-F3.5**: Migrate profile/* to Doc + emit (R2.F3.5.T1)
- **output_v2-F3.5**: Migrate source/* to Doc + emit (R2.F3.5.T2)
- **output_v2-F3.5**: Migrate module/{build,keys,apply_crd,export} (R2.F3.5.T3a)
- **output_v2-F3.5**: Migrate module/crud.rs (R2.F3.5.T3b)
- **output_v2-F3.5**: Migrate module/{registry,signature} + close T1 profile→module hybrid (R2.F3.5.T3c)
- **output_v2-F3.5**: Migrate config_cmd.rs + secret.rs to Doc + emit (R2.F3.5.T4)
- **output_v2-F3.5**: Migrate cli/{generate,upgrade,plugin,workflow,mod} extras to Doc + emit (R2.F3.5.T7)
- **output_v2-F4a**: Migrate cli/daemon.rs + close F4a-adjacent helper (R2.F4a.T1)
- **output_v2-F4b**: Migrate daemon lib chain (mod.rs + runner.rs + consumer flips) (R2.F4b.T1)
- **output_v2-F4c**: Migrate reconciler apply core + provider seam to v2 (R2.F4c.T1)
- **output_v2-F4c**: Migrate reconciler action paths to v2 + collapse T1 scaffolding (R2.F4c.T3)
- **output_v2-F4d**: Migrate server_client to v2 + checkin/enroll caller flip (R2.F4d.T1)
- **output_v2-F4d**: Migrate sources to v2 + CLI callers + legacy_printer cleanup (R2.F4d.T2)
- **output_v2-F4d**: Migrate CfgdFileManager (files/{apply,plan}) to v2 + cli/diff cmd restructure (R2.F4d.T3)
- **output_v2-F4d**: Migrate modules to v2 + caller cascade + daemon/reconcile.rs final v1 forwarder collapses (R2.F4d.T4)
- **output_v2-F4e**: Flip SystemConfigurator trait to v2 + migrate 20 impls + drop reconciler v1_forwarder (R2.F4e.T1)
- **output_v2-F4f**: Flip PackageManager trait + all 16 impls to v2 Printer + drain reconciler v1_forwarder shims (R2.F4f.T1)
- **output_v2-F4f**: Migrate OCI pull/push helpers to v2 + drop CLI v1 printer arg (R2.F4f.T2)
- **output_v2-F4f**: Migrate upgrade module + cli/upgrade caller to v2 (R2.F4f.T3)
- **output**: Truecolor (24-bit) rendering when terminal supports it
- **output**: Add accent + secondary theme slots
- **output**: Adopt accent + secondary at cfgd module list
- **output**: Adopt secondary at cfgd sync source loop
- **output**: Adopt secondary at cfgd status drift attribution
- **output**: Split apply partial-success into Ok + Accent lines
- **release**: Wire krew-release-bot for v0.4.0 auto-promotion
- **cli**: Add 'rm' alias to profile/module delete subcommands
- **cli**: Add 'alias' subcommand tree for spec.aliases CRUD
- **cli**: Add 'ls'/'rm' clap aliases on ConfigCommand and fix long_about examples
- **test-helpers**: Add ReconcilerTestHarness builder for reconciler stack
- **operator/test-helpers**: Add GatewayTestApp builder for router integration tests
- **cli/plugin**: Add kube-rs mock tests for kubectl plugin commands
- **test-helpers**: Add BareGitRepo builder for bare-repo test fixtures
- **release**: Adopt anodizer v0.4.0 features
- **upgrade**: Add --require-cosign flag for strict signature verification
- **config**: Deny_unknown_fields on user-facing config shapes to catch typos
- **config**: Add shell field to ScriptEntry for interpreter selection
- **scripts**: Source ~/.cfgd.env for bash/zsh lifecycle scripts
- **paths**: Consolidate cross-OS path/io text normalization
- **paths**: Wave 1 — route path-to-JSON through to_posix_string
- **paths**: Wave 5 — generate/files.rs blocked-pattern check via to_posix_string
- **test-helpers**: Add assert_snapshot_golden shared harness
- **paths**: Add PathDisplayExt trait for Wave 4 prep
- **paths**: Wave 4 batch 1 — reconciler .display() → .posix()
- **paths**: Wave 4 batch 2 — daemon/modules/upgrade .display() → .posix()
- **paths**: Wave 4 batch 3 — sources/output/oci/util .display() → .posix()
- **paths**: Wave 4 batch 4 — cli/ .display() → .posix()
- **paths**: Wave 4 batch 5 — cfgd subsystems .display() → .posix()
- **paths**: Wave 4 batch 6 — operator + csi .display() → .posix()
- **paths**: Wave 4 wrap-up — audit gate + apply-error display split
- **paths**: Always-fold wire/DB boundary paths via to_posix_string
- **cli**: --shell flag on cfgd apply forces inline-script interpreter
- **ci**: Add scheduled nightly workflow

### CI/CD

- Pin anodizer-action to @v1 instead of @master
- Pass apk-private-key to anodizer-action
- Switch to anodizer-action version: latest
- Switch anodizer-action to from-branch: publisher-required-config
- **e2e**: Install cosign on full-stack-tests runner
- Migrate to workflow_run-triggered release via anodizer tag
- Build anodizer once in ci.yml, download artifact in release.yml
- **release**: Nest determinism shards under per-crate parent via reusable workflow
- Bump actions/checkout to v6; gitignore restore-to-version.sh
- Rename arduino/setup-task to go-task/setup-task
- **release**: Wire `anodize tag rollback` failure-recovery step
- **release**: Extend rollback if: + drop restore-to-version.sh
- Trigger release retry after rollback recovery
- **release**: Drop redundant krew-bot step; add mcp oci ownership label
- **release**: Re-attempt v0.4.0 after anodizer publish-only fixes
- **release**: Re-attempt v0.4.0 after upload read-after-write fix
- **release**: Re-attempt v0.4.0 with Discussions enabled
- **release**: Re-attempt v0.4.0 with binary-producing determinism harness

### Changed

- **cli**: Carve upgrade into cli/upgrade.rs (S-6)
- **cli**: Carve verify into cli/verify.rs (S-6)
- **cli**: Carve diff into cli/diff.rs (S-6)
- **cli**: Carve status into cli/status.rs (S-6)
- **cli**: Carve apply into cli/apply.rs (S-6)
- **cli**: Drain S-6 reviewer cleanup-pass items
- **system**: Split system/mod.rs into per-configurator submodules
- **packages**: Split packages/mod.rs into per-manager submodules
- **packages**: Split per-manager tests into submodule test blocks
- **oci**: Split oci.rs into oci/ submodules
- **modules**: Split modules/mod.rs into per-concern submodules
- **controllers**: Split controllers/mod.rs into per-reconciler submodules
- **modules,controllers**: Externalize inline test blocks to sibling tests.rs
- **daemon**: Split daemon/mod.rs into per-concern submodules
- **cli/module**: Split cli/module.rs into module/ submodules
- **gateway/api**: Split gateway/api.rs into api/ submodules
- **cfgd-core/reconciler**: Split helpers into submodules + externalize tests
- **cfgd-core/reconciler**: Move provenance_suffix to format.rs
- **cfgd-core/reconciler**: Split Reconciler<'a> impl into per-phase submodules
- **cfgd-core/reconciler**: Tighten apply_action visibility + add method separators (9b review fix)
- **cfgd-core/composition**: Split mod.rs into per-concern submodules
- **cfgd-core/config**: Split mod.rs into per-concern submodules
- **cfgd-core/output**: Split mod.rs into per-concern submodules
- **cfgd-core/output**: Tighten non_interactive_err visibility (10A review fix)
- **cfgd-core/upgrade**: Promote to directory + externalize tests
- **cfgd/system/node**: Split into per-impl submodules
- **cfgd/files**: Split mod.rs into per-concern submodules
- **cfgd/files**: Tighten resolve_source_path visibility (10B review fix)
- **cfgd/secrets**: Split mod.rs into per-backend submodules
- **cfgd/cli/profile**: Split into per-handler submodules
- **cfgd/cli/init**: Split into per-concern submodules
- **cfgd/cli/explain**: Split into per-schema submodules
- **cfgd/cli**: Externalize tests to cli/tests.rs
- **cfgd/cli**: Extract cmd_doctor to cli/doctor.rs
- **cfgd/cli**: Extract cmd_workflow_* to cli/workflow.rs
- **cfgd/cli**: Extract cmd_compliance_* to cli/compliance.rs
- **cfgd/cli**: Extract cmd_config_* to cli/config_cmd.rs
- **cfgd/cli**: Extract cmd_log* to cli/log.rs
- **cfgd/cli**: Extract cmd_secret_* to cli/secret.rs
- **cfgd/cli**: Extract cmd_daemon* to cli/daemon.rs
- **cfgd/cli**: Extract cmd_rollback, cmd_sync, cmd_pull to siblings
- **cfgd/cli**: Extract cmd_checkin + cmd_decide to siblings
- **cfgd/cli**: Extract cmd_plan to cli/plan.rs
- **cfgd/cli**: Extract cmd_source_* to cli/source.rs
- **cfgd/packages**: Externalize tests to packages/tests.rs
- **cfgd-core**: Externalize tests in state/ and sources/
- Externalize tests in lib.rs, gateway/db, webhook
- Externalize tests in 5 more 1k+ line modules
- Externalize tests in csi/node, gpg_keys, shared, crds
- Externalize tests in server_client, oci/auth, system, cli/generate
- Externalize tests in 6 more 800+ line modules
- Externalize tests in 6 more 600+ line modules
- **cfgd/cli/source**: Split into per-handler submodules
- **cfgd/cli**: Extract structured output types to output_types.rs
- **cfgd/cli**: Extract plan helpers to plan_ops.rs
- **cfgd/cli**: Extract provider registry + secret backend wiring to registry.rs
- **cfgd/cli**: Extract remaining helpers + composition to helpers.rs
- **cfgd-core/state**: Decompose 1,637-line state/mod.rs into 12 sibling submodules
- **cfgd-operator/gateway/db**: Decompose 1,622-line db/mod.rs into 9 sibling submodules
- **cfgd-core/lib**: Split shared-utility kitchen-sink into 13 topic files
- **cfgd-operator**: Extract env-parsing helpers + 17 tests
- **cli/module/registry**: Extract search filter + review-summary helpers + 11 tests
- **sources**: Extract classify_signature_status + 10 tests
- **cli/module/registry**: Extract compute_lock_url + compute_pinned_ref + ensure_module_in_profile_doc
- **cli/init/enroll**: Extract build_device_credential + next_steps_lines
- **cli/plugin**: Extract build_inject_patch_json + 4 tests
- **daemon**: Extract run_daemon_loop harness + 26 tests
- **operator**: Extract runtime module from main.rs + 15 tests
- **daemon**: Extract build_pre_loop_setup from run_daemon
- **daemon**: Extract pre-loop helpers from run_daemon + 14 tests
- **util/git**: Add git_cmd_local + route 4 prod sites through it
- **util/time**: Extract iso8601_to_filename_safe helper
- **cli**: Expose Cli via lib.rs for integration testing
- **output_v2**: Align #[must_use] hint on section_or_collapse (R1.T15)
- **output_v2**: Rename _b binding in chained_detail test (R1.T16)
- **output_v2**: Harden T17 test + clarify enforce_top_level_emit (R1.T17)
- **output_v2**: DRY spinner/progress_bar constructors + branch test (R1.T18)
- **output_v2**: Simplify recv loop + DRY CommandOutput tail (R1.T19)
- **output_v2**: Simplify SectionBuilder::extend + add symmetric kv-coalesce test (R1.T22)
- **output_v2**: Consolidate for_test* via private build helper (R1.T26)
- **output_v2**: Consolidate section/table headers on theme.header
- **output_v2-F4c**: Collapse daemon/reconcile.rs v1 forwarders (R2.F4c.T4)
- **output_v2-F4d**: Extract snapshot-test helpers to output_v2::test_capture (R2.F4d.T1 drain)
- **output_v2-R3**: Drop all v1 Printer callers outside output/ (R3.T1)
- **output_v2-R3**: Delete dead v1 test_printer() + import from test_helpers (R3.T1 drain)
- **output_v2-R3**: Rename output_v2/ → output/ after R2 migration (R3.T2)
- **output-R3+**: Drop PrinterV2 alias post-rename (R3+.T1)
- **output-R3+**: Drop remaining v2 type aliases (R3+.T2)
- **output-R3+**: Rename v2_quiet helper, fix stale comment (R3+.T2)
- **output-R3+**: Rename v2_printer + v2_buf identifiers to plain names (R3+.T3)
- **output-R3+**: Drop _v2 suffix from plan_ops fns (R3+.T4a)
- **output-R3+**: Drop _v2 suffix from cli helpers + diff fns (R3+.T4b)
- **output-R3+**: Drop _v2 suffix from test_printer helper (R3+.T4c)
- **output-R3+**: Drop v2 from make_* / test_* test helpers (R3+.T4d)
- **output-R3+**: Delete duplicate _v2 test fns (R3+.T4e)
- **output-R3+**: Rename v2_theme_name + v2_local production locals (R3+.T4)
- **output-R3+**: Rename lib_printer → silent_printer in sync/show (R3+.T4)
- **output**: Delete CFGD_OUTPUT_V2_AUDIT gate, rename _EXTRA_PATH
- **output**: Scrub session narrative from output module comments
- **output**: Scrub session narrative from non-output test files
- **output**: Scrub session narrative from non-test production code
- **output**: Rename *_v2_snapshots.rs → *_snapshots.rs
- **output**: Hot-path Display for AttrSet + RAII colors guard
- **output**: Replace Printer::style with StatusBuilder::label
- **output**: Dedup label compose; harden test with #[serial]
- **output**: Hoist ColorsEnabledGuard to one shared test helper
- **output**: Tighten Table fields to pub(crate)
- **test**: Rename bucket_a..g test files to topic-only names
- **packages/versions**: Add tool_cmd seams for apt-cache/apk/pkg/pacman/dpkg-query/rpm/dnf/yum/zypper
- **packages/simple**: Route install/uninstall/update/list shell-outs through tool_cmd seams
- **taskfile**: Split `coverage` into `coverage:check` (local) and `coverage:publish` (CI)
- **packages**: Bootstrap_via_shell_script helper + 2 happy/sad tests
- **test_helpers**: Hoist install_named_path_shim — dedupes 4x ~22-line PATH-shim helper across scoop/choco/cargo/nix
- **packages/pipx**: Use install_named_path_shim — drops local 22-line curl-shim helper
- **test-helpers**: Consolidate inline #!/bin/sh + PATH-shim patterns via install_named_path_shim
- **test-helpers**: Replace inline Printer::new(Verbosity::Quiet) with shared test_printer helper
- **test-helpers**: Finish test_printer migration across signature/init/profile/files/node tests
- **test-helpers**: Add NoopDaemonHooks + install_named_path_shims; drop local duplicate impls
- **cfgd-operator**: Hoist main.rs orchestration into lib::app for coverage
- **cfgd-operator**: Downgrade app visibility, add run() coverage test
- **cfgd-operator**: Drop doc comments on private fns, dedup shutdown drain
- **cfgd-csi**: Hoist main.rs orchestration into lib::app for coverage
- **cfgd-csi**: Drop fixed-port metrics test; document Metrics variant
- **reconciler/scripts**: Tidy validation error path
- **cfgd-core**: Use shared time/path helpers and log silent rollback failures
- **cfgd-core**: Extract git_err closure and spawn_pipe_reader + hash_sorted_parts helpers
- **cli**: Switch checkin to anyhow::Context and surface error chain in spinner detail
- **tests**: Delegate 41 local assert_snapshot fns to shared harness
- **tests**: Delegate 15 normalize fns to normalize_for_snapshot

### Documentation

- **install**: Note kubectl-cfgd plugin pending Krew index PR review
- **anodizer**: Drop AI implementation comments from mcp block
- **output_v2**: Clarify flush_pending_section_headers semantics (R1.T9)
- **output_v2**: Update config schema reference to 17-field ThemeOverrides (R1.T25)
- **audit**: Correct bad_indent_tab regex note; document EXTRA_PATH replace semantics (R1.T35 review)
- **output_v2-F0**: Note R3 kill-date on _v2 dead_code allows (R2.F0.T1 review)
- **output_v2-F0**: Bridge marker on prompt_backup_choice_v2 + drop format!() (R2.F0.T2 review)
- **output_v2-F0**: N2 deref simplify + F1-reviewer notes on policy/decisions v2 (R2.F0.T3 review)
- **output_v2-F4c**: Clarify test_printer / test_printer_v2 coexistence rationale (R2.F4c.T1 drain)
- **output_v2-R3**: Output-module.md v2 vocab + §17.3 coverage gate (R3.T3)
- **output-R3+**: Drop stale v2 narrative from comments touched by T1 (R3+.T1)
- **output-R3+**: Drop stale v2 narrative in registry/enroll comments (R3+.T3)
- **output-R3+**: Drop stale v1/v2 narrative in comments (R3+.T5)
- **rules**: Scrub session narrative from hard-rules + output-module
- **theme**: Add accent and secondary entries to overrides docs
- **cli**: Clarify is_value_taking_flag short-flag wording
- **cli**: Clarify alias dispatch prefix and List hint
- **daemon**: Explicitly scope SIGHUP reload to timer intervals
- Add lifecycle script shell and env/alias documentation

### Fixed

- **cli**: Print help and exit 0 when no subcommand given
- **cfgd-core/state**: Tighten StateError variants + serde consistency
- **modules/registry**: Strip 'v' prefix when sorting registry tags
- **cfgd-core/util**: Strip leading v/V in parse_loose_version (root cause)
- **packages/shared**: Surface stderr in run_pkg_cmd_live install/uninstall errors
- **nfpm**: Match apk public key by glob, not exact name
- **sbom**: Replace Tera placeholders with shell-style $artifact / $document
- **output_v2**: Single-space em-dash glue in Status detail (R1.T32-pre)
- **output_v2**: Suppress blank between sibling subsections (R1.T32-pre)
- **output_v2**: Reorder Status fields to subject — detail (target) (R1.T32-pre)
- **audit**: Output_v2 in println/Command exclusions; \t escape in indent rules (R1.T34 review)
- **hooks**: Post-edit grep -E needs ANSI-C \t literal; tighten output exemption glob (R1.T36 review)
- **output_v2-F0**: Narrow output_types visibility + strip narrative test comment + drop unused Doc heading (R2.F0.T4 review)
- **output_v2-F1**: From impls for Verbosity/OutputFormat + assert_*_snapshot_in + tighter Enabled assertion + drop redundant flush (R2.F1.T1 review)
- **output_v2-F1**: With_data on module-not-found doc + simplify JSON test prefix-skip (R2.F1.T2 review)
- **output_v2-F1**: Drop last old-API call from source show + not_found Doc + SHORT_COMMIT_LEN const (R2.F1.T3 review)
- **output_v2-F1**: Last Apply key Status→Result + borrow with_data payload for parity (R2.F1.T4 review)
- **output_v2-F1**: Drop unused _cfg binding in cmd_profile_show (R2.F1.T5 review)
- **output_v2-F1**: Fold Config/Profile into Docs; drop dup-error from not-found Docs; ExitCode::NotFound; profile-show {name, resolved} envelope (F1 family review)
- **output_v2-F2**: Thread v2_printer through apply/plan/maybe_update_workflow + 4 callers; narrow InitOutput visibility (R2.F2.T1 review)
- **output_v2-F2**: Wrap export summary in section + add export JSON golden (R2.F2.T2 review)
- **output_v2-F2**: Drop stale T3-owns banner + spec ref + dead cli.output write (R2.F2.T3 review)
- **output_v2-F2**: Restore per-module package checks + bootstrap method + sops path + section ordering + bare-fixture coverage (R2.F2.T4 review)
- **output_v2-F2**: Drop dead buf.clear() + unused buf binding in verify-after-apply test (R2.F2.T5 review)
- **output_v2-F2**: Delete v1 display_pending_decisions + port multi-source/singular coverage to v2 + narrow payload visibility (R2.F2.T6 review)
- **output_v2-F2**: With_data on init/explain/enroll error paths + camelCase serde + snapshot floor (R2.F2 family review A)
- **output_v2-F2**: Add bridge-invariant snapshots for init apply + enroll cmd_token flow (R2.F2 family review B)
- **output_v2-F3**: Replace hand-rolled apply snapshots with real cmd_apply capture + drain T1 review (R2.F3.T1 review)
- **output_v2-F3**: Restore accept-perms commit line + drain T3 review (R2.F3.T3 review)
- **output_v2-F3**: Tighten pull_status_from_result visibility (R2.F3.T4 review)
- **output_v2-F3**: With_data on diff module-not-found path + DRY summary emit via build_diff_doc + docstring fix (R2.F3.T5 review)
- **output_v2-F3**: Accept snapshot + Warn header + Info-on-zero + build_rollback_doc DRY (R2.F3.T6 review)
- **output_v2-F3**: Clarify checkin bridge-synthetic docstring divergence (R2.F3.T7 review)
- **output_v2-F3**: Show_output empty-entries snapshot + JSON coverage + visibility tighten (R2.F3.T8 review)
- **output_v2-F3**: Role::Ok on checkin Saved-to + section_or_collapse for log Entries (R2.F3 family review A)
- **output_v2-F3.5**: Thread .duration() through apply summary (drain known-bugs) (R2.F3.5.T5)
- **output_v2-F3.5**: Drain T1 review (snapshot floor + error-path Docs + real cmd_x capture) (R2.F3.5.T1 review)
- **output_v2-F3.5**: Drain T2 review (Accept-confirm-then-success snaps + EditorGuard lift + error_doc helper + Cancel option for source remove) (R2.F3.5.T2 review)
- **output_v2-F3.5**: Drain T3 review (bridge snapshots + floor gaps + dead param + comment cleanup) (R2.F3.5.T3 review)
- **output_v2-F3.5**: Drain T4 review (cmd_config_show emit-then-bail + secret edit label + spec amends) (R2.F3.5.T4 review)
- **output_v2-F3.5**: Drain T7 review (bridge snapshots + plugin disconnected + comment + spec updates) (R2.F3.5.T7 review)
- **output_v2-F3.5**: Drain T6 reviewer findings (emit-then-bail + role discipline + §17.3 amend + module_build bridge + visibility narrowing) (R2.F3.5.T6 review)
- **output_v2-F4a**: Drain T1 reviewer findings (narrative comments + unused param + runtime_failed emit + install_failed extras) (R2.F4a.T1 review)
- **output_v2-F4b**: Drain T1 reviewer finding (stale print_startup_banner doc comment) (R2.F4b.T1 review)
- **output_v2-F4c**: Daemon service templates pass --quiet; collapse remaining reconcile.rs forwarders (R2.F4c.T7 — gap fix)
- **output_v2-F4c**: Renderer emits exactly one blank line at streaming → buffered Doc seam (R2.F4c.T8)
- **output_v2-F4c**: Drain T8 review — scripts.rs error messages no longer carry stray \n; bridge invariant gains positive seam assertion (R2.F4c.T8 drain)
- **output_v2-F4c**: Drain T8 carry-forward — source-layer fixes for embedded-\n Status subjects + write_line debug_assert (R2.F4c.T8 drain 2)
- **cfgd-core**: Gate test_helpers module behind cargo feature (R2.F4c.T9)
- **output_v2-F4d**: Drop cosmetic hybrid-quiet in source/add + source/update (R2.F4d.T2 drain)
- **output_v2-F4f**: Drop redundant CLI status_simple for cmd_module_push (R2.F4f.T2 drain)
- **system/node**: Collapse multi-line systemctl errors before status_simple subject
- **output**: Centralize multi-line error → subject collapsing
- **output**: Carry Table row_roles through Doc emit so styled cells render
- **output**: Preserve SGR attrs under NO_COLOR
- **output**: Disable colors under structured output formats
- **output**: Strip ANSI from status detail to block escape injection
- **output**: Align Table columns by Unicode display width
- **output**: Sanitize spinner+status subject ANSI at boundary
- **anodizer**: Krew publisher skip_upload after PR acceptance
- **taskfile**: Only enforce /tmp headroom when /tmp is tmpfs
- **gitignore**: Anchor top-level doc un-ignores so they don't leak into subdirs
- **packages/versions**: Address review findings — apk parser, lock-step seams, test hygiene
- **cli/tests**: Execute_module_export_dispatch leaks test-mod/ into repo cwd
- **coverage**: Publish-coverage.sh refuses to run outside CI
- **e2e/cli**: Tighten test-aliases.sh assertions to exact target match
- **cli**: Find_subcommand_index handles every value-taking global flag
- **hooks**: Allow rm -rf when path starts with target/ build dir
- **taskfile**: Activate cfgd-core/test-helpers in test:crate
- **daemon**: Per-user runtime IPC socket with 0600 perms and capped client read
- **daemon**: Isolate per-tick failures and wire per-module reconcile
- **operator**: Fail when webhook task exits to prevent silent admission bypass
- **process**: Escalate to SIGKILL after grace period when child traps SIGTERM
- **keys**: Surface restore failures during cosign key rotation
- **oci**: Replace TOFU signature sentinel with real cosign verify in pull_module
- **release**: Address MEDIUM/LOW audit findings + bump workspace to v0.4.0
- **test/leader**: Mark constructor env tests as tokio::test for kube client init
- **test/is_git_source**: Serial-gate rejects test to prevent race with sibling that sets CFGD_ALLOW_LOCAL_SOURCES
- **reconciler**: Propagate module spec.env to lifecycle script spawn env
- **reconciler**: Propagate module spec.aliases to inline lifecycle scripts
- **reconciler/scripts**: Validate working_dir before spawn
- **reconciler**: --phase {pre,post}-scripts catches module-level scripts
- **config**: Reject CFGD_* env var names at parse time
- **cli**: Rename 'completions' subcommand to 'completion' (singular)
- **ci**: Restore green CI by addressing platform-gating and missing deps
- **cli**: Align pin-version naming + fix SIGHUP e2e grep
- **tests**: Close remaining macos/windows/coverage holes
- **tests**: Comprehensive Windows cfg-gating sweep via cross-compile
- **tests**: Isolate cosign-missing tests from CFGD_COSIGN_BIN seam
- **ci**: [profile.dev] debug=line-tables-only to fit runner disk
- **e2e**: Docker.io rate-limit fallback to mirror.gcr.io
- **csi**: Isolate cache-pull-failure tests from allow-list env race
- **e2e**: Include golang:1.25 in docker.io rate-limit fallback
- **crds**: Drop deny_unknown_fields so schemars 0.8 emits k8s-valid CRD
- **git**: Harden git_cmd_safe against grandchild-pipe hangs on macOS/Windows
- **tests**: Cross-platform stability across windows + macos hangs/diffs
- **tests**: CRLF + file:// + windows asset suffix + macos bind via privileged port
- **tests**: Windows snapshot path-sep normalization + macos metrics gated
- **cross-os**: Drain remaining Windows test failures + E2E setup regressions
- **windows**: Cfg-gate PathDisplayExt import in daemon/health_ipc.rs
- **gateway**: Make enroll-rate-limit overridable via env, raise for E2E
- **windows**: Cfg-gate assert_snapshot import in module_keys_snapshots.rs
- **ci**: Align autotag job with anodizer's pattern; single workspace-aware tag invocation
- **gateway**: Make E2E gateway suite pass without rate-limit cranking
- **e2e**: GW-16 step 3 re-enrollment also needs retry-on-429
- **generate**: Scope git add/commit to repo_root so parallel tests don't collide
- **ci**: Per-crate autotag loop + suppress anodizer post-field deprecation #minor
- **release**: Resolve GITHUB_REF_NAME regression + skipped-tag guard + checkout races
- **release**: Matrix release job per crate to avoid context.json collisions
- **release**: Collapse matrix release to single per-crate-dist publish job
- **release**: Gate crossplane-push on crossplane-function success
- **release**: Configure git identity in tag job before anodizer tag
- **release**: Tag job's head_sha output reflects post-tag HEAD, not workflow_run SHA
- **release**: Correct homebrew cask directory ('Formula' -> 'Casks')

### Miscellaneous

- **ci**: Bump ANODIZER_REV to a19f15e
- Dogfood icon_url + .claude housekeeping
- **workflows**: Bump ANODIZER_REV to dac1f87
- **.claude**: Housekeeping — drain Wave B cycle 2 audits, archive done specs/prompts/plans
- **workflows**: Bump ANODIZER_REV to 073ee71
- **.anodizer**: Skip Krew publisher pending index PR review
- Remove Krew references — publisher dropped, not pending
- **.gitignore**: Untrack non-load-bearing .claude/ files
- **taskfile**: Rewrite commit recipe to respect staged-files contract
- **lockfile**: Record serial_test dev-dep for cfgd-csi (post a77f44d)
- **tests**: Clippy fixes — struct-init form for PolicyItems / ModuleSpec / CCPSpec test fixtures
- **deps**: Flip brontes from git-master to crates.io 0.1.0
- **deps**: Bump brontes 0.1.0 → 0.2.0
- **output_v2**: Extend audit.sh with banned-pattern rules (gated; R1.T34)
- **output_v2**: Audit-test fixtures + driver + ci wiring (R1.T35)
- **output_v2**: Wire rust-edit hook to new audit ruleset (gated; R1.T36)
- **output_v2-F0**: Strip narrative session comments from _v2 helpers
- **output_v2-F1**: Strip dead_code allow on load_config_and_profile_v2 (R2.F1.T6)
- **output_v2-F2**: Strip dead_code allows on F2-wired F0 helpers + collapse check_subject→check_key (R2.F2.T8)
- **output_v2-F3**: Strip dead_code allows on F3-wired F0 helpers + §17.3 entries (R2.F3.T9)
- **output_v2-F4a**: Acceptance — drain family reviewer minor + add cfg! rationale comment (R2.F4a.T2)
- **output_v2-F4b**: Acceptance — lift F4a carve-out + drain family reviewer (R2.F4b.T3)
- **output_v2-F4c**: Acceptance — F4c family closes; drain T6 family-review minor (R2.F4c.T6)
- **output_v2-F4d**: Acceptance — F4d family closes (R2.F4d.T5)
- **tasks**: Add _check:tmp-headroom precondition to cargo tasks
- **output_v2-F4e**: Acceptance — F4e family closes (R2.F4e.T4)
- **output_v2-F4f**: Acceptance — F4f family closes (R2.F4f.T5)
- **output_v2-R3**: Dedupe audit.sh output/ globs after T2 rename (R3.T2 drain)
- **output_v2-R3**: Consumer-script sweep for v1 vocabulary (R3.T4)
- **output-R3**: Acceptance — R3 family closes (R3.T5)
- **output-R3+**: Acceptance — v2 fossils drained (R3+.T5)
- **output**: Acceptance — R3++ narrative drain closed
- **output**: Final-sweep cleanup nits
- Restore Krew references — index PR accepted
- **anodizer**: Drop krew block commentary
- **taskfile**: Forward CLI_ARGS to test + test:crate:* wildcard pattern
- **reconciler**: Drop redundant type annotation, debug-log dropped CFGD_* spec.env entries
- **audit**: Add Wave 1-5 path-handling gates
- **dogfood**: Wire nightly + smartsemver + cargo required: in .anodizer.yaml
- **plans**: Remove upstream-kubernetes duplicate, consolidated into .claude/future/
- **anodize**: Drop redundant before-hooks from release pipeline
- **cfgd-core**: Release prep #minor
- **cfgd**: Release prep #minor
- **cfgd-operator**: Release prep #minor
- **cfgd-csi**: Release prep #minor
- Restore manifests to v0.3.5
- Restore [package].version to v0.3.5
- Restore [package].version to v0.3.5
- Restore [package].version to v0.3.5
- Restore [package].version to v0.3.5
- Restore [package].version to v0.3.5
- Restore [package].version to v0.3.5
- Restore [package].version to v0.3.5
- Restore [package].version to v0.3.5
- Restore [package].version to v0.3.5

### Performance

- **ci**: Nextest + sccache + buildx GHA cache + warm-restore-keys
- **e2e**: Cargo-chef + dedup gen-crds + cosign-installer + buildkit mirror

### Testing

- **packages,upgrade**: Extract testable parsers + cache fix from previous commit
- **daemon,packages/cargo**: Extract testable helpers + 13 new tests
- **packages/go**: Extract scan_go_bin_dir + go_install_path with 7 tests
- **cli/source/add**: Cover resolve_non_interactive_profile + parse_priority_input
- **cli/module,cli/init**: Extract URL builder + apply-mode predicates
- **cli/source/add**: Extract display_source_manifest with 8 output tests
- **packages/scripted**: Extract build_template_invocations + 7 escape tests
- **cli/init/enroll**: Extract first_existing_ssh_key + 8 priority tests
- **cli/source/add**: Extract conflict-preview helpers + 11 tests
- **upgrade**: Extract verify_archive_checksum + 8 tests
- **system/windows_service**: Extract sc_create_args/sc_config_args + 14 tests
- **cli**: Extract find_subcommand_index from expand_aliases + 12 tests
- **operator/controllers**: MockKubeHarness + reconcile_drift_alert tests
- **operator/controllers**: Reconcile_machine_config full branch coverage
- **operator/controllers**: Config_policy + cluster_config_policy reconciles
- **operator/controllers**: Reconcile_module + verification branches
- **operator/webhook**: Router::oneshot tests for validate handlers
- **operator**: Mutate-pods + gateway api Router::oneshot tests
- **operator/gateway/api**: Enroll endpoint router tests
- **operator/gateway**: Kube-mock drift.rs tests + harness pub(crate)
- **operator/webhook**: Kube-mock router tests for module + pod paths
- **oci/sign**: Fake-cosign shim covers all 4 cosign-shelling paths
- **upgrade**: Extract run_cosign_verify_blob + fake-cosign tests
- **secrets/age,sops**: Generic ToolShim + per-tool env-var seams
- **secrets/providers**: Tool seam + ToolShim across vault/op/lpass/bw
- **packages/brew**: Tool seam + ToolShim across all 3 brew managers
- **packages/{npm,pipx,go,cargo}**: Tool seam + ToolShim across 4 managers
- **upgrade**: Mockito + fake-cosign tests for download_and_install full path
- **operator/webhook**: Policy LIST 5xx error branches via shared kube-mock
- **upgrade,cli/module**: Download_to_file printer branches + keys.rs cosign seam
- **packages/nix**: Tool seam + ToolShim across nix profile + nix-env paths
- **packages/flatpak**: Tool seam + ToolShim across all 5 flatpak shell-out sites
- **packages/snap**: Tool seam + ToolShim covering install/uninstall/update/list/info
- **files,system/environment**: Tempdir tests for apply + macOS env paths
- **system/gpg_keys**: CFGD_GPG_BIN tool seam + ToolShim coverage
- **packages/shared**: Cover untested seam + bootstrap helpers
- **cfgd-operator/leader**: Kube-mock coverage for try_acquire branches
- **cli/module**: Build_module_crd_json pure-helper extraction + CRD contract tests
- **cli**: Expand_aliases happy-path + no-subcommand exit-0 contract
- **modules/git**: Git2-fixture coverage for get_head_commit_sha + check_tag_signature
- **system/windows_service**: Cover diff "absent" drift branch
- **modules/git**: Local-bare-repo fixture for clone/fetch/checkout
- **cli/source/helpers**: Cover count_policy_items + display_policy_items + display_pending_decisions branches
- **daemon**: Happy-path fixture drives handle_reconcile end-to-end
- **daemon**: Handle_sync with real git2 bare-and-clone fixture + 4 tests
- **sources**: Drive SourceManager end-to-end via local bare repo + 8 tests
- **daemon**: Drop flaky loop_processes_file_change_event
- **cli/init/enroll**: Drive cmd_enroll via mockito + 4 tests
- **cli/source/add**: Drive cmd_source_add via local bare repo + 3 tests
- **daemon/reconcile**: Drift arms for autoApply, onDrift scripts, notify_on_drift + 4 tests
- **cli/init**: Drive cmd_init --from local bare repo + apply orchestration arms + 6 tests
- **daemon**: Drive build_pre_loop_setup + compliance + version-check arms
- **daemon**: Drop timing-flaky compliance snapshot dedupe test
- **daemon**: Add DaemonRunOverrides + run_daemon_with end-to-end harness + 5 tests
- **cli/daemon**: Extract render_daemon_status + 4 tests for the display branches
- Cover setup_file_watcher + cli/source/add edge flags (5 new tests)
- **daemon+cli/init**: Drive 6 more end-to-end paths through existing harnesses
- **daemon**: Cover handle_health_connection via in-memory duplex (5 tests)
- **cli/source/add**: Cover platform-profiles auto-detect + empty-profiles bail
- **modules/registry**: Drive fetch_registry_modules + latest_module_version end-to-end (4 tests)
- **secrets/age**: Cover edit_file early-return + editor-failure paths
- **csi/node**: Cover registry_of + check_registry_allowed + parse_allowed_registries_from_env
- **cli/init**: Cover --apply-module module-only branch via bare-repo fixture
- **cli/module/registry**: Cover cmd_module_add_from_registry end-to-end against a local file:// registry
- **cli/init**: Cover --apply-profile + --apply-module combined arm + unknown-module bail in profile branch
- **cli/profile/update**: Cover module-removal lockfile + cache + state cleanup branches
- **cli/source/update**: Cover cmd_source_update happy + named + error-status branches end-to-end
- **cli/source/edit**: Cover happy-path with EDITOR=/bin/true → "Source manifest is valid"
- **cli/source/remove**: Cover --keep-all branch transferring resources to local management
- **cli/source/update**: Tighten named-filter assertion — must also exclude the un-named source
- **cli/module/registry**: Cover cmd_module_search end-to-end against local registry
- **cli/helpers**: Cover compose_with_sources end-to-end against local-bare source
- **cli/doctor**: Cover every-manager declared arm + config-sources section
- **cli/module/registry**: Cover cmd_module_upgrade no-new_ref + same-commit no-op
- **ai/client+cli/generate**: Inject CFGD_ANTHROPIC_URL seam + 5 mockito tests
- **compliance**: Cover malformed perm-string + file-encryption declaration arms
- **compliance**: Cover package-manager installed_packages Err arms via StubPackageManager
- **cli/init/enroll**: Cover key-based enroll_info failures via mockito
- **cli/init/cmd_init**: Cover apply_plan prompt-declined branch (Skipped + early return)
- **cli/source/update**: Cover permission-change prompt-cancelled skip arm
- **cli/source/show**: Cover cached-manifest display + Policy Summary section
- **cli/module/list_show**: Cover wide-format table + platform-filtered package arm
- **cli/plan_ops**: Cover ResolveEnv, module-cache symlink, unmanaged-file prompts, pending decisions, Update-diff render
- **ai/tools**: Pin Err arms for read/list/adopt files + validate/write yaml dispatchers
- **cfgd-csi/cache**: Cover get_or_pull cache-hit early-return + list_entries non-dir skips
- **sources**: Pin git-log-failure arm of verify_head_signature on non-git directory
- **webhook**: Pin try_into-Err arm of all 6 admission handlers
- **daemon/reconcile**: Cover plural-pending, pending_resource_paths, no-profile/missing-profile arms
- **daemon/sync**: Cover handle_compliance_snapshot resolve-Err and same-hash short-circuit
- **cli/generate**: Cover tool_use loop, present_yaml Accept arm, consent-decline abort
- **daemon/sync**: Drop flaky same-hash short-circuit assertion
- **upgrade**: Cover download_and_install_to spinner branches with printer
- **upgrade**: Cover verify_cosign_bundle warning paths for missing bundle/pubkey/cosign-cli
- **upgrade**: Cover extract_tarball symlink-skip + check_with_cache cached-version-parse-error
- **cli/module**: Pin cmd_module_update duplicate-basename bail
- **cli/module**: Cover cmd_module_delete dir-target purge + dir-source restore arms
- **cli/init**: Drive pick_profile multi-profile branch via JSON-format printer
- **modules/git**: Pin get_head_commit_sha empty-repo HEAD-read error arm
- **test_helpers**: Add shared EnvVarGuard + with_test_env_var
- **operator**: Add shared mc_list/machine_config_path/test_db fixtures
- **test_helpers**: Add consolidated CosignTestShim builder
- Migrate 5 with_env consumers to shared with_test_env_var
- Migrate 3 local EnvVarGuard structs to shared helper
- **operator**: Migrate mc_list/machine_config_path consumers to shared fixtures
- **operator**: Migrate test_db consumers to shared gateway helper
- Migrate 3 CosignShim variants to consolidated CosignTestShim
- **reconciler**: Remove local Mock duplicates of test_helpers types
- Replace 9 local printer() wrappers with test_helpers::test_printer
- Inline 6 local shim() wrappers, expose env-var at call site
- **oci**: Pin 8 previously-untested OciError variants
- **composition+upgrade**: Pin 11 previously-untested error variants
- **cli/compliance**: Add inline unit tests for cmd_compliance helpers
- **cli/config_cmd**: Add unit tests for config subcommand handlers
- **cli/status**: Add unit tests for cmd_status renderers and exit codes
- **gen-crds**: Cover inject_smd_annotations + inject_cel_rules
- **files/apply**: Cover symlink/hardlink strategies + readonly-parent + dir-as-file
- **upgrade**: CFGD_GITHUB_API_BASE shim unlocks check_latest + check_with_cache
- **gateway**: Cover build_cors_layer branches via preflight oneshot
- **daemon/reconcile**: Cover module-resolution block (lines 264-291)
- **cli**: Adopt prompt-mock harness for delete/rollback prompts
- **cli**: Cover kubectl shell-out, ApplyPhase maps, module create interactive
- **cli**: Cover editor-validate-loop save-with-errors paths
- **cli/module**: Cover detect_git_remote/head helpers
- **profile/backups**: Cover prompt_restore_backups arms
- **daemon/reconcile**: Cover auto_apply + sources branch
- **cli**: Cover apply-prompt-confirmed branches in init and module create
- **profile/create**: Cover interactive mode via prompt-mock harness
- **cli/explain**: Cover json/structured output paths
- **system/node**: Cover KubeletConfigurator::apply paths
- **system/node**: Cover ContainerdConfigurator::apply paths
- **system/node**: Cover apparmor + kernel_modules apply early-returns
- **system/node**: Strengthen no-op tests, drop tautological one
- **system/node**: Cover kubelet apply rollback-restore arm
- **system/node**: Cover containerd apply rollback-restore arm
- **coverage**: SystemConfigurator::apply + verify_enrollment success paths
- **coverage**: Cmd_init install_daemon + apply_plan prompt-declined arm
- **coverage**: Oci::push top-level + multiplatform paths via mockito
- **coverage**: Cmd_source_create interactive prompt branches
- **coverage**: Cli::helpers managers_map / module_state_map / default_device_id
- **coverage**: Ai::Conversation getters + cfgd-csi::env_or helper
- **coverage**: Cfgd::main exit_code_for_anyhow + cfgd-operator::main log_crd_info
- **daemon**: Bump sighup-reload sleep from 80ms to 200ms for llvm-cov
- **output_v2**: Serialize Printer color-state tests (R1.T14)
- **output_v2**: Cover data_line raw stdout contract + modernize format! (R1.T20)
- **output_v2**: Bucket (a) per-component baseline goldens, 9 cases (R1.T28)
- **output_v2**: Bucket (b) per-component verbosity sweep, 27 cases (R1.T29)
- **output_v2**: Buckets (c)(d)(e) — role/theme/indent goldens, 18 cases (R1.T30)
- **output_v2**: Bucket (f) layout corner cases, 12 goldens (R1.T31)
- **output_v2**: Bucket (g) regression anchors, 30 goldens (R1.T32)
- **output_v2-F0**: Round-trip output_types via emit() (R2.F0.T4)
- **output_v2-F4b**: Daemon snapshot floor (clean cycle + drift event) (R2.F4b.T2)
- **output_v2-F4b**: Live-daemon lifecycle e2e smoke (spawn + SIGHUP + SIGTERM + restart) (R2.F4b.T4)
- **output_v2-F4c**: §17.2 reconcile-cycle bridge snapshot floor (R2.F4c.T5)
- **output_v2-F4c**: Drain T5 review — bridge minimal, cycle goldens match spec, comment-policy fix (R2.F4c.T5 drain)
- **output_v2-F4c**: Drain T7 review — test-helper dedup, systemd format dedup, launchd flag-order parity (R2.F4c.T7 drain)
- **output_v2-F4d**: Normalize tempdir paths + spinner durations + git SHAs in source_{add,update,replace} snapshots (R2.F4d.T2 drain 2)
- **output_v2-F4e**: Add bridge snapshot tests for ssh_keys + systemd_unit (R2.F4e.T2)
- **output_v2-F4e**: Add bridge snapshot tests for seccomp + certificates (R2.F4e.T3)
- **output_v2-F4f**: Add bridge snapshot tests for brew + OCI helpers (R2.F4f.T4)
- **output**: Bucket-g regression goldens for accent surfaces
- **output**: Regenerate sync/happy snapshot for per-source secondary
- **output**: Regression test for tab/control chars in table cells
- **output**: Exercise Role::Accent and Role::Secondary in bucket_d_themes
- **cli/helpers**: Add unit tests, lift coverage 67% → 90.93%
- **cli/helpers**: Address review — git_cmd_local, drop dead var, strengthen source-compose assertion
- **cli/checkin**: Add unit tests, lift coverage 50% → 92%
- **secrets/age**: Add unit tests, lift coverage 65% → ≥80%
- **secrets/age**: Polish — strip test narration comments, fix import placement
- **cli/plan_ops**: Add unit tests, lift coverage 77% → ≥90%
- **cli/plan_ops**: Dedup filter_plan tests, strengthen duration assertion
- **cli**: Finish plan_ops dedup — remove duplicate pattern_matches tests
- **cli/module/keys**: Add unit tests, lift coverage 67% → ≥80%
- **cli/module/keys**: Remove section dividers; chore(cli/plan_ops): fix expect_fun_call clippy
- **cli/module/keys**: Polish — drop unneeded #[serial], dedup assertions, deterministic missing-cosign
- **files/plan**: Add unit tests, lift coverage 78% → ≥90%
- **files/plan**: Remove section dividers
- **cli/upgrade**: Add cmd_upgrade mockito tests, lift coverage 33% → ≥80%
- **cli/upgrade**: Remove section dividers
- **system/environment**: Cover macos+windows writers, lift coverage 61% → 79%
- **system/environment**: Remove divider, strengthen apply-test assertions
- **cli/module/push_pull**: Add unit tests, lift coverage 24% → 84%
- **cli/module/push_pull**: Polish — CwdGuard, RFC 5737 unreachable ref, move misplaced tests
- **cli**: Add dispatcher + helper tests, lift cli/mod.rs coverage 51% → 89%
- **cli/daemon**: Add unit tests, lift coverage 59% → 87%
- **files/apply**: Cover uncovered branches, lift coverage 69% → 85%
- **files/apply**: Remove section divider block
- **cli/doctor**: Cover remaining section-builder branches, lift 79% → ≥93%
- **cli/doctor**: Remove section divider block
- **cli/module/build**: Add unit tests; fix(cli/module/build): collapse multi-line errors before emit
- **packages/brew**: Cover install/uninstall/update/available_version branches, lift 70% → 88.57%
- **packages/shared**: Cover bootstrap/caveats/strip helpers, lift 72% → 88%
- **cli/profile/update**: Cover env/alias/file/module/hooks/error branches, lift 83% → 87%
- **cli/generate**: Add scan/dispatch tests, lift coverage 67% → 89%
- **cli/generate**: Switch bare is_ok() asserts to .expect() with diagnostics
- **cli/init/enroll**: Cover cmd_enroll branches via mockito, lift 73% → ≥88%
- **system/shell**: Cover diff/apply/current_state branches, lift 67% → 84%
- **cli/plugin**: Cover kube-connect-failed + cmd_version paths, lift 35→40 tests
- **generate/scan**: Cover dotfile/shell-config scan branches, lift 83% → 85%
- **e2e/cli**: Cover AL01 'cfgd add' and AL02 'cfgd remove' aliases
- **cfgd-csi**: Cover CsiError variants and From impls
- **gateway**: Cover GatewayError Display + IntoResponse per variant
- **config**: Cover sync/notify/secrets config blocks
- **cli/output_types**: Cover -o json|yaml wire shapes
- **output/structured**: Cover -o json|yaml|name|jsonpath|template router
- **e2e/cli**: Cover alias subcommand tree AL10-AL17
- **coverage**: Add focused unit tests for theme, process, and simple managers
- **coverage**: Expand workspace coverage across system, packages, and generate modules
- **coverage**: Cover webhook admission paths and sources composition
- **cleanup**: Replace tautological matches! assertions with field-checking equivalents
- **coverage**: Cover git module URL parsing, doc builder methods, and go package operations
- **coverage**: Cover SectionGuard methods, file action cloning, and format branches
- **coverage**: Cover render_doc component paths and status_builder methods
- **coverage**: Cover ThemeConfig deserialization and root config helpers
- **coverage**: Cover PhaseName parsing and ScriptPhase display names
- **secrets**: Comprehensive 1Password provider coverage via mock op binary
- **upgrade**: Cover remaining branch paths via mockito
- **coverage**: Cover scan, environment, plan_ops, and daemon CLI paths
- **coverage**: Add unit tests for encryption, process, file_io, reconcile, daemon config
- **coverage**: Expand daemon/mod.rs coverage for triggers, signal pumps, and startup checkin
- **daemon**: Assert mockito expectation for clearer failure diff
- **coverage**: Cover webhook server entrypoint, dispatchers, and liveness
- **coverage**: Expand plugin, upgrade, module/build, and csi/node coverage
- **coverage**: Batch-cover daemon, reconciler, sources, controllers, modules, gateway
- **coverage**: Deep-cover sources, csi/node, leader, daemon CLI, build, mcp
- **coverage**: Final-push — bare-repo end-to-end + secret-env + keys edge branches
- **coverage**: Controllers::run + gateway::start_gateway timeout-wrap
- **coverage**: Broaden providers + gateway db unit tests
- **coverage**: Cover metrics_handler + run_metrics_server setup
- **coverage**: Exercise windowsRegistry diff+apply iteration arms
- **coverage**: Cover prompt_select/text/confirm seeded + structured-refuse arms
- **coverage**: Cover server_client connection-refused + 500 error arms
- **coverage**: Drive reconciler on_change err + encryption-check err + plan_system drift + profile-vs-profile conflict branches
- **coverage**: Cover apply git_sources_json branch + diff Skip-arm + build_diff_doc role selection
- **coverage**: Plan_modules manager-priority sort + pipx available_version via curl shim
- **coverage**: Cover ensure_owner_private_dir happy + idempotent + create-failed paths
- **coverage**: Cover read_command_output success/err paths + registry remove NotFound-with-registries branch
- **coverage**: Cover sources fetch-failed spinner err arm when remote disappears
- **coverage**: Cover scoop + choco bootstrap powershell pipeline via named shim
- **coverage**: Run_daemon wrapper hits early-Err path on invalid config
- **coverage**: Exercise verify_head_signature no-git-on-PATH short-circuit arm
- **coverage**: Exercise LeaderElection::run win + retry+win paths via MockKubeHarness
- **daemon/launchd**: Add 3 unit tests for generate_launchd_plist
- **daemon/systemd**: Add 2 unit tests for generate_systemd_unit
- **daemon/service**: Cover install/uninstall via HOME-overridden tempdir
- **cfgd-operator/app**: Drive run() webhook-present branch via tls.crt fixture
- **cfgd-operator/app**: Drive run() leader-election branch
- **windows**: Route test path substitution through to_posix_string/normalize_for_snapshot
- **windows**: Fix daemon shutdown timeouts + libgit2 OS-error fold
- **snapshots**: Revert version strings 0.4.0 → 0.3.5
- **upgrade**: Use 9.9.x sentinel range for bridge fixture versions
- **snapshots**: Make version-bearing goldens version-agnostic

### Build

- **taskfile**: Wire e2e:* and coverage tasks; route GHA jobs through them

### Config

- Migrate to homebrew_casks (GR v2.16 alignment)
- Drop publish.homebrew block (homebrew_casks is the single path)

### Hardening

- **operator+csi**: Validate user keys, defense-in-depth path checks, surface silent errors

### Observability

- **ai**: Surface tool serialization errors and capture anthropic api error body

### Revert

- Drop pre-bumped 0.4.0 versions; let anodizer tag bump fresh
- **release**: Drop bumped versions; tags were deleted, let anodizer re-bump

## [0.3.5] - 2026-04-20

### Added

- **cfgd-operator**: Pool wait / in-use metrics for gateway DB

### Changed

- **cfgd-operator**: Gateway DB split-pool + async ServerDb (B1 W-6)
- Cosign shell-out through cfgd_core::cosign_cmd + dedup cleanup + anodize bump
- Rename anodize to anodizer (crates.io name clash) #none

### Fixed

- Krew description + bump ANODIZE_REV + stop test from polluting $HOME
- Bump ANODIZE_REV + tighten autotag bump signal
- Set tag.branch_history to full for monorepo autotag
- Bump ANODIZE_REV to honor Cargo.toml-ahead on autotag bump=None
- Bump ANODIZE_REV for publisher-branch orphan replace + force-lease
- Bump ANODIZE_REV for explicit fetch refspec on publisher branch
- Bump ANODIZE_REV to 86faad2 + drop brittle #none workflow gate
- Bump ANODIZE_REV to 5165fc6 — query upstream default branch for PR base
- Bump ANODIZE_REV to 128e003 — krew upstream default + crate-prefix previous_tag
- **cfgd-operator**: Route main.rs through lib crate; drop 9 #[allow(dead_code)]
- Drain cfgd v0.4 known-bugs (UX + dedup + safety) + bump anodize
- **ci**: Track exit/http/retry modules + kubectl dispatch (CI build fail)
- **taskfile**: Commit stages with git add -A so new files land
- **windows**: Bump PE stack via linker arg in build.rs (no runtime cost)
- **release**: Align v0.3.5 for all four workspaces  #patch
- **release**: Bump ANODIZER_REV to 0f552a2, re-enable cfgd snap
- **release**: Krew manifest metadata + CLA-matching commit author
- **release**: Bump ANODIZER_REV to 0211d15 for overwrite-or-skip + commit_author
- **winget**: Declare VCRedist 2015+ runtime dependency

### Miscellaneous

- Bump cfgd to v0.3.5 to exercise chocolatey graceful skip
- Bump ANODIZE_REV to 4532d8e for v0.3.5 re-release
- Bump crates/cfgd-core to 0.3.6 [skip ci]
- Bump crates/cfgd-operator to 0.3.6 [skip ci]
- Bump crates/cfgd-csi to 0.3.6 [skip ci]
- Revert autotag 0.3.6 bumps, keep lib crates at 0.3.5 #none
- Probe push-trigger behavior #none
- Re-trigger autotag after transient GH push-event drop
- Retrigger autotag (prior commit body had a quoted skip-marker)
- Rename homebrew.folder → directory for anodize parity #none
- Gitignore .claude/audits/ + fix clippy drift from Rust 1.95 #none
- **cfgd-operator**: Add r2d2, r2d2_sqlite, parking_lot deps
- **ci**: Bump ANODIZE_REV to e3d3e36 #none
- Bump crates/cfgd-operator to 0.4.0
- **config**: Cloudsmith org=jarvispro + explicit commit_author on all publishers

### Testing

- Thread-local HOME override so tests never touch real $HOME
- **cfgd-operator**: Gateway DB pool regression suite (B1 W-6)

## [0.3.4] - 2026-04-14

### Added

- Bump all crates to v0.3.4, revert webhook URL to jarvispro.io
- V0.3.5 — ANODIZE_REV 248c904 (native nupkg, tag-checkout hook skip)
- V0.3.6 — ANODIZE_REV f7d483d (docker_signs separate stage, winget UploadableBinary fix)

### Fixed

- ANODIZE_REV 85108ea (version_sync Cargo.lock fix), fix stale Cargo.lock
- Add serial to router_wires_routes test (CFGD_API_KEY race), ANODIZE_REV 85108ea, fix Cargo.lock
- Webhook URL to tj.jarvispro.io (Cloudflare redirect), ANODIZE_REV f3c1841 (idempotent commit, Cargo.lock)
- ANODIZE_REV 8ae51f9 (chocolatey multipart/form-data + NuGet UA)
- ANODIZE_REV 9cdfefd (chocolatey idempotent skip)

### Miscellaneous

- Reset to v0.3.4 baseline; let auto-tag bump to v0.3.5

## [0.3.3] - 2026-04-13

### CI/CD

- Bump ANODIZE_REV to 4a8f094 — v0.3.2 release fixes
- Trigger workflow run

### Fixed

- **ci**: Workflow improvements — tag status, cache keys, coverage script, OLM guards
- **ci**: OLM version string + package cache key
- **ci**: V0.3.3 release prep — makeself, verbose, publisher tokens
- Bump all crate versions to 0.3.3, remove coverage test dependency
- V0.3.4 release prep — version bump, ANODIZE_REV, --strict mode
- Sync dep versions to 0.3.3, update ANODIZE_REV to ce3e396
- Dockerfile docker_v2 paths, skip announce for cfgd-core, ANODIZE_REV
- ANODIZE_REV ae4916e, --debug in release for diagnostics
- ANODIZE_REV fbb83bb — split/merge env poisoning fix
- ANODIZE_REV 7f681f5 — docker_manifests skip for docker_v2 multi-arch
- Add release config to operator/CSI workspaces for proper GitHub releases + announce

### Miscellaneous

- Bump crates/cfgd-core to 0.3.3
- Bump crates/cfgd to 0.3.3
- Bump crates/cfgd-operator to 0.3.3
- Bump crates/cfgd-csi to 0.3.3

## [csi-v0.3.2] - 2026-04-12

### Miscellaneous

- Bump crates/cfgd-csi to 0.3.2

## [operator-v0.3.2] - 2026-04-12

### Miscellaneous

- Bump crates/cfgd-operator to 0.3.2

## [0.3.2] - 2026-04-12

### Miscellaneous

- Bump crates/cfgd to 0.3.2

## [core-v0.3.2] - 2026-04-12

### Added

- Dogfood anodize for releases — taskfile, action, .anodize.yaml refresh
- **anodize**: Exercise every applicable anodize stage for cfgd
- **ci**: Path-scoped per-workspace tagging via anodize
- **ci**: DRY release workflow with anodize-action features

### CI/CD

- Add package dry-run job, gate tag on it
- Install anodize via cargo install --git (temporary)
- Build anodize once in snapshot job, download everywhere else
- Bump ANODIZE_REV to aaceb419 — workspace crate resolution fix
- Bump ANODIZE_REV to 5190554 — workspace overlay inference
- Lift ANODIZE_REV to workflow env, bump to 68c9ef7
- Build anodize once per platform in test matrix, reuse everywhere
- Bump ANODIZE_REV to 9e40eb1 — same-OS cross-arch uses cargo
- **release**: Install snapcraft + rpmbuild for merge stage
- Bump ANODIZE_REV to bba2d6b — CI colors + publish transitive deps
- Bump ANODIZE_REV to de99152 — publish searches all workspaces
- Bump ANODIZE_REV to 758f633, increase index_timeout to 600s
- Bump ANODIZE_REV to 5e716bf — per-crate version in publish check

### Changed

- Consolidate duplicated test helpers into shared test_helpers module
- Extract linux/any_system_manager_available helpers, fix go bootstrap test

### Documentation

- Add coverage badge to README

### Fixed

- Deep audit — security, safety, observability, dedup, and cleanup across all crates
- Restore workspace scoping
- Temporarily disable audit as prerequisite of tag
- Resolve CI lint failures and E2E regressions
- Check file existence before tool availability in secret/cert tests
- Add Strawberry Perl to PATH for Windows OpenSSL build
- Vendor openssl only on non-Windows to unblock Windows CI build
- Don't unset openssl dir on linux/macos
- Resolve cross-platform test failures and audit violation
- Unused-mut in bootstrap test, fmt+re-stage in task commit
- /var tempdir false-positive on macOS, flatpak Linux-only bootstrap
- Go can_bootstrap checks Windows package managers
- Remove duplicate operator rollout restart from E2E test setup
- Wait for webhook service endpoints in E2E setup
- Gate linux_system_manager_available on target_os=linux
- Windows test failures, add audit to lint task
- Update git clone test assertion to match new content
- **ci**: Remove misused from-artifact: cfgd-linux from anodize-action
- Set branch_history=last so autotag ignores stale no-bump commits
- **release**: Install anodize inline, fetch tags on checkout
- **release**: Pass --crate to anodize release, gate publishers by crate
- **ci**: Per-tag concurrency group for independent workspace releases
- Update cfgd-core dependency version to 0.3.2
- **ci**: Resolve workspace via anodize-action + push version_sync commits

### Miscellaneous

- Bump crates/cfgd-core to 0.3.2

### Testing

- Add test coverage across CLI, operator, core, and system modules #none
- Strengthen core library tests — modules, output, reconciler #none
- Add state store tests — module CRUD, backups, journal, source apply #none
- Refactor hardcoded paths for testability, add behavioral tests #none
- Extract pure functions and add behavioral tests for coverage push
- Push coverage to 85% with behavioral tests across all major modules
- Consolidate tautological and redundant tests into table-driven patterns
- Replace weak assertions with value-verifying tests across all crates
- Increase meaningful test coverage from 84.3% to 86.3%

## [0.3.1] - 2026-04-04

### Fixed

- CI coverage tool (tarpaulin→llvm-cov), migration race, Crossplane RBAC #none
- Don't run interactive prompt on windows #none
- Windows test uses HOME env var which doesn't exist on Windows #none
- Isolate tests from real filesystem (plan_env, expand_tilde) #none
- E2E operator test flake — wait for webhook endpoints after pod restart #patch

## [0.3.0] - 2026-03-31

### Added

- Add test-helpers feature with reusable mocks and TestEnvBuilder

### Documentation

- Add file size audit and decomposition plan (#patch)

### Fixed

- Safety audit — default-deny gateway auth, shell injection prevention, SQLite contention, YAML bomb defense
- Second-pass safety audit — shell value escaping, process group kills, timeouts, error context
- Complete safety audit backlog — secrecy, SSH policy, SIGHUP, idle timeout, YAML bomb defense
- Deep hardening — security, correctness, and simplification (#patch)
- Logging system overhaul, CSI NodeGetVolumeStats, no-op audit (#patch)
- E2E test infrastructure overhaul — 544 tests, 0 failures, 0 skips (#patch)
- Ensure crossplane and cosign are installed #none
- Address review findings in file tests — remove duplicate, tighten assertion
- Reject empty age recipient, address review findings in secrets tests
- Address review findings — serial tests, pin hash, tighten assertions
- E2E RBAC for replicasets/PVCs, increase drift poll timeout
- Test audit — remove tautologies, strengthen assertions, fix E2E patterns
- CLI output overhaul — themed text, spinners, progress display
- Rollback content restoration, SEC05 no-change edit, Windows/Rust 2024 compat #minor
- Cargo fmt, CSI unmount EPERM handling #none
- FS-HELM-05 MachineConfig missing required hostname/profile fields #minor

### Testing

- Add server client credential parsing and construction edge cases
- Add source signature, path accessor, and removal tests
- Add upgrade cache and platform mismatch tests
- Add controller pure function tests — compliance, selectors, conditions
- Add webhook cert loading and annotation parsing edge case tests
- Add reconciler tests for rollback, partial apply, continueOnError, onChange
- Add package manager output parsing and alias tests
- Add system configurator diff edge cases and platform availability tests
- Add file strategy, template detection, and rendering tests
- Add age key parsing and secret reference resolution edge case tests
- Add CSI unpublish and cache eviction edge case tests
- Add composition merge edge cases and strengthen determinism
- Add daemon config detection and timer logic tests
- Add gateway SSH signature verification tests
- Add 575+ tests across all crates — mockito HTTP, axum handlers, pure logic
- Add compliance package/system/export tests with inline mock
- Wave 2 — direct CLI handler calls, reconciler apply paths, node configurators
- Wave 3 — CLI add/remove/apply/source/module/workflow direct handler tests

## [0.2.9] - 2026-03-28

### CI/CD

- Add gateway-tests job to E2E workflow

### Changed

- Consolidate stderr/stdout helpers to cfgd_core shared functions #none
- **e2e**: Update CI, Dockerfile, Taskfile for reorganized tests
- Extract git_cmd_safe helper, eliminate SSH hang duplication

### Documentation

- Update docs for E2E test reorganization
- E2E test coverage expansion design spec
- Fix E2E spec issues found by code review
- E2E coverage expansion implementation plan
- Add codebase-wide robustness audit to PLAN.md

### Fixed

- **e2e**: Add set -euo pipefail to operator setup-operator-env.sh
- **e2e**: Wire crossplane tests into CI and Taskfile
- **e2e**: OF08 module list accepts empty JSON array
- Module keys generate --output clap panic, install cosign in CI #patch
- **e2e**: Address MCP server test review feedback
- **e2e**: Address gateway setup review feedback
- **e2e**: Strengthen CO08/CO09 compliance assertions
- **e2e**: SEC09 use apply --yes to trigger actual decrypt failure
- Prevent SSH clone hang in non-interactive contexts
- Critical bugs — path traversal, fish config, double clone, test safety
- E2E test safety audit — eliminate all filesystem and cluster hazards
- Add e2e:gateway Taskfile target, fix RB07 symlink strategy casing
- Init/apply audit — eliminate duplicate work, fix race, cleanup
- Codebase-wide robustness audit — SSH safety, timeouts, silent failures, platform safety, test isolation
- File operation safety audit — symlink skipping, path traversal, secret permissions, dedup

### Testing

- **e2e**: Extract shared CLI test setup to setup-cli-env.sh
- **e2e**: Extract shared node test setup to setup-node-env.sh
- **e2e**: Extract shared operator test setup to setup-operator-env.sh
- **e2e**: Extract shared full-stack test setup to setup-fullstack-env.sh
- **e2e**: Extract node tests into per-provider domain files
- **e2e**: Extract full-stack tests into per-domain files
- **e2e**: Extract operator tests into per-CRD domain files
- **e2e**: Extract CLI tests into 29 per-command domain files
- **e2e**: Add CLI run-all.sh runner
- **e2e**: Add node run-all.sh runner, rename helm/server scripts
- **e2e**: Add generate tests (GEN01 through GEN06)
- **e2e**: Add MCP server tests (MCP01 through MCP06)
- **e2e**: Add gateway suite infrastructure
- **e2e**: Expand webhook tests (OP-WH-04 through OP-WH-15)
- **e2e**: Add multi-namespace policy tests (OP-NS-01 through OP-NS-06)
- **e2e**: Add CLI error path tests (ERR07 through ERR13)
- **e2e**: Add rollback depth tests (RB05 through RB10)
- **e2e**: Add source merge tests (SRC-MERGE-01 through SRC-MERGE-08)
- **e2e**: Add compliance depth tests (CO08 through CO14)
- **e2e**: Add operator error path tests (OP-ERR-01 through OP-ERR-04)
- **e2e**: Add gateway health and enrollment tests (GW-01 through GW-06)
- **e2e**: Add Crossplane depth tests (XP-06 through XP-14)
- **e2e**: Add gateway checkin tests (GW-07 through GW-10, GW-18)
- **e2e**: Add gateway API tests (GW-11 through GW-14, GW-19, GW-20)
- **e2e**: Add node error path tests (BIN-ERR-01 through BIN-ERR-04)
- **e2e**: Add secret backend detection tests (SEC06 through SEC10)
- **e2e**: Add operator lifecycle tests (OP-LC-01 through OP-LC-08)
- **e2e**: Add gateway admin, streaming, dashboard tests
- **e2e**: Complete gateway admin and streaming tests (GW-15 through GW-30)
- **e2e**: Expand CSI tests (FS-CSI-03 through FS-CSI-10)
- **e2e**: Add Helm chart lifecycle tests (FS-HELM-01 through FS-HELM-08)
- **e2e**: Add OCI supply chain E2E tests (OCI-E2E-01 through OCI-E2E-06)
- **e2e**: Add daemon reconciliation loop tests (DAEMON-10 through DAEMON-18)
- **e2e**: Add coverage gap tests for init, apply, source flows

## [0.2.8] - 2026-03-26

### Fixed

- Source add --yes skips prompts, EC test fixtures, source clone CLI-first #patch

## [0.2.7] - 2026-03-26

### Fixed

- Import ordering, stale README badge, T08 timeout, plan cleanup #none
- Git CLI-first clone, apt-get over apt, portable credential handling #patch

## [0.2.6] - 2026-03-26

### Added

- E2E compliance tests + compliance checkin feature + plan cleanup

### Fixed

- Deep audit — controller bugs, sudo-as-root, init --from apply, E2E gaps #patch

## [0.2.5] - 2026-03-26

### Fixed

- Full audit — DRY, module boundaries, atomic writes, test mocks #patch

## [0.2.4] - 2026-03-26

### Fixed

- Unify --from to accept URL or local path, fix Windows CI #patch
- Allow binaries/ in .dockerignore for release Docker builds #patch
- Local git repo --from, merge E2E CLI into E2E workflow #patch

## [0.2.3] - 2026-03-25

### Added

- Add encryption enforcement types to config structs
- Validate file encryption requirements in file manager
- Enforce encryption constraints in source composition
- Add envs field to SecretSpec, make target optional
- Inject secret-backed env vars into shell env file
- Add system field to ModuleSpec for configurator support
- Add GpgKeysConfigurator system configurator
- Add SshKeysConfigurator and GitConfigurator system configurators
- Add ComplianceConfig to ConfigSpec, extend parse_duration_str with days
- Add compliance snapshot collection and storage
- Add cfgd compliance CLI commands
- Integrate compliance snapshots into daemon reconciliation loop

### Documentation

- Add compliance-as-code implementation plan
- Update specs and guides for compliance-as-code features
- Add persona-based feature highlights to README

### Fixed

- Cargo fmt formatting in CLI test assertion #none
- Eliminate QEMU Rust compilation from Release Docker builds, expand E2E CLI coverage #none
- Address review findings from compliance Tasks 1-4
- Warn when SOPS secrets have envs field, document verify_env limitation
- Address review findings from Tasks 7-9 configurators
- Cargo fmt formatting in compliance CLI #none
- Remove lowercase serde rename_all from compliance enums
- Deduplicate compliance code, wire up watchPackageManagers
- Compliance diff status comparison uses consistent PascalCase
- File merge replaces entire entry, not just source
- Enforce per-file permissions, propagate module encryption, populate compliance sources
- Move is_file_encrypted to cfgd-core, validate module file encryption
- Cargo fmt, rename watch-path category to watchPath per spec
- Exact-match archive name in install script checksum verification #none
- Install script checksum, init --from target dir, git clone auth #patch
- Gate ssh_keys tests behind #[cfg(unix)] for Windows CI #patch

### Miscellaneous

- Remove completed compliance-as-code plan
- Move E2E test plan to .claude/, add compliance test cases

## [0.2.2] - 2026-03-24

### Documentation

- Add compliance-as-code design spec

### Fixed

- Give each E2E job its own namespace to prevent cleanup collisions #none
- T32 must apply + re-checkin before asserting healthy status #none
- Full-stack E2E robustness: pod wait, webhook, error handling #none

## [0.2.1] - 2026-03-24

### Fixed

- Gate Unix-only test code with #[cfg(unix)] for Windows CI #none
- Root-cause all CI/E2E failures across 5 workflows #none
- Setup-cluster.sh must not apply RBAC that ArgoCD owns #none
- Gate Unix-only tests, drop redundant Windows CI compilation #patch

### Miscellaneous

- Update README badges for consolidated E2E workflow #none

### Performance

- Consolidate 3 E2E workflows into one shared setup #none

## [0.2.0] - 2026-03-24

### Added

- Wire up implementation gaps, add deduplicating-code and detecting-implementation-gaps skills
- **e2e**: Expand operator and full-stack E2E suites, fix CLI bugs
- **sources**: Wire platform-aware profile auto-selection into source add
- **cli**: Convert daemon from flags to subcommands
- **cli**: Implement items 2,3,5-8 from CLI UX improvements plan
- **cli**: Rich output format enum replacing bare string (-o wide/name/jsonpath=/template=)
- **config**: Unified ScriptSpec with ScriptEntry for all 6 hook types
- **reconciler**: ReconcileContext, PreScripts/PostScripts phases, ActionResult.changed
- **reconciler**: Unified script executor with timeout, continueOnError, onChange
- **cli**: --skip-scripts, fixed hook flags, onDrift in daemon
- **cli**: Add plan command with structured output and context selection
- **cli**: Structured output for profile list, module search, registry list, keys list
- Ecosystem integration — policies for all CRD fields, naming fixes
- CLI UX improvements, script lifecycle, docs cross-references
- Expand_tilde checks USERPROFILE for Windows home directory
- Cross-platform create_symlink abstraction, migrate all callsites
- Cross-platform file permission abstractions, migrate all callsites
- Cross-platform is_same_inode, migrate from files/mod.rs
- Windows acquire_apply_lock via LockFileEx
- Cross-platform terminate_process and is_root
- Cross-platform daemon IPC (Unix sockets / Windows named pipes)
- Cross-platform script execution (cmd.exe /C for Windows inline commands)
- Windows self-upgrade (zip extraction, rename-dance binary replacement)
- Gate Unix-only system configurators and daemon service management
- Info log when file permissions configured on Windows
- Add winget, chocolatey, scoop fields to PackagesSpec
- Winget PackageManager implementation
- Chocolatey PackageManager implementation
- Scoop PackageManager implementation
- Module package alias support for winget, chocolatey, scoop
- PowerShell env file generation and profile injection
- ShellConfigurator Windows Terminal support
- EnvironmentConfigurator Windows registry/setx support
- WindowsRegistryConfigurator for managing registry settings
- WindowsServiceConfigurator for managing Windows Services
- Windows Service daemon lifecycle and Event Log subscriber
- Update JSON schema and explain command with Windows fields
- Add GsettingsConfigurator for GNOME/GTK desktop settings
- Add KdeConfigConfigurator for KDE Plasma settings
- Add XfconfConfigurator for XFCE desktop settings
- Register Linux desktop configurators in provider registry
- Update all schemas, docs, and integration points for Linux desktop configurators
- **e2e**: Add k3s infrastructure manifests for E2E migration
- **e2e**: Rewrite helpers.sh for k3s — kubectl-based pod/namespace/cleanup helpers
- **e2e**: Add setup-cluster.sh pre-flight script for k3s E2E
- **e2e**: Add reusable e2e-setup workflow and Helm test values
- **e2e**: Migrate CLI workflow to arc-cfgd, drop Docker job
- **e2e**: Migrate node test scripts from KIND to k3s privileged pod
- **e2e**: Migrate node workflow to k3s via e2e-setup.yml
- **e2e**: Migrate operator tests to k3s persistent infrastructure
- **e2e**: Migrate full-stack tests to k3s
- **e2e**: Add Taskfile e2e targets, delete KIND files
- **chart**: Add imagePullSecrets, podAnnotations, podLabels, configurable probes
- Move all workflows to arc-cfgd runners, remove runtime installs #minor
- Move all workflows to arc-cfgd runners, remove runtime installs #minor

### CI/CD

- Test self-hosted runner on E2E Full Stack workflow
- Bump E2E timeout to 45min for self-hosted baseline
- Revert E2E to ubuntu-latest while reworking runner infrastructure
- Add Windows build/test job for cfgd-core and cfgd
- Add Windows x86_64 and aarch64 release targets with .zip packaging

### Changed

- DRY consolidation — sha256_hex, atomic_write_str, wrapper cleanup
- **plan**: Split upstream k8s work into dedicated plan file #none
- Cargo fmt across cfgd-core and cfgd

### Documentation

- Update documentation for script lifecycle, plan command, and output formats
- Windows support design spec, update PLAN.md
- Add E2E & workflow architecture design spec #none
- Update CLAUDE.md shared utilities with cleanup_old_binary
- Add default_config_dir to CLAUDE.md shared utilities
- **README.md**: Update with actual 'why it exists' story
- Windows support — packages, configurators, daemon, configuration
- Mark Windows Support Plan 2 complete, update CLAUDE.md
- Add E2E test migration spec (KIND → k3s)
- Add E2E verification remaining work to PLAN.md
- Rework README tagline, comparison table, and profile example
- ToC and list ordering
- Cleanup

### Fixed

- **test**: Eliminate flaky test race by threading state_dir through Cli
- **ci**: Cargo fmt, .helmignore for kustomization.yaml, PackageRef schema in E2E tests
- **e2e**: Fixes from live E2E testing — all 34 tests pass on kind
- **helm**: Nest agent values under agent: key in E2E test values
- **e2e**: Disable webhook/cert-manager in Helm agent tests
- Address code review — add insecure registry test, remove redundant rustls init
- **e2e**: Simplify review fixes, CLI state isolation, remove dead alias tests
- Log rustls init error, add CSI_DRIVER_NAME constant to E2E helpers #none
- Remove DRY violations — sha256_hash wrapper and which() reimplementation
- Remove stale --active, --name references from e2e tests, docs, and config
- Resolve all review findings from code review, dedup, and gap analysis
- Address code review findings from Tasks 1-3
- Address Task 4 code review findings
- Address Task 6 code review findings
- Address Task 7 review suggestions
- Correct connect_daemon_ipc doc comment
- Add platform-specific executable hint, update timeout comment
- Wire cleanup_old_binary into main.rs startup, update comments
- Gate print_daemon_install_success behind #[cfg(unix)]
- Wrap Unix permissions code in #[cfg(not(windows))] to prevent unreachable_code warning
- Use shared file permission abstractions in CertificateConfigurator
- WingetManager exit status checks and UTF-8 safety
- Handle singular "package installed." footer in choco parser
- Use CommandFailed and imported Command in Windows manager available_version
- InjectSourceLine idempotency guard and PowerShell header consistency
- Add set_var to windows_set_var, remove dead parse_reg_path
- Check stdout not stderr for sc.exe error messages
- WindowsServiceConfigurator sc.exe argument format and error handling
- Run actual daemon loop inside Windows Service main
- Daemon graceful shutdown, logging, expect removal, path prefix
- Correct Windows documentation inaccuracies
- Schema startType case, explain type_desc consistency, add displayName
- Add daemon/ to allowed std::process::Command modules in CLAUDE.md
- Dedup and gaps from comprehensive Windows review
- Comprehensive Windows review — critical bugs and dedup
- Replace fs::write with atomic_write_str in production code
- Add Windows version queries table and bootstrap installation docs
- Comprehensive dedup and remaining review fixes (-263 lines)
- Gsettings value quoting bug and add display test
- Consolidate yaml value conversion, fix KDE boolean handling
- Add Windows service scanning to scan_system_settings for platform parity
- Add Windows registry scanning to scan_system_settings
- Expand Windows registry scan to cover policies, input, and DWM
- Use camelCase field names in docs prose instead of kebab-case
- Deduplicate yaml value conversion and registry line parsing
- Code review findings — CLAUDE.md map, xfconf create fallback, docs
- Windows cross-compilation and stale doc references
- 26 system configurator bugs — security, parity, correctness
- **e2e**: Review fixes — RBAC subresources, build optimization, error handling
- **ci**: AutoTag poisoned by #none in commit range — use BRANCH_HISTORY: last
- **e2e**: Add tests/e2e/helm/** path trigger to node workflow
- **e2e**: Replace --all with explicit name in operator driftalert cleanup
- **e2e**: Full-stack cleanup for cfgd-system CRDs, add chart path trigger
- **e2e**: Add imagePullSecrets for registry.jarvispro.io
- **e2e**: Use csiDriver.imagePullSecrets instead of global
- **e2e**: Correct CSI DaemonSet label selector
- **e2e**: Remove hardcoded registry, use REGISTRY env var everywhere
- **e2e**: Use Reflector for registry-credentials replication
- **e2e**: Add imagePullSecrets to agent DaemonSet chart template
- **e2e**: Don't overwrite ArgoCD-managed deployments in setup-cluster.sh
- **e2e**: Handle non-zero exit codes from exec_in_pod with set -e
- **e2e**: Push cfgd-server image tag, fix container name in set image
- **e2e**: Remove cfgd-server double-tag, production manifest now uses cfgd-operator
- **e2e**: Push :latest tags and rollout restart for ArgoCD-managed deployments
- **crds**: Merge packageVersions into PackageRef.version on policy CRDs
- **crds**: Fix clippy warnings and update stale packageVersions references
- **crds**: Remove redundant DriftAlertStatus.resolved bool
- **chart**: Remove redundant agent volume mounts
- **chart**: Separate agent ServiceAccount from operator
- **chart**: Guard test pod, PVC, cert-manager helpers, RBAC wildcards
- **chart**: Consistency fixes, CSI metrics, docs, label helpers
- **chart**: Update values.schema.json for all new values
- **chart**: Add cfgd.csiServiceAccountName helper for consistency
- **chart**: Add icon to Chart.yaml
- **chart**: DRY webhook rules with range loop, add Chart.yaml icon
- E2E tests fully passing against real k3s cluster
- **workflows**: Pass REGISTRY env var to all E2E test jobs
- Deduplicate yaml value helpers, tighten audit patterns #minor
- CI failures — audit cleanup, Windows type fix, E2E REGISTRY #minor
- Add mTLS support to crossplane function-cfgd #minor
- Cargo fmt #minor
- Move CI/Release to ubuntu-latest, fix protoc rate limit #minor

### Miscellaneous

- **docs**: Cleanup stale docs
- **plan**: Move completed source management section to COMPLETED.md
- **plan**: Mark CLI UX improvements complete, add docs follow-up
- **plan**: Mark CLI UX docs update complete
- Move CLI UX to COMPLETED.md, update PLAN.md with review findings, rename spec-reference to spec
- Consolidate Windows support tracking
- Mark Linux desktop configurators complete, clean PLAN.md
- Make Taskfile registry configurable, ignore specs dir
- Simplify .gitignore
- Move lightweight jobs to ubuntu-latest #minor
- Add workflow_dispatch to AutoTag #none
- Move non-E2E workflows to ubuntu-latest, add workflow_dispatch #minor

### Build

- Add windows-sys and zip dependencies for Windows support

### Nit

- Reword

## [0.1.0] - 2026-03-20

### Added

- **operator**: Add observedGeneration to Condition struct per KEP-1623
- **operator**: Refactor CRD API types — PackageRef, LabelSelector, typed refs, remove spec.name
- **operator**: Split MachineConfig Ready into 4 conditions, add DriftAlert conditions
- **operator**: Add printer columns, short names, categories, CEL validation to CRDs
- **operator**: Add MachineConfig finalizer and DriftAlert owner references
- **operator**: Add ClusterConfigPolicy CRD, controller, and webhook
- **operator**: Emit Kubernetes Events from all controllers
- **operator**: Add dedicated health probe server on port 8081
- **operator**: Add Lease-based leader election for HA
- **operator**: Add graceful shutdown with SIGTERM/SIGINT handling
- **operator**: Add Prometheus metrics endpoint on port 8443
- **operator**: Add OpenTelemetry tracing via OTEL_EXPORTER_OTLP_ENDPOINT
- **helm**: Consolidate operator + agent charts into unified chart/cfgd/
- **helm**: Add values schema, NOTES.txt, test hook, example values
- **helm**: Add multi-tenancy RBAC example templates and namespace isolation docs
- **test**: Add Crossplane E2E tests for TeamConfig generation
- **operator**: SSA field managers and structured merge diff annotations
- **operator**: Complete Tier 1 — PLAN.md updated, duplicate HEALTH_PORT fixed
- **tier2**: Module CRD, OCI push/pull, webhook policy enforcement
- **tier3**: OCI build, signing, supply chain security
- **csi**: Scaffold CSI driver crate with gRPC codegen
- **csi**: Identity service — GetPluginInfo, GetPluginCapabilities, Probe
- **csi**: LRU cache for OCI module artifacts
- **csi**: Node service — Publish/Unpublish/Stage/Unstage volumes
- **csi**: Metrics, main entry point, gRPC integration tests
- **helm**: CSI driver templates — DaemonSet, CSIDriver, RBAC
- **operator**: Pod module mutating webhook — /mutate-pods endpoint
- **helm**: MutatingWebhookConfiguration for pod module injection
- **cli**: Kubectl cfgd plugin — debug, exec, inject, status, version
- **crd**: Add mountPolicy (Always/Debug) to Module, debugModules to policies

### Changed

- **operator**: Cargo fmt
- **operator**: Cargo fmt
- **helm**: Move CRDs from templates/crds/ to Helm-native crds/
- Cargo fmt + fix CI protoc dependency #minor

### Documentation

- Fix kebab-case config field references to camelCase
- Check off all 123 steps in Tier 1 implementation plan
- Move Tier 3 to COMPLETED.md, update PLAN.md
- Tier 4 implementation plan — pod module injection
- Improve README — new tagline, richer module example, uncollapse docs table
- Krew manifest, move Tier 4 to COMPLETED.md, audit fixes
- Update PLAN.md with E2E test gaps, move distribution work to COMPLETED.md

### Fixed

- Use standard zsh completion syntax instead of ~/.zfunc #none
- Use standard shell completion syntax (source inline, not file redirect) #none
- **operator**: Implement match_expressions evaluation and DriftAlert Acknowledged/Escalated conditions
- **operator**: Review fixes — ReconcileError event, devices_enrolled metric, rename probe server
- Update audit script — add generate/ to shell exec, modules/ to config parse, expand exclusion lists
- **operator**: Review fixes — W3C trace propagator, test hook operator check, leader field manager
- **operator**: Round 2 review — validation gaps, printcolumn, SMD, webhook endpoint, doc comment
- **operator**: Round 2 review — all issues and suggestions
- **operator**: Production hardening — 35 issues from deep audit
- **tier3**: Address code review findings
- **tier3**: Add keyless verification CLI flags and missing tests
- **tier3**: Address all code review findings
- Eliminate audit warnings — DRY test PEM key, fix string literal check
- **csi**: Address code review findings on scaffold
- **csi**: Address Identity service review findings
- **csi**: Address all cache review findings
- **csi**: Address all Node service review findings
- **csi**: Wire metrics into Node service, fix labels, switch to axum
- **helm**: Address CSI template review findings
- **operator**: Address all mutating webhook review findings
- **cli**: Address kubectl plugin review findings
- **tier4**: Address final cross-cutting review findings
- **crd**: Policy debugModules overrides Module mountPolicy, add list annotations
- **release**: Close all distribution gaps — CSI image, kubectl plugin, Helm, OLM
- **release**: Address all 15 delivery audit findings
- **ci**: Install protobuf-compiler for cfgd-csi proto codegen

### Miscellaneous

- **helm**: Regenerate CRD templates from Rust types
- Remove plan file from tracking (gitignored)

## [0.0.1] - 2026-03-19

### Added

- Implement cfgd phases 1-7 — full workstation config management
- Implement module CLI integration (Phase D)
- Implement file management — deployment strategies, source:target mapping, private files, conflict detection
- Rewrite init, audit fixes, exhaustive test suite, brew PATH bootstrap
- CLI flag consistency audit — normalize flags across all commands
- Implement shell aliases in profiles and modules
- Pre-release hardening — security audit, panic fixes, correctness across all crates #minor
- Unified update flags, CI fixes, track audit script #patch
- Module delete --purge, fix E2E test failures #patch
- Add --output json/yaml and --jsonpath for structured CLI output
- Per-module and per-profile reconcile patches
- Warn when shell rc env/alias conflicts with cfgd-managed values #patch
- **errors**: Add GenerateError enum for AI-guided generation
- **config**: Add AiConfig type with provider, model, api-key-env fields
- **generate**: Add core types, session state, and generate module structure
- **providers**: Add installed_packages_with_versions and package_aliases to PackageManager trait
- **generate**: Add validate_yaml for schema validation with structured errors
- **generate**: Add annotated YAML schema export for Module, Profile, Config
- **generate**: Add scan_dotfiles and scan_shell_config tool implementations
- **generate**: Add read_file, list_directory, adopt_files with security model
- **generate**: Add inspect_tool and query_package_manager tool implementations
- **packages**: Implement installed_packages_with_versions and package_aliases for all providers
- **generate**: Add scan_installed_packages and scan_system_settings
- **mcp**: Add tool definitions, resource serving, and prompt definitions
- **generate**: AI-guided configuration generation with MCP server #minor
- Capture script output in journal + mask module env values #minor
- Profile-less status, verify, and apply for single modules #minor
- Ecosystem integration — policies, CI templates, OLM, DevContainer export, Homebrew tap #minor
- K8s convention alignment — camelCase fields, PascalCase enums, spec reference docs, audit hardening

### Changed

- Deduplicate E2E tests — merge CLI into Exhaustive suite #none

### Documentation

- Add cfgd generate design spec for AI-guided config generation
- Add cfgd generate implementation plan (24 tasks, 4 layers)
- Add AI config section, update cfgd-core module map for generate
- Add generate/, ai/, mcp/ to module map and Command allowlist
- Add comprehensive AI generate guide
- Add missing Env phase and complete system configurator list in reconciliation.md
- Fix drift_policy to kebab-case drift-policy in safety.md
- Fix --server to --server-url in enroll flag table
- Fix init to enroll in operator.md enrollment example
- Fix module add CLI to match actual implementation
- Fix module add syntax in bootstrap.md
- Fix module CLI syntax, add safety doc to table, fix manager count wording
- Fix module create --name to positional arg
- Add CLI reference cross-links to packages, system-configurators, templates
- Add Helm chart paths to CLAUDE.md module map
- Fix CLAUDE.md tree indentation and modules.md comment alignment
- Add cfgd generate and MCP server to README

### Fixed

- CI failures — allow_hyphen_values, NO_COLOR, E2E test fixes #patch
- Daemon panic, KIND v0.31.0, E2E test hardening #patch
- KIND node v1.32, CRD install, exhaustive test fixes #patch
- Correct KIND node image SHA, source repo branch #patch
- Remove kubeletExtraArgs, fix CRD binary name, source profiles #patch
- Source add interactive prompts, operator RBAC for E2E #patch
- Source add --yes flag, skip interactive prompts in E2E #patch
- Node E2E assertions, Helm chart daemon flags, source --yes #patch
- Exhaustive SRC/SEC/E11/AL01, drift-policy TitleCase #patch
- SRC11 --profile, source replace carries over settings, AL01/SEC fixes #patch
- Cargo fmt #patch
- SEC03 sops config path, TPL01 absolute path, DRIFT01 guard #patch
- SEC03 sops.yaml format, secrets subdir, DRIFT01 lenient #patch
- Sops config next to plaintext, JSON output in PLAN.md #none
- Operator resilience, T50 daemon assertion, SEC03 sops path #patch
- Sops --config path, T09 init syntax, server diagnostics #patch
- Cargo fmt #none
- Sops --config only on encrypt, not decrypt #none
- Rustls crypto provider, SEC04 key mismatch, concurrency groups #patch
- SEC04 use cfgd default age key path for sops #none
- File:// source URLs, T09 git init, SEC05 path — 202/0 local #none
- Increase controller reconcile wait to 60s, T09 git on KIND #patch
- Reduce controller requeue from 300s/3600s to 60s #patch
- Controller retry loop instead of silent death #patch
- MC controller checks live drift alerts instead of stale status #patch
- Jsonpath slice panic, yaml jsonpath support, remove unnecessary clones
- Operator T04 timestamp race, full-stack T09 no-op patch #none
- Rustfmt long lines, remove unwrap in daemon reconcile patch builder #patch
- Address batch review findings (security docs, dead-code, error handling, API client)
- Add conversation loop limit, config error handling, git status checks
- Remove dead GenerateError variants, update audit script for mcp/ boundary
- Review fixes — Tekton single-step, GH Action heredoc, OLM clusterPermissions, badge labels #none
- Vendor OpenSSL for cross-platform release builds #none
- Aarch64 cross-compilation linker, track skills/hooks in git
- Aarch64 cross-linker, Dockerfile vendored OpenSSL, E2E --set field names #minor
- Release pipeline — per-artifact checksums, cargo publish, homebrew reliability #minor
- MacOS sha256sum → shasum, fail-fast: false for build matrix #none
- Dockerfile vendored OpenSSL build deps (perl + make) #minor
- E2E test camelCase alignment, Crossplane Go version #minor

### Miscellaneous

- Remove committed Go binary, add to .gitignore #none

### Testing

- Add 36 unit tests to cfgd-core lib.rs #none
- Add 357 unit tests, rename e2e-cli workflow, add CI badges
- **generate**: Add write_module_yaml and write_profile_yaml integration tests
- **generate**: Add pipeline integration tests for tool dispatch; update CLI and bootstrap docs
- **mcp**: Add integration tests for MCP server protocol


