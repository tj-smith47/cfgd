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
use cfgd_core::upgrade::{
    SkillStaleness, StandaloneSkillOutcome, aggregate_skill_staleness, compute_update_surfaces,
    refresh_user_scope_skills, run_standalone_skill_action,
};
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

/// Install a claude-code skill for `kind` at `scope`, then rewrite its stamped
/// `cfgd-version` to `0.0.1` so `list` flags it stale (stamp != running). The
/// whole-file claude provider carries the stamp on a `cfgd-version:` frontmatter
/// line, so a line rewrite faithfully reproduces an old install.
fn seed_stale_skill(kind: SkillKind, scope: SkillScope) -> std::path::PathBuf {
    let path = ClaudeCodeProvider
        .install(&skill_model_for(kind), scope)
        .expect("install skill");
    let body = std::fs::read_to_string(&path).expect("read installed skill");
    let staled = body
        .lines()
        .map(|l| {
            if l.trim_start().starts_with("cfgd-version:") {
                "cfgd-version: 0.0.1".to_string()
            } else {
                l.to_string()
            }
        })
        .collect::<Vec<_>>()
        .join("\n");
    std::fs::write(&path, staled).expect("rewrite stale stamp");
    path
}

// ----- Rule 1: binary outranks skills (pure) -----

#[test]
fn rule1_binary_pending_suppresses_skill_surface() {
    // Skills stale AND a newer binary available → only the binary surface; the
    // skill surface is suppressed (≤1 surface, binary wins).
    let s = compute_update_surfaces(true, true);
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
    let s = compute_update_surfaces(false, true);
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
    let s = compute_update_surfaces(false, false);
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

// ----- Wired aggregation: staleness → compute_update_surfaces → action -----

#[test]
#[serial_test::serial]
fn aggregate_counts_stale_skills_per_scope() {
    let home = tempfile::tempdir().expect("home tempdir");
    let project = tempfile::tempdir().expect("project tempdir");
    let runtime = tempfile::tempdir().expect("runtime tempdir");
    let _runtime_env = EnvVarGuard::set("CFGD_RUNTIME_DIR", &runtime.path().to_string_lossy());
    let _cwd = CwdGuard::set(project.path()).expect("set cwd to project dir");

    with_test_home(home.path(), || {
        seed_stale_skill(SkillKind::Module, SkillScope::User);
        seed_stale_skill(SkillKind::Profile, SkillScope::User);
        seed_stale_skill(SkillKind::Module, SkillScope::Project);

        let staleness = aggregate_skill_staleness();
        assert_eq!(staleness.user, 2, "two stale user skills: {staleness:?}");
        assert_eq!(
            staleness.project, 1,
            "one stale project skill: {staleness:?}"
        );
        assert!(staleness.any());
    });
}

#[test]
#[serial_test::serial]
fn binary_pending_suppresses_wired_skill_surface() {
    // Rule 1 wired: stale skills present AND a binary update pending →
    // compute_update_surfaces fed the real aggregate yields binary-only.
    let home = tempfile::tempdir().expect("home tempdir");
    let runtime = tempfile::tempdir().expect("runtime tempdir");
    let _runtime_env = EnvVarGuard::set("CFGD_RUNTIME_DIR", &runtime.path().to_string_lossy());

    with_test_home(home.path(), || {
        seed_stale_skill(SkillKind::Module, SkillScope::User);

        let staleness = aggregate_skill_staleness();
        assert!(staleness.any(), "skill is stale");
        let surfaces = compute_update_surfaces(/*binary_available=*/ true, staleness.any());
        assert!(
            surfaces.shows_binary && !surfaces.shows_skills,
            "binary pending suppresses the skill surface: {surfaces:?}"
        );
    });
}

#[test]
#[serial_test::serial]
fn auto_standalone_refresh_clears_user_staleness_only() {
    // Auto standalone-stale (binary current): refreshing user-scope clears user
    // staleness; project staleness remains (never auto-written).
    let home = tempfile::tempdir().expect("home tempdir");
    let project = tempfile::tempdir().expect("project tempdir");
    let runtime = tempfile::tempdir().expect("runtime tempdir");
    let _runtime_env = EnvVarGuard::set("CFGD_RUNTIME_DIR", &runtime.path().to_string_lossy());
    let _cwd = CwdGuard::set(project.path()).expect("set cwd to project dir");

    with_test_home(home.path(), || {
        seed_stale_skill(SkillKind::Module, SkillScope::User);
        let project_path = seed_stale_skill(SkillKind::Module, SkillScope::Project);
        let project_before = std::fs::read(&project_path).expect("read project skill");

        let before = aggregate_skill_staleness();
        assert_eq!((before.user, before.project), (1, 1));

        let outcome =
            refresh_user_scope_skills(&cfg(UpdatePolicy::Auto, SkillUpdatePolicy::Inherit));
        assert!(outcome.user_scope_skills_refreshed);

        let after = aggregate_skill_staleness();
        assert_eq!(
            after.user, 0,
            "user staleness cleared by refresh: {after:?}"
        );
        assert_eq!(after.project, 1, "project staleness untouched: {after:?}");
        // Project bytes byte-identical — never auto-written.
        let project_after = std::fs::read(&project_path).expect("re-read project skill");
        assert_eq!(
            project_before, project_after,
            "project-scope skill bytes unchanged by Auto standalone refresh"
        );
    });
}

// ----- Single-sourced orchestration: run_standalone_skill_action -----

#[test]
#[serial_test::serial]
fn run_action_notify_yields_one_consolidated_notice_both_scopes() {
    let home = tempfile::tempdir().expect("home tempdir");
    let project = tempfile::tempdir().expect("project tempdir");
    let runtime = tempfile::tempdir().expect("runtime tempdir");
    let _runtime_env = EnvVarGuard::set("CFGD_RUNTIME_DIR", &runtime.path().to_string_lossy());
    let _cwd = CwdGuard::set(project.path()).expect("set cwd to project dir");

    with_test_home(home.path(), || {
        seed_stale_skill(SkillKind::Module, SkillScope::User);
        seed_stale_skill(SkillKind::Module, SkillScope::Project);

        let result = run_standalone_skill_action(
            &cfg(UpdatePolicy::Notify, SkillUpdatePolicy::Inherit),
            false,
        );
        assert_eq!(
            result,
            StandaloneSkillOutcome::NoticeNeeded(SkillStaleness {
                user: 1,
                project: 1
            }),
            "Notify standalone-stale → one consolidated both-scopes notice"
        );
    });
}

#[test]
#[serial_test::serial]
fn run_action_binary_pending_suppresses() {
    let home = tempfile::tempdir().expect("home tempdir");
    let runtime = tempfile::tempdir().expect("runtime tempdir");
    let _runtime_env = EnvVarGuard::set("CFGD_RUNTIME_DIR", &runtime.path().to_string_lossy());

    with_test_home(home.path(), || {
        seed_stale_skill(SkillKind::Module, SkillScope::User);
        // Rule 1: a binary update is pending → suppressed regardless of staleness.
        let result = run_standalone_skill_action(
            &cfg(UpdatePolicy::Notify, SkillUpdatePolicy::Inherit),
            true,
        );
        assert_eq!(result, StandaloneSkillOutcome::Suppressed);
    });
}

#[test]
#[serial_test::serial]
fn run_action_auto_refreshes_user_then_notices_project_only() {
    let home = tempfile::tempdir().expect("home tempdir");
    let project = tempfile::tempdir().expect("project tempdir");
    let runtime = tempfile::tempdir().expect("runtime tempdir");
    let _runtime_env = EnvVarGuard::set("CFGD_RUNTIME_DIR", &runtime.path().to_string_lossy());
    let _cwd = CwdGuard::set(project.path()).expect("set cwd to project dir");

    with_test_home(home.path(), || {
        seed_stale_skill(SkillKind::Module, SkillScope::User);
        let project_path = seed_stale_skill(SkillKind::Module, SkillScope::Project);
        let project_before = std::fs::read(&project_path).expect("read project skill");

        let result = run_standalone_skill_action(
            &cfg(UpdatePolicy::Auto, SkillUpdatePolicy::Inherit),
            false,
        );
        // User-scope refreshed in place → remaining notice covers project only.
        assert_eq!(
            result,
            StandaloneSkillOutcome::NoticeNeeded(SkillStaleness {
                user: 0,
                project: 1
            }),
            "Auto refreshes user-scope, leaves a project-only notice"
        );
        let project_after = std::fs::read(&project_path).expect("re-read project skill");
        assert_eq!(
            project_before, project_after,
            "project-scope skill bytes unchanged"
        );
    });
}

#[test]
#[serial_test::serial]
fn run_action_auto_with_only_user_stale_is_refreshed_no_notice() {
    let home = tempfile::tempdir().expect("home tempdir");
    let runtime = tempfile::tempdir().expect("runtime tempdir");
    let _runtime_env = EnvVarGuard::set("CFGD_RUNTIME_DIR", &runtime.path().to_string_lossy());

    with_test_home(home.path(), || {
        seed_stale_skill(SkillKind::Module, SkillScope::User);
        let result = run_standalone_skill_action(
            &cfg(UpdatePolicy::Auto, SkillUpdatePolicy::Inherit),
            false,
        );
        assert_eq!(
            result,
            StandaloneSkillOutcome::Refreshed,
            "Auto with only user-scope stale → refreshed, no notice needed"
        );
        assert_eq!(
            aggregate_skill_staleness().user,
            0,
            "user staleness cleared"
        );
    });
}

#[test]
#[serial_test::serial]
fn run_action_manual_is_silent() {
    let home = tempfile::tempdir().expect("home tempdir");
    let runtime = tempfile::tempdir().expect("runtime tempdir");
    let _runtime_env = EnvVarGuard::set("CFGD_RUNTIME_DIR", &runtime.path().to_string_lossy());

    with_test_home(home.path(), || {
        seed_stale_skill(SkillKind::Module, SkillScope::User);
        let result = run_standalone_skill_action(
            &cfg(UpdatePolicy::Manual, SkillUpdatePolicy::Inherit),
            false,
        );
        assert_eq!(result, StandaloneSkillOutcome::Silent);
    });
}

#[test]
#[serial_test::serial]
fn run_action_nothing_stale_is_suppressed() {
    let home = tempfile::tempdir().expect("home tempdir");
    let runtime = tempfile::tempdir().expect("runtime tempdir");
    let _runtime_env = EnvVarGuard::set("CFGD_RUNTIME_DIR", &runtime.path().to_string_lossy());

    with_test_home(home.path(), || {
        // Nothing installed → nothing stale → suppressed even under Notify.
        let result = run_standalone_skill_action(
            &cfg(UpdatePolicy::Notify, SkillUpdatePolicy::Inherit),
            false,
        );
        assert_eq!(result, StandaloneSkillOutcome::Suppressed);
    });
}
