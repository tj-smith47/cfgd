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
/// an interactive prompt" case — structured output, non-TTY stdin, or a piped
/// CI runner. Hanging on `inquire` here would deadlock scripts and silently
/// stall CI.
fn non_interactive_err(prompt: &str) -> inquire::InquireError {
    inquire::InquireError::Custom(
        format!(
            "refusing to prompt for '{prompt}' in non-interactive/structured output \
             mode (re-run without -o json or supply the answer via a flag / env var)"
        )
        .into(),
    )
}

/// True when the current process can interact with a human — stdin is a TTY.
/// Windows' `inquire` doesn't self-reject the non-TTY case, so the explicit
/// gate goes here.
fn stdin_is_tty() -> bool {
    std::io::stdin().is_terminal()
}

impl Printer {
    pub fn prompt_confirm(&self, message: &str) -> Result<bool, inquire::InquireError> {
        if let Some(answer) = self.pop_prompt_answer()
            && let PromptAnswer::Confirm(b) = answer
        {
            return Ok(b);
        }
        if self.is_structured() || !stdin_is_tty() {
            return Err(non_interactive_err(message));
        }
        inquire::Confirm::new(message).with_default(false).prompt()
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
        if self.is_structured() || !stdin_is_tty() {
            return Err(non_interactive_err(message));
        }
        if options.is_empty() {
            return Err(inquire::InquireError::Custom("no options available".into()));
        }
        let chosen = inquire::Select::new(message, options.to_vec()).prompt()?;
        Ok(options
            .iter()
            .find(|o| **o == chosen)
            .unwrap_or(&options[0]))
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
        if self.is_structured() || !stdin_is_tty() {
            return Err(non_interactive_err(message));
        }
        inquire::Text::new(message).with_default(default).prompt()
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
        let p = Printer::with_format(Verbosity::Normal, None, OutputFormat::Json);
        let r = p.prompt_confirm("really?");
        assert!(r.is_err());
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
        let p = Printer::with_format(Verbosity::Normal, None, OutputFormat::Json);
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
        let p = Printer::with_format(Verbosity::Normal, None, OutputFormat::Json);
        let err = p.prompt_text("name", "").expect_err("structured refuse");
        let msg = format!("{err}");
        assert!(
            msg.contains("non-interactive") || msg.contains("structured"),
            "expected non-interactive refusal: {msg}"
        );
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
