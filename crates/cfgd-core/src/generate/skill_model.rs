//! Structured, kind-aware authoring knowledge shared by the `generate` command
//! and the `skill` command tree.
//!
//! The legacy `generate` system prompt (the markdown the Anthropic API client
//! consumes) is reproduced verbatim by [`SkillModel::render_system_prompt`], so
//! the `generate` refactor onto this model is provably inert. The structured
//! fields hold the §7 thoroughness protocol that the provider skill bodies
//! render; they are distinct text from the legacy prompt and are populated
//! across later tasks (examples/exemplar/schema snapshot).

use serde::{Deserialize, Serialize};

/// The author-facing resource kinds a skill can teach. This is the same enum
/// the CLI's clap `skill` surface uses; it carries no `Full` variant because
/// `generate`'s full-scan mode orchestrates multiple per-kind models rather
/// than mapping to a single one.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum SkillKind {
    Module,
    Profile,
    Source,
    MachineConfig,
    ConfigPolicy,
    ClusterConfigPolicy,
}

impl SkillKind {
    /// The PascalCase kind token (matches `kind:` in resource YAML).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Module => "Module",
            Self::Profile => "Profile",
            Self::Source => "Source",
            Self::MachineConfig => "MachineConfig",
            Self::ConfigPolicy => "ConfigPolicy",
            Self::ClusterConfigPolicy => "ClusterConfigPolicy",
        }
    }

    /// The lowercase command token passed to `cfgd <kind> validate` / `cfgd explain <kind>`.
    pub fn command_token(&self) -> &'static str {
        match self {
            Self::Module => "module",
            Self::Profile => "profile",
            Self::Source => "source",
            Self::MachineConfig => "machineconfig",
            Self::ConfigPolicy => "configpolicy",
            Self::ClusterConfigPolicy => "clusterconfigpolicy",
        }
    }
}

/// How the rendered skill body tells the agent to enumerate a kind's fields,
/// preferring a live `cfgd explain <kind>` over the embedded snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldWalkSpec {
    /// Token passed to `cfgd explain <kind>`.
    pub explain_kind: &'static str,
    /// Whether to include the `<kind>.<field>` drill-down instruction.
    pub drill_hint: bool,
}

/// Generated, embedded fallback schema for a kind, stamped with the rendering
/// cfgd version. Populated from the §10A registry in a later task; carries an
/// honest empty `json_schema` until then.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaSnapshot {
    pub cfgd_version: semver::Version,
    /// The kind's JSON schema, serialized. Empty until the registry wires it.
    pub json_schema: String,
}

/// A captured, ground-truth resource example for a kind. `contents` is captured
/// from a real on-disk `examples/**` file; [`ResourceExample::source_path`]
/// names that file so a test can pin the example to its source.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceExample {
    /// Repo-relative path of the source file the example was captured from.
    pub source: &'static str,
    /// The captured file body.
    pub contents: String,
}

impl ResourceExample {
    /// The source file the example was captured from.
    pub fn source_path(&self) -> &'static str {
        self.source
    }
}

/// The nvim before/after worked example that concretely defines the quality bar.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Exemplar {
    pub before: String,
    pub after: String,
    pub note: String,
}

/// Structured authoring knowledge for one resource kind.
#[derive(Debug, Clone)]
pub struct SkillModel {
    pub kind: SkillKind,
    /// Provider frontmatter title, e.g. "Author a high-quality cfgd Module".
    pub title: String,
    /// Provider frontmatter description (the triggering text).
    pub description: String,
    /// The §7 thoroughness rubric (the nvim bar), kind-agnostic.
    pub thoroughness_rubric: &'static str,
    /// The §7 external best-practice research loop.
    pub research_protocol: &'static str,
    /// How to enumerate this kind's fields live via `cfgd explain`.
    pub field_walk: FieldWalkSpec,
    /// Generated fallback schema, stamped with the cfgd version.
    pub schema_snapshot: SchemaSnapshot,
    /// Ground-truth examples captured from real `examples/**` files.
    pub examples: Vec<ResourceExample>,
    /// The command the skill's validate step runs, e.g. `cfgd module validate <file>`.
    pub validate_cmd: String,
    /// Declared cfgd version floor for the runtime guard.
    pub min_cfgd_version: semver::Version,
    /// The nvim before/after worked example.
    pub exemplar: Exemplar,
}

impl SkillModel {
    /// Reproduces the legacy `generate` system prompt markdown byte-for-byte so
    /// the `generate` command's switch onto this model is provably inert.
    pub fn render_system_prompt(&self) -> String {
        LEGACY_GENERATE_PROMPT.to_string()
    }
}

/// The §7 thoroughness rubric, shared across all kinds. The quality bar is not
/// "valid YAML" — it is exhaustive field evaluation, external research, and a
/// documented rationale for every choice, matching the cfgd-config nvim module's
/// box-checking → thorough evolution.
const THOROUGHNESS_RUBRIC: &str = "\
The quality bar is NOT \"valid YAML\". It is exhaustive field evaluation, external \
research, and a documented rationale for every choice. A box-checking resource (every \
field technically present, no investigation behind it) fails this bar. Evaluate EVERY \
field the kind exposes; for each, either populate it with a justified value or omit it \
only after investigating enough to conclude it does not apply. Ground every version, \
ordering, and strategy choice in evidence, never a guess.";

/// The §7 external best-practice research loop, shared across all kinds.
const RESEARCH_PROTOCOL: &str = "\
For each field, consult external best practice before settling a value: the tool's own \
docs, the package managers that ship it, and community conventions. Record what you \
verified and your confidence level when a source was unavailable. Prefer live evidence \
over training-knowledge recall, and state explicitly when you could not confirm a claim.";

/// The legacy `generate` orchestration prompt, embedded at compile time. This is
/// the single source of truth for the prompt text consumed by the CLI `generate`
/// path and the MCP `cfgd_generate*` prompts / `cfgd://skill/generate` resource.
pub const LEGACY_GENERATE_PROMPT: &str = include_str!("skill.md");

/// Build the [`SkillModel`] for a given kind.
pub fn skill_model_for(kind: SkillKind) -> SkillModel {
    let cfgd_version = current_cfgd_version();
    let kind_word = kind.as_str();
    let token = kind.command_token();
    SkillModel {
        kind,
        title: format!("Author a high-quality cfgd {kind_word}"),
        description: format!(
            "Investigate thoroughly and author a complete, validated cfgd {kind_word} resource."
        ),
        thoroughness_rubric: THOROUGHNESS_RUBRIC,
        research_protocol: RESEARCH_PROTOCOL,
        field_walk: FieldWalkSpec {
            explain_kind: kind.command_token(),
            drill_hint: true,
        },
        schema_snapshot: SchemaSnapshot {
            cfgd_version: cfgd_version.clone(),
            json_schema: String::new(),
        },
        examples: Vec::new(),
        validate_cmd: format!("cfgd {token} validate <file>"),
        min_cfgd_version: version_floor(&cfgd_version),
        exemplar: Exemplar::default(),
    }
}

/// The running cfgd version, parsed from `CARGO_PKG_VERSION`. Falls back to
/// `0.0.0` if the crate version is ever unparseable (it always parses for a
/// released build), keeping this panic-free for library code.
fn current_cfgd_version() -> semver::Version {
    semver::Version::parse(env!("CARGO_PKG_VERSION"))
        .unwrap_or_else(|_| semver::Version::new(0, 0, 0))
}

/// The patch-zero floor of a version (e.g. `0.4.3` -> `0.4.0`), used as the
/// declared minimum cfgd version for the runtime guard.
fn version_floor(v: &semver::Version) -> semver::Version {
    semver::Version::new(v.major, v.minor, 0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn render_system_prompt_equals_embedded_legacy_prompt() {
        let model = skill_model_for(SkillKind::Module);
        assert_eq!(model.render_system_prompt(), LEGACY_GENERATE_PROMPT);
    }

    #[test]
    fn validate_cmd_is_verb_first_per_kind() {
        assert_eq!(
            skill_model_for(SkillKind::Module).validate_cmd,
            "cfgd module validate <file>"
        );
        assert_eq!(
            skill_model_for(SkillKind::ClusterConfigPolicy).validate_cmd,
            "cfgd clusterconfigpolicy validate <file>"
        );
    }

    #[test]
    fn min_version_floors_patch_to_zero() {
        let m = skill_model_for(SkillKind::Profile);
        assert_eq!(m.min_cfgd_version.patch, 0);
        assert_eq!(
            m.min_cfgd_version.major,
            m.schema_snapshot.cfgd_version.major
        );
        assert_eq!(
            m.min_cfgd_version.minor,
            m.schema_snapshot.cfgd_version.minor
        );
    }
}
