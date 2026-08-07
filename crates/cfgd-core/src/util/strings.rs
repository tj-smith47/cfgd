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
///
/// Escapes `\`, `"`, and `` ` ``, and escapes `$` **unless** it opens a plain
/// `$NAME` / `${NAME}` parameter reference. That one exemption is the whole
/// contract: a declared value such as `/opt/bin:$PATH` must still pick up the
/// surrounding environment when the login shell sources the generated file,
/// while every other `$` construct — `$(cmd)`, `$((…))`, `${x:-$(cmd)}`,
/// `${x@P}` — is a command-execution vector and becomes a literal `$`.
///
/// `!` is deliberately NOT escaped: history expansion applies to lines read
/// from the terminal, never to a sourced file, so `\!` would leave a literal
/// backslash in the value rather than protecting anything.
pub fn escape_double_quoted(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 8);
    let mut skip_to = 0usize;
    for (i, c) in s.char_indices() {
        if i < skip_to {
            continue;
        }
        match c {
            '\\' | '"' | '`' => {
                out.push('\\');
                out.push(c);
            }
            '$' => match plain_var_reference_end(bytes, i) {
                Some(end) => {
                    out.push_str(&s[i..end]);
                    skip_to = end;
                }
                None => out.push_str("\\$"),
            },
            _ => out.push(c),
        }
    }
    out
}

/// Byte index just past the plain `$NAME` / `${NAME}` reference starting at
/// `at`, or `None` when the `$` at `at` does not open one. A braced form
/// qualifies only when the braces hold nothing but the name — `${x:-…}`,
/// `${!x}` and `${x@P}` all expand through further shell evaluation and are
/// rejected so the caller escapes them.
fn plain_var_reference_end(b: &[u8], at: usize) -> Option<usize> {
    let mut i = at + 1;
    let braced = b.get(i) == Some(&b'{');
    if braced {
        i += 1;
    }
    if !matches!(b.get(i), Some(c) if c.is_ascii_alphabetic() || *c == b'_') {
        return None;
    }
    while matches!(b.get(i), Some(c) if c.is_ascii_alphanumeric() || *c == b'_') {
        i += 1;
    }
    if !braced {
        return Some(i);
    }
    (b.get(i) == Some(&b'}')).then_some(i + 1)
}

/// A value as a complete bash/zsh double-quoted word — quotes included.
///
/// Callers interpolate the result directly (`export NAME={}`) rather than
/// wrapping [`escape_double_quoted`] themselves, so a generated line cannot
/// end up carrying an unquoted value.
pub fn posix_double_quoted(value: &str) -> String {
    format!("\"{}\"", escape_double_quoted(value))
}

/// A value as a complete fish single-quoted word — quotes included, fully
/// literal.
///
/// Fish honours exactly two escapes inside single quotes, `\'` and `\\`, so
/// both the quote and the backslash must be escaped: a value ending in a lone
/// backslash otherwise swallows the closing quote and the rest of the file
/// parses as one unterminated string.
pub fn fish_single_quoted(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('\'');
    for c in value.chars() {
        if c == '\'' || c == '\\' {
            out.push('\\');
        }
        out.push(c);
    }
    out.push('\'');
    out
}

/// A value as a complete PowerShell single-quoted word — quotes included,
/// fully literal. PowerShell performs no expansion at all inside single
/// quotes; doubling `'` is the only escape.
pub fn powershell_single_quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "''"))
}

/// A value as a complete POSIX single-quoted word — quotes included.
///
/// No escape exists inside POSIX single quotes, so an embedded `'` closes the
/// quoted run and is re-supplied by the classic `'\''` concatenation idiom
/// (close, escaped quote, reopen). Everything else — backslashes, newlines,
/// `"`, `#`, leading and trailing spaces — survives byte-for-byte.
///
/// systemd's `environment.d` parser accepts the same idiom, which is what makes
/// a newline in a declared value inert there instead of a second `KEY=VALUE`
/// assignment. One caveat that quoting cannot cover: systemd expands `$NAME` /
/// `${NAME}` *after* parsing regardless of quotes (its own escape is `$$`), so
/// a caller relying on single quotes to suppress expansion will not get it —
/// only literal framing.
pub fn posix_single_quoted(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

/// Render C0/C1 control characters as visible `\xNN` text.
///
/// For untrusted content shown on a terminal before the operator approves it.
/// A raw `\x1b[2K` or a lone `\r` lets the value repaint or erase the very
/// lines describing it, so the operator approves something other than what
/// they read. Escaping rather than stripping keeps the display faithful: the
/// value's true length and shape stay visible, and a control character
/// announces itself instead of vanishing.
///
/// Backslash is deliberately left alone. Doubling it would turn every Windows
/// path on the surface into `C:\\Users\\…` to buy only the ability to tell a
/// real escape from the literal text `\x1b` — and both of those render
/// identically inert, so the ambiguity costs nothing.
pub fn escape_control_chars(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        // Unicode Cc covers C1 (U+0080..U+009F) as well as C0, which matters:
        // a terminal decoding UTF-8 still acts on U+009B as CSI.
        if c.is_control() {
            out.push_str(&format!("\\x{:02x}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

/// Escape a value for use inside PowerShell double quotes (single pass).
///
/// The PowerShell analogue of [`escape_double_quoted`]: backtick is the escape
/// character, and `$` is escaped unless it opens a plain variable reference
/// (`$NAME`, a scope- or drive-qualified `$env:NAME`, or a braced
/// `${env:NAME}`). Subexpressions — `$(…)`, `$($x.Foo)` — are the execution
/// vector and become a literal `$`.
pub fn escape_powershell_double_quoted(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = String::with_capacity(s.len() + s.len() / 8);
    let mut skip_to = 0usize;
    for (i, c) in s.char_indices() {
        if i < skip_to {
            continue;
        }
        match c {
            '`' => out.push_str("``"),
            '"' => out.push_str("`\""),
            '$' => match powershell_variable_reference_end(bytes, i) {
                Some(end) => {
                    out.push_str(&s[i..end]);
                    skip_to = end;
                }
                None => out.push_str("`$"),
            },
            _ => out.push(c),
        }
    }
    out
}

/// Byte index just past the PowerShell variable reference starting at `at`, or
/// `None` when the `$` at `at` does not open one.
fn powershell_variable_reference_end(b: &[u8], at: usize) -> Option<usize> {
    let mut i = at + 1;
    if b.get(i) == Some(&b'{') {
        i += 1;
        let start = i;
        while matches!(b.get(i), Some(c) if c.is_ascii_alphanumeric() || *c == b'_' || *c == b':') {
            i += 1;
        }
        if i == start || b.get(i) != Some(&b'}') {
            return None;
        }
        return Some(i + 1);
    }
    let mut end = powershell_bare_name_end(b, i)?;
    // A scope or drive qualifier (`env:`, `script:`) prefixes a second name.
    if b.get(end) == Some(&b':') {
        end = powershell_bare_name_end(b, end + 1)?;
    }
    Some(end)
}

/// Byte index just past a bare PowerShell identifier starting at `at`, or
/// `None` when no identifier starts there.
fn powershell_bare_name_end(b: &[u8], at: usize) -> Option<usize> {
    if !matches!(b.get(at), Some(c) if c.is_ascii_alphabetic() || *c == b'_') {
        return None;
    }
    let mut i = at;
    while matches!(b.get(i), Some(c) if c.is_ascii_alphanumeric() || *c == b'_') {
        i += 1;
    }
    Some(i)
}

/// A value as a complete PowerShell double-quoted word — quotes included,
/// with plain variable references left live.
pub fn powershell_double_quoted(value: &str) -> String {
    format!("\"{}\"", escape_powershell_double_quoted(value))
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

    /// The values a hostile module would reach for, each of which historically
    /// either executed at shell start or broke the quoted region it sat in.
    /// Every per-shell quoting test below runs the whole set.
    const HOSTILE: &[(&str, &str)] = &[
        ("cmdsub", "$(id)"),
        ("backtick", "`id`"),
        ("brace_ifs", "${IFS}"),
        ("double_quote", "a\"b"),
        ("single_quote", "a'b"),
        ("trailing_backslash", "a\\"),
        ("newline", "a\nb"),
        ("var_ref", "$HOME"),
        ("arith", "$((1+1))"),
        ("brace_default", "${x:-$(id)}"),
        ("prompt_op", "${x@P}"),
        ("combo", "x$(id)`id`\"'\\"),
    ];

    #[test]
    fn posix_double_quoted_neutralizes_command_substitution() {
        assert_eq!(posix_double_quoted("$(id)"), "\"\\$(id)\"");
        assert_eq!(posix_double_quoted("`id`"), "\"\\`id\\`\"");
        assert_eq!(posix_double_quoted("$((1+1))"), "\"\\$((1+1))\"");
        assert_eq!(posix_double_quoted("${x:-$(id)}"), "\"\\${x:-\\$(id)}\"");
        assert_eq!(posix_double_quoted("${x@P}"), "\"\\${x@P}\"");
        assert_eq!(posix_double_quoted("${!x}"), "\"\\${!x}\"");
    }

    #[test]
    fn posix_double_quoted_keeps_plain_variable_references_live() {
        assert_eq!(posix_double_quoted("/opt/bin:$PATH"), "\"/opt/bin:$PATH\"");
        assert_eq!(posix_double_quoted("${HOME}/bin"), "\"${HOME}/bin\"");
        assert_eq!(posix_double_quoted("$HOME/$USER"), "\"$HOME/$USER\"");
    }

    #[test]
    fn posix_double_quoted_escapes_quote_backslash_and_leaves_bang_alone() {
        assert_eq!(posix_double_quoted("a\"b"), "\"a\\\"b\"");
        assert_eq!(posix_double_quoted("a\\"), "\"a\\\\\"");
        assert_eq!(posix_double_quoted("a'b"), "\"a'b\"");
        // `\!` inside a sourced file is a literal backslash, not an escape.
        assert_eq!(posix_double_quoted("hi!"), "\"hi!\"");
    }

    #[test]
    fn posix_double_quoted_keeps_newline_literal_inside_the_quotes() {
        assert_eq!(posix_double_quoted("a\nb"), "\"a\nb\"");
    }

    /// Every quoted word is balanced (an even number of unescaped `"`) and
    /// carries no live command substitution, for the whole hostile set.
    #[test]
    fn posix_double_quoted_is_balanced_and_inert_for_every_hostile_value() {
        for (label, value) in HOSTILE {
            let quoted = posix_double_quoted(value);
            assert!(
                quoted.starts_with('"') && quoted.ends_with('"'),
                "{label}: not wrapped: {quoted}"
            );
            let inner = &quoted[1..quoted.len() - 1];
            assert!(
                !contains_unescaped(inner, "$("),
                "{label}: live command substitution in {quoted}"
            );
            assert!(
                !contains_unescaped(inner, "`"),
                "{label}: live backtick in {quoted}"
            );
            assert!(
                !contains_unescaped(inner, "\""),
                "{label}: unescaped quote in {quoted}"
            );
            assert!(
                !ends_with_odd_backslash_run(inner),
                "{label}: closing quote escaped by a trailing backslash run in {quoted}"
            );
        }
    }

    #[test]
    fn fish_single_quoted_escapes_backslash_and_quote() {
        assert_eq!(fish_single_quoted("a\\"), "'a\\\\'");
        assert_eq!(fish_single_quoted("it's"), "'it\\'s'");
        assert_eq!(fish_single_quoted("$(id)"), "'$(id)'");
        assert_eq!(fish_single_quoted("`id`"), "'`id`'");
        assert_eq!(fish_single_quoted("a\nb"), "'a\nb'");
    }

    #[test]
    fn fish_single_quoted_is_balanced_for_every_hostile_value() {
        for (label, value) in HOSTILE {
            let quoted = fish_single_quoted(value);
            assert!(
                quoted.starts_with('\'') && quoted.ends_with('\''),
                "{label}: not wrapped: {quoted}"
            );
            let inner = &quoted[1..quoted.len() - 1];
            assert!(
                !contains_unescaped(inner, "'"),
                "{label}: unescaped quote in {quoted}"
            );
            assert!(
                !ends_with_odd_backslash_run(inner),
                "{label}: closing quote escaped by a trailing backslash run in {quoted}"
            );
        }
    }

    #[test]
    fn powershell_single_quoted_doubles_quotes_and_stays_literal() {
        assert_eq!(powershell_single_quoted("it's"), "'it''s'");
        assert_eq!(powershell_single_quoted("$(id)"), "'$(id)'");
        assert_eq!(powershell_single_quoted("a\\"), "'a\\'");
        assert_eq!(powershell_single_quoted("a\nb"), "'a\nb'");
    }

    #[test]
    fn powershell_single_quoted_is_balanced_for_every_hostile_value() {
        for (label, value) in HOSTILE {
            let quoted = powershell_single_quoted(value);
            let inner = &quoted[1..quoted.len() - 1];
            assert_eq!(
                inner.matches('\'').count() % 2,
                0,
                "{label}: odd quote count in {quoted}"
            );
        }
    }

    #[test]
    fn powershell_double_quoted_neutralizes_subexpressions() {
        assert_eq!(powershell_double_quoted("$(id)"), "\"`$(id)\"");
        assert_eq!(powershell_double_quoted("`id`"), "\"``id``\"");
        assert_eq!(powershell_double_quoted("a\"b"), "\"a`\"b\"");
        // The opening `$(` is neutralized, so the inner `$x` is left resolving
        // inside what is now plain text — no subexpression runs.
        assert_eq!(powershell_double_quoted("$($x.Foo)"), "\"`$($x.Foo)\"");
    }

    #[test]
    fn powershell_double_quoted_keeps_variable_references_live() {
        assert_eq!(
            powershell_double_quoted(r"C:\tools;$env:PATH"),
            "\"C:\\tools;$env:PATH\""
        );
        assert_eq!(powershell_double_quoted("${env:PATH}"), "\"${env:PATH}\"");
        assert_eq!(powershell_double_quoted("$HOME"), "\"$HOME\"");
    }

    #[test]
    fn powershell_double_quoted_is_balanced_and_inert_for_every_hostile_value() {
        for (label, value) in HOSTILE {
            let quoted = powershell_double_quoted(value);
            let inner = &quoted[1..quoted.len() - 1];
            assert!(
                !contains_unescaped_by_backtick(inner, "$("),
                "{label}: live subexpression in {quoted}"
            );
            assert!(
                !contains_unescaped_by_backtick(inner, "\""),
                "{label}: unescaped quote in {quoted}"
            );
            assert!(
                !ends_with_odd_run(inner, '`'),
                "{label}: closing quote escaped by a trailing backtick run in {quoted}"
            );
        }
    }

    #[test]
    fn posix_single_quoted_re_supplies_an_embedded_quote() {
        assert_eq!(posix_single_quoted("a'b"), "'a'\\''b'");
        assert_eq!(posix_single_quoted("'"), "''\\'''");
    }

    #[test]
    fn posix_single_quoted_keeps_everything_else_byte_for_byte() {
        assert_eq!(posix_single_quoted("a\\"), "'a\\'");
        assert_eq!(posix_single_quoted("a\nb"), "'a\nb'");
        assert_eq!(posix_single_quoted("$(id)"), "'$(id)'");
        assert_eq!(posix_single_quoted(" a b # c "), "' a b # c '");
    }

    /// Every quoted word closes: once the `'\''` idiom is accounted for, one
    /// opening and one closing `'` remain and nothing between them can end the
    /// quoted run early — which is what stops a value ending the assignment or
    /// statement it sits in. Quote parity is deliberately NOT the invariant:
    /// the idiom spends three quotes per embedded one, so a correct word can
    /// hold an odd number.
    #[test]
    fn posix_single_quoted_closes_for_every_hostile_value() {
        for (label, value) in HOSTILE {
            let quoted = posix_single_quoted(value);
            let idiom_free = quoted.replace("'\\''", "\u{1}");
            assert!(
                idiom_free.starts_with('\'') && idiom_free.ends_with('\''),
                "{label}: not wrapped: {quoted}"
            );
            assert!(
                !idiom_free[1..idiom_free.len() - 1].contains('\''),
                "{label}: quoted run ends early in {quoted}"
            );
            assert_eq!(
                idiom_free,
                format!("'{}'", value.replace('\'', "\u{1}")),
                "{label}: value altered by quoting: {quoted}"
            );
        }
    }

    #[test]
    fn escape_control_chars_renders_terminal_control_sequences_visible() {
        assert_eq!(escape_control_chars("a\rb"), "a\\x0db");
        assert_eq!(escape_control_chars("a\x1b[2Kb"), "a\\x1b[2Kb");
        assert_eq!(escape_control_chars("a\nb\tc"), "a\\x0ab\\x09c");
    }

    #[test]
    fn escape_control_chars_covers_c1_and_leaves_text_alone() {
        // U+009B is CSI to a terminal decoding UTF-8, so it needs escaping
        // even though it is not a C0 byte.
        assert_eq!(escape_control_chars("a\u{9b}2Kb"), "a\\x9b2Kb");
        assert_eq!(
            escape_control_chars("C:\\Users\\me — café"),
            "C:\\Users\\me — café"
        );
    }

    /// True when `needle` occurs in `hay` at a position not preceded by an odd
    /// run of backslashes (i.e. it is live rather than escaped).
    fn contains_unescaped(hay: &str, needle: &str) -> bool {
        hay.match_indices(needle)
            .any(|(i, _)| !ends_with_odd_backslash_run(&hay[..i]))
    }

    /// The PowerShell counterpart: the escape character is the backtick.
    fn contains_unescaped_by_backtick(hay: &str, needle: &str) -> bool {
        hay.match_indices(needle)
            .any(|(i, _)| !ends_with_odd_run(&hay[..i], '`'))
    }

    fn ends_with_odd_backslash_run(s: &str) -> bool {
        ends_with_odd_run(s, '\\')
    }

    fn ends_with_odd_run(s: &str, c: char) -> bool {
        s.chars().rev().take_while(|ch| *ch == c).count() % 2 == 1
    }
}
