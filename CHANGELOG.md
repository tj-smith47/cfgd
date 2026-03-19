# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

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

### Documentation

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

- Address batch review findings (security docs, dead-code, error handling, API client)
- Add conversation loop limit, config error handling, git status checks
- Remove dead GenerateError variants, update audit script for mcp/ boundary

### Testing

- **generate**: Add write_module_yaml and write_profile_yaml integration tests
- **generate**: Add pipeline integration tests for tool dispatch; update CLI and bootstrap docs
- **mcp**: Add integration tests for MCP server protocol

## [0.0.26] - 2026-03-19

### Documentation

- Add cfgd generate design spec for AI-guided config generation

### Fixed

- Rustfmt long lines, remove unwrap in daemon reconcile patch builder #patch

## [0.0.25] - 2026-03-19

### Added

- Per-module and per-profile reconcile patches
- Warn when shell rc env/alias conflicts with cfgd-managed values #patch

### Fixed

- Operator T04 timestamp race, full-stack T09 no-op patch #none

### Testing

- Add 36 unit tests to cfgd-core lib.rs #none
- Add 357 unit tests, rename e2e-cli workflow, add CI badges

## [0.0.24] - 2026-03-18

### Added

- Add --output json/yaml and --jsonpath for structured CLI output

### Fixed

- Jsonpath slice panic, yaml jsonpath support, remove unnecessary clones

## [0.0.23] - 2026-03-18

### Fixed

- MC controller checks live drift alerts instead of stale status #patch

## [0.0.22] - 2026-03-18

### Fixed

- Controller retry loop instead of silent death #patch

## [0.0.21] - 2026-03-18

### Fixed

- Reduce controller requeue from 300s/3600s to 60s #patch

## [0.0.20] - 2026-03-18

### Fixed

- SEC04 use cfgd default age key path for sops #none
- File:// source URLs, T09 git init, SEC05 path — 202/0 local #none
- Increase controller reconcile wait to 60s, T09 git on KIND #patch

## [0.0.19] - 2026-03-18

### Fixed

- Cargo fmt #none
- Sops --config only on encrypt, not decrypt #none
- Rustls crypto provider, SEC04 key mismatch, concurrency groups #patch

## [0.0.18] - 2026-03-18

### Fixed

- Sops --config path, T09 init syntax, server diagnostics #patch

## [0.0.17] - 2026-03-18

### Changed

- Deduplicate E2E tests — merge CLI into Exhaustive suite #none

### Fixed

- Sops config next to plaintext, JSON output in PLAN.md #none
- Operator resilience, T50 daemon assertion, SEC03 sops path #patch

## [0.0.16] - 2026-03-18

### Fixed

- SEC03 sops.yaml format, secrets subdir, DRIFT01 lenient #patch

## [0.0.15] - 2026-03-18

### Fixed

- SEC03 sops config path, TPL01 absolute path, DRIFT01 guard #patch

## [0.0.14] - 2026-03-18

### Fixed

- Cargo fmt #patch

## [0.0.13] - 2026-03-18

### Fixed

- SRC11 --profile, source replace carries over settings, AL01/SEC fixes #patch

## [0.0.12] - 2026-03-18

### Fixed

- Exhaustive SRC/SEC/E11/AL01, drift-policy TitleCase #patch

## [0.0.11] - 2026-03-18

### Fixed

- Node E2E assertions, Helm chart daemon flags, source --yes #patch

## [0.0.10] - 2026-03-17

### Fixed

- Source add --yes flag, skip interactive prompts in E2E #patch

## [0.0.9] - 2026-03-17

### Fixed

- Source add interactive prompts, operator RBAC for E2E #patch

## [0.0.8] - 2026-03-17

### Fixed

- Remove kubeletExtraArgs, fix CRD binary name, source profiles #patch

## [0.0.7] - 2026-03-17

### Fixed

- Correct KIND node image SHA, source repo branch #patch

## [0.0.6] - 2026-03-17

### Fixed

- KIND node v1.32, CRD install, exhaustive test fixes #patch

## [0.0.5] - 2026-03-17

### Fixed

- Daemon panic, KIND v0.31.0, E2E test hardening #patch

## [0.0.4] - 2026-03-17

### Added

- Module delete --purge, fix E2E test failures #patch

## [0.0.3] - 2026-03-17

### Fixed

- CI failures — allow_hyphen_values, NO_COLOR, E2E test fixes #patch

## [0.0.2] - 2026-03-17

### Added

- Unified update flags, CI fixes, track audit script #patch

### Miscellaneous

- Remove committed Go binary, add to .gitignore #none

## [0.0.1] - 2026-03-17

### Added

- Implement cfgd phases 1-7 — full workstation config management
- Implement module CLI integration (Phase D)
- Implement file management — deployment strategies, source:target mapping, private files, conflict detection
- Rewrite init, audit fixes, exhaustive test suite, brew PATH bootstrap
- CLI flag consistency audit — normalize flags across all commands
- Implement shell aliases in profiles and modules
- Pre-release hardening — security audit, panic fixes, correctness across all crates #minor


