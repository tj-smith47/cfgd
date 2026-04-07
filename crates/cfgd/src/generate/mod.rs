pub mod files;
pub mod inspect;
pub mod scan;

/// The orchestration skill, embedded at compile time.
pub const GENERATE_SKILL: &str = include_str!("skill.md");
