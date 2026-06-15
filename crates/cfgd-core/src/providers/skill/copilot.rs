//! Copilot skill provider — renders an IDE prompt file under
//! `.github/prompts/cfgd-<kind>.prompt.md`. IDE-only (project scope); Copilot CLI
//! cannot invoke prompt files yet, and there is no user-scope primitive.

use std::path::PathBuf;

use super::{Detection, RenderedSkill, SkillProvider, SkillScope};
use crate::generate::{SkillKind, SkillModel};

/// Copilot: IDE prompt file (`.prompt.md`), project scope only.
pub struct CopilotProvider;

impl SkillProvider for CopilotProvider {
    fn id(&self) -> &'static str {
        "copilot"
    }

    fn detect(&self, _scope: SkillScope) -> Detection {
        Detection::Absent
    }

    fn target_path(&self, kind: SkillKind, _scope: SkillScope) -> Option<PathBuf> {
        Some(PathBuf::from(format!(
            ".github/prompts/cfgd-{}.prompt.md",
            kind.command_token()
        )))
    }

    fn render(&self, model: &SkillModel) -> RenderedSkill {
        RenderedSkill {
            relative_path: PathBuf::from(format!(
                ".github/prompts/cfgd-{}.prompt.md",
                model.kind.command_token()
            )),
            contents: String::new(),
            managed_section: None,
        }
    }
}
