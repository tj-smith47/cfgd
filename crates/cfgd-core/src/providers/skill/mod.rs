//! Multi-provider agent-skill rendering.
//!
//! A single logical [`SkillModel`](crate::generate::SkillModel) renders to each
//! coding-agent platform's native primitive (Claude Code `SKILL.md`, Gemini TOML
//! command, Copilot prompt file, Codex `AGENTS.md` block, Cursor `.mdc` rule).
//! Each platform implements [`SkillProvider`]; consumers depend on the registry
//! ([`all_skill_providers`]), never on a concrete provider — mirroring the
//! `PackageManager` / `SystemConfigurator` pattern in the parent module.

use std::path::{Path, PathBuf};

use crate::errors::{Result, SkillError};
use crate::generate::{SkillKind, SkillModel};
use crate::{ApplyLockGuard, acquire_apply_lock, atomic_write_str};

mod body;
mod claude_code;
mod codex;
mod copilot;
mod cursor;
mod gemini;

pub use body::render_skill_body;
pub use claude_code::ClaudeCodeProvider;
pub use codex::CodexProvider;
pub use copilot::CopilotProvider;
pub use cursor::CursorProvider;
pub use gemini::GeminiProvider;

/// Which root a skill is installed under: a single project (CWD) or the user's
/// home configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum SkillScope {
    /// Project-local primitive, rooted at the current working directory.
    Project,
    /// User-global primitive, rooted at the user's home configuration.
    User,
}

/// The result of probing whether a provider's agent is present at a scope.
///
/// Detection never shells out: it uses filesystem checks and/or a PATH lookup via
/// [`command_available`](crate::util::process::command_available) (never a fork).
/// A given provider may use either or both (claude-code is filesystem-only).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Detection {
    /// The agent is detected at this scope.
    Present,
    /// The agent is not detected; skipped unless the caller forces a write or
    /// names the provider explicitly.
    Absent,
    /// The provider has no primitive at this scope (e.g. Cursor or Copilot at
    /// user scope). The carried string is the human-readable reason.
    Unsupported(String),
}

/// A skill rendered into one provider's native format.
///
/// `managed_section` distinguishes the two write strategies: `None` means the
/// provider owns its whole file (overwrite via `atomic_write_str`); `Some` means
/// the skill occupies a delimited block inside a file shared with other content
/// (e.g. `AGENTS.md`), and only that block may be rewritten.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedSkill {
    /// Provider-native target path, relative to the scope root.
    pub relative_path: PathBuf,
    /// Fully-rendered native file body. For a managed-section provider this is
    /// the spliced block body alone, not the whole file.
    pub contents: String,
    /// `Some` for a surgical block-edit of a shared file; `None` for a
    /// whole-file provider.
    pub managed_section: Option<ManagedSection>,
}

/// The delimiters and body of a cfgd-managed block inside a file shared with
/// other content.
///
/// `install` rewrites only the bytes between `begin` and `end` (inclusive of the
/// markers), preserving every surrounding byte; `remove` excises that same span.
/// The markers carry the kind so multiple skills can coexist in one file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagedSection {
    /// The opening marker line, e.g. `<!-- cfgd:skill:module -->`.
    pub begin: String,
    /// The closing marker line, e.g. `<!-- /cfgd:skill:module -->`.
    pub end: String,
    /// The body spliced between the markers (excluding the markers themselves).
    pub body: String,
}

impl ManagedSection {
    /// Build the standard HTML-comment delimiters for a kind and pair them with a
    /// body. The markers match `<!-- cfgd:skill:<token> -->` … `<!-- /cfgd:skill:<token> -->`,
    /// where `<token>` is the kind's [`command_token`](SkillKind::command_token).
    pub fn for_kind(kind: SkillKind, body: impl Into<String>) -> Self {
        let token = kind.command_token();
        Self {
            begin: format!("<!-- cfgd:skill:{token} -->"),
            end: format!("<!-- /cfgd:skill:{token} -->"),
            body: body.into(),
        }
    }

    /// The full block as written into the shared file: begin marker, body, end
    /// marker, each on its own line.
    fn block(&self) -> String {
        format!("{}\n{}\n{}", self.begin, self.body, self.end)
    }
}

/// One installed skill, as reported by [`SkillProvider::list`].
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InstalledSkill {
    /// The resource kind the skill teaches.
    pub kind: SkillKind,
    /// The provider id that installed it (`SkillProvider::id`).
    pub provider: String,
    /// Absolute path of the installed native file.
    pub path: PathBuf,
    /// The cfgd version stamped into the file at install time, if readable.
    pub cfgd_version: Option<String>,
    /// `true` when the stamped version differs from the running cfgd version.
    pub stale: bool,
}

/// A coding-agent platform that cfgd can author skills for.
///
/// Implementors render a [`SkillModel`] to their native primitive and report
/// presence via [`detect`](SkillProvider::detect). The `install` / `remove` /
/// `list` defaults drive the filesystem I/O over `render` and `target_path`, so a
/// provider need only describe its format and paths.
pub trait SkillProvider: Send + Sync {
    /// Stable provider id, e.g. `"claude-code"`, `"gemini"`.
    fn id(&self) -> &'static str;

    /// Probe whether this provider's agent is present at `scope`. Never shells
    /// out (filesystem + PATH lookup only).
    fn detect(&self, scope: SkillScope) -> Detection;

    /// The absolute target path for a kind at a scope, or `None` when the
    /// provider has no primitive at that scope.
    fn target_path(&self, kind: SkillKind, scope: SkillScope) -> Option<PathBuf>;

    /// Render a model to this provider's native file format. Pure: no I/O.
    fn render(&self, model: &SkillModel) -> RenderedSkill;

    /// Render and write the skill for `model.kind` at `scope`, returning the
    /// absolute path written.
    ///
    /// Whole-file providers overwrite atomically. Managed-section providers take
    /// a short advisory lock around the read-modify-write so concurrent
    /// invocations cannot corrupt the block delimiters, then splice only the
    /// cfgd-managed block while preserving every surrounding byte.
    fn install(&self, model: &SkillModel, scope: SkillScope) -> Result<PathBuf> {
        let target = self
            .target_path(model.kind, scope)
            .ok_or_else(|| SkillError::Detect {
                provider: self.id().to_string(),
                message: format!("no target path for {} at {scope:?}", model.kind.as_str()),
            })?;
        let rendered = self.render(model);

        if let Some(parent) = target.parent() {
            std::fs::create_dir_all(parent).map_err(SkillError::Write)?;
        }

        match &rendered.managed_section {
            None => {
                atomic_write_str(&target, &rendered.contents).map_err(SkillError::Write)?;
            }
            Some(section) => {
                let _guard = lock_for(&target)?;
                let existing = read_to_string_optional(&target)?;
                let updated = splice_block(existing.as_deref(), section);
                atomic_write_str(&target, &updated).map_err(SkillError::Write)?;
            }
        }
        Ok(target)
    }

    /// Remove a previously-installed skill for `kind` at `scope`.
    ///
    /// Whole-file providers delete the file (and an emptied `cfgd-<kind>` parent
    /// directory). Managed-section providers excise only the cfgd-managed block
    /// under the same advisory lock, leaving surrounding bytes byte-identical.
    /// Returns the path acted on, or `None` when nothing was installed.
    fn remove(&self, kind: SkillKind, scope: SkillScope) -> Result<Option<PathBuf>> {
        let Some(target) = self.target_path(kind, scope) else {
            return Ok(None);
        };
        let rendered = self.render(&crate::generate::skill_model_for(kind));

        match &rendered.managed_section {
            None => {
                if !target.exists() {
                    return Ok(None);
                }
                std::fs::remove_file(&target).map_err(SkillError::Write)?;
                remove_empty_skill_dir(&target);
                Ok(Some(target))
            }
            Some(section) => {
                let _guard = lock_for(&target)?;
                let Some(existing) = read_to_string_optional(&target)? else {
                    return Ok(None);
                };
                match excise_block(&existing, section) {
                    Some(updated) => {
                        atomic_write_str(&target, &updated).map_err(SkillError::Write)?;
                        Ok(Some(target))
                    }
                    None => Ok(None),
                }
            }
        }
    }

    /// List the skills this provider has installed at `scope`, comparing each
    /// file's stamped cfgd version to the running version to flag staleness.
    fn list(&self, scope: SkillScope) -> Result<Vec<InstalledSkill>> {
        let running = running_cfgd_version();
        let mut out = Vec::new();
        for kind in ALL_SKILL_KINDS {
            let Some(target) = self.target_path(kind, scope) else {
                continue;
            };
            let rendered = self.render(&crate::generate::skill_model_for(kind));
            let present = match &rendered.managed_section {
                None => target.exists(),
                Some(section) => read_to_string_optional(&target)?
                    .map(|c| c.contains(&section.begin))
                    .unwrap_or(false),
            };
            if !present {
                continue;
            }
            let cfgd_version = read_to_string_optional(&target)?
                .as_deref()
                .and_then(parse_version_stamp);
            let stale = cfgd_version
                .as_deref()
                .map(|v| v != running)
                .unwrap_or(false);
            out.push(InstalledSkill {
                kind,
                provider: self.id().to_string(),
                path: target,
                cfgd_version,
                stale,
            });
        }
        Ok(out)
    }
}

/// Every author-facing kind a skill can teach, in stable order.
const ALL_SKILL_KINDS: [SkillKind; 6] = [
    SkillKind::Module,
    SkillKind::Profile,
    SkillKind::Source,
    SkillKind::MachineConfig,
    SkillKind::ConfigPolicy,
    SkillKind::ClusterConfigPolicy,
];

/// The cfgd version this binary reports, used to flag stale installed skills.
fn running_cfgd_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Acquire the advisory lock guarding a managed-section file's read-modify-write,
/// keyed off the target's parent directory so the lock is colocated with the file
/// it protects.
fn lock_for(target: &Path) -> Result<ApplyLockGuard> {
    let dir = target.parent().unwrap_or_else(|| Path::new("."));
    acquire_apply_lock(dir).map_err(|e| SkillError::Lock(Box::new(e)).into())
}

/// Read a file to a string, mapping a missing file to `None` and any other I/O
/// error to [`SkillError::Write`].
fn read_to_string_optional(path: &Path) -> Result<Option<String>> {
    match std::fs::read_to_string(path) {
        Ok(s) => Ok(Some(s)),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(SkillError::Write(e).into()),
    }
}

/// Splice a managed block into an existing file body. If the block's begin marker
/// is already present, its span is replaced in place; otherwise the block is
/// appended (separated by a blank line when the file is non-empty).
fn splice_block(existing: Option<&str>, section: &ManagedSection) -> String {
    let block = section.block();
    let Some(existing) = existing else {
        return format!("{block}\n");
    };
    match block_span(existing, section) {
        Some((start, end)) => {
            let mut out = String::with_capacity(existing.len() + block.len());
            out.push_str(&existing[..start]);
            out.push_str(&block);
            out.push_str(&existing[end..]);
            out
        }
        None => {
            let trimmed = existing.trim_end_matches('\n');
            if trimmed.is_empty() {
                format!("{block}\n")
            } else {
                format!("{trimmed}\n\n{block}\n")
            }
        }
    }
}

/// Excise a managed block from an existing file body, returning the new contents,
/// or `None` if the block is absent. Collapses the blank line that preceded the
/// block so repeated install/remove cycles do not accumulate whitespace.
fn excise_block(existing: &str, section: &ManagedSection) -> Option<String> {
    let (start, end) = block_span(existing, section)?;
    let before = existing[..start].trim_end_matches('\n');
    let after = existing[end..].trim_start_matches('\n');
    let out = match (before.is_empty(), after.is_empty()) {
        (true, true) => String::new(),
        (true, false) => format!("{after}\n"),
        (false, true) => format!("{before}\n"),
        (false, false) => format!("{before}\n\n{after}\n"),
    };
    Some(out)
}

/// Locate the byte span `[start, end)` of a managed block (begin marker through
/// end marker inclusive) within `haystack`, or `None` if the begin marker is
/// absent or the end marker does not follow it.
fn block_span(haystack: &str, section: &ManagedSection) -> Option<(usize, usize)> {
    let start = haystack.find(&section.begin)?;
    let end_marker_at = haystack[start..].find(&section.end)? + start;
    let end = end_marker_at + section.end.len();
    Some((start, end))
}

/// Extract the `cfgd-version` stamp from a rendered file body, scanning
/// frontmatter keys (`cfgd-version: X`), TOML keys (`cfgd-version = "X"`), and
/// `AGENTS.md` comment lines uniformly. Accepts either a `:` or `=` separator so
/// every provider's native metadata shape is read by the same parser.
fn parse_version_stamp(contents: &str) -> Option<String> {
    for line in contents.lines() {
        let line = line.trim().trim_start_matches(['#', '-', ' ']).trim();
        let Some(rest) = line.strip_prefix("cfgd-version") else {
            continue;
        };
        let Some(value) = rest.trim_start().strip_prefix([':', '=']) else {
            continue;
        };
        let value = value.trim().trim_matches(['"', '\'']);
        if !value.is_empty() {
            return Some(value.to_string());
        }
    }
    None
}

/// Delete an emptied `cfgd-<kind>` skill directory left after removing a
/// whole-file skill. Best-effort: a non-empty or shared directory is left alone.
fn remove_empty_skill_dir(target: &Path) {
    if let Some(parent) = target.parent()
        && parent
            .file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.starts_with("cfgd-"))
    {
        let _ = std::fs::remove_dir(parent);
    }
}

/// Every concrete skill provider, in stable order. Consumers iterate this rather
/// than naming a provider directly.
pub fn all_skill_providers() -> Vec<Box<dyn SkillProvider>> {
    vec![
        Box::new(ClaudeCodeProvider),
        Box::new(GeminiProvider),
        Box::new(CopilotProvider),
        Box::new(CodexProvider),
        Box::new(CursorProvider),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn registry_lists_five_providers() {
        let ids: Vec<&str> = all_skill_providers().iter().map(|p| p.id()).collect();
        for id in ["claude-code", "gemini", "copilot", "codex", "cursor"] {
            assert!(ids.contains(&id), "missing provider {id}");
        }
    }

    fn module_section() -> ManagedSection {
        ManagedSection::for_kind(SkillKind::Module, "MODULE GUIDANCE")
    }

    #[test]
    fn splice_block_is_idempotent() {
        let section = module_section();
        let user = "# My AGENTS.md\n\nSome existing guidance.\n";
        let once = splice_block(Some(user), &section);
        let twice = splice_block(Some(&once), &section);
        assert_eq!(once, twice, "re-splicing the same block must be a no-op");
        assert!(once.contains(&section.begin) && once.contains(&section.end));
        assert!(once.contains("MODULE GUIDANCE"));
    }

    #[test]
    fn splice_then_excise_preserves_surrounding_bytes() {
        let section = module_section();
        let user = "# My AGENTS.md\n\nSome existing guidance.\n";
        let spliced = splice_block(Some(user), &section);
        let restored = excise_block(&spliced, &section).expect("block was present");
        // Surrounding non-block bytes survive verbatim (modulo the deliberate
        // trailing-whitespace collapse, which normalizes to a single newline).
        assert_eq!(restored, "# My AGENTS.md\n\nSome existing guidance.\n");
        assert!(!restored.contains(&section.begin));
        assert!(!restored.contains("MODULE GUIDANCE"));
    }

    #[test]
    fn splice_into_empty_then_excise_yields_empty() {
        let section = module_section();
        let spliced = splice_block(None, &section);
        assert!(spliced.contains("MODULE GUIDANCE"));
        let restored = excise_block(&spliced, &section).expect("block was present");
        assert_eq!(restored, "", "removing the only content empties the file");
    }

    #[test]
    fn excise_and_span_on_absent_block_return_none() {
        let section = module_section();
        let user = "# My AGENTS.md\n\nNo cfgd block here.\n";
        assert!(block_span(user, &section).is_none());
        assert!(excise_block(user, &section).is_none());
    }

    #[test]
    fn parse_version_stamp_well_formed() {
        // Frontmatter / TOML key shapes.
        assert_eq!(
            parse_version_stamp("---\ncfgd-version: 0.4.0\nname: x\n---\n"),
            Some("0.4.0".to_string())
        );
        assert_eq!(
            parse_version_stamp("cfgd-version = \"1.2.3\"\n"),
            Some("1.2.3".to_string())
        );
        // AGENTS.md comment-line shape.
        assert_eq!(
            parse_version_stamp("# cfgd-version: 9.9.0\n"),
            Some("9.9.0".to_string())
        );
    }

    #[test]
    fn parse_version_stamp_missing_or_garbled_is_none() {
        assert_eq!(parse_version_stamp(""), None);
        assert_eq!(parse_version_stamp("no stamp anywhere\n"), None);
        // Key present but no value.
        assert_eq!(parse_version_stamp("cfgd-version:\n"), None);
        assert_eq!(parse_version_stamp("cfgd-version: \"\"\n"), None);
    }
}
