//! Copilot skill provider — renders an IDE prompt file under
//! `.github/prompts/cfgd-<kind>.prompt.md`. IDE-only (project scope); Copilot CLI
//! cannot invoke prompt files yet, and there is no user-scope primitive.

use std::path::PathBuf;

use super::{Detection, RenderedSkill, SkillProvider, SkillScope, render_skill_body};
use crate::generate::{SkillKind, SkillModel};

/// Copilot: IDE prompt file (`.prompt.md`), project scope only.
pub struct CopilotProvider;

/// The `.github/prompts/cfgd-<token>.prompt.md` path relative to the project root,
/// shared by `render` (relative form) and `target_path` (absolute form).
fn relative_prompt_path(token: &str) -> PathBuf {
    PathBuf::from(format!(".github/prompts/cfgd-{token}.prompt.md"))
}

impl SkillProvider for CopilotProvider {
    fn id(&self) -> &'static str {
        "copilot"
    }

    fn detect(&self, scope: SkillScope) -> Detection {
        let found = match scope {
            // `.github/` is the project-local marker; pure fs check, no shell-out.
            SkillScope::Project => std::env::current_dir()
                .ok()
                .is_some_and(|d| d.join(".github").exists()),
            // Copilot prompt files are an IDE/project primitive only — there is no
            // user-global location to install into.
            SkillScope::User => {
                return Detection::Unsupported(
                    "copilot prompt files are project-only (.github/prompts); no user-scope primitive"
                        .to_string(),
                );
            }
        };
        Detection::present(found)
    }

    fn target_path(&self, kind: SkillKind, scope: SkillScope) -> Option<PathBuf> {
        match scope {
            SkillScope::Project => Some(
                std::env::current_dir()
                    .ok()?
                    .join(relative_prompt_path(kind.command_token())),
            ),
            SkillScope::User => None,
        }
    }

    fn render(&self, model: &SkillModel) -> RenderedSkill {
        let token = model.kind.command_token();
        // `mode: agent` is the native Copilot prompt-file field; the two `cfgd-*`
        // keys live in the SAME frontmatter block so
        // [`parse_version_stamp`](super::parse_version_stamp) reads the stamped
        // version, matching every other provider. The frontmatter is hand-rolled
        // (not a serde kebab-rename struct) to carry the literal `cfgd-version` /
        // `cfgd-min-version` keys the parser expects.
        let contents = format!(
            "---\n\
             mode: agent\n\
             description: {description}\n\
             cfgd-version: {version}\n\
             cfgd-min-version: {min}\n\
             ---\n\
             \n\
             {body}",
            description = model.description,
            version = model.schema_snapshot.cfgd_version,
            min = model.min_cfgd_version,
            body = render_skill_body(model),
        );
        RenderedSkill {
            relative_path: relative_prompt_path(token),
            contents,
            managed_section: None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::{SkillKind, skill_model_for};

    #[test]
    fn copilot_renders_prompt_md_with_agent_mode() {
        let model = skill_model_for(SkillKind::Module);
        let r = CopilotProvider.render(&model);
        assert!(r.relative_path.ends_with("cfgd-module.prompt.md"));
        assert!(r.contents.contains("mode: agent"));
        assert!(r.contents.contains("cfgd explain module")); // body carried through
        assert!(r.managed_section.is_none());
    }

    #[test]
    fn copilot_user_scope_is_unsupported() {
        assert!(matches!(
            CopilotProvider.detect(SkillScope::User),
            Detection::Unsupported(_)
        ));
        assert!(
            CopilotProvider
                .target_path(SkillKind::Module, SkillScope::User)
                .is_none()
        );
    }

    #[test]
    fn frontmatter_carries_version_stamp_keys_that_parse() {
        let model = skill_model_for(SkillKind::Profile);
        let r = CopilotProvider.render(&model);
        assert!(r.contents.contains(&format!(
            "cfgd-version: {}",
            model.schema_snapshot.cfgd_version
        )));
        assert!(
            r.contents
                .contains(&format!("cfgd-min-version: {}", model.min_cfgd_version))
        );
        // The shared frontmatter parser reads the stamp the same way `list` does.
        assert_eq!(
            crate::providers::skill::parse_version_stamp(&r.contents).as_deref(),
            Some(model.schema_snapshot.cfgd_version.as_str())
        );
    }
}
