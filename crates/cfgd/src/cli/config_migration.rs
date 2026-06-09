//! One-time macOS config-location migration prompt.
//!
//! The native macOS config location is `~/Library/Application Support/cfgd`, but
//! builds that used `~/.config/cfgd` left existing users' config there. Config
//! resolution keeps reading the legacy dir in place (never breaking), and this
//! module offers an interactive, one-time choice on the first TTY run after an
//! upgrade: move the dir to the native location, or keep it and persist
//! `XDG_CONFIG_HOME` so future shells resolve there explicitly.

use std::io::Write;
use std::path::{Path, PathBuf};

use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Printer, Role, collapse_to_subject_line};

/// `export` written to a POSIX (bash/zsh) rc. `$HOME` stays unexpanded so the
/// shell resolves it at source time and the line is portable across machines.
const XDG_EXPORT_LINE: &str = r#"export XDG_CONFIG_HOME="$HOME/.config""#;

/// fish equivalent (fish has its own assignment syntax and never reads POSIX rc
/// files).
const FISH_XDG_LINE: &str = r#"set -gx XDG_CONFIG_HOME "$HOME/.config""#;

/// Substring marking an existing `XDG_CONFIG_HOME` assignment, so we never append
/// a second (possibly conflicting) one regardless of the exact syntax the user
/// already used.
const XDG_VAR_MARKER: &str = "XDG_CONFIG_HOME";

/// Sentinel filename recording that the user chose "keep ~/.config" on the macOS
/// config-location prompt. A state-dir migration sidecar artifact.
const MACOS_CONFIG_PINNED_SENTINEL: &str = "macos-config-pinned";

/// Offer the macOS config-location migration when applicable. Returns the new
/// config directory when the user chose to move (so the caller re-resolves the
/// config path), otherwise `None`.
///
/// No-op (returns `None`) when: not macOS; `XDG_CONFIG_HOME` or an explicit
/// `--config`/`CFGD_CONFIG` already pins the location; the user already chose
/// "keep" on a prior run (sentinel); there is no legacy dir to migrate; or the
/// session is non-interactive (`--yes`/`CFGD_YES`, no TTY, or structured `-o`
/// output) — in which case resolution keeps using the legacy dir in place and
/// the prompt fires on a later interactive run.
pub fn maybe_migrate_macos_config(
    printer: &Printer,
    explicit_config: bool,
    assume_yes: bool,
) -> Option<PathBuf> {
    // Behavior is macOS-only; the function compiles everywhere so it stays
    // type-checked in CI.
    if !cfg!(target_os = "macos") {
        return None;
    }
    if explicit_config || std::env::var_os("XDG_CONFIG_HOME").is_some() || assume_yes {
        return None;
    }
    // A prior "keep" recorded the user's choice; don't re-prompt in shells whose
    // env predates the rc change.
    if pin_sentinel().is_some_and(|m| m.exists()) {
        return None;
    }

    let home = PathBuf::from(std::env::var_os("HOME")?);
    let (legacy, native) = cfgd_core::macos_legacy_config_migration(&home)?;

    let move_opt = format!("Move it to {}", native.posix());
    let keep_opt = "Keep it at ~/.config (set XDG_CONFIG_HOME in your shell config)".to_string();
    let options = vec![move_opt.clone(), keep_opt];
    let message = format!(
        "Your cfgd config is at {}, but the native macOS location is now {}. \
         How would you like to proceed?",
        legacy.posix(),
        native.posix(),
    );

    // `prompt_select` self-rejects non-TTY / structured output; treat any such
    // refusal as "keep using the legacy dir in place this run".
    let choice = match printer.prompt_select(&message, &options) {
        Ok(c) => c.clone(),
        Err(_) => return None,
    };

    if choice == move_opt {
        migrate_move(printer, &legacy, &native)
    } else {
        keep_at_dotconfig(printer, &home);
        None
    }
}

/// Move the legacy config dir to the native location. Returns the native dir on
/// success (so the caller re-resolves), `None` on failure (the legacy dir stays
/// in place and is read as before).
fn migrate_move(printer: &Printer, legacy: &Path, native: &Path) -> Option<PathBuf> {
    match cfgd_core::move_dir(legacy, native) {
        Ok(()) => {
            printer.status_simple(Role::Ok, format!("Moved config to {}", native.posix()));
            Some(native.to_path_buf())
        }
        Err(e) => {
            printer.status_simple(
                Role::Warn,
                format!(
                    "Could not move config ({}); continuing from {}",
                    collapse_to_subject_line(&e),
                    legacy.posix()
                ),
            );
            None
        }
    }
}

/// Keep config at `~/.config`: set `XDG_CONFIG_HOME` for the current process so
/// this run (and the re-prompt gate) resolve there, and persist it for future
/// shells.
fn keep_at_dotconfig(printer: &Printer, home: &Path) {
    let xdg = home.join(".config");
    // SAFETY: runs during single-threaded CLI startup — the only thread/runtime
    // sources (the `mcp` tokio arm and `Printer::run`) are unreachable before
    // this point, so there is no concurrent getenv to race.
    unsafe {
        std::env::set_var("XDG_CONFIG_HOME", &xdg);
    }
    match persist_xdg_pin(home) {
        Ok(Some(rc)) => {
            printer.status_simple(Role::Ok, format!("Set XDG_CONFIG_HOME in {}", rc.posix()))
        }
        Ok(None) => printer.note(
            "Set XDG_CONFIG_HOME for this session; add \
             `export XDG_CONFIG_HOME=\"$HOME/.config\"` to your shell config to persist it.",
        ),
        Err(e) => printer.status_simple(
            Role::Warn,
            format!(
                "Set XDG_CONFIG_HOME for this session, but could not update your shell config: {}",
                collapse_to_subject_line(&e)
            ),
        ),
    }
    // Record the choice so already-open shells (whose env predates the rc write)
    // don't re-prompt on their next cfgd run. Best-effort: never block keep on a
    // sentinel-write failure.
    if let Some(marker) = pin_sentinel() {
        if let Some(parent) = marker.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let _ = cfgd_core::atomic_write_str(&marker, "");
    }
}

/// Sentinel marking that the user chose "keep ~/.config" — lives in the state
/// dir (machine-local, never part of the git-synced config).
fn pin_sentinel() -> Option<PathBuf> {
    cfgd_core::state::default_state_dir()
        .ok()
        .map(|d| d.join(MACOS_CONFIG_PINNED_SENTINEL))
}

/// rc target for persisting `XDG_CONFIG_HOME`, by shell family.
enum XdgRcTarget {
    /// POSIX `export` line into this file (bash/zsh).
    PosixExport(PathBuf),
    /// fish `set -gx` line into this file.
    Fish(PathBuf),
    /// Unrecognized shell — don't guess a file; the caller prints manual steps.
    None,
}

/// Persist `XDG_CONFIG_HOME=~/.config` to the user's shell config. Returns the
/// file it lives in (newly written or already present), or `None` when the shell
/// is unrecognized.
fn persist_xdg_pin(home: &Path) -> std::io::Result<Option<PathBuf>> {
    let shell = std::env::var("SHELL").unwrap_or_default();
    let name = Path::new(&shell).file_name().and_then(|n| n.to_str());
    match xdg_rc_target(home, name) {
        XdgRcTarget::PosixExport(rc) => {
            append_line_once(&rc, XDG_EXPORT_LINE, XDG_VAR_MARKER)?;
            Ok(Some(rc))
        }
        XdgRcTarget::Fish(rc) => {
            append_line_once(&rc, FISH_XDG_LINE, XDG_VAR_MARKER)?;
            Ok(Some(rc))
        }
        XdgRcTarget::None => Ok(None),
    }
}

/// Map a shell name to the file that makes an exported env var visible to **all**
/// of its future invocations (not just interactive ones):
/// - zsh → `~/.zshenv` (sourced by every zsh: interactive, login, scripts);
/// - bash → `~/.profile` (the shared login file; never create `.bash_profile`,
///   which would shadow `.profile`);
/// - fish → `~/.config/fish/conf.d/cfgd-xdg.fish`.
///
/// Mirrors the login/all-scope targeting in `reconciler::env_engine`; a direct
/// export is written rather than routing through `~/.cfgd.env` (which the env
/// engine regenerates wholesale from `spec.env`, and would clobber here).
fn xdg_rc_target(home: &Path, shell_name: Option<&str>) -> XdgRcTarget {
    match shell_name {
        Some(s) if s.contains("zsh") => XdgRcTarget::PosixExport(home.join(".zshenv")),
        Some(s) if s.contains("bash") => XdgRcTarget::PosixExport(home.join(".profile")),
        Some(s) if s.contains("fish") => {
            XdgRcTarget::Fish(home.join(".config/fish/conf.d/cfgd-xdg.fish"))
        }
        _ => XdgRcTarget::None,
    }
}

/// Append `line` to `path` unless a non-comment line already assigns `var_marker`
/// (any syntax). Follows a symlinked rc (e.g. `~/.zshenv` → a dotfiles repo) so
/// the target file is updated in place rather than the link being replaced with
/// a regular file. Creates the file (and parent dirs) when absent.
fn append_line_once(path: &Path, line: &str, var_marker: &str) -> std::io::Result<()> {
    // Resolve through a symlink to the real file; fall back to the original path
    // when it doesn't exist yet (first create) or can't be resolved.
    let real = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
    let existing = std::fs::read_to_string(&real).unwrap_or_default();
    let already = existing.lines().any(|l| {
        let t = l.trim_start();
        !t.starts_with('#') && t.contains(var_marker)
    });
    if already {
        return Ok(());
    }
    if let Some(parent) = real.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut f = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&real)?;
    if !existing.is_empty() && !existing.ends_with('\n') {
        f.write_all(b"\n")?;
    }
    writeln!(f, "{line}")?;
    Ok(())
}

/// Silently migrate a legacy combined data dir (state DB + `sources/` cache) to
/// the split state and cache roots. A no-op when no home is resolvable or the
/// new roots can't be resolved. Runs on every startup (idempotent); the heavy
/// lifting and all output live in [`migrate_legacy_data_dirs_at`].
pub fn migrate_legacy_data_dirs(printer: &Printer) {
    let Some(legacy) = cfgd_core::legacy_data_dir() else {
        return;
    };
    let Ok(new_state) = cfgd_core::state::default_state_dir() else {
        return;
    };
    let Ok(new_sources) = cfgd_core::default_cache_dir().map(|c| c.join("sources")) else {
        return;
    };
    migrate_legacy_data_dirs_at(printer, &legacy, &new_state, &new_sources);
}

/// Per-artifact migration of the allowlisted state files and the `sources/`
/// cache from `legacy` into `new_state` / `new_sources`.
///
/// Only the named state artifacts and the `sources/` subdir are moved — never a
/// whole-dir move — because the legacy data dir is shared (it also holds config
/// and modules on macOS). Every step is best-effort and never clobbers an
/// existing destination, so a partial earlier run resumes cleanly.
fn migrate_legacy_data_dirs_at(
    printer: &Printer,
    legacy: &Path,
    new_state: &Path,
    new_sources: &Path,
) {
    match cfgd_core::state::migrate_state_db(legacy, new_state) {
        Ok(true) => printer.status_simple(
            Role::Info,
            format!("Migrated state database to {}", new_state.posix()),
        ),
        Ok(false) => {}
        Err(e) => printer.status_simple(
            Role::Warn,
            format!(
                "Could not migrate state database ({}); continuing",
                collapse_to_subject_line(&e)
            ),
        ),
    }

    for name in [
        cfgd_core::state::PENDING_CONFIG_FILENAME,
        cfgd_core::server_client::DEVICE_CREDENTIAL_FILENAME,
        MACOS_CONFIG_PINNED_SENTINEL,
    ] {
        migrate_state_file(printer, legacy, new_state, name);
    }
    // The device credential is sensitive: lock the destination dir to owner-only
    // once it has landed there (defense in depth; no-op on Windows).
    if new_state
        .join(cfgd_core::server_client::DEVICE_CREDENTIAL_FILENAME)
        .exists()
    {
        let _ = cfgd_core::set_file_permissions(new_state, 0o700);
    }

    let legacy_sources = legacy.join("sources");
    if legacy_sources.is_dir() && !new_sources.exists() {
        match cfgd_core::move_dir(&legacy_sources, new_sources) {
            Ok(()) => printer.status_simple(
                Role::Info,
                format!("Migrated sources cache to {}", new_sources.posix()),
            ),
            Err(e) => printer.status_simple(
                Role::Warn,
                format!(
                    "Could not migrate sources cache ({}); continuing",
                    collapse_to_subject_line(&e)
                ),
            ),
        }
    }

    // Non-recursive: succeeds only when the legacy dir is now empty (the Linux
    // case where it held only state + sources). On macOS/Windows it still holds
    // config/modules, so this fails and is ignored — never a recursive delete.
    let _ = std::fs::remove_dir(legacy);
}

/// Move a single allowlisted state file from `legacy` to `new_state` when it is
/// present at the source and absent at the destination. Best-effort: a failure
/// warns and continues rather than aborting the whole migration.
fn migrate_state_file(printer: &Printer, legacy: &Path, new_state: &Path, name: &str) {
    let src = legacy.join(name);
    let dst = new_state.join(name);
    if !src.exists() || dst.exists() {
        return;
    }
    if let Err(e) = std::fs::create_dir_all(new_state) {
        printer.status_simple(
            Role::Warn,
            format!(
                "Could not migrate {name} ({}); continuing",
                collapse_to_subject_line(&e)
            ),
        );
        return;
    }
    match cfgd_core::move_file(&src, &dst) {
        Ok(()) => printer.status_simple(Role::Info, format!("Migrated {name} to {}", dst.posix())),
        Err(e) => printer.status_simple(
            Role::Warn,
            format!(
                "Could not migrate {name} ({}); continuing",
                collapse_to_subject_line(&e)
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn xdg_rc_target_picks_all_context_file_per_shell() {
        let home = Path::new("/home/u");
        assert!(matches!(
            xdg_rc_target(home, Some("zsh")),
            XdgRcTarget::PosixExport(p) if p == home.join(".zshenv")
        ));
        assert!(matches!(
            xdg_rc_target(home, Some("bash")),
            XdgRcTarget::PosixExport(p) if p == home.join(".profile")
        ));
        assert!(matches!(
            xdg_rc_target(home, Some("fish")),
            XdgRcTarget::Fish(p) if p == home.join(".config/fish/conf.d/cfgd-xdg.fish")
        ));
        // Unknown / missing shell → no guess.
        assert!(matches!(xdg_rc_target(home, Some("nu")), XdgRcTarget::None));
        assert!(matches!(xdg_rc_target(home, None), XdgRcTarget::None));
    }

    #[test]
    fn append_line_once_creates_then_dedups() {
        let tmp = tempfile::tempdir().unwrap();
        let rc = tmp.path().join(".zshenv");
        append_line_once(&rc, XDG_EXPORT_LINE, XDG_VAR_MARKER).unwrap();
        // Second call is a no-op — assignment already present.
        append_line_once(&rc, XDG_EXPORT_LINE, XDG_VAR_MARKER).unwrap();
        let body = std::fs::read_to_string(&rc).unwrap();
        assert_eq!(body.matches("XDG_CONFIG_HOME").count(), 1);
        assert!(body.ends_with('\n'));
    }

    #[test]
    fn append_line_once_dedups_on_any_assignment_syntax() {
        let tmp = tempfile::tempdir().unwrap();
        let rc = tmp.path().join(".zshenv");
        // User already has a differently-quoted assignment; don't add a second.
        std::fs::write(&rc, "export XDG_CONFIG_HOME=$HOME/.config\n").unwrap();
        append_line_once(&rc, XDG_EXPORT_LINE, XDG_VAR_MARKER).unwrap();
        let body = std::fs::read_to_string(&rc).unwrap();
        assert_eq!(body.matches("XDG_CONFIG_HOME").count(), 1);
    }

    #[test]
    fn append_line_once_ignores_commented_assignment() {
        let tmp = tempfile::tempdir().unwrap();
        let rc = tmp.path().join(".zshenv");
        std::fs::write(&rc, "# export XDG_CONFIG_HOME=/somewhere\n").unwrap();
        append_line_once(&rc, XDG_EXPORT_LINE, XDG_VAR_MARKER).unwrap();
        let body = std::fs::read_to_string(&rc).unwrap();
        // The commented line doesn't count — a real export is added.
        assert!(body.contains(XDG_EXPORT_LINE));
    }

    #[test]
    fn append_line_once_preserves_existing_and_adds_newline() {
        let tmp = tempfile::tempdir().unwrap();
        let rc = tmp.path().join(".profile");
        std::fs::write(&rc, "alias ll='ls -la'").unwrap(); // no trailing newline
        append_line_once(&rc, XDG_EXPORT_LINE, XDG_VAR_MARKER).unwrap();
        let body = std::fs::read_to_string(&rc).unwrap();
        assert_eq!(body, format!("alias ll='ls -la'\n{XDG_EXPORT_LINE}\n"));
    }

    #[cfg(unix)]
    #[test]
    fn append_line_once_follows_symlinked_rc_without_replacing_link() {
        // ~/.zshenv is often a symlink into a dotfiles repo; appending must edit
        // the real file and leave the symlink intact.
        let tmp = tempfile::tempdir().unwrap();
        let real = tmp.path().join("dotfiles").join("zshenv");
        std::fs::create_dir_all(real.parent().unwrap()).unwrap();
        std::fs::write(&real, "# dotfiles zshenv\n").unwrap();
        let link = tmp.path().join(".zshenv");
        std::os::unix::fs::symlink(&real, &link).unwrap();

        append_line_once(&link, XDG_EXPORT_LINE, XDG_VAR_MARKER).unwrap();

        // The link is still a symlink, and the export landed in the real file.
        assert!(std::fs::symlink_metadata(&link).unwrap().is_symlink());
        assert!(
            std::fs::read_to_string(&real)
                .unwrap()
                .contains(XDG_EXPORT_LINE)
        );
    }

    // --- migrate_legacy_data_dirs_at ---

    use cfgd_core::output::{Printer, Verbosity};
    use cfgd_core::state::StateStore;

    /// Seed a legacy data dir with a schema-bearing state DB and the allowlisted
    /// sidecar artifacts plus a `sources/foo/bar` cache tree.
    fn seed_legacy(legacy: &Path) {
        {
            let store = StateStore::open_in_dir(legacy).unwrap();
            store
                .record_apply("default", "h", cfgd_core::state::ApplyStatus::Success, None)
                .unwrap();
        }
        std::fs::write(
            legacy.join(cfgd_core::server_client::DEVICE_CREDENTIAL_FILENAME),
            b"{\"id\":\"x\"}",
        )
        .unwrap();
        std::fs::write(
            legacy.join(cfgd_core::state::PENDING_CONFIG_FILENAME),
            b"{}",
        )
        .unwrap();
        let nested = legacy.join("sources").join("foo");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(nested.join("bar"), b"clone").unwrap();
    }

    #[test]
    fn migrates_all_allowlisted_artifacts() {
        let legacy_t = tempfile::tempdir().unwrap();
        let roots = tempfile::tempdir().unwrap();
        let legacy = legacy_t.path();
        let new_state = roots.path().join("state");
        let new_sources = roots.path().join("cache").join("sources");
        seed_legacy(legacy);

        // Normal verbosity so the success Info lines render and can be asserted.
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        migrate_legacy_data_dirs_at(&printer, legacy, &new_state, &new_sources);

        // State DB reopens at the new location.
        let store = StateStore::open_in_dir(&new_state).unwrap();
        assert!(store.last_apply().unwrap().is_some());
        assert!(
            new_state
                .join(cfgd_core::server_client::DEVICE_CREDENTIAL_FILENAME)
                .exists()
        );
        assert!(
            new_state
                .join(cfgd_core::state::PENDING_CONFIG_FILENAME)
                .exists()
        );
        assert!(new_sources.join("foo").join("bar").exists());

        // Originals moved away.
        assert!(
            !legacy
                .join(cfgd_core::server_client::DEVICE_CREDENTIAL_FILENAME)
                .exists()
        );
        assert!(
            !legacy
                .join(cfgd_core::state::PENDING_CONFIG_FILENAME)
                .exists()
        );
        assert!(!legacy.join("sources").exists());
        assert!(!legacy.join(cfgd_core::state::STATE_DB_FILENAME).exists());

        // The credential is sensitive: its destination dir is locked to 0700.
        #[cfg(unix)]
        {
            let meta = std::fs::metadata(&new_state).unwrap();
            assert_eq!(
                cfgd_core::file_permissions_mode(&meta),
                Some(0o700),
                "new state dir must be 0700 after migrating the device credential"
            );
        }

        // The success line reads as a full sentence ending in the new state path,
        // not merely a substring — the human-facing status shape is load-bearing.
        let out = buf.lock().unwrap().clone();
        let expected = format!("Migrated state database to {}", new_state.posix());
        let matched = out.lines().any(|l| l.trim_end().ends_with(&expected));
        assert!(
            matched,
            "expected a status line ending in {expected:?}, got:\n{out}"
        );
    }

    #[test]
    fn is_idempotent_on_second_run() {
        let legacy_t = tempfile::tempdir().unwrap();
        let roots = tempfile::tempdir().unwrap();
        let legacy = legacy_t.path();
        let new_state = roots.path().join("state");
        let new_sources = roots.path().join("cache").join("sources");
        seed_legacy(legacy);

        let (printer, _buf) = Printer::for_test_at(Verbosity::Quiet);
        migrate_legacy_data_dirs_at(&printer, legacy, &new_state, &new_sources);
        // Second run must not panic and must leave everything in place.
        migrate_legacy_data_dirs_at(&printer, legacy, &new_state, &new_sources);

        assert!(new_state.join(cfgd_core::state::STATE_DB_FILENAME).exists());
        assert!(new_sources.join("foo").join("bar").exists());
        let store = StateStore::open_in_dir(&new_state).unwrap();
        assert!(store.last_apply().unwrap().is_some());
    }

    #[test]
    fn never_clobbers_existing_new_state_db() {
        let legacy_t = tempfile::tempdir().unwrap();
        let roots = tempfile::tempdir().unwrap();
        let legacy = legacy_t.path();
        let new_state = roots.path().join("state");
        let new_sources = roots.path().join("cache").join("sources");
        std::fs::create_dir_all(&new_state).unwrap();
        let new_db = new_state.join(cfgd_core::state::STATE_DB_FILENAME);
        std::fs::write(&new_db, b"KEEP-ME").unwrap();
        seed_legacy(legacy);

        let (printer, _buf) = Printer::for_test_at(Verbosity::Quiet);
        migrate_legacy_data_dirs_at(&printer, legacy, &new_state, &new_sources);

        assert_eq!(std::fs::read(&new_db).unwrap(), b"KEEP-ME");
        assert!(
            legacy.join(cfgd_core::state::STATE_DB_FILENAME).exists(),
            "legacy DB stays put when the new DB already exists"
        );
    }

    #[test]
    fn never_sweeps_non_allowlisted_config_file() {
        let legacy_t = tempfile::tempdir().unwrap();
        let roots = tempfile::tempdir().unwrap();
        let legacy = legacy_t.path();
        let new_state = roots.path().join("state");
        let new_sources = roots.path().join("cache").join("sources");
        seed_legacy(legacy);
        // A config file shares the legacy dir on macOS — it must never move.
        let config = legacy.join("cfgd.yaml");
        std::fs::write(&config, b"apiVersion: cfgd.io/v1alpha1").unwrap();

        let (printer, _buf) = Printer::for_test_at(Verbosity::Quiet);
        migrate_legacy_data_dirs_at(&printer, legacy, &new_state, &new_sources);

        assert!(config.exists(), "config file must remain at legacy");
        assert!(!new_state.join("cfgd.yaml").exists());
    }
}
