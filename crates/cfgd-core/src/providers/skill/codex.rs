//! Codex skill provider — renders a managed `cfgd:skill:<kind>` block inside the
//! shared `AGENTS.md` (always-on context, not per-skill invocable).
//!
//! Unlike the whole-file providers, codex co-owns `AGENTS.md` with the user (and
//! other tools): `install` splices only the delimited `cfgd:skill:<token>` block,
//! preserving every surrounding byte. The skill payload therefore rides in
//! [`RenderedSkill::managed_section`], not `contents` (which stays empty — the
//! default `install` ignores it when a managed section is present).

use std::path::{Path, PathBuf};

use super::{
    Detection, ManagedSection, RenderedSkill, SkillProvider, SkillScope, render_skill_body,
};
use crate::generate::{SkillKind, SkillModel};
use crate::{command_available, expand_tilde};

/// Codex: a managed block inside the shared `AGENTS.md`.
pub struct CodexProvider;

/// The `AGENTS.md` path relative to a scope root. Project scope roots it at the
/// CWD; user scope roots it under `~/.codex/`.
fn relative_agents_path() -> PathBuf {
    PathBuf::from("AGENTS.md")
}

impl SkillProvider for CodexProvider {
    fn id(&self) -> &'static str {
        "codex"
    }

    fn detect(&self, scope: SkillScope) -> Detection {
        let present = match scope {
            SkillScope::Project => {
                std::env::current_dir()
                    .ok()
                    .is_some_and(|d| d.join(relative_agents_path()).exists())
                    || command_available("codex")
            }
            SkillScope::User => expand_tilde(Path::new("~/.codex")).exists(),
        };
        if present {
            Detection::Present
        } else {
            Detection::Absent
        }
    }

    fn target_path(&self, _kind: SkillKind, scope: SkillScope) -> Option<PathBuf> {
        match scope {
            SkillScope::Project => Some(std::env::current_dir().ok()?.join(relative_agents_path())),
            SkillScope::User => Some(expand_tilde(Path::new("~/.codex/AGENTS.md"))),
        }
    }

    fn render(&self, model: &SkillModel) -> RenderedSkill {
        // The skill payload rides in the managed section, not `contents`: the
        // default `install` splices `managed_section` into the existing file and
        // ignores `contents` when a section is present, so leaving `contents`
        // empty is the honest representation of what this provider writes. The
        // shared body already carries the `<!-- cfgd-version: … · cfgd-min-version: … -->`
        // stamp that `parse_version_stamp` reads back out of the block.
        RenderedSkill {
            relative_path: relative_agents_path(),
            contents: String::new(),
            managed_section: Some(ManagedSection::for_kind(
                model.kind,
                render_skill_body(model),
            )),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::generate::{SkillKind, skill_model_for};

    #[test]
    fn codex_renders_managed_section_with_delimiters() {
        let model = skill_model_for(SkillKind::Module);
        let r = CodexProvider.render(&model);
        assert!(r.relative_path.ends_with("AGENTS.md"));
        let section = r.managed_section.expect("codex uses a managed section");
        assert!(section.begin.contains("cfgd:skill:module"));
        assert!(section.end.contains("/cfgd:skill:module"));
        assert!(section.body.contains("cfgd explain module")); // shared body carried through
    }

    #[test]
    fn contents_is_empty_and_payload_rides_in_the_block() {
        let model = skill_model_for(SkillKind::Profile);
        let r = CodexProvider.render(&model);
        // `contents` is empty by design (default `install` ignores it for a
        // managed-section provider); the on-disk bytes come from the spliced
        // block, which carries the body and a parser-readable version stamp.
        assert!(r.contents.is_empty());
        let effective = r.effective_fresh_install();
        assert!(effective.contains("<!-- cfgd:skill:profile -->"));
        assert!(effective.contains("<!-- /cfgd:skill:profile -->"));
        assert_eq!(
            crate::providers::skill::parse_version_stamp(&effective).as_deref(),
            Some(model.schema_snapshot.cfgd_version.as_str()),
            "the block's body stamp must be readable by the same parser `list` uses"
        );
    }

    #[test]
    fn user_target_path_is_absolute_under_home_codex() {
        let home = tempfile::tempdir().expect("tempdir");
        crate::with_test_home(home.path(), || {
            let target = CodexProvider
                .target_path(SkillKind::Module, SkillScope::User)
                .expect("user scope always has a target");
            assert!(target.is_absolute(), "target must be absolute: {target:?}");
            assert!(target.starts_with(home.path()));
            assert!(target.ends_with(".codex/AGENTS.md"));
        });
    }

    #[test]
    #[serial_test::serial]
    fn user_detect_reflects_home_codex_dir() {
        let home = tempfile::tempdir().expect("tempdir");
        crate::with_test_home(home.path(), || {
            assert_eq!(
                CodexProvider.detect(SkillScope::User),
                Detection::Absent,
                "no ~/.codex yet"
            );
            std::fs::create_dir_all(home.path().join(".codex")).expect("create ~/.codex");
            assert_eq!(CodexProvider.detect(SkillScope::User), Detection::Present);
        });
    }
}
