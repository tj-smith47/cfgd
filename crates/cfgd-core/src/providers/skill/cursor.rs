//! Cursor skill provider — renders a `.mdc` rule under
//! `.cursor/rules/cfgd-<kind>.mdc`. Project-only primitive; there is no
//! user-scope target.

use std::path::PathBuf;

use super::{Detection, RenderedSkill, SkillProvider, SkillScope};
use crate::generate::{SkillKind, SkillModel};

/// Cursor: `.mdc` rule, project scope only.
pub struct CursorProvider;

impl SkillProvider for CursorProvider {
    fn id(&self) -> &'static str {
        "cursor"
    }

    fn detect(&self, _scope: SkillScope) -> Detection {
        Detection::Absent
    }

    fn target_path(&self, kind: SkillKind, _scope: SkillScope) -> Option<PathBuf> {
        Some(PathBuf::from(format!(
            ".cursor/rules/cfgd-{}.mdc",
            kind.command_token()
        )))
    }

    fn render(&self, model: &SkillModel) -> RenderedSkill {
        RenderedSkill {
            relative_path: PathBuf::from(format!(
                ".cursor/rules/cfgd-{}.mdc",
                model.kind.command_token()
            )),
            contents: String::new(),
            managed_section: None,
        }
    }
}
