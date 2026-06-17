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

/// Generate bash/zsh env file content from merged env vars and aliases.
pub(super) fn generate_env_file_content(
    env: &[crate::config::EnvVar],
    aliases: &[crate::config::ShellAlias],
) -> String {
    let mut lines = vec![ENV_FILE_HEADER.to_string()];
    for ev in env {
        if crate::validate_env_var_name(&ev.name).is_err() {
            tracing::warn!("skipping env var with unsafe name: {}", ev.name);
            continue;
        }
        lines.push(format!(
            "export {}=\"{}\"",
            ev.name,
            crate::escape_double_quoted(&crate::expand_env_value_tilde(&ev.value))
        ));
    }
    for alias in aliases {
        if crate::validate_alias_name(&alias.name).is_err() {
            tracing::warn!("skipping alias with unsafe name: {}", alias.name);
            continue;
        }
        lines.push(format!(
            "alias {}=\"{}\"",
            alias.name,
            crate::escape_double_quoted(&alias.command)
        ));
    }
    lines.push(String::new()); // trailing newline
    lines.join("\n")
}

/// Generate fish env file content from merged env vars and aliases.
pub(super) fn generate_fish_env_content(
    env: &[crate::config::EnvVar],
    aliases: &[crate::config::ShellAlias],
) -> String {
    let mut lines = vec![ENV_FILE_HEADER.to_string()];
    for ev in env {
        if crate::validate_env_var_name(&ev.name).is_err() {
            tracing::warn!("skipping env var with unsafe name: {}", ev.name);
            continue;
        }
        if ev.name == "PATH" {
            // Fish uses a space-separated list for PATH, not colon-separated.
            // Split the RAW value on the `:` separator before tilde expansion:
            // on Windows `~` expands to a drive-prefixed path (`C:/Users/...`),
            // and splitting post-expansion would shatter that drive colon into a
            // bogus extra PATH entry. Each segment is then expanded and
            // single-quoted to suppress fish expansion.
            let parts: Vec<String> = ev
                .value
                .split(':')
                .map(crate::expand_env_value_tilde)
                .map(|p| format!("'{}'", p.replace('\'', "\\'")))
                .collect();
            lines.push(format!("set -gx PATH {}", parts.join(" ")));
        } else {
            // Expand a leading/`:`-prefixed `~` to home before single-quoting:
            // fish single quotes suppress tilde expansion, so a literal `~` would
            // break the path. (`$VAR` in a fish single-quoted value is a separate
            // gap.)
            let value = crate::expand_env_value_tilde(&ev.value);
            // Single-quote to prevent fish command substitution via ()
            lines.push(format!(
                "set -gx {} '{}'",
                ev.name,
                value.replace('\'', "\\'")
            ));
        }
    }
    for alias in aliases {
        if crate::validate_alias_name(&alias.name).is_err() {
            tracing::warn!("skipping alias with unsafe name: {}", alias.name);
            continue;
        }
        lines.push(format!(
            "abbr -a {} '{}'",
            alias.name,
            alias.command.replace('\'', "\\'")
        ));
    }
    lines.push(String::new());
    lines.join("\n")
}

/// Generate PowerShell env file content from merged env vars and aliases.
pub(super) fn generate_powershell_env_content(
    env: &[crate::config::EnvVar],
    aliases: &[crate::config::ShellAlias],
) -> String {
    let mut lines = vec![ENV_FILE_HEADER.to_string()];
    for ev in env {
        if crate::validate_env_var_name(&ev.name).is_err() {
            tracing::warn!("skipping env var with unsafe name: {}", ev.name);
            continue;
        }
        // Expand a leading/`:`-prefixed `~` to home before quoting (PowerShell
        // does not perform Unix tilde expansion on env values).
        let value = crate::expand_env_value_tilde(&ev.value);
        if value.contains("$env:") {
            // Value references other env vars — double-quote with PS escaping
            lines.push(format!(
                "$env:{} = \"{}\"",
                ev.name,
                value.replace('"', "`\"")
            ));
        } else {
            // Single-quote prevents all PS interpolation
            lines.push(format!(
                "$env:{} = '{}'",
                ev.name,
                value.replace('\'', "''")
            ));
        }
    }
    for alias in aliases {
        if crate::validate_alias_name(&alias.name).is_err() {
            tracing::warn!("skipping alias with unsafe name: {}", alias.name);
            continue;
        }
        if alias.command.split_whitespace().count() == 1 {
            // Simple alias — use Set-Alias
            lines.push(format!(
                "Set-Alias -Name {} -Value {}",
                alias.name, alias.command
            ));
        } else {
            // Complex alias — use function wrapper
            lines.push(format!(
                "function {} {{ {} @args }}",
                alias.name, alias.command
            ));
        }
    }
    lines.push(String::new()); // trailing newline
    lines.join("\n")
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
