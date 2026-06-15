//! At-most-one-update-surface dedup (spec §9): the three precedence/consolidation
//! rules and the user-scope skill ride-along.
//!
//! The two filesystem tests resolve scope-rooted skill paths, so each runs inside
//! `with_test_home` (user scope) plus a `CwdGuard` (project scope) and is
//! `#[serial]` — the HOME override and cwd are process-global.

use cfgd_core::config::{SkillUpdateConfig, SkillUpdatePolicy, UpdateConfig, UpdatePolicy};
use cfgd_core::generate::{SkillKind, skill_model_for};
use cfgd_core::providers::skill::{ClaudeCodeProvider, SkillProvider, SkillScope};
use cfgd_core::test_helpers::{CwdGuard, EnvVarGuard};
use cfgd_core::upgrade::{compute_update_surfaces, refresh_user_scope_skills};
use cfgd_core::with_test_home;

/// An [`UpdateConfig`] with the given binary policy and skills policy.
fn cfg(policy: UpdatePolicy, skills: SkillUpdatePolicy) -> UpdateConfig {
    UpdateConfig {
        policy,
        interval: "24h".to_string(),
        channel: None,
        skills: SkillUpdateConfig { policy: skills },
    }
}

// ----- Rule 1: binary outranks skills (pure) -----

#[test]
fn rule1_binary_pending_suppresses_skill_surface() {
    // Skills stale AND a newer binary available → only the binary surface; the
    // skill surface is suppressed (≤1 surface, binary wins).
    let c = cfg(UpdatePolicy::Prompt, SkillUpdatePolicy::Inherit);
    let s = compute_update_surfaces(true, true, &c);
    assert!(
        s.shows_binary && !s.shows_skills,
        "binary outranks; ≤1 surface: {s:?}"
    );
    assert_eq!(s.skill_surface_count, 0, "skill surface suppressed: {s:?}");
}

// ----- Rule 3: one consolidated skill surface (pure) -----

#[test]
fn rule3_standalone_stale_shows_one_consolidated_skill_surface() {
    // Binary current, user+project skills both stale → exactly ONE skill notice
    // (not one per scope).
    let c = cfg(UpdatePolicy::Notify, SkillUpdatePolicy::Inherit);
    let s = compute_update_surfaces(false, true, &c);
    assert!(
        s.shows_skills && !s.shows_binary,
        "skill surface only: {s:?}"
    );
    assert_eq!(
        s.skill_surface_count, 1,
        "exactly one consolidated skill surface: {s:?}"
    );
}

#[test]
fn neither_pending_shows_no_surface() {
    let c = cfg(UpdatePolicy::Auto, SkillUpdatePolicy::Inherit);
    let s = compute_update_surfaces(false, false, &c);
    assert!(!s.shows_binary && !s.shows_skills && s.skill_surface_count == 0);
}

// ----- Rule 2: ride-along refresh of user-scope skills (fs) -----

#[test]
#[serial_test::serial]
fn rule2_skill_refresh_rides_along_with_binary_upgrade_no_second_prompt() {
    // An accepted/auto binary upgrade refreshes already-present user-scope skills
    // in the SAME action — no second prompt.
    let home = tempfile::tempdir().expect("home tempdir");
    let runtime = tempfile::tempdir().expect("runtime tempdir");
    let _runtime_env = EnvVarGuard::set("CFGD_RUNTIME_DIR", &runtime.path().to_string_lossy());

    with_test_home(home.path(), || {
        // A user-scope skill must already be installed for the ride-along to
        // refresh it (never freshly installs a kind the user never had).
        ClaudeCodeProvider
            .install(&skill_model_for(SkillKind::Module), SkillScope::User)
            .expect("seed a user-scope skill");

        let outcome =
            refresh_user_scope_skills(&cfg(UpdatePolicy::Auto, SkillUpdatePolicy::Inherit));

        assert_eq!(outcome.prompt_count, 0, "ride-along never prompts");
        assert!(
            outcome.user_scope_skills_refreshed,
            "an already-present user-scope skill must be refreshed"
        );
    });
}

#[test]
#[serial_test::serial]
fn ride_along_does_not_install_kinds_the_user_never_had() {
    // With NOTHING installed at user scope, an Auto ride-along must write nothing
    // — it refreshes present skills, never fresh-installs.
    let home = tempfile::tempdir().expect("home tempdir");
    let runtime = tempfile::tempdir().expect("runtime tempdir");
    let _runtime_env = EnvVarGuard::set("CFGD_RUNTIME_DIR", &runtime.path().to_string_lossy());

    with_test_home(home.path(), || {
        let outcome =
            refresh_user_scope_skills(&cfg(UpdatePolicy::Auto, SkillUpdatePolicy::Inherit));
        assert!(
            !outcome.user_scope_skills_refreshed,
            "nothing installed → nothing refreshed"
        );

        let target = ClaudeCodeProvider
            .target_path(SkillKind::Module, SkillScope::User)
            .expect("user scope has a target");
        assert!(
            !target.exists(),
            "ride-along must not fresh-install a kind: {target:?}"
        );
    });
}

#[test]
#[serial_test::serial]
fn notify_and_manual_policies_do_not_write_during_ride_along() {
    let home = tempfile::tempdir().expect("home tempdir");
    let runtime = tempfile::tempdir().expect("runtime tempdir");
    let _runtime_env = EnvVarGuard::set("CFGD_RUNTIME_DIR", &runtime.path().to_string_lossy());

    with_test_home(home.path(), || {
        ClaudeCodeProvider
            .install(&skill_model_for(SkillKind::Module), SkillScope::User)
            .expect("seed a user-scope skill");

        for policy in [SkillUpdatePolicy::Notify, SkillUpdatePolicy::Manual] {
            let outcome = refresh_user_scope_skills(&cfg(UpdatePolicy::Auto, policy));
            assert!(
                !outcome.user_scope_skills_refreshed,
                "{policy:?} must not write during ride-along"
            );
            assert_eq!(outcome.prompt_count, 0, "{policy:?} never prompts");
        }
    });
}

// ----- Git-safety invariant: project-scope is never auto-rewritten -----

#[test]
#[serial_test::serial]
fn project_scope_skills_are_never_auto_rewritten() {
    // A project-scope skill present; an Auto ride-along must leave its bytes
    // UNCHANGED (project files are tracked — a surprise diff is unacceptable).
    let home = tempfile::tempdir().expect("home tempdir");
    let project = tempfile::tempdir().expect("project tempdir");
    let runtime = tempfile::tempdir().expect("runtime tempdir");
    let _runtime_env = EnvVarGuard::set("CFGD_RUNTIME_DIR", &runtime.path().to_string_lossy());
    let _cwd = CwdGuard::set(project.path()).expect("set cwd to project dir");

    with_test_home(home.path(), || {
        // Install the skill at PROJECT scope and capture its on-disk bytes.
        let project_path = ClaudeCodeProvider
            .install(&skill_model_for(SkillKind::Module), SkillScope::Project)
            .expect("seed a project-scope skill");
        let before = std::fs::read(&project_path).expect("read project skill");

        // Run the ride-along with an effective-Auto policy.
        let outcome =
            refresh_user_scope_skills(&cfg(UpdatePolicy::Auto, SkillUpdatePolicy::Inherit));

        // No user-scope skill was present, so nothing rode along; and crucially
        // the project file is byte-identical (never auto-written, by construction
        // — the ride-along only ever passes SkillScope::User).
        assert!(
            !outcome.user_scope_skills_refreshed,
            "no user-scope skill present → none refreshed"
        );
        let after = std::fs::read(&project_path).expect("re-read project skill");
        assert_eq!(
            before, after,
            "project-scope skill bytes must be unchanged by a ride-along refresh"
        );
    });
}
