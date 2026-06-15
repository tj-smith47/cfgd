//! Gemini skill provider — renders a TOML custom command under
//! `.gemini/commands/cfgd-<kind>.toml`.

use std::path::PathBuf;

use super::{Detection, RenderedSkill, SkillProvider, SkillScope};
use crate::generate::{SkillKind, SkillModel};

/// Gemini: TOML custom command (`description` + `prompt`).
pub struct GeminiProvider;

impl SkillProvider for GeminiProvider {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn detect(&self, _scope: SkillScope) -> Detection {
        Detection::Absent
    }

    fn target_path(&self, kind: SkillKind, _scope: SkillScope) -> Option<PathBuf> {
        Some(PathBuf::from(format!(
            ".gemini/commands/cfgd-{}.toml",
            kind.command_token()
        )))
    }

    fn render(&self, model: &SkillModel) -> RenderedSkill {
        RenderedSkill {
            relative_path: PathBuf::from(format!(
                ".gemini/commands/cfgd-{}.toml",
                model.kind.command_token()
            )),
            contents: String::new(),
            managed_section: None,
        }
    }
}
