pub mod files;
pub mod inspect;
pub mod scan;

/// The orchestration skill, embedded at compile time.
pub const GENERATE_SKILL: &str = include_str!("skill.md");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_skill_embeds() {
        assert!(!GENERATE_SKILL.is_empty());
        assert!(GENERATE_SKILL.contains("configuration generator"));
    }
}
