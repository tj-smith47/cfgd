//! Gemini skill provider — renders a TOML custom command under
//! `.gemini/commands/cfgd-<kind>.toml`.
//!
//! A flat `cfgd-<token>.toml` maps to the slash command `/cfgd-<token>`; nested
//! directories would create `:`-namespaced commands, so the flat layout is the
//! decision (matching the `cfgd-<token>` naming across every provider).

use std::path::{Path, PathBuf};

use super::{Detection, RenderedSkill, SkillProvider, SkillScope, render_skill_body};
use crate::generate::{SkillKind, SkillModel};
use crate::{command_available, expand_tilde};

/// Gemini: TOML custom command (`description` + `prompt`).
pub struct GeminiProvider;

/// The `.gemini/commands/cfgd-<token>.toml` path relative to a scope root, shared
/// by `render` (relative form) and `target_path` (absolute form).
fn relative_command_path(token: &str) -> PathBuf {
    PathBuf::from(format!(".gemini/commands/cfgd-{token}.toml"))
}

impl SkillProvider for GeminiProvider {
    fn id(&self) -> &'static str {
        "gemini"
    }

    fn detect(&self, scope: SkillScope) -> Detection {
        let present = match scope {
            SkillScope::Project => std::env::current_dir()
                .ok()
                .is_some_and(|d| d.join(".gemini").exists()),
            SkillScope::User => {
                expand_tilde(Path::new("~/.gemini")).exists() || command_available("gemini")
            }
        };
        if present {
            Detection::Present
        } else {
            Detection::Absent
        }
    }

    fn target_path(&self, kind: SkillKind, scope: SkillScope) -> Option<PathBuf> {
        let token = kind.command_token();
        match scope {
            SkillScope::Project => Some(
                std::env::current_dir()
                    .ok()?
                    .join(relative_command_path(token)),
            ),
            SkillScope::User => Some(expand_tilde(
                &Path::new("~/.gemini/commands").join(format!("cfgd-{token}.toml")),
            )),
        }
    }

    fn render(&self, model: &SkillModel) -> RenderedSkill {
        let token = model.kind.command_token();
        // `description` and `prompt` are the native fields the Gemini CLI reads;
        // the two `cfgd-*` keys are ignored by Gemini but let
        // [`parse_version_stamp`](super::parse_version_stamp) read the stamped
        // version, matching every other provider's native-metadata stamp. The
        // keys are inserted as explicit kebab-case strings (not via a serde
        // rename) so the value carries the literal `cfgd-version` /
        // `cfgd-min-version` keys `parse_version_stamp` expects, with the `toml`
        // crate handling all string escaping.
        let mut table = toml::map::Map::new();
        table.insert(
            "description".to_string(),
            toml::Value::String(model.description.clone()),
        );
        table.insert(
            "prompt".to_string(),
            toml::Value::String(render_skill_body(model)),
        );
        table.insert(
            "cfgd-version".to_string(),
            toml::Value::String(model.schema_snapshot.cfgd_version.clone()),
        );
        table.insert(
            "cfgd-min-version".to_string(),
            toml::Value::String(model.min_cfgd_version.to_string()),
        );
        // The table holds only `String` values under fixed keys, so TOML
        // serialization is total — the `Err` arm is unreachable. The trait's
        // `render` is infallible by signature, so rather than `unwrap` (Hard Rule
        // 2) the impossible error is logged and degraded to an empty (still valid)
        // TOML document, keeping the surface infallible without a silent panic.
        let contents = match toml::to_string(&toml::Value::Table(table)) {
            Ok(s) => s,
            Err(e) => {
                tracing::error!(provider = "gemini", error = %e, "gemini TOML serialization failed (unreachable for String-only values)");
                String::new()
            }
        };
        RenderedSkill {
            relative_path: relative_command_path(token),
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
    fn gemini_renders_valid_toml_command() {
        let model = skill_model_for(SkillKind::Module);
        let r = GeminiProvider.render(&model);
        assert!(r.relative_path.ends_with("cfgd-module.toml"));
        assert!(r.managed_section.is_none());
        // round-trips as TOML and carries the body + description
        let parsed: toml::Value = toml::from_str(&r.contents).expect("valid TOML");
        assert!(
            parsed
                .get("prompt")
                .and_then(|v| v.as_str())
                .expect("prompt key")
                .contains("cfgd explain module")
        );
        assert!(parsed.get("description").is_some());
    }

    #[test]
    fn toml_carries_version_stamp_keys_that_parse() {
        let model = skill_model_for(SkillKind::Profile);
        let r = GeminiProvider.render(&model);
        let parsed: toml::Value = toml::from_str(&r.contents).expect("valid TOML");
        assert_eq!(
            parsed.get("cfgd-version").and_then(|v| v.as_str()),
            Some(model.schema_snapshot.cfgd_version.as_str())
        );
        assert_eq!(
            parsed.get("cfgd-min-version").and_then(|v| v.as_str()),
            Some(model.min_cfgd_version.to_string().as_str())
        );
        // The shared metadata parser reads the stamp the same way `list` does.
        assert_eq!(
            crate::providers::skill::parse_version_stamp(&r.contents).as_deref(),
            Some(model.schema_snapshot.cfgd_version.as_str())
        );
    }

    #[test]
    fn user_target_path_is_absolute_under_home() {
        let home = tempfile::tempdir().expect("tempdir");
        crate::with_test_home(home.path(), || {
            let target = GeminiProvider
                .target_path(SkillKind::Module, SkillScope::User)
                .expect("user scope always has a target");
            assert!(target.is_absolute(), "target must be absolute: {target:?}");
            assert!(target.starts_with(home.path()));
            assert!(target.ends_with(".gemini/commands/cfgd-module.toml"));
        });
    }

    #[test]
    #[serial_test::serial]
    fn user_detect_reflects_home_gemini_dir() {
        let home = tempfile::tempdir().expect("tempdir");
        crate::with_test_home(home.path(), || {
            // No ~/.gemini and (in CI) no `gemini` on PATH → Absent. When the host
            // happens to have a real `gemini` binary, detection is Present via the
            // PATH probe; the dir-creation half of the contract is still asserted
            // below regardless.
            if !command_available("gemini") {
                assert_eq!(
                    GeminiProvider.detect(SkillScope::User),
                    Detection::Absent,
                    "no ~/.gemini and no gemini on PATH"
                );
            }
            std::fs::create_dir_all(home.path().join(".gemini")).expect("create ~/.gemini");
            assert_eq!(GeminiProvider.detect(SkillScope::User), Detection::Present);
        });
    }
}
