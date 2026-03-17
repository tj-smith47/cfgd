# Contributing to cfgd

Thank you for your interest in contributing to cfgd! This document provides guidelines for contributing.

## Code of Conduct

Please read and follow our [Code of Conduct](CODE_OF_CONDUCT.md).

## How to Contribute

### Reporting Bugs

Before filing a bug, please:

1. Search [existing issues](https://github.com/tj-smith47/cfgd/issues) to avoid duplicates
2. Include the output of `cfgd --version` and `cfgd doctor`
3. Provide steps to reproduce the issue
4. Include relevant config snippets (redact secrets)

### Suggesting Features

Open a [feature request](https://github.com/tj-smith47/cfgd/issues/new?template=feature_request.yml) with:

- A clear description of the problem you're solving
- Your proposed solution
- Any alternatives you've considered

### Pull Requests

1. Fork the repository
2. Create a feature branch: `git checkout -b feat/my-feature`
3. Make your changes following the coding standards below
4. Run all checks: `task check`
5. Commit with [conventional commit](https://www.conventionalcommits.org/) messages
6. Push and open a PR against `master`

## Development Setup

### Prerequisites

- Rust 1.94+ (install via [rustup](https://rustup.rs/))
- `sops` (for secrets tests)
- `age` (for encryption tests)

### Building

```sh
cargo build
```

### Testing

```sh
cargo test                        # run all tests
cargo test -p cfgd-core           # test a specific crate
```

### Linting

```sh
cargo fmt --check                 # check formatting
cargo clippy -- -D warnings       # lint
bash .claude/scripts/audit.sh     # project-specific audit
```

### Project Structure

```
crates/
  cfgd-core/    # shared library: config, providers, reconciler, state, daemon
  cfgd/         # CLI binary: packages, files, secrets, system configurators
  cfgd-operator/# k8s operator: CRDs, controllers, webhook, device gateway (fleet API, web UI, enrollment)
```

## Coding Standards

### Hard Rules

1. **All terminal output goes through `output::Printer`** — no `println!`, `eprintln!`, or direct use of `console`/`indicatif`
2. **No `unwrap()` or `expect()` in library code** — use `?` with proper error types
3. **All providers implement their traits** — the reconciler depends on `ProviderRegistry`, never concrete implementations
4. **`thiserror` for library errors, `anyhow` only in `main.rs` and `cli/`**
5. **Config structs in `config/` only** — with `serde::Deserialize` + `serde::Serialize`
6. **No `std::process::Command` outside `cli/`, `packages/`, `secrets/`, `system/`, `reconciler/`, `platform/`, `sources/`, and `gateway/`**

### Style

- `cargo fmt` defaults (no custom rustfmt.toml)
- `cargo clippy -- -D warnings` — all warnings are errors
- Group imports: std, external crates, internal modules (separated by blank lines)
- `#[serde(rename_all = "kebab-case")]` on config structs
- Co-located unit tests in `#[cfg(test)] mod tests {}`

### Commit Messages

Use [conventional commits](https://www.conventionalcommits.org/):

```
feat: add pipx package manager support
fix: handle missing sops binary gracefully
docs: update profile inheritance examples
chore: update dependencies
```

Version bump tags in commit messages:

- `#major` — bump major version (breaking changes)
- `#minor` — bump minor version (new features)
- `#patch` — bump patch version (bug fixes)
- `#none` — skip version bump (docs, chore, CI)

## Getting Help

- Open a [discussion](https://github.com/tj-smith47/cfgd/discussions) for questions
- Check [existing issues](https://github.com/tj-smith47/cfgd/issues) for known problems
