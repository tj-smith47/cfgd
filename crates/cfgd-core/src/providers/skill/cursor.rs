//! Cursor skill provider — renders a `.mdc` rule under
//! `.cursor/rules/cfgd-<kind>.mdc`. Project-only primitive; there is no
//! user-scope target.

use std::path::PathBuf;

use super::{
    Detection, RenderedSkill, SkillProvider, SkillScope, frontmatter_envelope, render_skill_body,
};
use crate::errors::Result;
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
        let found = match scope {
            // `.cursor/` is the project-local marker; pure fs check, no shell-out.
            SkillScope::Project => std::env::current_dir()
                .ok()
                .is_some_and(|d| d.join(".cursor").exists()),
            // Cursor rules are a project-only primitive (`.cursor/rules`); there is
            // no user-global location, so `-g` should skip with a reported warning
            // rather than fabricate a target.
            SkillScope::User => {
                return Detection::Unsupported(
                    "cursor rules are project-only (.cursor/rules); no user-scope primitive"
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
                    .join(relative_rule_path(kind.command_token())),
            ),
            SkillScope::User => None,
        }
    }

    fn render(&self, model: &SkillModel) -> Result<RenderedSkill> {
        let token = model.kind.command_token();
        // `description` is the native `.mdc` frontmatter field Cursor reads; the
        // shared `frontmatter_envelope` appends the `cfgd-version` /
        // `cfgd-min-version` stamp keys into the SAME frontmatter block so
        // [`parse_version_stamp`](super::parse_version_stamp) reads them back,
        // matching every other frontmatter provider.
        let contents = frontmatter_envelope(
            model,
            &[format!("description: {}", model.description)],
            &render_skill_body(model),
        );
        Ok(RenderedSkill {
            relative_path: relative_rule_path(token),
            contents,
            managed_section: None,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::{SkillKind, skill_model_for};

    #[test]
    fn cursor_renders_mdc_with_description_frontmatter() {
        let model = skill_model_for(SkillKind::Module);
        let r = CursorProvider
            .render(&model)
            .expect("render is infallible for these fixtures");
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
        let r = CursorProvider
            .render(&model)
            .expect("render is infallible for these fixtures");
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
