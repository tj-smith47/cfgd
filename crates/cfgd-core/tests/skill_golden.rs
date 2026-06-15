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
use cfgd_core::providers::skill::{
    ClaudeCodeProvider, CodexProvider, CopilotProvider, GeminiProvider, SkillProvider,
};

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
/// placeholders so a version bump cannot flip the goldens.
///
/// Order is load-bearing. At a `*.*.0` release the running version EQUALS its
/// floor (e.g. `0.4.0` == `0.4.0`), so a bare `replace(version)` first would
/// consume the min-version stamps too — collapsing both into `<CFGD_VERSION>` and
/// leaving the gate unable to catch a `cfgd-version`↔`cfgd-min-version` swap, the
/// exact runtime-guard bug class these skills exist to prevent. So the floor is
/// replaced *by position* first, in every context the min-version literal appears
/// (frontmatter key, body middot stamp, step-0 prose), THEN the bare version, THEN
/// a trailing bare-floor catch-all for any straggler. `<CFGD_MIN_VERSION>` stays
/// distinct from `<CFGD_VERSION>` even when the two version values are identical.
fn normalize_version(rendered: &str) -> String {
    let version = env!("CARGO_PKG_VERSION");
    let parts: Vec<&str> = version.split('.').collect();
    let floor = format!("{}.{}.0", parts[0], parts.get(1).copied().unwrap_or("0"));
    rendered
        // Positional min-version sites first, so they survive the equal-value case.
        // Both the frontmatter `key: value` shape and the TOML `key = "value"`
        // shape are matched, so the gemini stamp's min-version is distinguished
        // from its version even when the two literals are identical at a `*.*.0`.
        .replace(
            &format!("cfgd-min-version: {floor}"),
            "cfgd-min-version: <CFGD_MIN_VERSION>",
        )
        .replace(
            &format!("cfgd-min-version = \"{floor}\""),
            "cfgd-min-version = \"<CFGD_MIN_VERSION>\"",
        )
        .replace(
            &format!("install cfgd >= {floor}"),
            "install cfgd >= <CFGD_MIN_VERSION>",
        )
        .replace(
            &format!("older than {floor}"),
            "older than <CFGD_MIN_VERSION>",
        )
        // The bare running version everywhere else (cfgd-version key, body stamp
        // left half, fallback-schema preamble).
        .replace(version, "<CFGD_VERSION>")
        // Any remaining floor literal that was a genuine min-version occurrence.
        .replace(&floor, "<CFGD_MIN_VERSION>")
}

/// The golden filename suffix for a provider's native file shape, so each
/// provider's snapshot reads with its real extension (`*.SKILL.md`, `*.toml`).
fn golden_suffix(provider: &str) -> &'static str {
    match provider {
        "claude-code" => "SKILL.md",
        "gemini" => "toml",
        "copilot" => "prompt.md",
        // codex snapshots the per-kind managed block as it is written into a fresh
        // `AGENTS.md`, not a literal whole file — hence the real target's name.
        "codex" => "AGENTS.md",
        // An unmapped provider would otherwise write a mislabeled fixture; fail
        // loudly so a new provider's goldens cannot silently land under the wrong
        // extension.
        other => panic!("golden_suffix: unmapped provider {other:?}"),
    }
}

fn golden_path(provider: &str, token: &str) -> String {
    format!(
        "tests/snapshots/skill/{provider}__{token}.{}",
        golden_suffix(provider)
    )
}

/// Bless or assert every (kind) golden for one provider, applying the shared
/// version normalization so a version bump cannot flip the goldens.
fn check_provider_goldens(provider: &dyn SkillProvider) {
    let bless = std::env::var("CFGD_BLESS_SKILL").is_ok();
    for kind in ALL_KINDS {
        let model = skill_model_for(kind);
        // Snapshot the bytes the provider actually writes to a fresh target. A
        // whole-file provider's are its `contents`; a managed-section provider's
        // (codex) `contents` is empty — its payload is the spliced block — so
        // `effective_fresh_install` routes through the same `splice_block` writer
        // `install` uses, keeping the golden byte-faithful instead of vacuously
        // snapshotting an empty file.
        let rendered = normalize_version(&provider.render(&model).effective_fresh_install());
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
            "{} {} skill changed. If intentional: `task skill:bless`, then review the diff.",
            provider.id(),
            kind.as_str(),
        );
    }
}

#[test]
fn claude_code_renders_match_committed_goldens() {
    check_provider_goldens(&ClaudeCodeProvider);
}

#[test]
fn gemini_renders_match_committed_goldens() {
    check_provider_goldens(&GeminiProvider);
}

#[test]
fn copilot_renders_match_committed_goldens() {
    check_provider_goldens(&CopilotProvider);
}

#[test]
fn codex_renders_match_committed_goldens() {
    check_provider_goldens(&CodexProvider);
}
