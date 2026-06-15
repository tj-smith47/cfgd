#[test]
fn generate_system_prompt_is_unchanged() {
    let got = cfgd_core::generate::skill_model_for(cfgd_core::generate::SkillKind::Module)
        .render_system_prompt();
    // For the full-scan mode, generate composes per-kind prompts; the inertness fixture
    // captures the Module-target prompt specifically (the path most reused by the skill).
    let golden = include_str!("fixtures/generate_system_prompt.txt");
    assert_eq!(got.trim(), golden.trim());
}
