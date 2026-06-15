//! At-most-one-update-surface dedup and the user-scope skill ride-along.
//!
//! Binary and skill staleness are two update surfaces that, left unmanaged,
//! could both fire on one invocation. The three [spec §9] rules collapse them
//! to **at most one surface, ever**, and this module makes that guarantee
//! structural rather than incidental:
//!
//! 1. **Binary outranks skills** — [`compute_update_surfaces`] suppresses the
//!    skill surface whenever a binary update is pending.
//! 2. **Ride-along** — [`refresh_user_scope_skills`] re-renders already-present
//!    *user-scope* skills as part of an applied binary upgrade, never as a
//!    second prompt.
//! 3. **One consolidated skill surface** — the standalone-stale case yields a
//!    single skill notice covering both scopes, never one per scope.
//!
//! The git-safety invariant is enforced by construction: the ride-along only
//! ever passes [`SkillScope::User`], so a tracked project file can never be
//! auto-rewritten. Project staleness surfaces only as the consolidated notice.

use crate::config::{UpdateConfig, UpdatePolicy};
use crate::providers::skill::{SkillScope, all_skill_providers};

/// The deduplicated update surfaces to show for one invocation.
///
/// At most one of `shows_binary` / `shows_skills` is ever `true`, and
/// `skill_surface_count` is `0` or `1` — never per-scope. The flags drive what
/// the caller renders; nothing here writes or prompts.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UpdateSurfaces {
    /// A binary update surface should be shown (a newer binary is pending).
    pub shows_binary: bool,
    /// A skill staleness surface should be shown (binary current, skills stale).
    pub shows_skills: bool,
    /// The number of *skill* surfaces — `1` when one consolidated skill notice
    /// covers both user- and project-scope staleness, `0` otherwise. Never more
    /// than one, by construction.
    pub skill_surface_count: usize,
}

/// Resolve the single update surface to show, encoding the three [spec §9]
/// dedup rules.
///
/// * **Rule 1 (binary outranks skills):** when `binary_available`, the binary
///   surface alone shows and the skill surface is suppressed — refreshing skills
///   against a binary about to be replaced is wasted work.
/// * **Rule 3 (one consolidated skill surface):** when the binary is current but
///   `skills_stale`, exactly one skill notice covers both scopes.
/// * Neither pending → no surface.
///
/// Pure and side-effect-free: the caller supplies the two booleans (a binary
/// check and an aggregate staleness flag) and renders per the returned flags.
/// `_cfg` is accepted so the signature stays stable as policy-dependent surface
/// shaping is layered on, without forcing callers to thread it later.
pub fn compute_update_surfaces(
    binary_available: bool,
    skills_stale: bool,
    _cfg: &UpdateConfig,
) -> UpdateSurfaces {
    if binary_available {
        // Rule 1: the binary surface wins outright; the skill surface is
        // suppressed so the two can never both show.
        return UpdateSurfaces {
            shows_binary: true,
            shows_skills: false,
            skill_surface_count: 0,
        };
    }
    if skills_stale {
        // Rule 3: a single consolidated notice for both scopes — never one per
        // scope.
        return UpdateSurfaces {
            shows_binary: false,
            shows_skills: true,
            skill_surface_count: 1,
        };
    }
    UpdateSurfaces {
        shows_binary: false,
        shows_skills: false,
        skill_surface_count: 0,
    }
}

/// Outcome of the [`refresh_user_scope_skills`] ride-along.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RideAlongOutcome {
    /// `true` when at least one already-present user-scope skill was re-rendered.
    pub user_scope_skills_refreshed: bool,
    /// How many additional prompts the ride-along raised. **Always `0`** — the
    /// refresh rides along with the already-decided binary upgrade and never
    /// asks a second question.
    pub prompt_count: usize,
}

/// Refresh already-installed **user-scope** skills as the ride-along half of an
/// applied binary upgrade ([spec §9] rule 2).
///
/// Call this *after* the binary install has happened (`Auto`, an accepted
/// `Prompt`, or a manual `cfgd upgrade`). It re-renders every skill already
/// present at [`SkillScope::User`] across every provider, so a version bump's
/// stamp refresh lands in the same action — never a second prompt
/// (`prompt_count` is always `0`).
///
/// The effective skills policy ([`UpdateConfig::effective_skill_policy`]) gates
/// the *write*:
///
/// * `Auto` / `Prompt` (incl. their `Inherit` resolutions) → re-render in place.
///   `Prompt` writes here without re-asking because the binary upgrade it rides
///   along with was already accepted.
/// * `Notify` → no write (the consolidated notice already surfaced it).
/// * `Manual` → no write (silent; the user runs `cfgd skill update`).
///
/// **Project-scope skills are never touched** — only [`SkillScope::User`] is
/// ever passed, making an auto-rewrite of a tracked project file unrepresentable
/// here. Only skills already present are refreshed; a kind the user never
/// installed is never freshly created during an upgrade.
///
/// Failures to refresh an individual skill are logged and skipped rather than
/// aborting the post-upgrade tail: the binary upgrade has already succeeded, and
/// a stale skill is a far smaller problem than a panicked upgrade path.
pub fn refresh_user_scope_skills(cfg: &UpdateConfig) -> RideAlongOutcome {
    let policy = cfg.effective_skill_policy();
    // Only Auto/Prompt write during the ride-along; Notify/Manual never do.
    let should_write = matches!(policy, UpdatePolicy::Auto | UpdatePolicy::Prompt);
    if !should_write {
        return RideAlongOutcome::default();
    }

    let mut refreshed = false;
    for provider in all_skill_providers() {
        // Drive strictly off what is already present at user scope: never
        // newly install a kind the user did not have.
        let installed = match provider.list(SkillScope::User) {
            Ok(list) => list,
            Err(e) => {
                tracing::warn!(
                    provider = provider.id(),
                    error = %e,
                    "ride-along: listing user-scope skills failed; skipping provider",
                );
                continue;
            }
        };
        for skill in installed {
            let model = crate::generate::skill_model_for(skill.kind);
            // SkillScope::User is the ONLY scope passed here — the git-safety
            // invariant (no auto-write of tracked project files) holds by
            // construction.
            match provider.install(&model, SkillScope::User) {
                Ok(_) => refreshed = true,
                Err(e) => tracing::warn!(
                    provider = provider.id(),
                    kind = skill.kind.as_str(),
                    error = %e,
                    "ride-along: re-rendering user-scope skill failed; leaving stale",
                ),
            }
        }
    }

    RideAlongOutcome {
        user_scope_skills_refreshed: refreshed,
        prompt_count: 0,
    }
}
