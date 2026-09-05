use std::collections::HashMap;

use crate::PathDisplayExt;

pub(super) const ENV_FILE_HEADER: &str = "# managed by cfgd \u{2014} do not edit";

/// Detect whether fish shell is in use by the current user.
///
/// On Unix, `$SHELL` is the canonical signal — it points at the user's login
/// shell. On Windows, `$SHELL` is not a Windows convention (and is rarely set
/// even when a Unix-style fish lives at PATH via Cygwin / MSYS2 / Scoop), so
/// fall back to `command_available` so Windows fish users still get a managed
/// fish env file generated.
pub(super) fn fish_in_use() -> bool {
    if cfg!(windows) {
        crate::command_available("fish")
    } else {
        shell_var_indicates_fish(std::env::var("SHELL").ok().as_deref())
    }
}

/// Pure inner of the Unix branch of `fish_in_use` — reads the `$SHELL` value
/// and returns whether it names fish. Split out so tests can exercise the
/// branching without mutating process-wide environment state (`set_var` is
/// `unsafe` in the 2024 edition and racy across parallel tests).
pub(super) fn shell_var_indicates_fish(shell: Option<&str>) -> bool {
    shell.unwrap_or("").contains("fish")
}

/// Generate bash/zsh env file content from merged env vars, aliases, and the
/// PATH directories contributed by bootstrappable package managers.
pub(super) fn generate_env_file_content(
    env: &[crate::config::EnvVar],
    aliases: &[crate::config::ShellAlias],
    path: Option<&super::env_engine::FoldedPath>,
    origins: &super::env_engine::EnvOrigins,
) -> String {
    let mut lines = vec![ENV_FILE_HEADER.to_string()];
    if let Some(path) = path {
        // Ahead of the user's own exports so a `spec.env` value may reference a
        // binary that only exists on the bootstrapped manager's PATH.
        lines.push(format!(
            "export PATH=\"{}\"{}",
            path.value(crate::escape_double_quoted, "$PATH", ":"),
            path.comment
        ));
    }
    for ev in env {
        if crate::validate_env_var_name(&ev.name).is_err() {
            // tracing-ok: an env var the user declared under a name no shell can carry; the generated file simply omits it and no row names it
            tracing::warn!("skipping env var with unsafe name: {}", ev.name);
            continue;
        }
        // `PATH` is written once, by the fold above, whichever producers fed it.
        if ev.name == "PATH" {
            continue;
        }
        lines.push(format!(
            "export {}={}{}",
            ev.name,
            crate::posix_double_quoted(&crate::expand_env_value_tilde(&ev.value)),
            origins.env_comment(&ev.name)
        ));
    }
    for alias in aliases {
        if crate::validate_alias_name(&alias.name).is_err() {
            // tracing-ok: an alias the user declared under a name no shell can carry; same omission
            tracing::warn!("skipping alias with unsafe name: {}", alias.name);
            continue;
        }
        // The body is quoted, not interpolated: a `$(…)` in the command becomes
        // part of the alias and runs when the user invokes it, instead of
        // running once while the login shell is still sourcing this file.
        lines.push(format!(
            "alias {}={}{}",
            alias.name,
            crate::posix_double_quoted(&alias.command),
            origins.alias_comment(&alias.name)
        ));
    }
    lines.push(String::new()); // trailing newline
    lines.join("\n")
}

/// Generate fish env file content from merged env vars, aliases, and the PATH
/// directories contributed by bootstrappable package managers.
pub(super) fn generate_fish_env_content(
    env: &[crate::config::EnvVar],
    aliases: &[crate::config::ShellAlias],
    path: Option<&super::env_engine::FoldedPath>,
    origins: &super::env_engine::EnvOrigins,
) -> String {
    let mut lines = vec![ENV_FILE_HEADER.to_string()];
    if let Some(path) = path {
        // Fish uses a space-separated list for PATH, not colon-separated, and
        // a bare `$PATH` splices its existing list variable in place; single
        // quotes suppress fish expansion of each entry, which is why the fold
        // spelled every directory literally.
        lines.push(format!(
            "set -gx PATH {}{}",
            path.value(crate::fish_single_quoted, "$PATH", " "),
            path.comment
        ));
    }
    for ev in env {
        if crate::validate_env_var_name(&ev.name).is_err() {
            // tracing-ok: an env var the user declared under a name no shell can carry; the generated file simply omits it and no row names it
            tracing::warn!("skipping env var with unsafe name: {}", ev.name);
            continue;
        }
        // `PATH` is written once, by the fold above, whichever producers fed it.
        if ev.name == "PATH" {
            continue;
        }
        // Expand a leading/`:`-prefixed `~` to home before single-quoting:
        // fish single quotes suppress tilde expansion, so a literal `~` would
        // break the path. (`$VAR` in a fish single-quoted value is a separate
        // gap.)
        let value = crate::expand_env_value_tilde(&ev.value);
        // Single-quote to prevent fish command substitution via ()
        lines.push(format!(
            "set -gx {} {}{}",
            ev.name,
            crate::fish_single_quoted(&value),
            origins.env_comment(&ev.name)
        ));
    }
    for alias in aliases {
        if crate::validate_alias_name(&alias.name).is_err() {
            // tracing-ok: an alias the user declared under a name no shell can carry; same omission
            tracing::warn!("skipping alias with unsafe name: {}", alias.name);
            continue;
        }
        lines.push(format!(
            "abbr -a {} {}{}",
            alias.name,
            crate::fish_single_quoted(&alias.command),
            origins.alias_comment(&alias.name)
        ));
    }
    lines.push(String::new());
    lines.join("\n")
}

/// Generate PowerShell env file content from merged env vars, aliases, and the
/// PATH directories contributed by bootstrappable package managers.
pub(super) fn generate_powershell_env_content(
    env: &[crate::config::EnvVar],
    aliases: &[crate::config::ShellAlias],
    path: Option<&super::env_engine::FoldedPath>,
    origins: &super::env_engine::EnvOrigins,
) -> String {
    let mut lines = vec![ENV_FILE_HEADER.to_string()];
    if let Some(path) = path {
        // Double-quoted so `$env:PATH` and `$HOME` interpolate; `;` is the
        // Windows PATH separator. Backtick is PowerShell's escape character
        // inside "".
        lines.push(format!(
            "$env:PATH = \"{}\"{}",
            path.value(crate::escape_powershell_double_quoted, "$env:PATH", ";"),
            path.comment
        ));
    }
    for ev in env {
        if crate::validate_env_var_name(&ev.name).is_err() {
            // tracing-ok: an env var the user declared under a name no shell can carry; the generated file simply omits it and no row names it
            tracing::warn!("skipping env var with unsafe name: {}", ev.name);
            continue;
        }
        // `PATH` is written once, by the fold above, whichever producers fed it.
        if ev.name == "PATH" {
            continue;
        }
        // Expand a leading/`:`-prefixed `~` to home before quoting (PowerShell
        // does not perform Unix tilde expansion on env values).
        let value = crate::expand_env_value_tilde(&ev.value);
        if value.contains("$env:") {
            // Value references other env vars — double-quote so those
            // references still resolve, with subexpressions neutralized.
            lines.push(format!(
                "$env:{} = {}{}",
                ev.name,
                crate::powershell_double_quoted(&value),
                origins.env_comment(&ev.name)
            ));
        } else {
            // Single-quote prevents all PS interpolation
            lines.push(format!(
                "$env:{} = {}{}",
                ev.name,
                crate::powershell_single_quoted(&value),
                origins.env_comment(&ev.name)
            ));
        }
    }
    for alias in aliases {
        if crate::validate_alias_name(&alias.name).is_err() {
            // tracing-ok: an alias the user declared under a name no shell can carry; same omission
            tracing::warn!("skipping alias with unsafe name: {}", alias.name);
            continue;
        }
        if alias.command.split_whitespace().count() == 1 {
            // Simple alias — use Set-Alias
            lines.push(format!(
                "Set-Alias -Name {} -Value {}{}",
                alias.name,
                crate::powershell_single_quoted(&alias.command),
                origins.alias_comment(&alias.name)
            ));
        } else {
            // Complex alias — a function wrapper. The command is carried as a
            // quoted string and turned into a script block at CALL time: pasted
            // into the braces directly, a `}` in the command closes the
            // function early and everything after it runs while the profile is
            // still loading.
            lines.push(format!(
                "function {} {{ & ([scriptblock]::Create({})) @args }}{}",
                alias.name,
                crate::powershell_single_quoted(&format!("{} @args", alias.command)),
                origins.alias_comment(&alias.name)
            ));
        }
    }
    lines.push(String::new()); // trailing newline
    lines.join("\n")
}

/// The one line a single env var renders as in cfgd's PRIMARY managed env
/// file for `platform` — bash/zsh syntax on Unix, PowerShell on Windows, the
/// dialect of the first `EnvTarget::ManagedFile` `env_targets` always
/// produces when there is anything to write. Built by calling the same
/// generator that writes the real file with a one-item slice, so a verify
/// pass can attribute a content mismatch to the declared item that caused it
/// without re-deriving that dialect's quoting rules. `None` when the name
/// fails the generator's own safety check, matching what a real write
/// silently skips.
///
/// `path` is the file's folded `PATH` assignment, which the caller must supply
/// for `PATH` itself: that one variable's line is written by the fold and not
/// by the declaration loop, so rendering it without the fold would produce a
/// line the real file never holds — or, for a declaration alone, no line at
/// all.
pub(super) fn primary_env_var_line(
    ev: &crate::config::EnvVar,
    platform: super::env_engine::EnvPlatform,
    origins: &super::env_engine::EnvOrigins,
    path: Option<&super::env_engine::FoldedPath>,
) -> Option<String> {
    let one = std::slice::from_ref(ev);
    let path = if ev.name == "PATH" { path } else { None };
    let generated = if platform == super::env_engine::EnvPlatform::Windows {
        generate_powershell_env_content(one, &[], path, origins)
    } else {
        generate_env_file_content(one, &[], path, origins)
    };
    generated
        .lines()
        .nth(1)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
}

/// The alias counterpart of `primary_env_var_line`.
pub(super) fn primary_alias_line(
    alias: &crate::config::ShellAlias,
    platform: super::env_engine::EnvPlatform,
    origins: &super::env_engine::EnvOrigins,
) -> Option<String> {
    let one = std::slice::from_ref(alias);
    let generated = if platform == super::env_engine::EnvPlatform::Windows {
        generate_powershell_env_content(&[], one, None, origins)
    } else {
        generate_env_file_content(&[], one, None, origins)
    };
    generated
        .lines()
        .nth(1)
        .filter(|l| !l.is_empty())
        .map(str::to_string)
}

/// The shared derivation behind the `*_line_prefix` helpers below: the common
/// prefix of two renders of one entry whose values differ at their FIRST
/// character, which is exactly the rendered line up to where the value
/// starts. A trailing quote is stripped because PowerShell picks its quote
/// per value (`'` normally, `"` for a `$env:`-referencing one), so the
/// quote-free prefix claims a line rendered under either. `None` when either
/// render refused the name, or when nothing stable precedes the value.
fn stable_line_prefix(a: Option<String>, b: Option<String>) -> Option<String> {
    let (a, b) = (a?, b?);
    let mut n = a.bytes().zip(b.bytes()).take_while(|(x, y)| x == y).count();
    while n > 0 && !a.is_char_boundary(n) {
        n -= 1;
    }
    let prefix = a[..n]
        .strip_suffix(|c| c == '\'' || c == '"')
        .unwrap_or(&a[..n]);
    (!prefix.is_empty()).then(|| prefix.to_string())
}

/// The dialect-rendered prefix of the line env var `name` renders as in the
/// primary managed file, up to (not including) its value — `export FOO=` /
/// `$env:FOO = `. Built by rendering the real generator twice with sentinel
/// values rather than restating its format string, so the two cannot drift.
/// A deployed line starting with this prefix is accounted for by the CURRENT
/// declaration of `name` (a value change, not a deletion).
pub(super) fn env_var_line_prefix(
    name: &str,
    platform: super::env_engine::EnvPlatform,
) -> Option<String> {
    // `PATH` has ONE line however many producers fed it, so it has one prefix
    // too — the fold's, not a declaration's.
    if name == "PATH" {
        return path_dirs_line_prefix(platform);
    }
    let line = |value: &str| {
        primary_env_var_line(
            &crate::config::EnvVar {
                name: name.to_string(),
                value: value.to_string(),
                platforms: vec![],
            },
            platform,
            // A prefix ends where the two sentinel values first differ, which
            // is before any trailing provenance comment either render carries.
            &Default::default(),
            None,
        )
    };
    stable_line_prefix(line("0cfgdsentinel"), line("1cfgdsentinel"))
}

/// The alias counterpart of [`env_var_line_prefix`]. Plural because
/// PowerShell renders a one-word command as `Set-Alias` and a multi-word one
/// as a function wrapper — two line shapes for one declared name, both of
/// which must claim a deployed line however the OLD value was shaped.
pub(super) fn alias_line_prefixes(
    name: &str,
    platform: super::env_engine::EnvPlatform,
) -> Vec<String> {
    let line = |command: &str| {
        primary_alias_line(
            &crate::config::ShellAlias {
                name: name.to_string(),
                command: command.to_string(),
                platforms: vec![],
            },
            platform,
            &Default::default(),
        )
    };
    let mut prefixes: Vec<String> = [
        ("0cfgdsentinel", "1cfgdsentinel"),
        ("0cfgd sentinel", "1cfgd sentinel"),
    ]
    .into_iter()
    .filter_map(|(a, b)| stable_line_prefix(line(a), line(b)))
    .collect();
    prefixes.dedup();
    prefixes
}

/// The prefix of the generator's own PATH scaffolding line (each dialect's
/// own assignment syntax), so a PATH line rendered from a PAST run's bootstrapped
/// directories is claimed as cfgd's own scaffolding rather than read as some
/// layer's deleted entry.
pub(super) fn path_dirs_line_prefix(platform: super::env_engine::EnvPlatform) -> Option<String> {
    let line = |dir: &str| {
        // Nothing is named: the prefix ends where the two sentinel DIRS first
        // differ, which is before any trailing provenance comment.
        let fold = super::env_engine::FoldedPath::literal([dir.to_string()]);
        let content = if platform == super::env_engine::EnvPlatform::Windows {
            generate_powershell_env_content(&[], &[], Some(&fold), &Default::default())
        } else {
            generate_env_file_content(&[], &[], Some(&fold), &Default::default())
        };
        content
            .lines()
            .nth(1)
            .filter(|l| !l.is_empty())
            .map(str::to_string)
    };
    stable_line_prefix(line("0cfgdsentinel"), line("1cfgdsentinel"))
}

/// Read a cfgd-generated env file for comparison against the content about to
/// be written. `None` means "no usable comparison" and the file is regenerated.
///
/// The counterpart of [`read_rc_baseline`], and deliberately the opposite
/// policy, because the target is a file cfgd authors in full: `~/.cfgd.env`,
/// the fish `conf.d` snippet, `environment.d/cfgd.conf`, the PowerShell
/// profile fragment, the LaunchAgent plist. There is nothing of the user's to
/// preserve and no merge to corrupt, so an unreadable or non-UTF-8 file is a
/// damaged artifact rather than a reason to refuse: refusing would wedge every
/// future apply on a single stray byte, with no command that repairs it. The
/// pre-apply backup captures the damaged bytes before the write replaces them.
pub(super) fn read_managed_baseline(path: &std::path::Path) -> Option<String> {
    match std::fs::read(path) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => Some(text),
            Err(_) => {
                // tracing-ok: the file cfgd itself wrote is damaged; the regeneration below is the recovery and prints nothing
                tracing::warn!(
                    path = %path.posix(),
                    "cfgd-generated env file is not valid UTF-8; regenerating it",
                );
                None
            }
        },
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => {
            // tracing-ok: same, for an unreadable baseline
            tracing::warn!(
                path = %path.posix(),
                error = %e,
                "cannot read cfgd-generated env file to compare; regenerating it",
            );
            None
        }
    }
}

/// Read an rc file as the baseline a source-line merge is applied to.
///
/// Absent is the one benign failure — the file has not been created yet, and an
/// empty baseline is the truthful starting point. Every other failure
/// (`EACCES` after an elevated run left the file root-owned, `EIO`, a latin-1
/// file that is not valid UTF-8) is fatal: an empty baseline there would make
/// the merge below rewrite the user's whole rc file down to cfgd's one line.
///
/// Strictly for a file the USER owns. A file cfgd generates in full takes
/// [`read_managed_baseline`], whose failures are recoverable by regenerating.
pub(super) fn read_rc_baseline(path: &std::path::Path) -> crate::errors::Result<String> {
    let bytes = match std::fs::read(path) {
        Ok(bytes) => bytes,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(String::new()),
        Err(source) => {
            return Err(crate::errors::FileError::Io {
                path: path.to_path_buf(),
                source,
            }
            .into());
        }
    };
    String::from_utf8(bytes).map_err(|_| {
        crate::errors::FileError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "file is not valid UTF-8; cfgd will not rewrite it — re-encode it as UTF-8, \
                 or move the cfgd source line to a file cfgd can read",
            ),
        }
        .into()
    })
}

/// Reject a write whose baseline says "empty" while the file on disk holds
/// bytes, and a write to a file whose permissions say "do not touch me".
///
/// The first is unreachable while [`read_rc_baseline`] is the only baseline
/// source, and stays here so a future read path that degrades to an empty
/// string truncates nothing. The second honors a read-only rc as the refusal
/// the user meant it as, rather than silently replacing it — a rename lands
/// regardless of the write bit.
///
/// Only the user-owned rc files get this guard. A cfgd-generated env file is
/// rewritten whether or not the user has `chmod -w`'d it, because a stale one
/// keeps exporting values they already deleted from their config, and the
/// deletion would never take effect. The lever over that file's content is the
/// config, not its mode bits.
pub(super) fn guard_rc_write(path: &std::path::Path, baseline: &str) -> crate::errors::Result<()> {
    let Ok(meta) = std::fs::metadata(path) else {
        return Ok(());
    };
    if baseline.is_empty() && meta.len() > 0 {
        return Err(crate::errors::FileError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                format!(
                    "refusing to replace {} bytes of existing content with a cfgd source line \
                     derived from an empty baseline",
                    meta.len()
                ),
            ),
        }
        .into());
    }
    if meta.permissions().readonly() {
        return Err(crate::errors::FileError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(
                std::io::ErrorKind::PermissionDenied,
                "file is read-only; cfgd will not modify it — make it writable to let cfgd \
                 inject its source line",
            ),
        }
        .into());
    }
    // Real-access probe, the same shape `probe_dir_writable` uses for
    // directories: mode bits answer the wrong question. A root-owned 0644 rc
    // file in a directory the user can write is not `readonly()`, and the user
    // still cannot write it — but the rename would succeed anyway, because a
    // rename consults only the directory, and the file would silently change
    // owner. Opening for write (no truncate, so the file is not modified) asks
    // the kernel instead, and so honors uid, ACLs, and read-only mounts.
    if let Err(source) = std::fs::OpenOptions::new().write(true).open(path) {
        return Err(crate::errors::FileError::Io {
            path: path.to_path_buf(),
            source: std::io::Error::new(
                source.kind(),
                format!(
                    "cannot open the file for writing ({source}); cfgd will not replace a file \
                     it cannot write in place, because the rename would succeed and silently \
                     change the file's owner"
                ),
            ),
        }
        .into());
    }
    Ok(())
}

/// Whether `line` is cfgd's own loader for the managed env file `marker`.
///
/// Containing the marker is not enough. A user's note about the file, and a
/// loader they commented out on purpose, both mention it and must survive
/// untouched — deleting the disabled form and appending a live one turns a
/// deliberate opt-out back on. So a match additionally requires an
/// uncommented line that begins with a loader construct: the `[ -f … ]` guard
/// cfgd writes today, or the bare `.`/`source`/`test` forms an older cfgd (or
/// the user's own hand) may have left.
///
/// The match is per line, with no shell parse behind it, so a loader form
/// sitting inside a heredoc or a quoted string reads as the real thing. It
/// cannot corrupt the file — a match is rewritten where it sits — but if the
/// quoted copy is already byte-identical to the desired line, the merge reports
/// the rc as converged and no live loader is ever injected. Recognizing that
/// needs a shell parser; the alternative, dropping the loader-construct
/// requirement, re-opens the far likelier failure of eating a deliberately
/// commented-out loader.
fn is_managed_loader(line: &str, marker: &str) -> bool {
    let trimmed = line.trim();
    if trimmed.starts_with('#') || !trimmed.contains(marker) {
        return false;
    }
    trimmed.starts_with('[')
        || trimmed.starts_with(". ")
        || trimmed.starts_with("source ")
        || trimmed.starts_with("test ")
        || trimmed.starts_with("if ")
}

/// Ensure the cfgd source `line` is present in an rc file's `existing` content
/// exactly once, upgrading any stale variant of cfgd's own source line in place.
/// Returns `None` when the file already holds exactly the desired line and no
/// stale variant (nothing to write); `Some(new_content)` otherwise.
///
/// Keyed on the managed-file marker (`.cfgd.env` / `.cfgd-env.ps1`) rather than
/// an exact-string match so that changing the loader keyword — the `source` →
/// POSIX `.` fix — migrates a dotfile written by an older cfgd instead of
/// appending a second, duplicate line. A stale variant is rewritten where it
/// already sits, and a first injection is appended last, so the line still
/// follows the user definitions whose order `detect_rc_env_conflicts` reads.
///
/// Every other byte of the file is carried through verbatim, including each
/// line's own terminator: rewriting a CRLF rc file wholesale to LF, or dropping
/// the blank lines a user left at the end, turns one injected line into a
/// whole-file diff in their dotfile repo.
pub(super) fn merge_source_line(existing: &str, line: &str) -> Option<String> {
    let marker = if line.contains(".cfgd-env.ps1") {
        ".cfgd-env.ps1"
    } else {
        ".cfgd.env"
    };
    // `split_inclusive` keeps each terminator attached to its line, so a
    // reassembly of the untouched segments is byte-identical to the input.
    let segments: Vec<&str> = existing.split_inclusive('\n').collect();
    let managed: Vec<usize> = segments
        .iter()
        .enumerate()
        .filter(|(_, seg)| is_managed_loader(strip_eol(seg), marker))
        .map(|(idx, _)| idx)
        .collect();
    if managed.len() == 1 && strip_eol(segments[managed[0]]) == line {
        return None;
    }

    let file_eol = if existing.contains("\r\n") {
        "\r\n"
    } else {
        "\n"
    };
    let mut content = String::with_capacity(existing.len() + line.len() + 2);
    for (idx, seg) in segments.iter().enumerate() {
        if managed.first() == Some(&idx) {
            content.push_str(line);
            content.push_str(eol_of(seg).unwrap_or(file_eol));
        } else if !managed.contains(&idx) {
            content.push_str(seg);
        }
    }
    if managed.is_empty() {
        if !content.is_empty() && !content.ends_with('\n') {
            content.push_str(file_eol);
        }
        content.push_str(line);
        content.push_str(file_eol);
    }
    Some(content)
}

/// The line terminator a `split_inclusive('\n')` segment carries, or `None` for
/// a final line the file left unterminated.
fn eol_of(segment: &str) -> Option<&'static str> {
    if segment.ends_with("\r\n") {
        Some("\r\n")
    } else if segment.ends_with('\n') {
        Some("\n")
    } else {
        None
    }
}

/// A `split_inclusive('\n')` segment without its terminator.
fn strip_eol(segment: &str) -> &str {
    segment
        .strip_suffix('\n')
        .map_or(segment, |s| s.strip_suffix('\r').unwrap_or(s))
}

/// Scan a shell rc file for `export` and `alias` definitions that appear before
/// the cfgd source line. If any match a cfgd-managed name with a different value,
/// return warnings advising the user to move the definition after the source line.
pub(super) fn detect_rc_env_conflicts(
    rc_path: &std::path::Path,
    env: &[crate::config::EnvVar],
    aliases: &[crate::config::ShellAlias],
) -> Vec<String> {
    let rc_content = match std::fs::read_to_string(rc_path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    // Only look at lines before the cfgd source line
    let mut before_lines = Vec::new();
    for line in rc_content.lines() {
        if line.contains("cfgd.env") {
            break;
        }
        before_lines.push(line);
    }

    let rc_display = rc_path.posix();
    let mut warnings = Vec::new();

    // Build lookup maps for cfgd-managed values
    let env_map: HashMap<&str, &str> = env
        .iter()
        .map(|e| (e.name.as_str(), e.value.as_str()))
        .collect();
    let alias_map: HashMap<&str, &str> = aliases
        .iter()
        .map(|a| (a.name.as_str(), a.command.as_str()))
        .collect();

    for line in &before_lines {
        let trimmed = line.trim();

        // Match: export NAME=VALUE
        if let Some(rest) = trimmed.strip_prefix("export ")
            && let Some((name, raw_value)) = rest.split_once('=')
        {
            let name = name.trim();
            let value = strip_shell_quotes(raw_value);
            if let Some(&cfgd_value) = env_map.get(name)
                && value != cfgd_value
            {
                warnings.push(format!(
                    "{} sets export {}={} before cfgd source line — cfgd will override to \"{}\"; move it after the source line to keep your value",
                    rc_display, name, raw_value, cfgd_value,
                ));
            }
        }

        // Match: alias NAME=VALUE or alias NAME="VALUE"
        if let Some(rest) = trimmed.strip_prefix("alias ")
            && let Some((name, raw_value)) = rest.split_once('=')
        {
            let name = name.trim();
            let value = strip_shell_quotes(raw_value);
            if let Some(&cfgd_value) = alias_map.get(name)
                && value != cfgd_value
            {
                warnings.push(format!(
                    "{} sets alias {}={} before cfgd source line — cfgd will override to \"{}\"; move it after the source line to keep your value",
                    rc_display, name, raw_value, cfgd_value,
                ));
            }
        }
    }

    warnings
}

/// Strip surrounding single or double quotes from a shell value.
pub(super) fn strip_shell_quotes(s: &str) -> &str {
    let s = s.trim();
    if (s.starts_with('"') && s.ends_with('"')) || (s.starts_with('\'') && s.ends_with('\'')) {
        &s[1..s.len() - 1]
    } else {
        s
    }
}

#[cfg(test)]
mod tests {
    use super::super::env_engine::{FoldedPath, ManagerPathDir};
    use crate::config::{EnvVar, ShellAlias};

    fn env_var(value: &str) -> Vec<EnvVar> {
        vec![EnvVar {
            name: "V".to_string(),
            value: value.to_string(),
            platforms: vec![],
        }]
    }

    fn alias(command: &str) -> Vec<ShellAlias> {
        vec![ShellAlias {
            name: "q".to_string(),
            command: command.to_string(),
            platforms: vec![],
        }]
    }

    /// Every generated line the value lands on, for one hostile value. Each
    /// expectation was confirmed against the real shell: the value round-trips
    /// intact and nothing executes while the file is being sourced.
    struct Expect {
        value: &'static str,
        env_line: &'static str,
        alias_line: &'static str,
    }

    const BASH: &[Expect] = &[
        Expect {
            value: "$(id)",
            env_line: "export V=\"\\$(id)\"",
            alias_line: "alias q=\"\\$(id)\"",
        },
        Expect {
            value: "`id`",
            env_line: "export V=\"\\`id\\`\"",
            alias_line: "alias q=\"\\`id\\`\"",
        },
        Expect {
            value: "${IFS}",
            env_line: "export V=\"${IFS}\"",
            alias_line: "alias q=\"${IFS}\"",
        },
        Expect {
            value: "a\"b",
            env_line: "export V=\"a\\\"b\"",
            alias_line: "alias q=\"a\\\"b\"",
        },
        Expect {
            value: "a'b",
            env_line: "export V=\"a'b\"",
            alias_line: "alias q=\"a'b\"",
        },
        Expect {
            value: "a\\",
            env_line: "export V=\"a\\\\\"",
            alias_line: "alias q=\"a\\\\\"",
        },
        Expect {
            value: "a\nb",
            env_line: "export V=\"a\nb\"",
            alias_line: "alias q=\"a\nb\"",
        },
        Expect {
            value: "$HOME",
            env_line: "export V=\"$HOME\"",
            alias_line: "alias q=\"$HOME\"",
        },
    ];

    const FISH: &[Expect] = &[
        Expect {
            value: "$(id)",
            env_line: "set -gx V '$(id)'",
            alias_line: "abbr -a q '$(id)'",
        },
        Expect {
            value: "`id`",
            env_line: "set -gx V '`id`'",
            alias_line: "abbr -a q '`id`'",
        },
        Expect {
            value: "${IFS}",
            env_line: "set -gx V '${IFS}'",
            alias_line: "abbr -a q '${IFS}'",
        },
        Expect {
            value: "a\"b",
            env_line: "set -gx V 'a\"b'",
            alias_line: "abbr -a q 'a\"b'",
        },
        Expect {
            value: "a'b",
            env_line: "set -gx V 'a\\'b'",
            alias_line: "abbr -a q 'a\\'b'",
        },
        Expect {
            value: "a\\",
            env_line: "set -gx V 'a\\\\'",
            alias_line: "abbr -a q 'a\\\\'",
        },
        Expect {
            value: "a\nb",
            env_line: "set -gx V 'a\nb'",
            alias_line: "abbr -a q 'a\nb'",
        },
        Expect {
            value: "$HOME",
            env_line: "set -gx V '$HOME'",
            alias_line: "abbr -a q '$HOME'",
        },
    ];

    const POWERSHELL: &[Expect] = &[
        Expect {
            value: "$(id)",
            env_line: "$env:V = '$(id)'",
            alias_line: "Set-Alias -Name q -Value '$(id)'",
        },
        Expect {
            value: "`id`",
            env_line: "$env:V = '`id`'",
            alias_line: "Set-Alias -Name q -Value '`id`'",
        },
        Expect {
            value: "${IFS}",
            env_line: "$env:V = '${IFS}'",
            alias_line: "Set-Alias -Name q -Value '${IFS}'",
        },
        Expect {
            value: "a\"b",
            env_line: "$env:V = 'a\"b'",
            alias_line: "Set-Alias -Name q -Value 'a\"b'",
        },
        Expect {
            value: "a'b",
            env_line: "$env:V = 'a''b'",
            alias_line: "Set-Alias -Name q -Value 'a''b'",
        },
        Expect {
            value: "a\\",
            env_line: "$env:V = 'a\\'",
            alias_line: "Set-Alias -Name q -Value 'a\\'",
        },
        Expect {
            value: "a\nb",
            env_line: "$env:V = 'a\nb'",
            // Whitespace-split sees two words, so this takes the function arm.
            alias_line: "function q { & ([scriptblock]::Create('a\nb @args')) @args }",
        },
        Expect {
            value: "$HOME",
            env_line: "$env:V = '$HOME'",
            alias_line: "Set-Alias -Name q -Value '$HOME'",
        },
    ];

    fn assert_line(generated: &str, expected: &str, value: &str, shell: &str) {
        let body = generated
            .strip_prefix(super::ENV_FILE_HEADER)
            .unwrap_or(generated)
            .trim_matches('\n');
        assert_eq!(
            body, expected,
            "{shell} emitted the wrong line for value {value:?}"
        );
    }

    #[test]
    fn bash_env_and_alias_lines_quote_every_hostile_value() {
        for case in BASH {
            assert_line(
                &super::generate_env_file_content(
                    &env_var(case.value),
                    &[],
                    None,
                    &Default::default(),
                ),
                case.env_line,
                case.value,
                "bash",
            );
            assert_line(
                &super::generate_env_file_content(
                    &[],
                    &alias(case.value),
                    None,
                    &Default::default(),
                ),
                case.alias_line,
                case.value,
                "bash",
            );
        }
    }

    #[test]
    fn fish_env_and_alias_lines_quote_every_hostile_value() {
        for case in FISH {
            assert_line(
                &super::generate_fish_env_content(
                    &env_var(case.value),
                    &[],
                    None,
                    &Default::default(),
                ),
                case.env_line,
                case.value,
                "fish",
            );
            assert_line(
                &super::generate_fish_env_content(
                    &[],
                    &alias(case.value),
                    None,
                    &Default::default(),
                ),
                case.alias_line,
                case.value,
                "fish",
            );
        }
    }

    #[test]
    fn powershell_env_and_alias_lines_quote_every_hostile_value() {
        for case in POWERSHELL {
            assert_line(
                &super::generate_powershell_env_content(
                    &env_var(case.value),
                    &[],
                    None,
                    &Default::default(),
                ),
                case.env_line,
                case.value,
                "powershell",
            );
            assert_line(
                &super::generate_powershell_env_content(
                    &[],
                    &alias(case.value),
                    None,
                    &Default::default(),
                ),
                case.alias_line,
                case.value,
                "powershell",
            );
        }
    }

    /// A lone trailing backslash used to close the fish single-quoted region
    /// early, leaving the rest of the file inside an unterminated string
    /// (`fish: Unexpected end of string, quotes are not balanced`).
    #[test]
    fn fish_path_segments_keep_a_trailing_backslash_inside_its_quotes() {
        let env = vec![EnvVar {
            name: "PATH".to_string(),
            value: "a\\:b".to_string(),
            platforms: vec![],
        }];
        let fold = super::super::env_engine::fold_path_line(
            &env,
            &[],
            &Default::default(),
            std::path::Path::new("/home/tj"),
            EnvPlatform::Linux,
            None,
        );
        let content =
            super::generate_fish_env_content(&env, &[], fold.as_ref(), &Default::default());
        assert!(
            content.contains("set -gx PATH 'a\\\\' 'b'"),
            "trailing backslash escaped out of its quotes: {content}"
        );
    }

    #[test]
    fn fish_bootstrapped_path_dirs_escape_a_trailing_backslash() {
        let content = super::generate_fish_env_content(
            &[],
            &[],
            Some(&FoldedPath::derived(&[ManagerPathDir::unowned("/opt/a\\")])),
            &Default::default(),
        );
        assert!(
            content.contains("set -gx PATH '/opt/a\\\\' $PATH"),
            "trailing backslash escaped out of its quotes: {content}"
        );
    }

    /// The interpolating PowerShell arm keeps `$env:NAME` resolving while a
    /// subexpression in the same value stays inert.
    #[test]
    fn powershell_env_ref_branch_neutralizes_a_subexpression() {
        let env = vec![EnvVar {
            name: "V".to_string(),
            value: "$env:PATH;$(Write-Output pwned)".to_string(),
            platforms: vec![],
        }];
        let content = super::generate_powershell_env_content(&env, &[], None, &Default::default());
        assert!(
            content.contains("$env:V = \"$env:PATH;`$(Write-Output pwned)\""),
            "subexpression survived: {content}"
        );
    }

    #[test]
    fn powershell_bootstrapped_path_dirs_neutralize_a_subexpression() {
        let content = super::generate_powershell_env_content(
            &[],
            &[],
            Some(&FoldedPath::derived(&[ManagerPathDir::unowned(
                "C:/a$(Write-Output x)",
            )])),
            &Default::default(),
        );
        assert!(
            content.contains("$env:PATH = \"C:/a`$(Write-Output x);$env:PATH\""),
            "subexpression survived: {content}"
        );
    }

    /// A `}` in a multi-word alias command used to close the generated
    /// function body, so everything after it ran while the profile loaded.
    #[test]
    fn powershell_function_alias_cannot_close_its_own_body() {
        let aliases = alias("Write-Output benign }; Write-Output pwned; #");
        let content =
            super::generate_powershell_env_content(&[], &aliases, None, &Default::default());
        assert!(
            content.contains(
                "function q { & ([scriptblock]::Create('Write-Output benign }; \
                 Write-Output pwned; # @args')) @args }"
            ),
            "function body is not deferred to call time: {content}"
        );
        assert!(
            !content.contains("function q { Write-Output benign }"),
            "command pasted straight into the function body: {content}"
        );
    }

    #[test]
    fn powershell_function_alias_doubles_a_single_quote_in_the_command() {
        let aliases = alias("Write-Output 'hi there'");
        let content =
            super::generate_powershell_env_content(&[], &aliases, None, &Default::default());
        assert!(
            content.contains(
                "function q { & ([scriptblock]::Create('Write-Output ''hi there'' @args')) @args }"
            ),
            "single quotes not doubled: {content}"
        );
    }

    /// The bash arm deliberately keeps a plain `$NAME` reference live so a
    /// declared `PATH: /opt/bin:$PATH` still composes with the inherited
    /// value, while the execution forms next to it stay inert.
    #[test]
    fn bash_keeps_a_plain_reference_live_beside_a_neutralized_substitution() {
        let env = vec![EnvVar {
            name: "V".to_string(),
            value: "/opt/bin:$PATH:$(id)".to_string(),
            platforms: vec![],
        }];
        let content = super::generate_env_file_content(&env, &[], None, &Default::default());
        assert!(
            content.contains("export V=\"/opt/bin:$PATH:\\$(id)\""),
            "unexpected quoting: {content}"
        );
    }

    #[test]
    fn bash_bootstrapped_path_dirs_neutralize_a_substitution() {
        // The prefix line joins each directory inside ONE pair of quotes, so it
        // takes the escaper body rather than a whole quoted token — the trailing
        // `:$PATH` has to stay live for the prefix to mean anything.
        let content = super::generate_env_file_content(
            &[],
            &[],
            Some(&FoldedPath::derived(&[
                ManagerPathDir::unowned("/opt/$(id)/bin"),
                ManagerPathDir::unowned("/opt/a\"b/bin"),
            ])),
            &Default::default(),
        );
        assert!(
            content.contains("export PATH=\"/opt/\\$(id)/bin:/opt/a\\\"b/bin:$PATH\""),
            "unexpected quoting: {content}"
        );
    }

    use super::super::env_engine::EnvPlatform;

    #[test]
    fn stable_line_prefix_stops_at_the_value_and_strips_its_quote() {
        assert_eq!(
            super::stable_line_prefix(
                Some("export A=\"0x\"".to_string()),
                Some("export A=\"1x\"".to_string())
            )
            .as_deref(),
            Some("export A=")
        );
        assert_eq!(super::stable_line_prefix(None, Some("x".to_string())), None);
        // Lines differing at their first character leave nothing stable to
        // claim by, and an empty prefix would claim everything.
        assert_eq!(
            super::stable_line_prefix(Some("axx".to_string()), Some("bxx".to_string())),
            None
        );
    }

    #[test]
    fn env_var_line_prefix_prefixes_the_real_rendered_line_in_both_dialects() {
        // `$env:OTHER;x` is the value that flips PowerShell to double quotes:
        // only a quote-stripped prefix claims lines rendered under either.
        for platform in [EnvPlatform::Linux, EnvPlatform::Windows] {
            let prefix = super::env_var_line_prefix("EDITOR", platform).unwrap();
            assert!(
                prefix.contains("EDITOR") && !prefix.contains("cfgd"),
                "prefix must carry the name and never a sentinel: {prefix:?}"
            );
            for value in ["nvim", "a\"b$(x)", "$env:OTHER;x"] {
                let line = super::primary_env_var_line(
                    &EnvVar {
                        name: "EDITOR".to_string(),
                        value: value.to_string(),
                        platforms: vec![],
                    },
                    platform,
                    &Default::default(),
                    None,
                )
                .unwrap();
                assert!(
                    line.starts_with(&prefix),
                    "{prefix:?} must prefix {line:?} ({platform:?})"
                );
            }
        }
    }

    #[test]
    fn alias_line_prefixes_cover_every_rendered_alias_shape() {
        // `git` renders as Set-Alias on Windows, `git status --short` as a
        // function wrapper: one declared name, two line shapes, both claimed.
        for platform in [EnvPlatform::Linux, EnvPlatform::Windows] {
            let prefixes = super::alias_line_prefixes("g", platform);
            assert!(!prefixes.is_empty(), "no prefixes for {platform:?}");
            for prefix in &prefixes {
                assert!(
                    prefix.contains('g') && !prefix.contains("cfgd"),
                    "prefix must carry the name and never a sentinel: {prefix:?}"
                );
            }
            for command in ["git", "git status --short"] {
                let line = super::primary_alias_line(
                    &ShellAlias {
                        name: "g".to_string(),
                        command: command.to_string(),
                        platforms: vec![],
                    },
                    platform,
                    &Default::default(),
                )
                .unwrap();
                assert!(
                    prefixes.iter().any(|p| line.starts_with(p.as_str())),
                    "no prefix in {prefixes:?} claims {line:?} ({platform:?})"
                );
            }
        }
    }

    /// Every dialect that has a trailing-comment grammar names the managers
    /// whose bootstrapped directories the PATH line carries — once each, in
    /// directory order. `environment.d` and the launchd plist have no such
    /// grammar and are deliberately absent.
    #[test]
    fn every_dialect_names_the_managers_on_its_bootstrapped_path_line() {
        let dirs = [
            ManagerPathDir::new("brew", "/opt/homebrew/bin"),
            ManagerPathDir::new("brew", "/opt/homebrew/sbin"),
            ManagerPathDir::new("cargo", "/home/u/.cargo/bin"),
        ];
        for content in [
            super::generate_env_file_content(
                &[],
                &[],
                Some(&FoldedPath::derived(&dirs)),
                &Default::default(),
            ),
            super::generate_fish_env_content(
                &[],
                &[],
                Some(&FoldedPath::derived(&dirs)),
                &Default::default(),
            ),
            super::generate_powershell_env_content(
                &[],
                &[],
                Some(&FoldedPath::derived(&dirs)),
                &Default::default(),
            ),
        ] {
            let path_line = content.lines().nth(1).expect("a PATH line is rendered");
            assert!(
                path_line.ends_with(" # manager:brew,cargo"),
                "the PATH line names each manager once, in dir order: {path_line}"
            );
        }
    }

    /// The prefix that claims a past run's PATH line is derived from a sentinel
    /// render, and the comment is the line's TAIL — so a real line carrying one
    /// must still start with it, or a converged machine reads its own PATH line
    /// as a stranger's deleted entry.
    #[test]
    fn path_dirs_line_prefix_still_claims_a_line_that_names_its_manager() {
        for (platform, dir) in [
            (EnvPlatform::Linux, "/opt/homebrew/bin"),
            (EnvPlatform::Windows, "C:/tools/bin"),
        ] {
            let dirs = [ManagerPathDir::new("brew", dir)];
            let content = if platform == EnvPlatform::Windows {
                super::generate_powershell_env_content(
                    &[],
                    &[],
                    Some(&FoldedPath::derived(&dirs)),
                    &Default::default(),
                )
            } else {
                super::generate_env_file_content(
                    &[],
                    &[],
                    Some(&FoldedPath::derived(&dirs)),
                    &Default::default(),
                )
            };
            let path_line = content.lines().nth(1).unwrap();
            let prefix = super::path_dirs_line_prefix(platform).unwrap();
            assert!(
                path_line.contains(" # manager:brew"),
                "the fixture must actually carry a comment: {path_line}"
            );
            assert!(
                path_line.starts_with(&prefix),
                "{prefix:?} must still claim {path_line:?}"
            );
        }
    }

    #[test]
    fn path_dirs_line_prefix_prefixes_the_generated_path_line() {
        let unix = super::generate_env_file_content(
            &[],
            &[],
            Some(&FoldedPath::derived(&[ManagerPathDir::unowned(
                "/opt/homebrew/bin",
            )])),
            &Default::default(),
        );
        let prefix = super::path_dirs_line_prefix(EnvPlatform::Linux).unwrap();
        assert!(
            unix.lines().nth(1).unwrap().starts_with(&prefix),
            "{prefix:?} must prefix the unix PATH line: {unix}"
        );
        assert!(!prefix.contains("cfgd"), "sentinel leaked: {prefix:?}");

        let ps = super::generate_powershell_env_content(
            &[],
            &[],
            Some(&FoldedPath::derived(&[ManagerPathDir::unowned(
                "C:/tools/bin",
            )])),
            &Default::default(),
        );
        let prefix = super::path_dirs_line_prefix(EnvPlatform::Windows).unwrap();
        assert!(
            ps.lines().nth(1).unwrap().starts_with(&prefix),
            "{prefix:?} must prefix the PowerShell PATH line: {ps}"
        );
        assert!(!prefix.contains("cfgd"), "sentinel leaked: {prefix:?}");
    }
}
