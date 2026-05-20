//! Prompts — interaction, not output. Kept API-compatible with the old
//! `output/printer.rs` prompt_* trio so call sites migrate cleanly. Spec §5b:
//! refuse to prompt under structured output (would deadlock pipelines), and
//! honor a test-seeded answer queue (set via `for_test_with_prompt_responses`,
//! wired in T26) so tests can drive prompt_* past the non-interactive guard.

use super::Printer;
use super::printer::PromptAnswer;

/// Build an `InquireError::Custom` for the "structured-output asked for an
/// interactive prompt" case. `-o json|yaml|name|jsonpath|template` implies a
/// non-interactive consumer; hanging on `inquire` here would deadlock scripts.
fn non_interactive_err(prompt: &str) -> inquire::InquireError {
    inquire::InquireError::Custom(
        format!(
            "refusing to prompt for '{prompt}' in non-interactive/structured output \
             mode (re-run without -o json or supply the answer via a flag / env var)"
        )
        .into(),
    )
}

impl Printer {
    pub fn prompt_confirm(&self, message: &str) -> Result<bool, inquire::InquireError> {
        if let Some(answer) = self.pop_prompt_answer()
            && let PromptAnswer::Confirm(b) = answer
        {
            return Ok(b);
        }
        if self.is_structured() {
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
        if self.is_structured() {
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
        if self.is_structured() {
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
}
