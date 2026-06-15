//! End-to-end filesystem behavior of the `SkillProvider` default `install` /
//! `remove` / `list` impls (spec §5, §5A): idempotent surgical writes, AGENTS.md
//! managed-block surgery preserving surrounding bytes, whole-file roundtrip with
//! empty-dir cleanup, and the `list` payload + version-stamp staleness flag.
//!
//! Every test that resolves a user-scope path runs inside `with_test_home` and is
//! `#[serial]` because the HOME override is a process-global thread-local guard and
//! these tests mutate the same env.

use cfgd_core::generate::{SkillKind, skill_model_for};
use cfgd_core::providers::skill::{ClaudeCodeProvider, CodexProvider, SkillProvider, SkillScope};
use cfgd_core::with_test_home;

#[test]
#[serial_test::serial]
fn install_then_remove_is_clean_roundtrip() {
    let home = tempfile::tempdir().expect("tempdir");
    with_test_home(home.path(), || {
        let provider = ClaudeCodeProvider;
        let model = skill_model_for(SkillKind::Module);

        let path = provider
            .install(&model, SkillScope::User)
            .expect("install writes the SKILL.md");

        // The file lands at the provider's user-scope target under the test HOME,
        // with exactly the rendered contents (not merely "contains").
        let expected_path = provider
            .target_path(SkillKind::Module, SkillScope::User)
            .expect("user scope has a target");
        assert_eq!(path, expected_path);
        assert!(
            path.starts_with(home.path()),
            "must write under test HOME: {path:?}"
        );
        assert!(path.ends_with(".claude/skills/cfgd-module/SKILL.md"));
        let on_disk = std::fs::read_to_string(&path).expect("read installed file");
        assert_eq!(on_disk, provider.render(&model).contents);

        let parent = path.parent().expect("file has a parent dir");
        assert!(parent.is_dir(), "cfgd-module dir created");

        // remove deletes the file AND the now-empty cfgd-<kind> parent dir.
        let removed = provider
            .remove(SkillKind::Module, SkillScope::User)
            .expect("remove succeeds");
        assert_eq!(removed.as_deref(), Some(path.as_path()));
        assert!(!path.exists(), "SKILL.md must be gone after remove");
        assert!(!parent.exists(), "emptied cfgd-module dir must be removed");

        // A second remove is a reported no-op (nothing installed → None).
        let again = provider
            .remove(SkillKind::Module, SkillScope::User)
            .expect("second remove succeeds");
        assert_eq!(again, None, "remove on absent skill is a None no-op");
    });
}

#[test]
#[serial_test::serial]
fn agents_md_managed_section_surgery_preserves_user_content() {
    let home = tempfile::tempdir().expect("tempdir");
    with_test_home(home.path(), || {
        let provider = CodexProvider;
        let model = skill_model_for(SkillKind::Module);

        let target = provider
            .target_path(SkillKind::Module, SkillScope::User)
            .expect("user scope has a target");
        std::fs::create_dir_all(target.parent().expect("parent")).expect("mkdir ~/.codex");

        // Seed user content BOTH above and below where the cfgd block will splice.
        // The block appends after existing content, so both halves are "before" the
        // block on disk; we assert every original byte survives verbatim.
        let seed = "# My AGENTS.md\n\nTop user guidance.\n\nBottom user guidance.\n";
        std::fs::write(&target, seed).expect("seed AGENTS.md");

        let path = provider
            .install(&model, SkillScope::User)
            .expect("codex install splices the block");
        assert_eq!(path, target);

        let after_install = std::fs::read_to_string(&target).expect("read after install");
        // The cfgd block is present, fully delimited by its kind-stamped markers.
        let begin = "<!-- cfgd:skill:module -->";
        let end = "<!-- /cfgd:skill:module -->";
        assert!(after_install.contains(begin), "begin marker present");
        assert!(after_install.contains(end), "end marker present");
        let bpos = after_install.find(begin).expect("begin offset");
        let epos = after_install.find(end).expect("end offset");
        assert!(bpos < epos, "begin precedes end");

        // The user's seed survives byte-identically as the file's leading bytes:
        // splice appends, so `after_install` starts with the trimmed seed + blank.
        assert!(
            after_install.starts_with(seed.trim_end_matches('\n')),
            "user content above the block must be byte-identical, got:\n{after_install}"
        );
        // And the literal user lines are still present, unmangled.
        assert!(after_install.contains("Top user guidance."));
        assert!(after_install.contains("Bottom user guidance."));

        // remove excises ONLY the block, leaving the file with user content intact.
        let removed = provider
            .remove(SkillKind::Module, SkillScope::User)
            .expect("codex remove succeeds");
        assert_eq!(removed.as_deref(), Some(target.as_path()));
        let after_remove =
            std::fs::read_to_string(&target).expect("file still exists after remove");
        assert!(target.exists(), "codex remove leaves the shared file");
        assert!(
            !after_remove.contains(begin),
            "block markers gone after remove"
        );
        assert!(!after_remove.contains(end));
        // Surrounding bytes restored byte-identical to the original seed (the
        // excise collapses the trailing whitespace it added back to the seed).
        assert_eq!(
            after_remove, seed,
            "removing the block must restore the original user bytes verbatim"
        );
    });
}

#[test]
#[serial_test::serial]
fn reinstall_is_idempotent_noop_diff() {
    let home = tempfile::tempdir().expect("tempdir");
    with_test_home(home.path(), || {
        // Whole-file provider (claude-code): second write == first write byte-for-byte.
        let claude = ClaudeCodeProvider;
        let cmodel = skill_model_for(SkillKind::Profile);
        let cpath = claude
            .install(&cmodel, SkillScope::User)
            .expect("first claude install");
        let first = std::fs::read_to_string(&cpath).expect("read first");
        let cpath2 = claude
            .install(&cmodel, SkillScope::User)
            .expect("second claude install");
        assert_eq!(cpath, cpath2, "same target on reinstall");
        let second = std::fs::read_to_string(&cpath2).expect("read second");
        assert_eq!(
            first, second,
            "claude-code reinstall must be byte-identical"
        );

        // Managed-section provider (codex): re-splicing the same block must not
        // duplicate it or drift surrounding bytes.
        let codex = CodexProvider;
        let xmodel = skill_model_for(SkillKind::Profile);
        let xtarget = codex
            .target_path(SkillKind::Profile, SkillScope::User)
            .expect("codex user target");
        std::fs::create_dir_all(xtarget.parent().expect("parent")).expect("mkdir ~/.codex");
        std::fs::write(&xtarget, "# AGENTS\n\nuser line\n").expect("seed");

        let xp1 = codex
            .install(&xmodel, SkillScope::User)
            .expect("first codex install");
        let xfirst = std::fs::read_to_string(&xp1).expect("read first codex");
        let xp2 = codex
            .install(&xmodel, SkillScope::User)
            .expect("second codex install");
        assert_eq!(xp1, xp2, "same target on reinstall");
        let xsecond = std::fs::read_to_string(&xp2).expect("read second codex");
        assert_eq!(
            xfirst, xsecond,
            "codex reinstall must be byte-identical (no block duplication)"
        );
        // Exactly one cfgd block, not two.
        assert_eq!(
            xsecond.matches("<!-- cfgd:skill:profile -->").count(),
            1,
            "reinstall must not duplicate the managed block"
        );
    });
}

#[test]
#[serial_test::serial]
fn list_reports_installed_skill_with_current_version_not_stale() {
    let home = tempfile::tempdir().expect("tempdir");
    with_test_home(home.path(), || {
        let provider = ClaudeCodeProvider;
        let model = skill_model_for(SkillKind::Source);

        // Nothing installed yet: list at this scope is empty.
        let before = provider
            .list(SkillScope::User)
            .expect("list before install");
        assert!(
            before.is_empty(),
            "no skills installed yet, got: {before:?}"
        );

        let path = provider.install(&model, SkillScope::User).expect("install");

        let listed = provider.list(SkillScope::User).expect("list after install");
        assert_eq!(
            listed.len(),
            1,
            "exactly the one installed skill, got: {listed:?}"
        );
        let entry = &listed[0];
        assert_eq!(entry.kind, SkillKind::Source);
        assert_eq!(entry.provider, "claude-code");
        assert_eq!(entry.path, path);
        // The stamp written at install time is the running cfgd version, so it must
        // parse back out and read as NOT stale (stamped == running).
        assert_eq!(
            entry.cfgd_version.as_deref(),
            Some(env!("CARGO_PKG_VERSION")),
            "list must read back the stamped running version"
        );
        assert!(
            !entry.stale,
            "freshly installed skill at current version is not stale"
        );
    });
}
