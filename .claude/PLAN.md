# cfgd — Remaining Work

Single source of truth for all incomplete work. Items are in dependency order — earlier sections unblock later ones. Completed work is in `INITIAL-PLAN.md`.

---

## CLI restructure

Command consolidation and consistency fixes. Depends on: nothing.

### Collapse `plan` into `apply --dry-run`

- [x] Remove `Command::Plan` — merge into `Command::Apply` with `--dry-run` flag
- [x] `--dry-run` supports `--phase` for phase-specific dry runs
- [x] Move all plan flags (`--skip`, `--only`, `--module`) onto `apply` (most already there)
- [x] Update all help text, error messages, and docs referencing `cfgd plan`

### CLI flag parity

Missing flags for existing schema fields.

- [x] `profile create --secret source:target` (repeatable) — `ProfileSpec.secrets` has no CLI flag
- [x] `profile create --pre-reconcile` (repeatable) — `ScriptSpec.pre_reconcile` has no CLI flag
- [x] `profile update --add-inherits` / `--remove-inherits` — `inherits` only settable at create time
- [x] `profile update --add-post-reconcile` / `--remove-post-reconcile` — post-reconcile only settable at create time
- [x] `profile update --add-pre-reconcile` / `--remove-pre-reconcile` — pre-reconcile has no flag at all
- [x] `profile update --add-secret` / `--remove-secret` — secrets have no flag at all

### Profile file layout consistency

- [x] Change profile file storage from `files/<profile>/` to `profiles/<name>/files/`
- [x] Update `cmd_profile_create`: write files to `profiles/<name>/files/`
- [x] Update `cmd_profile_update`: add/remove files from `profiles/<name>/files/`
- [x] Update `cmd_profile_delete`: clean up `profiles/<name>/files/`
- [x] Update `ManagedFileSpec.source` paths in profile YAML to use `profiles/<name>/files/` prefix
- [x] Migration: detect old `files/<name>/` layout, move to new location on first access

### Alias system

Like `gh alias`. Depends on: plan/apply collapse (aliases reference final command names).

- [x] `spec.aliases` field in `cfgd.yaml` config schema — map of alias name → command string
- [x] Built-in default aliases: `add` → `profile update --active --add-file`, `remove` → `profile update --active --remove-file`
- [x] `--active` flag on `profile update` — resolves active profile from config so you don't type the name
- [x] Alias resolution in CLI dispatch — expand alias before clap parsing
- [x] Remove hardcoded `Command::Add` and `Command::Remove` variants
- [x] Users can override defaults or add custom aliases

## File management

File deployment, mapping, privacy, and conflict handling. Depends on: profile file layout.

### File deployment strategy

- [x] `FileStrategy` enum: `Symlink` (default), `Copy`, `Template`, `Hardlink`
- [x] Global default in `cfgd.yaml`: `spec.file-strategy: symlink`
- [x] Per-file override: `strategy: copy` field on `ManagedFileSpec` and `ModuleFileEntry`
- [x] Template files auto-upgrade to `Copy` regardless of global setting (can't symlink unrendered templates)
- [x] `files/mod.rs` `apply()`: dispatch on strategy — symlink via `std::os::unix::fs::symlink`, copy via current behavior, hardlink via `std::fs::hard_link`
- [x] `--file` on `module create` / `profile create`: move original into repo, symlink back (adopt)
- [x] External module files: symlink from target to `~/.cache/cfgd/modules/` checkout

### File source:target mapping

- [x] `--file <path>` without `:<target>` = adopt in place (move into repo, symlink back to same location)
- [x] `--file <source>:<target>` = explicit source and destination (e.g., `--file ./my-config:~/.config/app/config`)
- [x] Parse `:` separator in `--file` flag values for both module and profile create/update commands

### Private files

- [x] `private: true` field on `ManagedFileSpec` (profile files) and `ModuleFileEntry` (module files)
- [x] When `private: true`: auto-add source path to `.gitignore`
- [x] On other machines: skip silently during apply, show "private (local only)" in plan
- [x] Missing source + `private: false` (default) = error. Missing source + `private: true` = skip.
- [x] `--private` flag on file-adding commands (`profile update --add-file`, `module create --file`)

### Conflict detection

Depends on: file deployment strategy.

- [x] At plan time: collect all target paths from all active modules and profile files
- [x] Two sources targeting same path with **different** content = hard error in plan
- [x] Two sources targeting same path with **identical** content = no error
- [x] Clear error message listing both sources and the conflicting target path

### Backup/restore prompts

Depends on: conflict detection.

- [ ] `cfgd apply`: if target path exists as unmanaged file (not a cfgd symlink), prompt: adopt / backup (`<path>.cfgd-backup`) / skip
- [ ] When removing a module via `profile update --remove-module`: if `.cfgd-backup` exists at a managed path, prompt to restore

## Enrollment key verification

Replaces bootstrap token flow. Depends on: nothing (server-side change).

- [ ] Admin adds user to TeamConfig with SSH or GPG public key
- [ ] `cfgd enroll --server <url>` signs a server-issued challenge with private key (SSH agent or GPG)
- [ ] Server verifies signature against stored public key → enrolls device
- [ ] Remove bootstrap token generation/validation (or keep as fallback for environments without SSH/GPG)
- [ ] Update `team-config-controller.md` with new enrollment flow

## Release readiness

Last. No new features — completeness and polish only.

- [ ] Update e2e tests for CLI restructure (`module source` → `module registry`, `module-sources` → `modules.registries`, killed `add-to-profile`/`remove-from-profile`, `plan` → `apply --dry-run`)
- [ ] Shell completions (bash/zsh/fish via `clap_complete`)
- [ ] Documentation sweep: README covers all shipped features, examples for every use case
- [ ] JSON Schema generation: committed schemas for cfgd.yaml, profile YAML, cfgd-source.yaml
- [ ] CRD manifests committed and auto-generated
- [ ] CLAUDE.md + module map accuracy — verify descriptions match reality
- [ ] CONTRIBUTING.md updated for final crate layout
- [ ] Document operational mode taxonomy (Q8) in architecture.md
- [ ] No TODO/FIXME/placeholder URLs remain in shipped files
