use crate::config;

/// Parse a `KEY=VALUE` string into an `EnvVar`.
pub fn parse_env_var(input: &str) -> std::result::Result<config::EnvVar, String> {
    let (key, value) = input
        .split_once('=')
        .ok_or_else(|| format!("invalid env var '{}' — expected KEY=VALUE", input))?;
    validate_env_var_user_name(key)?;
    Ok(config::EnvVar {
        name: key.to_string(),
        value: value.to_string(),
    })
}

/// Validate that an environment variable name is safe for shell interpolation
/// and is not in the reserved `CFGD_*` namespace.
pub fn validate_env_var_user_name(name: &str) -> std::result::Result<(), String> {
    validate_env_var_name(name)?;
    if name.starts_with("CFGD_") {
        return Err(format!(
            "env var name '{}' is reserved — the CFGD_* prefix is for \
             cfgd runtime metadata. Rename to e.g. APP_{} or MY_{}.",
            name,
            name.trim_start_matches("CFGD_"),
            name.trim_start_matches("CFGD_"),
        ));
    }
    if name == "BASH_ENV" || name == "ZDOTDIR" {
        return Err(format!(
            "env var name '{name}' is reserved — cfgd uses it for \
             alias delivery to lifecycle scripts"
        ));
    }
    Ok(())
}

/// Validate that an environment variable name is safe for shell interpolation.
/// Accepts names matching `[A-Za-z_][A-Za-z0-9_]*`.
pub fn validate_env_var_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("environment variable name must not be empty".to_string());
    }
    let first = name.as_bytes()[0];
    if !first.is_ascii_alphabetic() && first != b'_' {
        return Err(format!(
            "invalid env var name '{}' — must start with a letter or underscore",
            name
        ));
    }
    if !name.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_') {
        return Err(format!(
            "invalid env var name '{}' — must contain only letters, digits, and underscores",
            name
        ));
    }
    Ok(())
}

/// Expand `$VAR` and `${VAR}` references in `value`, resolving each name via
/// `lookup`. A name `lookup` returns `None` for expands to the empty string
/// (shell-faithful for an unset variable). A `$` that does not introduce a valid
/// reference (`$5`, a trailing `$`, an unterminated `${`) is preserved literally.
///
/// This is the non-shell equivalent of the expansion a login shell performs when
/// it sources an `export FOO=...:$PATH` line. It exists because declared
/// `spec.env` values are injected directly into a child process environment,
/// where no shell is present to expand them — and a literal `$PATH` would corrupt
/// the variable (e.g. break `PATH` so the interpreter itself can't be found).
pub fn expand_env_vars(value: &str, lookup: &dyn Fn(&str) -> Option<String>) -> String {
    let b = value.as_bytes();
    let mut out = String::with_capacity(value.len());
    let mut i = 0;
    let mut literal_start = 0;
    while i < b.len() {
        if b[i] != b'$' {
            i += 1;
            continue;
        }
        // Resolve the name span and the index just past the whole reference.
        let (name_start, name_end, next) = if b.get(i + 1) == Some(&b'{') {
            let s = i + 2;
            let mut e = s;
            while e < b.len() && (b[e].is_ascii_alphanumeric() || b[e] == b'_') {
                e += 1;
            }
            if e > s && b.get(e) == Some(&b'}') {
                (s, e, e + 1)
            } else {
                i += 1; // not a valid `${...}` — keep the `$` literal
                continue;
            }
        } else {
            let s = i + 1;
            if s < b.len() && (b[s].is_ascii_alphabetic() || b[s] == b'_') {
                let mut e = s + 1;
                while e < b.len() && (b[e].is_ascii_alphanumeric() || b[e] == b'_') {
                    e += 1;
                }
                (s, e, e)
            } else {
                i += 1; // bare `$`, or `$` + non-name — keep literal
                continue;
            }
        };
        out.push_str(&value[literal_start..i]);
        if let Some(v) = lookup(&value[name_start..name_end]) {
            out.push_str(&v);
        }
        i = next;
        literal_start = next;
    }
    out.push_str(&value[literal_start..]);
    out
}

/// Validate that a shell alias name is safe for shell interpolation.
/// Accepts names matching `[A-Za-z0-9_.-]+`.
pub fn validate_alias_name(name: &str) -> std::result::Result<(), String> {
    if name.is_empty() {
        return Err("alias name must not be empty".to_string());
    }
    if !name
        .bytes()
        .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-' || b == b'.')
    {
        return Err(format!(
            "invalid alias name '{}' — must contain only letters, digits, underscores, hyphens, and dots",
            name
        ));
    }
    Ok(())
}

/// Parse a `name=command` string into a `ShellAlias`.
pub fn parse_alias(input: &str) -> std::result::Result<config::ShellAlias, String> {
    let (name, command) = input
        .split_once('=')
        .ok_or_else(|| format!("invalid alias '{}' — expected name=command", input))?;
    validate_alias_name(name)?;
    Ok(config::ShellAlias {
        name: name.to_string(),
        command: command.to_string(),
    })
}

/// Sanitize a string for use as a Kubernetes object name (RFC 1123 DNS label).
/// Lowercases, replaces underscores with hyphens, filters non-alphanumeric chars,
/// and trims leading/trailing hyphens.
pub fn sanitize_k8s_name(name: &str) -> String {
    name.to_ascii_lowercase()
        .replace('_', "-")
        .chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-')
        .collect::<String>()
        .trim_matches('-')
        .to_string()
}

/// Escape a value for use in shell `export` statements.
///
/// Uses single quotes for values containing shell metacharacters (`$`, backtick,
/// `\`, `"`). Single quotes within the value are escaped via `'\''`.
/// Single-pass scan: returns double-quoted string when no metacharacters are present
/// (zero intermediate allocations in the common case).
pub fn shell_escape_value(value: &str) -> String {
    if !value
        .bytes()
        .any(|b| matches!(b, b'$' | b'`' | b'\\' | b'"' | b'\''))
    {
        return format!("\"{}\"", value);
    }
    // Single-quote strategy: only `'` needs escaping inside single quotes
    if !value.contains('\'') {
        return format!("'{}'", value);
    }
    // Value contains both metacharacters and single quotes — break-out escaping
    let mut out = String::with_capacity(value.len() + 8);
    out.push('\'');
    for c in value.chars() {
        if c == '\'' {
            out.push_str("'\\''");
        } else {
            out.push(c);
        }
    }
    out.push('\'');
    out
}

/// Escape a value for use inside bash/zsh double quotes (single pass).
/// Escapes `\`, `"`, `` ` ``, and `!` — the four characters with special
/// meaning inside double-quoted strings.
pub fn escape_double_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + s.len() / 8);
    for c in s.chars() {
        match c {
            '\\' | '"' | '`' | '!' => {
                out.push('\\');
                out.push(c);
            }
            _ => out.push(c),
        }
    }
    out
}

/// Escape a string for safe inclusion in XML/plist content (single pass).
pub fn xml_escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + s.len() / 8);
    for c in s.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(c),
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expand_env_vars_basic_and_braced() {
        let look = |n: &str| match n {
            "HOME" => Some("/h".to_string()),
            "X" => Some("v".to_string()),
            _ => None,
        };
        // $NAME, ${NAME}, and an unset $PATH (→ empty) in one value.
        assert_eq!(expand_env_vars("$HOME/bin:${X}:$PATH", &look), "/h/bin:v:");
    }

    #[test]
    fn expand_env_vars_unknown_expands_to_empty() {
        let look = |_: &str| None;
        assert_eq!(expand_env_vars("x${NOPE}y", &look), "xy");
    }

    #[test]
    fn expand_env_vars_preserves_non_references() {
        let look = |_: &str| Some("SHOULD_NOT_APPEAR".to_string());
        // `$5` (digit), a trailing `$`, and an unterminated `${` stay literal.
        assert_eq!(expand_env_vars("$5 and $", &look), "$5 and $");
        assert_eq!(expand_env_vars("a${UNCLOSED b", &look), "a${UNCLOSED b");
    }

    #[test]
    fn expand_env_vars_preserves_utf8_literals() {
        let look = |n: &str| (n == "V").then(|| "→".to_string());
        assert_eq!(expand_env_vars("café $V θ", &look), "café → θ");
    }
}
