//! Codex skill provider — renders a managed `cfgd:skill:<kind>` block appended to
//! `AGENTS.md` (always-on context, not per-skill invocable).

use std::path::PathBuf;

use super::{Detection, ManagedSection, RenderedSkill, SkillProvider, SkillScope};
use crate::generate::{SkillKind, SkillModel};

/// Codex: a managed block inside the shared `AGENTS.md`.
pub struct CodexProvider;

impl SkillProvider for CodexProvider {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn detect(&self, _scope: SkillScope) -> Detection {
        Detection::Absent
    }

    fn target_path(&self, _kind: SkillKind, _scope: SkillScope) -> Option<PathBuf> {
        Some(PathBuf::from("AGENTS.md"))
    }

    fn render(&self, model: &SkillModel) -> RenderedSkill {
        RenderedSkill {
            relative_path: PathBuf::from("AGENTS.md"),
            contents: String::new(),
            managed_section: Some(ManagedSection::for_kind(model.kind, String::new())),
        }
    }
}
