//! Prompts — interaction, not output. Three invariants:
//!   - Refuse to prompt under structured output (would deadlock pipelines).
//!   - Refuse to prompt when stdin is not a TTY (CI runners, piped invocations).
//!     `inquire` self-rejects this on Unix but blocks on Windows.
//!   - Honor a test-seeded answer queue (set via
//!     `for_test_with_prompt_responses`) so tests can drive prompt_* past the
//!     non-interactive guard.

use std::io::IsTerminal;

use super::Printer;
use super::printer::PromptAnswer;

/// Build an `InquireError::Custom` for the "non-interactive context asked for
/// an interactive prompt" case. Hanging on `inquire` here would deadlock scripts
/// and silently stall CI. The remedy differs by cause, so the message reflects
/// which guard fired: structured output (`-o json`/`yaml`) is dropped by
/// switching back to text output, whereas a non-TTY stdin (piped/CI invocation)
/// can never prompt regardless of `-o`, so the only fix is to supply the answer
/// up front via a flag or environment variable.
fn non_interactive_err(structured: bool, prompt: &str) -> inquire::InquireError {
    let reason = if structured {
        "structured output is active — re-run with `-o text` on a terminal, or supply \
         the answer via a flag / env var (e.g. `--yes` / `CFGD_YES` for confirmations)"
    } else {
        "stdin is not a TTY, so interactive prompts are unavailable — supply the answer \
         via a flag / env var (e.g. `--yes` / `CFGD_YES` for confirmations)"
    };
    inquire::InquireError::Custom(format!("refusing to prompt for '{prompt}': {reason}").into())
}

/// The one fold every prompt applies to the text it draws.
///
/// `inquire` writes straight to the terminal without passing the renderer, so
/// nothing else sanitizes a prompt's message or its option list — and both
/// routinely carry text cfgd did not author: a remote source manifest's
/// profile names, a module name, a file path a plan is asking about.
///
/// It ESCAPES rather than folding through `cursor_safe`, because a prompt is a
/// pre-approval surface: the fold strips an ANSI sequence, and an operator
/// answering "yes" to a value they never saw has approved something other than
/// what they read.
fn shown(text: &str) -> String {
    crate::escape_control_chars(text)
}

/// The fold a prompt applies to a DEFAULT, which is a different slot from the
/// text a prompt shows.
///
/// A message is text being shown, so [`shown`] escapes it and hides none of
/// it. A default is a value cfgd is PROPOSING: `inquire` pre-fills it into the
/// editable buffer and hands it straight back as the answer when the user
/// presses enter. Escaping it would write `\x0d` into the value instead of
/// merely showing it, and leaving it raw lets a proposal repaint the line
/// offering it — a directory basename may legally carry a `\r`, and one is
/// offered as the default source name. Dropping the character is the only
/// resolution under which what is DRAWN and what is RETURNED are the same
/// string. An `ESC` goes with the rest, leaving its payload (`[2K`) standing
/// as ordinary visible text rather than as a sequence.
fn proposed(default: &str) -> String {
    default.chars().filter(|c| !c.is_control()).collect()
}

/// True when the current process can interact with a human — stdin is a TTY.
/// Windows' `inquire` doesn't self-reject the non-TTY case, so the explicit
/// gate goes here.
///
/// Read exactly once per `Printer`, at construction, and stored as
/// `interactive_stdin`; every prompt below asks the printer rather than the
/// process, so a capture printer answers "no human" whatever terminal the
/// suite was invoked from.
pub(super) fn stdin_is_terminal() -> bool {
    std::io::stdin().is_terminal()
}

impl Printer {
    /// Whether a `prompt_*` call could reach a human at all.
    ///
    /// The predicate behind `non_interactive_err`, exposed so a caller that
    /// wraps a prompt failure in its own error can tell "nowhere to ask" apart
    /// from a prompt that was reached and then failed — and word the two
    /// differently instead of quoting the prompt's message back inside its own.
    pub fn can_prompt(&self) -> bool {
        !self.is_structured() && self.interactive_stdin
    }

    pub fn prompt_confirm(&self, message: &str) -> Result<bool, inquire::InquireError> {
        if let Some(answer) = self.pop_prompt_answer()
            && let PromptAnswer::Confirm(b) = answer
        {
            return Ok(b);
        }
        if !self.can_prompt() {
            return Err(non_interactive_err(self.is_structured(), message));
        }
        inquire::Confirm::new(&shown(message))
            .with_default(false)
            .prompt()
    }

    pub fn prompt_select<'a>(
        &self,
        message: &str,
        options: &'a [String],
    ) -> Result<&'a String, inquire::InquireError> {
        if let Some(answer) = self.pop_prompt_answer()
            && let PromptAnswer::Select(s) = answer
        {
            return options.iter().find(|o| **o == s).ok_or_else(|| {
                inquire::InquireError::Custom(
                    format!("test prompt response '{s}' not in option list").into(),
                )
            });
        }
        if !self.can_prompt() {
            return Err(non_interactive_err(self.is_structured(), message));
        }
        if options.is_empty() {
            return Err(inquire::InquireError::Custom("no options available".into()));
        }
        // The list is drawn escaped, so the selection comes back by INDEX
        // rather than by value: matching the drawn string against the raw
        // options would miss exactly the option that carried a control
        // character, and silently answer with the first one instead.
        let shown_options: Vec<String> = options.iter().map(|o| shown(o)).collect();
        let chosen = inquire::Select::new(&shown(message), shown_options).raw_prompt()?;
        options.get(chosen.index).ok_or_else(|| {
            inquire::InquireError::Custom(
                format!(
                    "inquire returned index {} out of range for {} option(s)",
                    chosen.index,
                    options.len()
                )
                .into(),
            )
        })
    }

    pub fn prompt_text(
        &self,
        message: &str,
        default: &str,
    ) -> Result<String, inquire::InquireError> {
        if let Some(answer) = self.pop_prompt_answer()
            && let PromptAnswer::Text(s) = answer
        {
            return Ok(s);
        }
        if !self.can_prompt() {
            return Err(non_interactive_err(self.is_structured(), message));
        }
        inquire::Text::new(&shown(message))
            .with_default(&proposed(default))
            .prompt()
    }

    pub(crate) fn pop_prompt_answer(&self) -> Option<PromptAnswer> {
        self.prompt_queue
            .as_ref()?
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .pop_front()
    }
}

#[cfg(test)]
mod tests {
    use super::super::{OutputFormat, Verbosity};
    use super::*;

    #[test]
    fn structured_mode_refuses_prompt() {
        let p = Printer::with_format(
            Verbosity::Normal,
            None,
            OutputFormat::Json,
            crate::output::ColorChoice::Auto,
        );
        let r = p.prompt_confirm("really?");
        assert!(r.is_err());
    }

    /// `inquire` draws to the terminal itself, so nothing downstream folds a
    /// prompt's message or its option list — the escape here is the only one
    /// they get. It ESCAPES (a pre-approval surface shows what it is asking
    /// about) rather than folding, so the ANSI stays visible as text.
    #[test]
    fn drawn_prompt_text_shows_control_characters_instead_of_obeying_them() {
        let drawn = shown("profile\r\x1b[2Kevil");
        assert_eq!(drawn, "profile\\x0d\\x1b[2Kevil");
        assert!(!drawn.contains('\r') && !drawn.contains('\u{1b}'));
    }

    /// The drawn list is escaped but the ANSWER is the caller's own option,
    /// byte-exact — which is why the selection comes back by index. Matching
    /// the drawn string against the raw list would miss precisely the option
    /// that carried a control character.
    #[test]
    fn escaping_the_drawn_options_preserves_their_order_and_count() {
        let options = [
            "a\rb".to_string(),
            "plain".to_string(),
            "c\x1b[2K".to_string(),
        ];
        let drawn: Vec<String> = options.iter().map(|o| shown(o)).collect();
        assert_eq!(drawn.len(), options.len());
        assert_eq!(drawn[1], "plain", "an untouched option must not move");
        assert!(drawn[0].contains("\\x0d") && drawn[2].contains("\\x1b[2K"));
    }

    /// The DEFAULT is a proposal, not a display, and the two slots are folded
    /// differently for that reason: `inquire` pre-fills it into the editable
    /// buffer and hands it back as the answer, so a `\r` left in it repaints
    /// the line offering it and an escaped copy would put `\x0d` into the
    /// value the user accepts. Dropping the control characters is what makes
    /// the drawn string and the returned string the same string.
    #[test]
    fn a_proposed_default_carries_no_control_character_either_way() {
        let cleaned = proposed("my\rconfig\x1b[2K");
        assert_eq!(
            cleaned, "myconfig[2K",
            "the escape's payload must stand as visible text, not as a sequence"
        );
        assert!(
            !cleaned.contains('\r') && !cleaned.contains('\u{1b}'),
            "a proposed value must not be able to move a cursor: {cleaned:?}"
        );
        assert_eq!(
            proposed("acme-config"),
            "acme-config",
            "an ordinary basename must reach the prompt untouched"
        );
    }

    #[test]
    fn seeded_select_returns_matching_option() {
        let (printer, _buf) =
            Printer::for_test_with_prompt_responses(vec![PromptAnswer::Select("yes".into())]);
        let options = vec!["yes".to_string(), "no".to_string()];
        let chosen = printer
            .prompt_select("pick", &options)
            .expect("seeded select must resolve to a listed option");
        assert_eq!(chosen, "yes");
    }

    #[test]
    fn seeded_select_with_unknown_response_is_custom_error() {
        let (printer, _buf) =
            Printer::for_test_with_prompt_responses(vec![PromptAnswer::Select("missing".into())]);
        let options = vec!["yes".to_string(), "no".to_string()];
        let err = printer
            .prompt_select("pick", &options)
            .expect_err("response not in options must Err");
        let msg = format!("{err}");
        assert!(msg.contains("missing"), "msg must echo unknown: {msg}");
    }

    #[test]
    fn structured_select_refuses_when_no_seeded_answer() {
        let p = Printer::with_format(
            Verbosity::Normal,
            None,
            OutputFormat::Json,
            crate::output::ColorChoice::Auto,
        );
        let options = vec!["a".to_string(), "b".to_string()];
        let err = p
            .prompt_select("pick", &options)
            .expect_err("structured mode must refuse");
        let msg = format!("{err}");
        assert!(
            msg.contains("non-interactive") || msg.contains("structured"),
            "expected non-interactive refusal: {msg}"
        );
    }

    #[test]
    fn seeded_text_returns_value() {
        let (printer, _buf) =
            Printer::for_test_with_prompt_responses(vec![PromptAnswer::Text("answer".into())]);
        let text = printer.prompt_text("name", "").expect("seeded text answer");
        assert_eq!(text, "answer");
    }

    #[test]
    fn structured_text_refuses_when_no_seeded_answer() {
        let p = Printer::with_format(
            Verbosity::Normal,
            None,
            OutputFormat::Json,
            crate::output::ColorChoice::Auto,
        );
        let err = p.prompt_text("name", "").expect_err("structured refuse");
        let msg = format!("{err}");
        assert!(
            msg.contains("non-interactive") || msg.contains("structured"),
            "expected non-interactive refusal: {msg}"
        );
    }

    #[test]
    fn structured_refusal_points_at_output_format_not_tty() {
        // -o json/yaml is the cause → tell the user to drop structured output.
        // It must NOT claim the TTY is the problem.
        let msg = format!("{}", non_interactive_err(true, "Continue?"));
        assert!(msg.contains("structured output"), "msg: {msg}");
        assert!(msg.contains("-o text"), "msg: {msg}");
        assert!(
            !msg.contains("not a TTY"),
            "structured cause must not blame TTY: {msg}"
        );
    }

    #[test]
    fn non_tty_refusal_blames_stdin_not_output_format() {
        // Plain text on a piped stdin → the fix is a flag/env, and re-running
        // "without -o json" is wrong guidance (no -o was passed).
        let msg = format!("{}", non_interactive_err(false, "Continue?"));
        assert!(msg.contains("not a TTY"), "msg: {msg}");
        assert!(msg.contains("flag / env var"), "msg: {msg}");
        assert!(
            !msg.contains("-o json"),
            "non-TTY cause must not mention -o json: {msg}"
        );
        assert!(
            !msg.contains("-o text"),
            "non-TTY cause must not suggest -o text: {msg}"
        );
    }

    /// A capture printer whose seeded queue is empty (or absent) must refuse.
    /// The alternative is `inquire` reading the suite's own terminal, which
    /// blocks until someone types — a hang no test harness reports as a
    /// failure, and one that only appears when the suite is run under a pty.
    #[test]
    fn capture_printer_refuses_an_unqueued_prompt() {
        let (p, _buf) = Printer::for_test_at(Verbosity::Normal);
        assert!(!p.can_prompt());
        let err = p
            .prompt_confirm("really?")
            .expect_err("an unqueued confirm must refuse, never block");
        assert!(format!("{err}").contains("not a TTY"), "msg: {err}");

        // Draining the seeded answer leaves the same printer in the same state:
        // the second ask has nothing to pop and must refuse too.
        let (seeded, _b) =
            Printer::for_test_with_prompt_responses(vec![PromptAnswer::Confirm(true)]);
        assert!(seeded.prompt_confirm("first?").expect("seeded answer"));
        assert!(seeded.prompt_confirm("second?").is_err());
    }

    #[test]
    fn seeded_confirm_returns_bool() {
        let (printer, _b1) =
            Printer::for_test_with_prompt_responses(vec![PromptAnswer::Confirm(true)]);
        assert!(printer.prompt_confirm("really?").expect("seeded confirm"));
        let (printer2, _b2) =
            Printer::for_test_with_prompt_responses(vec![PromptAnswer::Confirm(false)]);
        assert!(
            !printer2
                .prompt_confirm("really?")
                .expect("seeded confirm false")
        );
    }
}
