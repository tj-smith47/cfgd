use std::sync::LazyLock;

pub mod files;
pub mod inspect;
pub mod scan;

/// The orchestration system prompt for AI-guided generation.
///
/// Sourced from the single `SkillModel` knowledge core in `cfgd-core` so the
/// prompt text has one home; the CLI generate path and both MCP surfaces
/// (`cfgd_generate*` prompts, `cfgd://skill/generate` resource) render from it.
pub static GENERATE_SKILL: LazyLock<String> = LazyLock::new(|| {
    cfgd_core::generate::skill_model_for(cfgd_core::generate::SkillKind::Module)
        .render_system_prompt()
});
