---
paths: ["crates/**/*.rs", "**/*.sh"]
---
# cfgd Style

- **Formatting**: `cargo fmt` (rustfmt defaults). No custom `rustfmt.toml`.
- **Linting**: `cargo clippy -- -D warnings`. All clippy warnings are errors.
- **Naming**: Rust conventions. `snake_case` for functions/variables, `PascalCase` for types/traits, `SCREAMING_SNAKE` for constants.
- **Imports**: Group by std, external crates, internal modules. Separated by blank lines.
- **Config serde**: `#[serde(rename_all = "camelCase")]` on config structs to match Kubernetes ecosystem conventions (maps Rust `snake_case` to YAML `camelCase`). Enums have no `rename_all` — they serialize as `PascalCase` by default.
- **Comments**: Only where the "why" isn't obvious. No doc comments on private functions unless the logic is genuinely complex.

## What NOT to do

- Don't add `#[allow(dead_code)]` — if code is unused, delete it.
- Don't add backwards-compatibility shims. Just change the code.
- Don't over-abstract. Three similar lines > a premature abstraction.

## Shell scripts

- Every tracked `*.sh` passes `task shellcheck`, which `task lint`, `task ci` and the CI `audit` job run. The population is `git ls-files '*.sh'`, so a new script is checked the moment it is added; CI pins the version in `.github/actions/setup-shellcheck`.
- `.shellcheckrc` at the repo root sets `shell=bash`, `source-path=SCRIPTDIR` and `external-sources=true`. It disables no code. Unquoted expansions (SC2086) and unused variables (SC2034) are fixed, never silenced tree-wide.
- A sourced file with no shebang starts with `# shellcheck shell=bash`.
- An argument list that is meant to split into several words is an array, expanded as `"${args[@]}"`; never an unquoted string.
- A `# shellcheck disable=SCxxxx` covers one command, is for a false positive only, and gives its reason on the same line: `# shellcheck disable=SC2016  # an awk program: its $ fields are awk's`.
