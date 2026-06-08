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
        .map(|d| d.join("macos-config-pinned"))
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
}
