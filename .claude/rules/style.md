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
- Every tracked `tests/e2e/*.sh` parses under the shfmt pinned in `.github/actions/setup-shfmt/action.yml` (`task e2e:tags:check` reads its syntax tree and fails on a script that does not parse); quote an associative-array key that holds a `.` (`["a.sh"]=1`), which shfmt otherwise reads as a floating-point index.
- `.shellcheckrc` at the repo root sets `source-path=SCRIPTDIR` and `external-sources=true`, and disables no code. Each script's shebang picks its dialect, so a `#!/bin/sh` script is held to POSIX sh. SC2086 and SC2034 findings are fixed in the script; `.shellcheckrc` does not disable them.
- A sourced file with no shebang starts with `# shellcheck shell=bash` (shellcheck reports SC2148 until it does).
- An argument list meant to split into several words is an array, expanded as `"${args[@]}"`.
- A captured value reaches a reader through a here-string: `grep -qF "$want" <<<"$out"`, `head -10 <<<"$out"`. A reader that can stop before its input ends (`grep -q`/`-m`/`-l`, `head`, a `sed` that runs `q`, an `awk` that runs `exit`, a `read` outside a `while` condition) closes the pipe early, its producer dies of SIGPIPE while still writing, and under `pipefail` a match reads as a miss. A command producer reaches a reader that reads to the end: `sed -n '1p'` for the first line, `grep ... >/dev/null` for a test, or `grep -m1` over a file or here-string. The audit gate "Pipelines whose reader can exit before the producer is done" fails on a captured value echoed or printf-ed into any grep, head, `sed -n` or awk (it does not tell a `q` or `exit` apart), and on a command piped into a reader that can stop early, in every tracked `*.sh`, `*.tpl`, `Taskfile.yml`, workflow and composite action. A pipeline whose producer is bounded carries `# drain-ok: <why>` in a comment on one of its own lines, which exempts every pipeline on the lines it sits on, and no pipeline on another line joined to them. A heredoc the gate sees open at the end of a file is an error at the line that opened it. A `#!/bin/sh` script has no here-string, so it matches with `case` or reads to the end.
- A `# shellcheck disable=SCxxxx` covers one command, is for a false positive only, and gives its reason on the same line: `# shellcheck disable=SC2016  # an awk program; the $ fields belong to awk`.
