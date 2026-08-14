---
paths: ["crates/**/*.rs"]
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
| `crate::to_posix_string(path)` | `path.to_string_lossy().into_owned()` | an owned `String` with `\`→`/` folded, for a key that is only ever COMPARED |
| `crate::to_posix_fs_key(path)` | `crate::to_posix_string(path)` | a persisted key that is also REOPENED as a path (`file_backups.file_path`, `module_file_manifest.file_path`); folds on Windows only |
| `crate::normalize_for_snapshot(captured, &[(path, label)])` | hand-rolled `.replace('\\', "/")` | snapshot goldens (also folds CRLF→LF + substitutes paths) |
| `crate::strip_windows_verbatim(s)` | inline `s.strip_prefix(r"\\?\")` | dropping the Windows `\\?\` verbatim prefix |

## When folding is MANDATORY

Anywhere a path crosses into a value that must agree across operating systems:

- **resource-ids** — `format!("env:write:{}", crate::to_posix_string(p))`, `env:inject:…`,
  `secret:decrypt:…`. These land in SQLite and are matched by exact string equality on
  every reconcile tick, so they take the unconditional `to_posix_string` fold —
  `posix()` is a `cfg(windows)` display adapter and folds nothing on a POSIX host
- **state / lockfiles** — anything serialized to JSON, YAML, or SQLite
- **snapshot goldens** — route the captured output through `normalize_for_snapshot`
- **effective config** — `effective.rs` is host-agnostic; its rendered paths fold
- **env-file / rc-file bodies** — content written into shell files consumed cross-OS
- **OCI annotations, `file://` URLs, gateway API payloads**

**The one exception — a key that is also a path.** `file_backups.file_path` and
`module_file_manifest.file_path` are read back out and handed to `Path::new` to
restore or delete a real file. A backslash is a legal POSIX filename character, so
folding one of those rows renames it onto a file that exists: a rollback then writes
backup content over, or deletes, the wrong target. Those two columns take
`to_posix_fs_key`, which folds on Windows (where `\` cannot occur in a filename, so
the substitution is reversible) and leaves POSIX exact.

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
