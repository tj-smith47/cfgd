---
paths: ["**/*.rs"]
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
