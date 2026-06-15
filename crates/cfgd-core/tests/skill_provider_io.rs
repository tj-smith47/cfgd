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
        assert_eq!(
            on_disk,
            provider
                .render(&model)
                .expect("render is infallible for these fixtures")
                .contents
        );

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
fn agents_md_inplace_block_preserves_trailing_user_content() {
    let home = tempfile::tempdir().expect("tempdir");
    with_test_home(home.path(), || {
        let provider = CodexProvider;
        let model = skill_model_for(SkillKind::Module);

        let target = provider
            .target_path(SkillKind::Module, SkillScope::User)
            .expect("user scope has a target");
        std::fs::create_dir_all(target.parent().expect("parent")).expect("mkdir ~/.codex");

        let begin = "<!-- cfgd:skill:module -->";
        let end = "<!-- /cfgd:skill:module -->";
        let leading = "# My AGENTS.md\n\nTop user guidance.\n\n";
        // An already-present cfgd block with a STALE body, plus user prose AFTER the end
        // marker. This forces install down `splice_block`'s in-place REPLACE path
        // (block_span returns Some) rather than the append path — the only path that
        // can drop or duplicate trailing bytes if the splice ever appended instead.
        let trailing = "\n\n## My custom section\n\nTRAILING_SENTINEL line below the block.\n";
        let seed = format!("{leading}{begin}\nSTALE BODY — must be replaced.\n{end}{trailing}");
        std::fs::write(&target, &seed).expect("seed AGENTS.md with an existing block");

        let path = provider
            .install(&model, SkillScope::User)
            .expect("codex install splices in place");
        assert_eq!(path, target);

        let after = std::fs::read_to_string(&target).expect("read after install");
        // The block was replaced in place, not appended: exactly one begin/end pair.
        assert_eq!(
            after.matches(begin).count(),
            1,
            "in-place replace must not duplicate the begin marker, got:\n{after}"
        );
        assert_eq!(after.matches(end).count(), 1, "exactly one end marker");
        // Leading user prose is byte-preserved as the file head.
        assert!(
            after.starts_with(leading),
            "leading user bytes must survive verbatim, got:\n{after}"
        );
        // Trailing user prose after the end marker survives — the in-place splice
        // re-emits `existing[end..]` unchanged, so the sentinel and its section header
        // remain. This is the assertion that goes red if splice ever appended.
        assert!(
            after.contains("## My custom section"),
            "trailing user section header must survive, got:\n{after}"
        );
        assert!(
            after.contains("TRAILING_SENTINEL line below the block."),
            "trailing user sentinel must survive in place, got:\n{after}"
        );
        // The block body was actually refreshed: the stale body is gone and the
        // freshly-rendered guidance landed between the markers.
        assert!(
            !after.contains("STALE BODY — must be replaced."),
            "stale block body must be replaced, got:\n{after}"
        );
        let fresh_body = provider
            .render(&model)
            .expect("render is infallible for these fixtures")
            .managed_section
            .expect("codex renders a managed section")
            .body;
        assert!(
            after.contains(&fresh_body),
            "the rendered body must be spliced in, got:\n{after}"
        );

        // remove excises only the block; both halves of user prose round-trip
        // (modulo the documented blank-line collapse around the excised span).
        let removed = provider
            .remove(SkillKind::Module, SkillScope::User)
            .expect("codex remove succeeds");
        assert_eq!(removed.as_deref(), Some(target.as_path()));
        let after_remove = std::fs::read_to_string(&target).expect("file survives remove");
        assert!(
            !after_remove.contains(begin) && !after_remove.contains(end),
            "block markers gone after remove, got:\n{after_remove}"
        );
        assert!(
            after_remove.contains("Top user guidance."),
            "leading user prose round-trips, got:\n{after_remove}"
        );
        assert!(
            after_remove.contains("## My custom section")
                && after_remove.contains("TRAILING_SENTINEL line below the block."),
            "trailing user prose round-trips after excise, got:\n{after_remove}"
        );
    });
}

#[test]
#[serial_test::serial]
fn concurrent_installs_of_different_kinds_dont_corrupt_delimiters() {
    // Spec §12: two concurrent installs of DIFFERENT kinds into the same AGENTS.md
    // must not corrupt delimiters. The advisory lock (`acquire_apply_lock`) is
    // non-blocking — a contending install fails fast with a lock-held error rather
    // than racing into the read-modify-write, so the integrity guarantee holds by
    // serialization. A real caller retries on that transient lock-held error; each
    // thread does the same here, which is precisely what forces them to interleave
    // on the flock. Spawned threads do NOT inherit the thread-local HOME guard, so
    // each re-enters `with_test_home` to resolve the same user-scope target and
    // contend on the real flock keyed off that file's parent dir.
    let home = tempfile::tempdir().expect("tempdir");
    let home_path = home.path().to_path_buf();

    let begin_profile = "<!-- cfgd:skill:profile -->";
    let end_profile = "<!-- /cfgd:skill:profile -->";
    let begin_source = "<!-- cfgd:skill:source -->";
    let end_source = "<!-- /cfgd:skill:source -->";

    let target = with_test_home(home.path(), || {
        let target = CodexProvider
            .target_path(SkillKind::Profile, SkillScope::User)
            .expect("user scope has a target");
        std::fs::create_dir_all(target.parent().expect("parent")).expect("mkdir ~/.codex");
        target
    });

    // Loop the contended spawn-join so the two installs reliably interleave on the
    // lock; kept to a bound that runs well under a second.
    const ROUNDS: usize = 200;
    let install_kind = |kind: SkillKind, hp: std::path::PathBuf| {
        std::thread::spawn(move || {
            with_test_home(&hp, || {
                let model = skill_model_for(kind);
                loop {
                    match CodexProvider.install(&model, SkillScope::User) {
                        Ok(_) => break,
                        // The non-blocking lock surfaces contention as a lock-held
                        // error; a real caller retries, which is the contention this
                        // test means to exercise. Any other error is a genuine fault.
                        Err(e) if e.to_string().to_lowercase().contains("lock") => {
                            std::thread::yield_now();
                        }
                        Err(e) => panic!("concurrent codex install failed: {e}"),
                    }
                }
            });
        })
    };

    for _ in 0..ROUNDS {
        let t_profile = install_kind(SkillKind::Profile, home_path.clone());
        let t_source = install_kind(SkillKind::Source, home_path.clone());
        t_profile.join().expect("profile install thread");
        t_source.join().expect("source install thread");
    }

    let final_contents = std::fs::read_to_string(&target).expect("read final AGENTS.md");

    // Exactly one well-formed block per kind — no duplication from a lost-update race.
    for (begin, end, label) in [
        (begin_profile, end_profile, "profile"),
        (begin_source, end_source, "source"),
    ] {
        assert_eq!(
            final_contents.matches(begin).count(),
            1,
            "exactly one {label} begin marker, got:\n{final_contents}"
        );
        assert_eq!(
            final_contents.matches(end).count(),
            1,
            "exactly one {label} end marker, got:\n{final_contents}"
        );
    }

    // No torn/interleaved delimiters: each begin is immediately closed by its own end
    // with no second begin opening before it closes. Walking the markers in document
    // order must yield a strict begin→end→begin→end sequence with matching kinds.
    let mut markers: Vec<(usize, &str, bool)> = Vec::new();
    for (begin, end, _) in [
        (begin_profile, end_profile, "profile"),
        (begin_source, end_source, "source"),
    ] {
        let b = final_contents.find(begin).expect("begin present");
        let e = final_contents.find(end).expect("end present");
        markers.push((b, begin, true));
        markers.push((e, end, false));
    }
    markers.sort_by_key(|(pos, _, _)| *pos);
    let mut open_block: Option<&str> = None;
    for (_, marker, is_begin) in &markers {
        if *is_begin {
            assert!(
                open_block.is_none(),
                "a block opened before the previous one closed (torn delimiters): {marker}"
            );
            // The matching end is the begin marker with a leading slash inserted.
            open_block = Some(marker.trim_start_matches("<!-- cfgd:skill:"));
        } else {
            let opener = open_block.take().expect("an end marker with no open block");
            let closing = marker.trim_start_matches("<!-- /cfgd:skill:");
            assert_eq!(
                opener, closing,
                "end marker closes a different kind than was opened: {marker}"
            );
        }
    }
    assert!(open_block.is_none(), "unclosed block at end of file");

    // Both kinds' bodies are present and the version stamp parses out of the file.
    with_test_home(home.path(), || {
        let profile_body = CodexProvider
            .render(&skill_model_for(SkillKind::Profile))
            .expect("render is infallible for these fixtures")
            .managed_section
            .expect("profile section")
            .body;
        let source_body = CodexProvider
            .render(&skill_model_for(SkillKind::Source))
            .expect("render is infallible for these fixtures")
            .managed_section
            .expect("source section")
            .body;
        assert!(
            final_contents.contains(&profile_body),
            "profile body present, got:\n{final_contents}"
        );
        assert!(
            final_contents.contains(&source_body),
            "source body present, got:\n{final_contents}"
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
