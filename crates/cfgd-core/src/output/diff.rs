use similar::{ChangeTag, TextDiff};

use super::{Printer, Verbosity};

impl Printer {
    pub fn diff(&self, old: &str, new: &str) {
        if self.test_buf.is_some() {
            let diff = TextDiff::from_lines(old, new);
            for change in diff.iter_all_changes() {
                let sign = match change.tag() {
                    ChangeTag::Delete => "-",
                    ChangeTag::Insert => "+",
                    ChangeTag::Equal => " ",
                };
                self.capture(&format!("{}{}", sign, change.to_string().trim_end()));
            }
        }
        if self.verbosity == Verbosity::Quiet {
            return;
        }
        let diff = TextDiff::from_lines(old, new);
        for change in diff.iter_all_changes() {
            let (sign, style) = match change.tag() {
                ChangeTag::Delete => ("-", &self.theme.diff_remove),
                ChangeTag::Insert => ("+", &self.theme.diff_add),
                ChangeTag::Equal => (" ", &self.theme.diff_context),
            };
            let line = style.apply_to(format!("{}{}", sign, change));
            let _ = self.term.write_str(&line.to_string());
        }
    }
}
