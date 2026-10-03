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

- Every tracked `*.sh` passes `task shellcheck`, which `task lint`, `task ci` and the CI `audit` job run. The population is `git ls-files '*.sh'`, so a new script is checked the moment it is added.
- The installer `task snapshot` renders from `scripts/install.sh.tpl` passes `task installer:shellcheck`, which lints `dist/install.sh` as POSIX sh. `task lint` runs it after `task snapshot`; CI runs it in the `snapshot` job.
- The shellcheck version is pinned once, as the `version` default in `.github/actions/setup-shellcheck/action.yml`. Both tasks refuse a local shellcheck of any other version.
- `.shellcheckrc` at the repo root sets `source-path=SCRIPTDIR` and `external-sources=true`, and disables no code. Each script's shebang picks its dialect, so a `#!/bin/sh` script is held to POSIX sh. SC2086 and SC2034 findings are fixed in the script; `.shellcheckrc` does not disable them.
- A sourced file with no shebang starts with `# shellcheck shell=bash` (shellcheck reports SC2148 until it does).
- An argument list meant to split into several words is an array, expanded as `"${args[@]}"`.
- A `# shellcheck disable=SCxxxx` covers one command, is for a false positive only, and gives its reason on the same line: `# shellcheck disable=SC2016  # an awk program; the $ fields belong to awk`.
