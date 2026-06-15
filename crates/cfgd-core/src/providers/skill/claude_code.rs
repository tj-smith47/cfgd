//! Claude Code skill provider — renders a `SKILL.md` (frontmatter + markdown)
//! under `.claude/skills/cfgd-<kind>/`.

use std::path::{Path, PathBuf};

use super::{
    Detection, RenderedSkill, SkillProvider, SkillScope, frontmatter_envelope, render_skill_body,
};
use crate::errors::Result;
use crate::expand_tilde;
use crate::generate::{SkillKind, SkillModel};

/// Claude Code: `SKILL.md` (frontmatter + markdown), 1:1 fit at both scopes.
pub struct ClaudeCodeProvider;

/// The `.claude/skills/cfgd-<token>/SKILL.md` path relative to a scope root,
/// shared by `render` (relative form) and `target_path` (absolute form).
fn relative_skill_path(token: &str) -> PathBuf {
    PathBuf::from(format!(".claude/skills/cfgd-{token}/SKILL.md"))
}

impl SkillProvider for ClaudeCodeProvider {
    fn id(&self) -> &'static str {
        "claude-code"
    }

    fn detect(&self, scope: SkillScope) -> Detection {
        let found = match scope {
            SkillScope::Project => std::env::current_dir()
                .ok()
                .is_some_and(|d| d.join(".claude").exists()),
            SkillScope::User => expand_tilde(Path::new("~/.claude")).exists(),
        };
        Detection::present(found)
    }

    fn target_path(&self, kind: SkillKind, scope: SkillScope) -> Option<PathBuf> {
        let relative = relative_skill_path(kind.command_token());
        match scope {
            SkillScope::Project => Some(std::env::current_dir().ok()?.join(relative)),
            SkillScope::User => Some(expand_tilde(&Path::new("~").join(relative))),
        }
    }

    fn render(&self, model: &SkillModel) -> Result<RenderedSkill> {
        let token = model.kind.command_token();
        let contents = frontmatter_envelope(
            model,
            &[
                format!("name: cfgd-{token}"),
                format!("description: {}", model.description),
                "user-invocable: true".to_string(),
            ],
            &render_skill_body(model),
        );
        Ok(RenderedSkill {
            relative_path: relative_skill_path(token),
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
    fn claude_code_renders_valid_skill_md() {
        let model = skill_model_for(SkillKind::Module);
        let r = ClaudeCodeProvider
            .render(&model)
            .expect("render is infallible for these fixtures");
        assert!(r.relative_path.ends_with("cfgd-module/SKILL.md"));
        assert!(r.contents.starts_with("---\n"));
        assert!(r.contents.contains("name: cfgd-module"));
        assert!(r.contents.contains("cfgd explain module")); // body carried through
        assert!(r.managed_section.is_none());
    }

    #[test]
    fn frontmatter_carries_version_stamp_keys_that_parse() {
        let model = skill_model_for(SkillKind::Profile);
        let r = ClaudeCodeProvider
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

    #[test]
    fn user_target_path_is_absolute_under_home() {
        let home = tempfile::tempdir().expect("tempdir");
        crate::with_test_home(home.path(), || {
            let target = ClaudeCodeProvider
                .target_path(SkillKind::Module, SkillScope::User)
                .expect("user scope always has a target");
            assert!(target.is_absolute(), "target must be absolute: {target:?}");
            assert!(target.starts_with(home.path()));
            assert!(target.ends_with(".claude/skills/cfgd-module/SKILL.md"));
        });
    }

    #[test]
    #[serial_test::serial]
    fn user_detect_reflects_home_claude_dir() {
        let home = tempfile::tempdir().expect("tempdir");
        crate::with_test_home(home.path(), || {
            assert_eq!(
                ClaudeCodeProvider.detect(SkillScope::User),
                Detection::Absent,
                "no ~/.claude yet"
            );
            std::fs::create_dir_all(home.path().join(".claude")).expect("create ~/.claude");
            assert_eq!(
                ClaudeCodeProvider.detect(SkillScope::User),
                Detection::Present
            );
        });
    }
}
