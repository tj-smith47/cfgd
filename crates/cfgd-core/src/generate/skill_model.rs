//! Structured, kind-aware authoring knowledge shared by the `generate` command
//! and the `skill` command tree.
//!
//! The legacy `generate` system prompt (the markdown the Anthropic API client
//! consumes) is reproduced verbatim by [`SkillModel::render_system_prompt`], so
//! the `generate` command reads its prompt from this model. The structured
//! fields hold the thoroughness protocol that the provider skill bodies render:
//! a per-kind rubric, research loop, field-walk instructions, an embedded
//! fallback schema, ground-truth examples, and a worked exemplar.

use serde::{Deserialize, Serialize};

use crate::generate::validate::entry_for_kind;
use crate::schema::snapshot::{SchemaSnapshot, snapshot_for};

/// The per-variant string and flag facts for one [`SkillKind`], kept in a single
/// table so adding a kind is one new row rather than edits across several
/// `match`es.
struct KindDescriptor {
    /// The PascalCase kind token (matches `kind:` in resource YAML).
    pascal: &'static str,
    /// The lowercase token passed to `cfgd <kind> validate` / `cfgd explain <kind>`.
    command_token: &'static str,
    /// The `kind` string this maps to in `KIND_REGISTRY`. The local `Source`
    /// document kind registers under `ConfigSource`, so it differs from `pascal`.
    registry_kind: &'static str,
}

/// The author-facing resource kinds a skill can teach.
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
    /// The single source of per-variant string/flag facts. Every other
    /// string-mapping accessor delegates here.
    fn descriptor(self) -> KindDescriptor {
        match self {
            Self::Module => KindDescriptor {
                pascal: "Module",
                command_token: "module",
                registry_kind: "Module",
            },
            Self::Profile => KindDescriptor {
                pascal: "Profile",
                command_token: "profile",
                registry_kind: "Profile",
            },
            Self::Source => KindDescriptor {
                pascal: "Source",
                command_token: "source",
                registry_kind: "ConfigSource",
            },
            Self::MachineConfig => KindDescriptor {
                pascal: "MachineConfig",
                command_token: "machineconfig",
                registry_kind: "MachineConfig",
            },
            Self::ConfigPolicy => KindDescriptor {
                pascal: "ConfigPolicy",
                command_token: "configpolicy",
                registry_kind: "ConfigPolicy",
            },
            Self::ClusterConfigPolicy => KindDescriptor {
                pascal: "ClusterConfigPolicy",
                command_token: "clusterconfigpolicy",
                registry_kind: "ClusterConfigPolicy",
            },
        }
    }

    /// The PascalCase kind token (matches `kind:` in resource YAML).
    pub fn as_str(self) -> &'static str {
        self.descriptor().pascal
    }

    /// The lowercase command token passed to `cfgd <kind> validate` / `cfgd explain <kind>`.
    pub fn command_token(self) -> &'static str {
        self.descriptor().command_token
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

/// A captured, ground-truth resource example for a kind. `contents` is captured
/// from a real on-disk file via `include_str!`; [`ResourceExample::source_path`]
/// names that same file as an absolute path so a test can pin the example to its
/// source regardless of the working directory it runs from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResourceExample {
    /// Absolute path of the source file the example was captured from, built
    /// from `CARGO_MANIFEST_DIR` so it resolves from any working directory.
    source: &'static str,
    /// The captured file body, embedded at compile time via `include_str!`.
    pub contents: String,
}

impl ResourceExample {
    /// The absolute path of the source file the example was captured from.
    pub fn source_path(&self) -> &'static str {
        self.source
    }
}

/// A before/after worked example that concretely defines the quality bar: the
/// `before` is a box-checking resource, the `after` is its thorough rewrite, and
/// `note` explains the gap between them.
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
    /// The thoroughness rubric: the quality bar the authored resource must meet.
    pub thoroughness_rubric: &'static str,
    /// The external best-practice research loop the agent follows per field.
    pub research_protocol: &'static str,
    /// How to enumerate this kind's fields live via `cfgd explain`.
    pub field_walk: FieldWalkSpec,
    /// The kind's JSON schema embedded as an offline fallback, stamped with the
    /// cfgd version that produced it.
    pub schema_snapshot: SchemaSnapshot,
    /// Ground-truth examples captured from crate-local fixture copies of the
    /// user-facing `examples/**` files. The copies live inside the crate so
    /// `include_str!` still resolves in the published tarball (workspace-root
    /// paths are not packaged); a ground-truth test pins them byte-for-byte to
    /// their workspace sources.
    pub examples: Vec<ResourceExample>,
    /// The command the skill's validate step runs, e.g. `cfgd module validate <file>`.
    pub validate_cmd: String,
    /// Declared cfgd version floor for the runtime guard.
    pub min_cfgd_version: semver::Version,
    /// The before/after worked example.
    pub exemplar: Exemplar,
}

impl SkillModel {
    /// Reproduces the legacy `generate` system prompt markdown byte-for-byte so
    /// the `generate` command's switch onto this model is provably inert.
    pub fn render_system_prompt(&self) -> String {
        LEGACY_GENERATE_PROMPT.to_string()
    }
}

/// The thoroughness rubric, shared across all kinds. The quality bar is not
/// "valid YAML" — it is exhaustive field evaluation, external research, and a
/// documented rationale for every choice.
const THOROUGHNESS_RUBRIC: &str = "\
The quality bar is NOT \"valid YAML\". It is exhaustive field evaluation, external \
research, and a documented rationale for every choice. A box-checking resource (every \
field technically present, no investigation behind it) fails this bar. Evaluate EVERY \
field the kind exposes; for each, either populate it with a justified value or omit it \
only after investigating enough to conclude it does not apply. Ground every version, \
ordering, and strategy choice in evidence, never a guess.";

/// The external best-practice research loop, shared across all kinds.
const RESEARCH_PROTOCOL: &str = "\
For each field, consult external best practice before settling a value: the tool's own \
docs, the package managers that ship it, and community conventions. Record what you \
verified and your confidence level when a source was unavailable. Prefer live evidence \
over training-knowledge recall, and state explicitly when you could not confirm a claim.";

/// The legacy `generate` orchestration prompt, embedded at compile time. The
/// single source of truth for the prompt text consumed by the CLI `generate`
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
            explain_kind: token,
            drill_hint: true,
        },
        schema_snapshot: schema_snapshot_for(kind),
        examples: examples_for(kind),
        validate_cmd: format!("cfgd {token} validate <file>"),
        min_cfgd_version: version_floor(&cfgd_version),
        exemplar: exemplar_for(kind),
    }
}

/// Build a [`ResourceExample`] pairing an embedded file body with the absolute
/// path it was captured from. `$rel` is the file path relative to the
/// `cfgd-core` crate root; `include_str!` resolves it relative to this source
/// file (hence the `../../` prefix back to the crate root), while `source_path`
/// resolves it from `CARGO_MANIFEST_DIR` so a test reads the same file from any
/// working directory. `$rel` must stay inside the crate directory — a path
/// reaching the workspace root compiles in-repo but is absent from the
/// published tarball, failing `cargo publish`'s verify build.
macro_rules! resource_example {
    ($rel:literal) => {
        ResourceExample {
            source: concat!(env!("CARGO_MANIFEST_DIR"), "/", $rel),
            contents: include_str!(concat!("../../", $rel)).to_string(),
        }
    };
}

/// Ground-truth examples per kind: the most complete real resource first, then a
/// minimal one. `examples.first()` is always the complete example, which the
/// drift test pins to its on-disk source.
fn examples_for(kind: SkillKind) -> Vec<ResourceExample> {
    match kind {
        // Complete: the thorough nvim Module (the exemplar `after`). Minimal: the
        // small-but-complete `clift` Module.
        SkillKind::Module => vec![
            resource_example!("tests/fixtures/exemplar_nvim_after.yaml"),
            resource_example!("tests/fixtures/examples/modules/clift.yaml"),
        ],
        SkillKind::Profile => vec![
            resource_example!("tests/fixtures/examples/profiles/base.yaml"),
            resource_example!("tests/fixtures/examples/profiles/work.yaml"),
        ],
        SkillKind::Source => vec![resource_example!(
            "tests/fixtures/examples/sources/acme-corp-dev.yaml"
        )],
        SkillKind::MachineConfig => {
            vec![resource_example!(
                "tests/fixtures/examples/cluster/machineconfig.yaml"
            )]
        }
        SkillKind::ConfigPolicy => {
            vec![resource_example!(
                "tests/fixtures/examples/cluster/configpolicy.yaml"
            )]
        }
        SkillKind::ClusterConfigPolicy => {
            vec![resource_example!(
                "tests/fixtures/examples/cluster/clusterconfigpolicy.yaml"
            )]
        }
    }
}

/// The before/after worked exemplar. Only the Module kind ships one today: the
/// nvim manifest at its box-checking revision (`before`) versus its thorough
/// rewrite (`after`), both captured verbatim from real history.
fn exemplar_for(kind: SkillKind) -> Exemplar {
    match kind {
        SkillKind::Module => Exemplar {
            before: include_str!("../../tests/fixtures/exemplar_nvim_before.yaml").to_string(),
            after: include_str!("../../tests/fixtures/exemplar_nvim_after.yaml").to_string(),
            note: "The before is a box-checking module: one prefer-list, no version \
investigation, no documented rationale. The after is the thorough version — every \
field evaluated, external best-practice research, and a documented reason for each \
choice — demonstrating the quality bar a skill must reach for."
                .to_string(),
        },
        _ => Exemplar::default(),
    }
}

/// Capture the embedded fallback schema for `kind` from the unified registry,
/// resolved through the canonical [`entry_for_kind`] lookup.
///
/// CRD entries only exist when the default-on `crd` feature is enabled, so a
/// missing entry yields a snapshot with an empty `json_schema` (stamped with the
/// current version) rather than panicking.
fn schema_snapshot_for(kind: SkillKind) -> SchemaSnapshot {
    match entry_for_kind(kind.descriptor().registry_kind) {
        Some(entry) => snapshot_for(entry),
        None => SchemaSnapshot {
            cfgd_version: env!("CARGO_PKG_VERSION").to_string(),
            json_schema: String::new(),
        },
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
    fn render_system_prompt_is_kind_agnostic() {
        assert_eq!(
            skill_model_for(SkillKind::Profile).render_system_prompt(),
            skill_model_for(SkillKind::Module).render_system_prompt()
        );
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
        let running = current_cfgd_version();
        assert_eq!(m.min_cfgd_version.patch, 0);
        assert_eq!(m.min_cfgd_version.major, running.major);
        assert_eq!(m.min_cfgd_version.minor, running.minor);
    }

    #[test]
    fn schema_snapshot_carries_live_schema_for_local_kind() {
        let m = skill_model_for(SkillKind::Module);
        assert_eq!(m.schema_snapshot.cfgd_version, env!("CARGO_PKG_VERSION"));
        assert!(
            m.schema_snapshot.json_schema.contains("packages"),
            "module snapshot should carry the live registry schema"
        );
    }

    #[cfg(feature = "crd")]
    #[test]
    fn schema_snapshot_carries_live_schema_for_crd_kind() {
        let m = skill_model_for(SkillKind::ClusterConfigPolicy);
        assert!(
            !m.schema_snapshot.json_schema.is_empty(),
            "CRD-kind snapshot should carry the live registry schema when the crd feature is on"
        );
    }
}
