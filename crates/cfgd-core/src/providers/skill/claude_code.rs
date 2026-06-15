//! Claude Code skill provider — renders a `SKILL.md` (frontmatter + markdown)
//! under `.claude/skills/cfgd-<kind>/`.

use std::path::PathBuf;

use super::{Detection, RenderedSkill, SkillProvider, SkillScope};
use crate::generate::{SkillKind, SkillModel};

/// Claude Code: `SKILL.md` (frontmatter + markdown), 1:1 fit at both scopes.
pub struct ClaudeCodeProvider;

impl SkillProvider for ClaudeCodeProvider {
    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn detect(&self, _scope: SkillScope) -> Detection {
        Detection::Absent
    }

    fn target_path(&self, kind: SkillKind, _scope: SkillScope) -> Option<PathBuf> {
        Some(PathBuf::from(format!(
            ".claude/skills/cfgd-{}/SKILL.md",
            kind.command_token()
        )))
    }

    fn render(&self, model: &SkillModel) -> RenderedSkill {
        RenderedSkill {
            relative_path: PathBuf::from(format!(
                ".claude/skills/cfgd-{}/SKILL.md",
                model.kind.command_token()
            )),
            contents: String::new(),
            managed_section: None,
        }
    }
}
