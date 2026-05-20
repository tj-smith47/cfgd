---
paths: ["**/*.rs"]
---
# cfgd Module Boundaries — `std::process::Command` allow-list

If you need to shell out, it must go through a controlled execution layer, not scattered across the codebase.

`std::process::Command` is permitted **only** in:

| Module | Purpose |
|---|---|
| `cli/` | spawns `$EDITOR` for resource editing commands |
| `packages/` | `PackageManager` trait implementations |
| `secrets/` | `sops`, `op`, `bw`, `vault` external CLIs |
| `system/` | `SystemConfigurator` trait implementations |
| `reconciler/` | pre/post-reconcile script execution |
| `platform/` | OS detection (`sw_vers`, `freebsd-version`) |
| `sources/` | `git` signature verification + clone fallback |
| `gateway/` | `ssh-keygen`, `gpg` for enrollment signature verification |
| `output/` | `Printer::run_with_output` — the controlled execution layer for buffered progress display |
| `generate/` | tool inspection (`--version` checks), system settings scanning |
| `oci/` | Docker credential helpers (`docker-credential-*`) |
| `daemon/` | `sc.exe` for Windows Service lifecycle |
| `crates/cfgd-csi/` | `mount`/`umount` fallback for bind mount operations |
| `cfgd-core/src/util/git.rs::git_cmd_safe` / `try_git_cmd` | shared git-command factory with safe-env hardening for REMOTE-touching git operations (clone, fetch, ls-remote). Re-exported as `cfgd_core::git_cmd_safe` / `cfgd_core::try_git_cmd` |
| `cfgd-core/src/util/git.rs::git_cmd_local` | shared git-command factory for LOCAL-only git operations (`config` get/set, `tag -v`, `add`, `commit`, `rev-parse`, `log`); sets `GIT_TERMINAL_PROMPT=0` and skips `GIT_SSH_COMMAND`. Required for every local git invocation in `cli/` and `system/` — no module is permitted to construct `Command::new("git")` directly. Re-exported as `cfgd_core::git_cmd_local` |
| `cfgd-core/src/util/git.rs::cosign_cmd` | shared cosign-command factory for Sigstore signature / attestation work; consumed from `oci.rs`, `cli/module.rs`, and `upgrade.rs` — the controlled layer for every cosign shell-out in the workspace. Re-exported as `cfgd_core::cosign_cmd` |
| `cfgd-core/src/util/process.rs::command_output_with_timeout` | timeout-bounded `Command` execution; consumers pass a built `Command` (no direct construction in this layer). Re-exported as `cfgd_core::command_output_with_timeout` |

All other modules that need to invoke external commands must call through one of the above layers.

See Hard Rule #6 in `hard-rules.md`.
