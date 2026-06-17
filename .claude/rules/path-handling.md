---
paths: ["**/*.rs"]
---
# cfgd Path Handling — fold to `/` at every cross-OS string boundary

A `Path` rendered with the host-native separator is a **runtime correctness bug**,
not a cosmetic one. On Windows `Path::display()` / `to_string_lossy()` emit `\`.
The moment that string becomes a value compared against, stored, serialized, or
matched on another OS, it silently disagrees with its Unix-authored counterpart.

> **War story (the bug class this rule exists to kill):** `env:write:{path}`
> resource-ids were built with `path.display()`. On Windows the id rendered
> `env:write:C:\Users\…\.bashrc`; the desired-state id (authored Unix-side) was
> `env:write:C:/Users/…/.bashrc`. They never matched, so the env file was
> re-planned as drift on **every** reconcile and never converged. Same failure
> mode hid in `effective.rs` source rendering and in tilde-expanded env-file
> bodies written into bash/fish/PowerShell files.

## The central API — `crates/cfgd-core/src/util/paths.rs`

Do not invent a new normalizer. The crate already standardizes this in one place:

| Use this | Instead of | For |
|---|---|---|
| `path.posix()` (via `use crate::PathDisplayExt;`) | `path.display()` | a `Display` that always emits `/` |
| `crate::to_posix_string(path)` | `path.to_string_lossy().into_owned()` | an owned `String` with `\`→`/` folded |
| `crate::normalize_for_snapshot(captured, &[(path, label)])` | hand-rolled `.replace('\\', "/")` | snapshot goldens (also folds CRLF→LF + substitutes paths) |
| `crate::strip_windows_verbatim(s)` | inline `s.strip_prefix(r"\\?\")` | dropping the Windows `\\?\` verbatim prefix |

## When folding is MANDATORY

Anywhere a path crosses into a value that must agree across operating systems:

- **resource-ids** — `format!("file:link:{}", p.posix())`, `env:write:…`, `env:inject:…`
- **state / lockfiles** — anything serialized to JSON, YAML, or SQLite
- **snapshot goldens** — route the captured output through `normalize_for_snapshot`
- **effective config** — `effective.rs` is host-agnostic; its rendered paths fold
- **env-file / rc-file bodies** — content written into shell files consumed cross-OS
- **OCI annotations, `file://` URLs, gateway API payloads**

## When native IS correct (opt out explicitly)

Terminal output, `tracing` log lines, and human-facing error messages may keep the
native separator — a Windows user reading a log wants `\`. To keep one of those
when the post-edit hook flags it, append a justification on the same line:

```rust
tracing::warn!("cannot read {}", path.display()); // native-ok: log line, not a key
```

The hook flags **newly-added** native renders only; the documented legacy baseline
(`grep -rn '\.display()\|to_string_lossy()' crates/cfgd-core/src`) is swept
separately. Never reintroduce a native render for a string that must match across
OSes — reach for the helper above.

See also `module-boundaries.md` (the same allow-list-plus-escape enforcement shape).
