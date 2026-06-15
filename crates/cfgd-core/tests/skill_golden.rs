//! Golden snapshot gate for rendered skills.
//!
//! A committed golden file per (provider × kind) in `tests/snapshots/skill/`,
//! pinned against the provider's live `render`. Any drift in a provider's native
//! envelope — a renamed frontmatter key, a reordered field, a body change — trips
//! this test, forcing a conscious review rather than silently shipping a change to
//! the file shape every consuming agent reads.
//!
//! Bless (regenerate the goldens) by running with `CFGD_BLESS_SKILL=1` set —
//! `task skill:bless` does exactly that. The bless writer and the assert apply the
//! SAME version normalization, so the bytes written match the bytes asserted.
//!
//! The version stamp embeds `CARGO_PKG_VERSION` (the running cfgd version) in both
//! the frontmatter (`cfgd-version` / `cfgd-min-version`) and the body. Committing
//! those raw would flip every golden red on an unrelated version bump, so the
//! running version and its `major.minor.0` floor are replaced with stable
//! placeholders before comparison — the formatting invariant is what is pinned,
//! not the version literal.

use cfgd_core::generate::{SkillKind, skill_model_for};
use cfgd_core::providers::skill::{ClaudeCodeProvider, SkillProvider};

/// Every author-facing kind, in the registry's stable order.
const ALL_KINDS: [SkillKind; 6] = [
    SkillKind::Module,
    SkillKind::Profile,
    SkillKind::Source,
    SkillKind::MachineConfig,
    SkillKind::ConfigPolicy,
    SkillKind::ClusterConfigPolicy,
];

/// Replace the running cfgd version and its `major.minor.0` floor with stable
/// placeholders so a version bump cannot flip the goldens. Order matters: the
/// full version is replaced first so a `0.4.0`-style floor that is a substring of
/// the full version is not clobbered mid-string.
fn normalize_version(rendered: &str) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let parts: Vec<&str> = version.split('.').collect();
    let floor = format!("{}.{}.0", parts[0], parts.get(1).copied().unwrap_or("0"));
    rendered
        .replace(version, "<CFGD_VERSION>")
        .replace(&floor, "<CFGD_MIN_VERSION>")
}

fn golden_path(provider: &str, token: &str) -> String {
    format!("tests/snapshots/skill/{provider}__{token}.SKILL.md")
}

#[test]
fn claude_code_renders_match_committed_goldens() {
    let bless = std::env::var("CFGD_BLESS_SKILL").is_ok();
    let provider = ClaudeCodeProvider;
    for kind in ALL_KINDS {
        let model = skill_model_for(kind);
        let rendered = normalize_version(&provider.render(&model).contents);
        let path = golden_path(provider.id(), kind.command_token());
        if bless {
            std::fs::write(&path, &rendered)
                .unwrap_or_else(|e| panic!("failed to write golden {path}: {e}"));
            continue;
        }
        let golden = std::fs::read_to_string(&path)
            .unwrap_or_else(|_| panic!("missing golden {path} — run `task skill:bless`"));
        assert_eq!(
            rendered.trim_end(),
            golden.trim_end(),
            "{} {} SKILL.md changed. If intentional: `task skill:bless`, then review the diff.",
            provider.id(),
            kind.as_str(),
        );
    }
}
