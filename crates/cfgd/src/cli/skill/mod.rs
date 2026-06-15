//! `cfgd skill ...` — install, list, remove, and update agent-skill primitives
//! across the supported coding-agent providers (Claude Code, Gemini, Copilot,
//! Codex, Cursor).
//!
//! Each author kind ([`SkillKind`]) renders to every provider's native primitive
//! via [`cfgd_core::providers::skill`]. The command bodies are implemented in a
//! later task; the variants and dispatch stubs here pin the CLI surface.

use cfgd_core::output::Printer;

/// The author-facing resource kinds a skill can teach, as a clap positional
/// value. Maps 1:1 to [`cfgd_core::generate::SkillKind`] via [`SkillKind::to_core`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum SkillKind {
    Module,
    Profile,
    Source,
    MachineConfig,
    ConfigPolicy,
    ClusterConfigPolicy,
}

impl SkillKind {
    /// Map the clap-facing kind to the `cfgd-core` authoring kind.
    pub fn to_core(self) -> cfgd_core::generate::SkillKind {
        use cfgd_core::generate::SkillKind as Core;
        match self {
            Self::Module => Core::Module,
            Self::Profile => Core::Profile,
            Self::Source => Core::Source,
            Self::MachineConfig => Core::MachineConfig,
            Self::ConfigPolicy => Core::ConfigPolicy,
            Self::ClusterConfigPolicy => Core::ClusterConfigPolicy,
        }
    }
}

/// Install an agent skill for one author kind across detected providers.
pub fn cmd_skill_install(
    _printer: &Printer,
    _kind: SkillKind,
    _global: bool,
    _providers: &[String],
    _force: bool,
    _yes: bool,
) -> anyhow::Result<()> {
    Ok(())
}

/// List installed agent skills.
pub fn cmd_skill_list(_printer: &Printer, _global: bool) -> anyhow::Result<()> {
    Ok(())
}

/// Remove an installed agent skill for one author kind.
pub fn cmd_skill_remove(
    _printer: &Printer,
    _kind: SkillKind,
    _global: bool,
    _providers: &[String],
    _yes: bool,
) -> anyhow::Result<()> {
    Ok(())
}

/// Update one or all installed agent skills to the current rendering.
pub fn cmd_skill_update(
    _printer: &Printer,
    _kind: Option<SkillKind>,
    _all: bool,
    _global: bool,
    _providers: &[String],
) -> anyhow::Result<()> {
    Ok(())
}
