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
pub fn compute_update_surfaces(binary_available: bool, skills_stale: bool) -> UpdateSurfaces {
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

/// Aggregate count of stale installed skills at each scope, across every
/// provider. Drives the rule-3 consolidated surface (a single notice covering
/// both scopes) — the per-scope counts are reported in the notice, never as
/// separate surfaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct SkillStaleness {
    /// Number of stale user-scope (home) skills across all providers.
    pub user: usize,
    /// Number of stale project-scope (CWD) skills across all providers.
    pub project: usize,
}

impl SkillStaleness {
    /// Whether any skill at either scope is stale (the rule-3 trigger).
    pub fn any(self) -> bool {
        self.user > 0 || self.project > 0
    }
}

/// Count stale installed skills at `scope` across every provider. A provider
/// whose `list` errors contributes zero (best-effort: a transient read hiccup
/// must not fabricate a phantom stale surface).
fn count_stale_skills(scope: SkillScope) -> usize {
    all_skill_providers()
        .iter()
        .map(|p| match p.list(scope) {
            Ok(skills) => skills.iter().filter(|s| s.stale).count(),
            Err(e) => {
                tracing::debug!(
                    error = %e,
                    scope = ?scope,
                    provider = p.id(),
                    "skill staleness probe failed; treating as 0",
                );
                0
            }
        })
        .sum()
}

/// Aggregate stale-skill counts at both scopes — the input the rule-3 path feeds
/// into [`compute_update_surfaces`] (as `skills_stale = staleness.any()`) and
/// reports in the single consolidated notice.
pub fn aggregate_skill_staleness() -> SkillStaleness {
    SkillStaleness {
        user: count_stale_skills(SkillScope::User),
        project: count_stale_skills(SkillScope::Project),
    }
}

/// The single consolidated rule-3 notice covering BOTH scopes (never one per
/// scope). Single-sources the wording so the CLI human surface and the daemon
/// notifier message cannot drift.
pub fn consolidated_skill_stale_message(staleness: SkillStaleness) -> String {
    format!(
        "cfgd skills are stale (user: {}, project: {}); run `cfgd skill update`",
        staleness.user, staleness.project
    )
}

/// What the standalone-stale path (binary current, skills stale) should do,
/// resolved from the effective skills policy per the [spec §9] scope table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandaloneSkillAction {
    /// `Auto` / `Inherit→Auto`: re-render USER-scope skills directly, then a
    /// notice if project-scope skills remain stale (project is never written).
    RefreshUserThenNoticeProject,
    /// `Notify` / `Prompt` (and their `Inherit` resolutions): emit the single
    /// consolidated notice covering both scopes; write nothing.
    ///
    /// `Prompt` standalone-stale has no binary upgrade to ride along, so per the
    /// §9 headline "at most ONE surface" it shows the consolidated notice exactly
    /// like `Notify` — never a separate skill prompt and never an auto-write.
    ConsolidatedNotice,
    /// `Manual` / `Inherit→Manual`: silent — the user runs `cfgd skill update`.
    Silent,
}

/// Resolve the standalone-stale action from the effective skills policy. Pure:
/// the caller performs the I/O (refresh / notice / nothing). Keeps the §9 scope
/// table in one place so the CLI and daemon consumers cannot diverge.
pub fn resolve_standalone_skill_action(cfg: &UpdateConfig) -> StandaloneSkillAction {
    match cfg.effective_skill_policy() {
        UpdatePolicy::Auto => StandaloneSkillAction::RefreshUserThenNoticeProject,
        UpdatePolicy::Notify | UpdatePolicy::Prompt => StandaloneSkillAction::ConsolidatedNotice,
        UpdatePolicy::Manual => StandaloneSkillAction::Silent,
    }
}

/// What the §9 skill-surface orchestration decided, as a pure value for a
/// consumer to render. At most ONE consolidated notice is ever indicated
/// ([`StandaloneSkillOutcome::NoticeNeeded`] carries the single staleness it
/// covers); every other variant indicates no notice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StandaloneSkillOutcome {
    /// No skill surface: a binary update is pending (rule 1) or nothing is stale.
    Suppressed,
    /// `Auto` re-rendered the stale user-scope skills and no project-scope skill
    /// remains stale, so no notice is needed.
    Refreshed,
    /// Exactly one consolidated notice is needed, covering these per-scope counts
    /// (both scopes for `Notify`/`Prompt`; the project-only remainder for `Auto`).
    NoticeNeeded(SkillStaleness),
    /// `Manual`: silent — nothing emitted or written.
    Silent,
}

/// Run the §9 skill-surface orchestration end to end and return a pure outcome
/// for the consumer to render (CLI Doc or daemon notifier).
///
/// This is the SINGLE owner of the effectful "how the standalone-stale action
/// runs" — both the CLI and daemon consumers call it and render off the returned
/// [`StandaloneSkillOutcome`], so the *decision* and the *orchestration* (refresh
/// → re-aggregate → notice-iff-project-remains) can never drift between them.
///
/// Sequence:
/// 1. Aggregate stale-skill counts and apply [`compute_update_surfaces`]: a
///    pending binary update (`binary_available`) suppresses skills (rule 1), and
///    nothing-stale yields no surface.
/// 2. Otherwise dispatch on [`resolve_standalone_skill_action`] (the §9 scope
///    table): `Auto` re-renders USER-scope skills via [`refresh_user_scope_skills`]
///    then re-aggregates — a non-zero project remainder needs a notice, else
///    `Refreshed`; `Notify`/`Prompt` need one consolidated both-scopes notice with
///    no write; `Manual` is silent.
///
/// Project-scope is never written under any policy (the refresh only ever touches
/// [`SkillScope::User`]).
pub fn run_standalone_skill_action(
    cfg: &UpdateConfig,
    binary_available: bool,
) -> StandaloneSkillOutcome {
    let staleness = aggregate_skill_staleness();
    let surfaces = compute_update_surfaces(binary_available, staleness.any());
    if !surfaces.shows_skills {
        // Rule 1 (binary pending) or nothing stale.
        return StandaloneSkillOutcome::Suppressed;
    }

    match resolve_standalone_skill_action(cfg) {
        StandaloneSkillAction::RefreshUserThenNoticeProject => {
            // Auto: re-render user-scope in place; project-scope is never written.
            let _ = refresh_user_scope_skills(cfg);
            // After refreshing user-scope, only project-scope skills can remain
            // stale — surface those (and only those) so the user can
            // `cfgd skill update` and commit deliberately.
            let remaining = aggregate_skill_staleness();
            if remaining.project > 0 {
                StandaloneSkillOutcome::NoticeNeeded(remaining)
            } else {
                StandaloneSkillOutcome::Refreshed
            }
        }
        StandaloneSkillAction::ConsolidatedNotice => {
            StandaloneSkillOutcome::NoticeNeeded(staleness)
        }
        StandaloneSkillAction::Silent => StandaloneSkillOutcome::Silent,
    }
}
