---
paths: ["**/*.rs"]
---
# cfgd Patterns

- **Builder pattern** for complex structs (plans, configs).
- **Trait objects** (`Box<dyn PackageManager>`) for runtime polymorphism over package managers.
- **`impl Into<T>`** for function parameters where multiple types make sense.
- **Structured logging** via `tracing`. Use `tracing::info!`, `tracing::debug!`, etc. Never `log::*`.
