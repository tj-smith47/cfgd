//! Cursor skill provider — renders a `.mdc` rule under
//! `.cursor/rules/cfgd-<kind>.mdc`. Project-only primitive; there is no
//! user-scope target.

use std::path::PathBuf;

use super::{Detection, RenderedSkill, SkillProvider, SkillScope, render_skill_body};
use crate::generate::{SkillKind, SkillModel};

/// Cursor: `.mdc` rule, project scope only.
pub struct CursorProvider;

/// The `.cursor/rules/cfgd-<token>.mdc` path relative to the project root, shared
/// by `render` (relative form) and `target_path` (absolute form).
fn relative_rule_path(token: &str) -> PathBuf {
    PathBuf::from(format!(".cursor/rules/cfgd-{token}.mdc"))
}

impl SkillProvider for CursorProvider {
    fn id(&self) -> &'static str {
        "cursor"
    }

    fn detect(&self, scope: SkillScope) -> Detection {
        match scope {
            // `.cursor/` is the project-local marker; pure fs check, no shell-out.
            SkillScope::Project => {
                let present = std::env::current_dir()
                    .ok()
                    .is_some_and(|d| d.join(".cursor").exists());
                if present {
                    Detection::Present
                } else {
                    Detection::Absent
                }
            }
            // Cursor rules are a project-only primitive (`.cursor/rules`); there is
            // no user-global location, so `-g` should skip with a reported warning
            // rather than fabricate a target.
            SkillScope::User => Detection::Unsupported(
                "cursor rules are project-only (.cursor/rules); no user-scope primitive"
                    .to_string(),
            ),
        }
    }

    fn target_path(&self, kind: SkillKind, scope: SkillScope) -> Option<PathBuf> {
        match scope {
            SkillScope::Project => Some(
                std::env::current_dir()
                    .ok()?
                    .join(relative_rule_path(kind.command_token())),
            ),
            SkillScope::User => None,
        }
    }

    fn render(&self, model: &SkillModel) -> RenderedSkill {
        let token = model.kind.command_token();
        // `description` is the native `.mdc` frontmatter field Cursor reads; the
        // two `cfgd-*` keys live in the SAME frontmatter block so
        // [`parse_version_stamp`](super::parse_version_stamp) reads the stamped
        // version, matching every other provider. The frontmatter is hand-rolled
        // (not a serde kebab-rename struct) to carry the literal `cfgd-version` /
        // `cfgd-min-version` keys the parser expects.
        let contents = format!(
            "---\n\
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
            relative_path: relative_rule_path(token),
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
    fn cursor_renders_mdc_with_description_frontmatter() {
        let model = skill_model_for(SkillKind::Module);
        let r = CursorProvider.render(&model);
        assert!(r.relative_path.ends_with("cfgd-module.mdc"));
        assert!(r.contents.starts_with("---\n"));
        assert!(r.contents.contains("description:"));
        assert!(r.contents.contains("cfgd explain module")); // body carried through
        assert!(r.managed_section.is_none());
    }

    #[test]
    fn cursor_user_scope_is_unsupported() {
        assert!(matches!(
            CursorProvider.detect(SkillScope::User),
            Detection::Unsupported(_)
        ));
        assert!(
            CursorProvider
                .target_path(SkillKind::Module, SkillScope::User)
                .is_none()
        );
    }

    #[test]
    fn frontmatter_carries_version_stamp_keys_that_parse() {
        let model = skill_model_for(SkillKind::Profile);
        let r = CursorProvider.render(&model);
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
