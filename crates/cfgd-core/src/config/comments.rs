//! Leading YAML comment preservation for serde rewrite paths.
//!
//! cfgd rewrites user-owned YAML documents (config, profile, module) via
//! deserialize → mutate → `serde_yaml::to_string`, which discards every
//! comment — including the editor schema modeline emitted at scaffold time.
//! A full comment-preserving round-trip is not achievable (no
//! production-grade comment-preserving YAML CST crate exists), so rewrite
//! paths preserve the one recoverable class: the leading comment block.
//! Mid-document comments are still lost on rewrite.

/// Return the leading comment block of a YAML document: every line from the
/// beginning of `original` up to (not including) the first line that is
/// neither a `#` comment nor blank. Line endings (`\n` and `\r\n`) are
/// preserved verbatim. Empty when the document starts with content.
pub fn leading_comment_block(original: &str) -> &str {
    let mut end = 0;
    for line in original.split_inclusive('\n') {
        let body = line.trim_start().trim_end_matches(['\n', '\r']);
        if !(body.is_empty() || body.starts_with('#')) {
            break;
        }
        end += line.len();
    }
    &original[..end]
}

/// Re-prepend the leading comment block captured from `original` (the file
/// content read before a serde rewrite) onto the freshly serialized YAML.
///
/// Preserves ONLY what the original already had — an original without leading
/// comments returns `serialized` unchanged, so rewrite paths never inject a
/// modeline or banner (the scaffold/rewrite split invariant). Idempotent like
/// [`super::with_schema_modeline`]: output that already starts with the exact
/// block is returned as-is. Mid-document comments in `original` are not
/// recoverable and remain lost.
pub fn with_leading_comments(original: &str, serialized: &str) -> String {
    let block = leading_comment_block(original);
    if block.is_empty() || serialized.starts_with(block) {
        return serialized.to_string();
    }
    if block.ends_with('\n') {
        format!("{block}{serialized}")
    } else {
        // Comment-only original without a trailing newline: keep the block
        // and the body on separate lines.
        format!("{block}\n{serialized}")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn captures_plain_comment_block() {
        let src = "# banner\n# second line\napiVersion: cfgd.io/v1alpha1\n";
        assert_eq!(leading_comment_block(src), "# banner\n# second line\n");
    }

    #[test]
    fn captures_blank_interleaved_block() {
        let src = "# banner\n\n# after gap\n\nkind: Config\n";
        assert_eq!(leading_comment_block(src), "# banner\n\n# after gap\n\n");
    }

    #[test]
    fn no_comments_yields_empty_block() {
        let src = "apiVersion: cfgd.io/v1alpha1\n# mid comment\nkind: Config\n";
        assert_eq!(leading_comment_block(src), "");
    }

    #[test]
    fn comment_only_file_is_captured_whole() {
        let src = "# just\n# comments\n";
        assert_eq!(leading_comment_block(src), src);
    }

    #[test]
    fn crlf_lines_are_tolerated_and_preserved() {
        let src = "# banner\r\n\r\n# more\r\nkind: Config\r\n";
        assert_eq!(leading_comment_block(src), "# banner\r\n\r\n# more\r\n");
        let out = with_leading_comments(src, "kind: Config\n");
        assert_eq!(out, "# banner\r\n\r\n# more\r\nkind: Config\n");
    }

    #[test]
    fn indented_comment_counts_as_comment() {
        let src = "  # indented\nkind: Config\n";
        assert_eq!(leading_comment_block(src), "  # indented\n");
    }

    #[test]
    fn prepends_block_to_serialized_output() {
        let original =
            "# yaml-language-server: $schema=https://example/s.json\n# mine\nkind: Config\n";
        let out = with_leading_comments(original, "kind: Config\nspec: {}\n");
        assert_eq!(
            out,
            "# yaml-language-server: $schema=https://example/s.json\n# mine\nkind: Config\nspec: {}\n"
        );
    }

    #[test]
    fn never_injects_when_original_has_no_leading_comments() {
        let out = with_leading_comments("kind: Config\n", "kind: Config\nspec: {}\n");
        assert_eq!(out, "kind: Config\nspec: {}\n");
    }

    #[test]
    fn idempotent_when_output_already_carries_block() {
        let original = "# banner\nkind: Config\n";
        let once = with_leading_comments(original, "kind: Config\n");
        let twice = with_leading_comments(original, &once);
        assert_eq!(once, twice);
        assert_eq!(once, "# banner\nkind: Config\n");
    }

    #[test]
    fn comment_only_original_without_trailing_newline_gets_separator() {
        let out = with_leading_comments("# lone", "kind: Config\n");
        assert_eq!(out, "# lone\nkind: Config\n");
    }
}
