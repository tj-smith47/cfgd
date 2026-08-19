//! Module lockfile — tracking remote modules with integrity hashes,
//! and module-spec diffing for sync output.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use crate::PathDisplayExt;
use crate::config::{ModuleLockEntry, ModuleLockfile};
use crate::errors::{ConfigError, ModuleError, Result};
use crate::output::Role;

use super::git::{GitSource, fetch_git_source, git_cache_dir, parse_git_source, resolve_subdir};
use super::loader::{load_module, load_modules};
use super::{LoadedModule, SourceModuleRoot};

/// Load the module lockfile from `<config_dir>/modules.lock`.
/// Returns an empty lockfile if the file does not exist.
pub fn load_lockfile(config_dir: &Path) -> Result<ModuleLockfile> {
    let lockfile_path = config_dir.join("modules.lock");
    crate::record_config_input(&lockfile_path);
    if !lockfile_path.exists() {
        return Ok(ModuleLockfile::default());
    }
    let contents = std::fs::read_to_string(&lockfile_path).map_err(|e| ConfigError::Invalid {
        message: format!("cannot read lockfile {}: {e}", lockfile_path.posix()),
    })?;
    let lockfile: ModuleLockfile = serde_yaml::from_str(&contents).map_err(ConfigError::from)?;
    Ok(lockfile)
}

/// Save the module lockfile to `<config_dir>/modules.lock`.
/// Uses `atomic_write_str` (temp file + rename) to prevent corruption.
pub fn save_lockfile(config_dir: &Path, lockfile: &ModuleLockfile) -> Result<()> {
    let lockfile_path = config_dir.join("modules.lock");
    let contents = serde_yaml::to_string(lockfile).map_err(ConfigError::from)?;
    crate::atomic_write_str(&lockfile_path, &contents).map_err(|e| ConfigError::Invalid {
        message: format!("cannot write lockfile {}: {e}", lockfile_path.posix()),
    })?;
    Ok(())
}

/// Compute SHA-256 integrity hash of a module directory's contents.
/// Hashes file paths (relative to module dir) and their contents, sorted for determinism.
///
/// The digest is taken over `<rel-path>\0<contents>\0` per file in sorted order,
/// streamed into the hasher a chunk at a time. The byte sequence is exactly the
/// one a buffered concatenation produced, because an existing `modules.lock`
/// entry has to keep verifying — what changed is that a module's whole tree is
/// no longer resident (twice) to answer a 32-byte question.
pub fn hash_module_contents(module_dir: &Path) -> Result<String> {
    let mut entries: Vec<(String, std::path::PathBuf)> = Vec::new();
    collect_files_for_hash(module_dir, module_dir, &mut entries)?;
    entries.sort_by(|a, b| a.0.cmp(&b.0));

    let mut hasher = crate::Sha256Stream::new();
    for (rel_path, path) in &entries {
        hasher.update(rel_path.as_bytes());
        hasher.update(&[0]);
        hasher.absorb_file(path)?;
        hasher.update(&[0]);
    }

    Ok(hasher.finish_digest())
}

pub(super) fn collect_files_for_hash(
    base: &Path,
    current: &Path,
    entries: &mut Vec<(String, std::path::PathBuf)>,
) -> Result<()> {
    if !current.is_dir() {
        return Ok(());
    }
    let dir_entries = std::fs::read_dir(current)?;

    for entry in dir_entries {
        let entry = entry?;
        let path = entry.path();
        // Skip git metadata. `.git` is the repo internals; `.gitattributes`
        // controls checkout (e.g. line-ending normalization) and is tooling
        // metadata, not deployable module content — hashing it would make the
        // integrity digest depend on the author's checkout config rather than
        // the module's bytes.
        if path
            .file_name()
            .is_some_and(|n| n == ".git" || n == ".gitattributes")
        {
            continue;
        }
        // Skip symlinks — only hash real files to avoid infinite recursion
        // and to avoid hashing files outside the module tree
        let meta = std::fs::symlink_metadata(&path)?;
        if meta.is_symlink() {
            continue;
        }
        if meta.is_dir() {
            collect_files_for_hash(base, &path, entries)?;
        } else {
            // The relative-path key feeds the integrity digest, so it must be
            // byte-identical across operating systems. Fold `\` → `/` via the
            // central helper; a native `to_string_lossy()` would key a nested
            // file as `templates\foo.conf` on Windows vs `templates/foo.conf`
            // on Linux, diverging the digest for identical module bytes.
            let rel = crate::to_posix_string(path.strip_prefix(base).unwrap_or(&path));
            entries.push((rel, path));
        }
    }
    Ok(())
}

/// Verify the integrity of a locked remote module against its lockfile entry.
pub fn verify_lockfile_integrity(lock_entry: &ModuleLockEntry, cache_base: &Path) -> Result<()> {
    let git_src = parse_git_source(&lock_entry.url)?;
    let local_path = resolve_subdir(
        git_cache_dir(cache_base, &git_src.repo_url),
        &lock_entry.subdir,
        &lock_entry.name,
        &lock_entry.url,
    )?;

    if !local_path.exists() {
        return Err(ModuleError::GitFetchFailed {
            module: lock_entry.name.clone(),
            url: lock_entry.url.clone(),
            message: format!(
                "cached module directory does not exist — run 'cfgd module upgrade {}'",
                lock_entry.name
            ),
        }
        .into());
    }

    let actual_integrity = hash_module_contents(&local_path)?;
    if actual_integrity != lock_entry.integrity {
        return Err(ModuleError::IntegrityMismatch {
            name: lock_entry.name.clone(),
            expected: lock_entry.integrity.clone(),
            actual: actual_integrity,
        }
        .into());
    }

    Ok(())
}

/// Load remote modules from the lockfile, fetching if needed, and merge
/// them into the given modules map.
///
/// A locked entry is resolved by the COMMIT the lock records, not by the tag it
/// was locked from. Both name the same tree — `commit` is what `pinnedRef`
/// resolved to at lock time, and re-pinning either is what `cfgd module upgrade`
/// is for — but only the commit is an immutable object id, which is what lets
/// [`fetch_git_source`] answer it out of the cache with no network at all. Every
/// run used to pay one full fetch cycle per locked entry to re-learn where a
/// pinned tag pointed, which by the lockfile's own contract cannot have moved.
///
/// A cache that cannot answer the pin is still materialized from the remote: the
/// lockfile is a determinism guarantee, not an offline one (see `docs/modules.md`
/// — a machine that has never fetched a locked module has nothing to resolve
/// from, and cloning a recorded commit is deterministic). What is gone is the
/// per-run fetch of an entry the cache already holds.
pub fn load_locked_modules(
    config_dir: &Path,
    cache_base: &Path,
    modules: &mut HashMap<String, LoadedModule>,
    printer: &crate::output::Printer,
) -> Result<()> {
    let lockfile = load_lockfile(config_dir)?;

    for entry in &lockfile.modules {
        // Skip if a local module with the same name already exists (local wins)
        if modules.contains_key(&entry.name) {
            continue;
        }

        let git_src = parse_git_source(&entry.url)?;

        // Build a GitSource with the pinned ref
        let pinned_src = GitSource {
            repo_url: git_src.repo_url.clone(),
            tag: Some(locked_ref(entry)),
            git_ref: None,
            subdir: entry.subdir.clone(),
        };

        // Fetch to cache (no-op if already present at correct ref)
        let local_path = fetch_git_source(&pinned_src, cache_base, &entry.name, printer)?;

        // Verify integrity
        verify_lockfile_integrity(entry, cache_base)?;

        // Load the module
        let module = load_module(&local_path)?;
        modules.insert(entry.name.clone(), module);
    }

    Ok(())
}

/// The ref a locked entry is checked out at: its recorded commit when the lock
/// carries a full object id, and otherwise the tag it was locked from.
///
/// The fallback is not a legacy shim — `commit` has always been written by
/// `get_head_commit_sha`, so every lockfile cfgd wrote carries a full id — it is
/// for a lockfile a human edited or truncated, where a short or empty `commit`
/// must degrade to the tag rather than fail the load with an unresolvable ref.
fn locked_ref(entry: &ModuleLockEntry) -> String {
    let commit = entry.commit.trim();
    if super::git::is_full_object_id(commit) {
        return commit.to_string();
    }
    entry.pinned_ref.clone()
}

/// Load module bodies delivered by subscribed ConfigSources into `modules`.
///
/// Precedence: a name already present (consumer-local or locked) is never
/// overwritten, and among `source_roots` the higher `priority` wins. Each root's
/// `offered` list is the publisher-declared allow-list (the source manifest's
/// `provides.modules`): a body present on disk but absent from `offered` is NOT
/// loaded. Loaded modules are tagged with `origin = Some(source_name)`.
pub fn load_source_modules(
    source_roots: &[SourceModuleRoot],
    modules: &mut HashMap<String, LoadedModule>,
) -> Result<()> {
    let mut roots: Vec<&SourceModuleRoot> = source_roots.iter().collect();
    // Higher priority first so it wins the first-insert race for a shared name.
    // Equal priorities tie-break on source_name so the winner is deterministic
    // regardless of slice order.
    roots.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| a.source_name.cmp(&b.source_name))
    });

    for root in roots {
        for name in &root.offered {
            if modules.contains_key(name) {
                continue;
            }
            let module_yaml = root.modules_dir.join(name).join("module.yaml");
            // Recorded BEFORE the existence test, and so recorded even when the
            // answer is "nothing there": an offered module whose body arrives on
            // a later sync is a change to the desired state, and the only thing
            // that can report it is an absent-input entry for the file that was
            // missing. The source checkout's own directory stamp cannot — the
            // body lands two levels below it.
            crate::record_config_input(&module_yaml);
            if !module_yaml.exists() {
                continue;
            }
            // Body integrity rides the source's HEAD commit-signature verification
            // (`sources::verify_commit_signature`), which covers the whole source
            // repo including delivered module bodies — there is no separate
            // per-module signature to check here.
            let mut module = load_module(&root.modules_dir.join(name))?;
            module.origin = Some(root.source_name.clone());
            // Fail-closed: a source not permitted to run scripts may not deliver a
            // module body carrying lifecycle scripts or `prefer: [script]` package
            // installs. This mirrors the profile-layer no_scripts enforcement in
            // the composition constraint check, applied at the module-delivery
            // boundary where the per-root `scripts_permitted` decision is known.
            if !root.scripts_permitted
                && let Some(kind) = module_script_kind(&module)
            {
                return Err(ModuleError::ScriptsNotAllowed {
                    source_name: root.source_name.clone(),
                    module: name.clone(),
                    kind,
                }
                .into());
            }
            modules.insert(name.clone(), module);
        }
    }
    Ok(())
}

/// Describe the first script-bearing element of a module body, or `None` if the
/// body runs no source-supplied code: no lifecycle scripts, no `prefer: [script]`
/// package installs, and no `strategy: Patch` filter script. Used to enforce a
/// source's `noScripts` constraint over delivered bodies.
fn module_script_kind(module: &LoadedModule) -> Option<String> {
    if let Some(ref scripts) = module.spec.scripts {
        let lifecycle = [
            ("preApply", &scripts.pre_apply),
            ("postApply", &scripts.post_apply),
            ("preReconcile", &scripts.pre_reconcile),
            ("postReconcile", &scripts.post_reconcile),
            ("onChange", &scripts.on_change),
            ("onDrift", &scripts.on_drift),
        ];
        for (label, entries) in lifecycle {
            if !entries.is_empty() {
                return Some(format!("a {label} script"));
            }
        }
    }
    for pkg in &module.spec.packages {
        if pkg.prefer.iter().any(|p| p == "script") {
            return Some(format!(
                "a 'prefer: [script]' install for package '{}'",
                pkg.name
            ));
        }
    }
    // A `strategy: Patch` filter runs on EVERY command, read-only ones
    // included — the merge is computed by executing it — so it is the widest
    // reaching of the three surfaces, not the narrowest.
    for file in &module.spec.files {
        if file
            .patch
            .as_ref()
            .is_some_and(|patch| patch.script.is_some())
        {
            return Some(format!("a patch script for {}", file.target));
        }
    }
    None
}

/// Load all modules: local modules from disk + remote locked modules +
/// source-delivered bodies (lowest precedence; see [`load_source_modules`]).
pub fn load_all_modules(
    config_dir: &Path,
    cache_base: &Path,
    source_roots: &[SourceModuleRoot],
    printer: &crate::output::Printer,
) -> Result<HashMap<String, LoadedModule>> {
    let mut modules = load_modules(config_dir)?;
    load_locked_modules(config_dir, cache_base, &mut modules, printer)?;
    load_source_modules(source_roots, &mut modules)?;
    Ok(modules)
}

/// Diff two module specs, returning a human-readable summary of changes.
///
/// Each entry carries the [`Role`] the caller renders it with — `Ok` for an
/// addition, `Fail` for a removal, `Warn` for a value change on an existing
/// entry, `Info` for the no-changes sentinel — instead of baking a `+`/`-`/`~`
/// marker into the text: the role's own icon at render time IS the marker, so
/// this stays a single source of add/remove/change signal rather than two
/// (a hand-typed glyph in the string AND a role driving the icon beside it).
///
/// This runs inside `cfgd module upgrade`'s pre-approval security review —
/// the user is deciding whether to let the upgrade run on their machine — so
/// every added/removed entry also spells the word into the text
/// ("dependency added: …" / "dependency removed: …"), matching the
/// postApply-script arm below. `Role::Ok`/`Role::Fail` alone would leave a
/// removed package (the new version simply stopped declaring it, not a
/// failure) reading as "this upgrade failed" at the exact moment the user is
/// approving it; the words remove that ambiguity without inventing a
/// dedicated add/remove role the rest of the theme has no other use for.
pub fn diff_module_specs(
    old: &LoadedModule,
    new: &LoadedModule,
    arrow: &str,
) -> Vec<(Role, String)> {
    let mut changes = Vec::new();

    // Dependencies
    let old_deps: HashSet<&str> = old.spec.depends.iter().map(|s| s.as_str()).collect();
    let new_deps: HashSet<&str> = new.spec.depends.iter().map(|s| s.as_str()).collect();
    for dep in new_deps.difference(&old_deps) {
        changes.push((Role::Ok, format!("dependency added: {dep}")));
    }
    for dep in old_deps.difference(&new_deps) {
        changes.push((Role::Fail, format!("dependency removed: {dep}")));
    }

    // Packages
    let old_pkgs: HashSet<&str> = old.spec.packages.iter().map(|p| p.name.as_str()).collect();
    let new_pkgs: HashSet<&str> = new.spec.packages.iter().map(|p| p.name.as_str()).collect();
    for pkg in new_pkgs.difference(&old_pkgs) {
        changes.push((Role::Ok, format!("package added: {pkg}")));
    }
    for pkg in old_pkgs.difference(&new_pkgs) {
        changes.push((Role::Fail, format!("package removed: {pkg}")));
    }

    // Check for version constraint changes on existing packages
    for new_pkg in &new.spec.packages {
        if let Some(old_pkg) = old.spec.packages.iter().find(|p| p.name == new_pkg.name)
            && old_pkg.min_version != new_pkg.min_version
        {
            changes.push((
                Role::Warn,
                format!(
                    "package '{}': minVersion {} {} {}",
                    new_pkg.name,
                    old_pkg.min_version.as_deref().unwrap_or("(none)"),
                    arrow,
                    new_pkg.min_version.as_deref().unwrap_or("(none)")
                ),
            ));
        }
    }

    // Files
    let old_files: HashSet<&str> = old.spec.files.iter().map(|f| f.target.as_str()).collect();
    let new_files: HashSet<&str> = new.spec.files.iter().map(|f| f.target.as_str()).collect();
    for file in new_files.difference(&old_files) {
        changes.push((Role::Ok, format!("file target added: {file}")));
    }
    for file in old_files.difference(&new_files) {
        changes.push((Role::Fail, format!("file target removed: {file}")));
    }

    // Env vars — an upgrade that introduces one reaches the login shell of
    // every new terminal, so it belongs on the approval surface next to a
    // post-apply script. Iterated in declaration order rather than through a
    // set difference so the approval output is stable between runs.
    let old_env: HashMap<&str, &str> = old
        .spec
        .env
        .iter()
        .map(|e| (e.name.as_str(), e.value.as_str()))
        .collect();
    let new_env: HashMap<&str, &str> = new
        .spec
        .env
        .iter()
        .map(|e| (e.name.as_str(), e.value.as_str()))
        .collect();
    for ev in &new.spec.env {
        match old_env.get(ev.name.as_str()) {
            None => changes.push((Role::Ok, format!("env added: {}={}", ev.name, ev.value))),
            Some(prev) if *prev != ev.value.as_str() => changes.push((
                Role::Warn,
                format!("env '{}': {} {} {}", ev.name, prev, arrow, ev.value),
            )),
            Some(_) => {}
        }
    }
    for ev in &old.spec.env {
        if !new_env.contains_key(ev.name.as_str()) {
            changes.push((Role::Fail, format!("env removed: {}={}", ev.name, ev.value)));
        }
    }

    // Aliases
    let old_aliases: HashMap<&str, &str> = old
        .spec
        .aliases
        .iter()
        .map(|a| (a.name.as_str(), a.command.as_str()))
        .collect();
    let new_aliases: HashMap<&str, &str> = new
        .spec
        .aliases
        .iter()
        .map(|a| (a.name.as_str(), a.command.as_str()))
        .collect();
    for alias in &new.spec.aliases {
        match old_aliases.get(alias.name.as_str()) {
            None => changes.push((
                Role::Ok,
                format!("alias added: {}={}", alias.name, alias.command),
            )),
            Some(prev) if *prev != alias.command.as_str() => changes.push((
                Role::Warn,
                format!(
                    "alias '{}': {} {} {}",
                    alias.name, prev, arrow, alias.command
                ),
            )),
            Some(_) => {}
        }
    }
    for alias in &old.spec.aliases {
        if !new_aliases.contains_key(alias.name.as_str()) {
            changes.push((
                Role::Fail,
                format!("alias removed: {}={}", alias.name, alias.command),
            ));
        }
    }

    // Scripts
    let old_scripts: Vec<&str> = old
        .spec
        .scripts
        .as_ref()
        .map(|s| s.post_apply.iter().map(|e| e.run_str()).collect())
        .unwrap_or_default();
    let new_scripts: Vec<&str> = new
        .spec
        .scripts
        .as_ref()
        .map(|s| s.post_apply.iter().map(|e| e.run_str()).collect())
        .unwrap_or_default();
    let old_script_set: HashSet<&str> = old_scripts.into_iter().collect();
    let new_script_set: HashSet<&str> = new_scripts.into_iter().collect();
    for script in new_script_set.difference(&old_script_set) {
        // This is the pre-approval security review of a module upgrade — the
        // user must see the FULL script body before approving it running on
        // their machine, so push the raw body untouched. Never condense here;
        // the caller (`cmd_module_upgrade` in `cli/module/registry.rs`)
        // decides bullet-vs-code_block rendering based on embedded `\n`. A
        // multi-line script renders as a `code_block`, which carries no
        // per-line Role icon, so "added"/"removed" is spelled out in the
        // label itself rather than relying on a marker the block can't show.
        changes.push((Role::Ok, format!("postApply script added: {script}")));
    }
    for script in old_script_set.difference(&new_script_set) {
        changes.push((Role::Fail, format!("postApply script removed: {script}")));
    }

    if changes.is_empty() {
        changes.push((Role::Info, "(no spec changes)".to_string()));
    }

    changes
}
