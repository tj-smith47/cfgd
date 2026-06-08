use super::*;

#[test]
fn parse_duration_str_seconds() {
    let d = parse_duration_str("30s").unwrap();
    assert_eq!(d, std::time::Duration::from_secs(30));
}

#[test]
fn parse_duration_str_minutes() {
    let d = parse_duration_str("5m").unwrap();
    assert_eq!(d, std::time::Duration::from_secs(300));
}

#[test]
fn parse_duration_str_hours() {
    let d = parse_duration_str("1h").unwrap();
    assert_eq!(d, std::time::Duration::from_secs(3600));
}

#[test]
fn parse_duration_str_plain_seconds() {
    let d = parse_duration_str("60").unwrap();
    assert_eq!(d, std::time::Duration::from_secs(60));
}

#[test]
fn parse_duration_str_whitespace() {
    let d = parse_duration_str(" 10 s ").unwrap();
    assert_eq!(d, std::time::Duration::from_secs(10));
}

#[test]
fn parse_duration_str_days() {
    let d = parse_duration_str("30d").unwrap();
    assert_eq!(d, std::time::Duration::from_secs(30 * 86400));
}

#[test]
fn parse_duration_str_invalid() {
    assert!(
        parse_duration_str("abc")
            .unwrap_err()
            .contains("invalid timeout"),
        "bare letters should fail with a useful message"
    );
    assert!(
        parse_duration_str("")
            .unwrap_err()
            .contains("invalid timeout"),
        "empty string should fail"
    );
    assert!(
        parse_duration_str("xs")
            .unwrap_err()
            .contains("invalid timeout"),
        "non-numeric prefix should fail"
    );
}

#[test]
fn parse_duration_str_zero() {
    assert_eq!(
        parse_duration_str("0s").unwrap(),
        std::time::Duration::from_secs(0)
    );
    assert_eq!(
        parse_duration_str("0").unwrap(),
        std::time::Duration::from_secs(0)
    );
}

#[test]
fn parse_duration_str_negative() {
    assert!(
        parse_duration_str("-5s").is_err(),
        "negative durations should be rejected"
    );
}

#[test]
fn parse_loose_version_full_semver() {
    assert_eq!(
        parse_loose_version("1.28.3"),
        Some(semver::Version::new(1, 28, 3))
    );
    assert_eq!(
        parse_loose_version("0.1.0"),
        Some(semver::Version::new(0, 1, 0))
    );
}

#[test]
fn parse_loose_version_two_part() {
    let ver = parse_loose_version("1.28").unwrap();
    assert_eq!(ver, semver::Version::new(1, 28, 0));
}

#[test]
fn parse_loose_version_single_part() {
    let ver = parse_loose_version("1").unwrap();
    assert_eq!(ver, semver::Version::new(1, 0, 0));
}

#[test]
fn parse_loose_version_rejects_garbage() {
    assert!(parse_loose_version("abc").is_none());
    assert!(parse_loose_version("").is_none());
    assert!(parse_loose_version("1.2.3.4").is_none());
    assert!(
        parse_loose_version("-1").is_none(),
        "negative numbers are not valid versions"
    );
}

#[test]
fn parse_loose_version_preserves_prerelease() {
    // semver::Version::parse handles pre-release tags
    let ver = parse_loose_version("1.2.3-beta.1").unwrap();
    assert_eq!(ver.major, 1);
    assert_eq!(ver.minor, 2);
    assert_eq!(ver.patch, 3);
    assert!(!ver.pre.is_empty(), "pre-release should be preserved");
}

#[test]
fn parse_loose_version_strips_v_prefix() {
    // git/OCI tag convention is `vX.Y.Z` — the parser must accept it so
    // `latest_module_version` ranks `v1.10.0` > `v1.9.0` instead of falling
    // back to lexical compare (which would silently invert the ordering).
    assert_eq!(
        parse_loose_version("v1.10.0"),
        Some(semver::Version::new(1, 10, 0))
    );
    assert_eq!(
        parse_loose_version("v1.9.0"),
        Some(semver::Version::new(1, 9, 0))
    );
    // Capital V is rare but cheap to accept and matches what some tag
    // generators emit.
    assert_eq!(
        parse_loose_version("V2.0.0"),
        Some(semver::Version::new(2, 0, 0))
    );
    // Loose two-part still works under the prefix.
    assert_eq!(
        parse_loose_version("v1.28"),
        Some(semver::Version::new(1, 28, 0))
    );
    assert_eq!(
        parse_loose_version("v1"),
        Some(semver::Version::new(1, 0, 0))
    );
}

#[test]
fn parse_loose_version_v_prefix_compares_correctly() {
    // The actual ordering invariant the registry tag-sort relies on.
    let lo = parse_loose_version("v1.9.0").unwrap();
    let hi = parse_loose_version("v1.10.0").unwrap();
    assert!(lo < hi, "v1.9.0 must sort before v1.10.0");
}

#[test]
fn parse_loose_version_does_not_strip_other_prefixes() {
    // Only `v`/`V` are stripped — anything else is rejected so callers can
    // distinguish "git tag" from arbitrary identifier strings.
    assert!(parse_loose_version("release-1.0.0").is_none());
    assert!(parse_loose_version("ver1.0.0").is_none());
    assert!(parse_loose_version("1v.0.0").is_none());
}

#[test]
fn version_satisfies_accepts_v_prefixed_version() {
    // `available_version` from package managers usually doesn't include
    // `v`, but module entries pulled from git tags can — verify the loose
    // parser pulls through.
    assert!(version_satisfies("v1.28.3", ">=1.28"));
    assert!(!version_satisfies("v1.27.0", ">=1.28"));
}

#[test]
fn version_satisfies_basic() {
    assert!(version_satisfies("1.28.3", ">=1.28"));
    assert!(!version_satisfies("1.27.0", ">=1.28"));
    assert!(version_satisfies("2.40.1", "~2.40"));
    assert!(!version_satisfies("2.39.0", "~2.40"));
}

#[test]
fn version_satisfies_loose() {
    assert!(version_satisfies("1.28", ">=1.28"));
    assert!(version_satisfies("2", ">=1.28"));
    assert!(!version_satisfies("1", ">=1.28"));
}

#[test]
fn version_satisfies_invalid_requirement() {
    assert!(!version_satisfies("1.0.0", "not valid"));
}

#[cfg(unix)]
#[test]
fn home_dir_var_uses_home_on_unix() {
    let result = home_dir_var();
    assert!(result.is_some());
    assert_eq!(result.unwrap(), std::env::var("HOME").unwrap());
}

#[test]
fn version_satisfies_invalid_version() {
    assert!(!version_satisfies("abc", ">=1.0"));
}

#[test]
fn atomic_write_creates_file_with_content() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("test.txt");
    let hash = atomic_write(&target, b"hello world").unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "hello world");
    assert!(!hash.is_empty());
    assert_eq!(hash.len(), 64); // SHA256 hex
}

#[test]
fn atomic_write_creates_parent_dirs() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("a/b/c/test.txt");
    atomic_write(&target, b"nested").unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "nested");
}

#[cfg(unix)]
#[test]
fn atomic_write_preserves_permissions() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("perms.txt");
    std::fs::write(&target, "old").unwrap();
    std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o600)).unwrap();

    atomic_write(&target, b"new").unwrap();

    let mode = std::fs::metadata(&target).unwrap().permissions().mode() & 0o777;
    assert_eq!(mode, 0o600);
}

#[test]
fn atomic_write_str_works() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("str.txt");
    let hash = atomic_write_str(&target, "string content").unwrap();
    assert_eq!(std::fs::read_to_string(&target).unwrap(), "string content");
    assert_eq!(hash.len(), 64);
}

#[test]
fn capture_file_state_regular_file() {
    let dir = tempfile::tempdir().unwrap();
    let target = dir.path().join("file.txt");
    std::fs::write(&target, "contents").unwrap();

    let state = capture_file_state(&target).unwrap().unwrap();
    assert_eq!(state.content, b"contents");
    assert!(!state.content_hash.is_empty());
    assert!(!state.is_symlink);
    assert!(state.symlink_target.is_none());
    assert!(!state.oversized);
}

#[test]
#[cfg(unix)]
fn capture_file_state_symlink() {
    let dir = tempfile::tempdir().unwrap();
    let real = dir.path().join("real.txt");
    let link = dir.path().join("link.txt");
    std::fs::write(&real, "target").unwrap();
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let state = capture_file_state(&link).unwrap().unwrap();
    assert!(state.is_symlink);
    assert_eq!(state.symlink_target.unwrap(), real);
    assert!(state.content.is_empty());
}

#[test]
fn capture_file_state_missing_returns_none() {
    let dir = tempfile::tempdir().unwrap();
    let missing = dir.path().join("does_not_exist.txt");
    let state = capture_file_state(&missing).unwrap();
    assert!(state.is_none());
}

#[test]
fn create_symlink_creates_link() {
    let dir = tempfile::tempdir().unwrap();
    let source = dir.path().join("source.txt");
    std::fs::write(&source, "hello").unwrap();
    let link = dir.path().join("link.txt");
    create_symlink(&source, &link).unwrap();
    assert!(link.symlink_metadata().unwrap().file_type().is_symlink());
    assert_eq!(std::fs::read_to_string(&link).unwrap(), "hello");
}

#[cfg(unix)]
#[test]
fn file_permissions_mode_returns_mode() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.txt");
    std::fs::write(&file, "test").unwrap();
    let meta = std::fs::metadata(&file).unwrap();
    let mode = file_permissions_mode(&meta);
    assert!(mode.is_some());
    let bits = mode.unwrap();
    assert!(bits & 0o777 > 0, "mode bits should be non-zero");
    assert!(
        bits & 0o400 != 0,
        "owner read bit should be set on a newly created file"
    );
}

#[cfg(unix)]
#[test]
fn set_file_permissions_changes_mode() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("test.txt");
    std::fs::write(&file, "test").unwrap();
    set_file_permissions(&file, 0o755).unwrap();
    let meta = std::fs::metadata(&file).unwrap();
    assert_eq!(file_permissions_mode(&meta), Some(0o755));
}

#[cfg(unix)]
#[test]
fn is_executable_checks_mode() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("script.sh");
    std::fs::write(&file, "#!/bin/sh").unwrap();

    set_file_permissions(&file, 0o644).unwrap();
    let meta = std::fs::metadata(&file).unwrap();
    assert!(!is_executable(&file, &meta));

    set_file_permissions(&file, 0o755).unwrap();
    let meta = std::fs::metadata(&file).unwrap();
    assert!(is_executable(&file, &meta));
}

#[cfg(unix)]
#[test]
fn is_same_inode_detects_hard_links() {
    let dir = tempfile::tempdir().unwrap();
    let file = dir.path().join("original.txt");
    std::fs::write(&file, "content").unwrap();
    let link = dir.path().join("hardlink.txt");
    std::fs::hard_link(&file, &link).unwrap();

    assert!(is_same_inode(&file, &link));
    assert!(!is_same_inode(&file, &dir.path().join("nonexistent")));
}

#[test]
fn validate_path_within_accepts_child() {
    let dir = tempfile::tempdir().unwrap();
    let child = dir.path().join("sub/file.txt");
    std::fs::create_dir_all(dir.path().join("sub")).unwrap();
    std::fs::write(&child, "").unwrap();
    assert!(validate_path_within(&child, dir.path()).is_ok());
}

#[test]
fn validate_path_within_rejects_escape() {
    // Use two independent tempdirs so the target exists on every platform
    // (/tmp is absent on Windows) and lives outside our designated root.
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let result = validate_path_within(outside.path(), root.path());
    let err = result.unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::PermissionDenied);
    assert!(
        err.to_string().contains("escapes root"),
        "expected 'escapes root' message, got: {err}"
    );
}

#[test]
fn validate_no_traversal_accepts_clean_path() {
    assert!(validate_no_traversal(std::path::Path::new("a/b/c")).is_ok());
    assert!(validate_no_traversal(std::path::Path::new("/absolute/path")).is_ok());
}

#[test]
fn validate_no_traversal_rejects_dotdot() {
    assert!(validate_no_traversal(std::path::Path::new("a/../b")).is_err());
    assert!(validate_no_traversal(std::path::Path::new("../../etc")).is_err());
}

#[test]
fn shell_escape_value_simple() {
    assert_eq!(shell_escape_value("hello"), "\"hello\"");
}

#[test]
fn shell_escape_value_with_dollar() {
    assert_eq!(shell_escape_value("$HOME/bin"), "'$HOME/bin'");
}

#[test]
fn shell_escape_value_with_single_quote() {
    assert_eq!(shell_escape_value("it's"), "'it'\\''s'");
}

#[test]
fn xml_escape_special_chars() {
    assert_eq!(xml_escape("<tag>&\"'"), "&lt;tag&gt;&amp;&quot;&apos;");
}

#[test]
fn xml_escape_passthrough() {
    assert_eq!(xml_escape("normal text"), "normal text");
}

#[test]
#[cfg(unix)] // Windows LockFileEx prevents reading lock file content while held
fn acquire_apply_lock_works() {
    let dir = tempfile::tempdir().unwrap();
    let guard = acquire_apply_lock(dir.path()).unwrap();
    // Lock file should contain our PID
    let content = std::fs::read_to_string(dir.path().join("apply.lock")).unwrap();
    assert_eq!(content, format!("{}", std::process::id()));
    drop(guard);
}

#[test]
fn acquire_apply_lock_detects_contention() {
    let dir = tempfile::tempdir().unwrap();
    let _guard = acquire_apply_lock(dir.path()).unwrap();
    // Second acquire should fail with ApplyLockHeld
    let result = acquire_apply_lock(dir.path());
    assert!(result.is_err());
    let err = result.unwrap_err();
    assert!(
        matches!(
            err,
            errors::CfgdError::State(errors::StateError::ApplyLockHeld { .. })
        ),
        "expected ApplyLockHeld, got: {}",
        err
    );
}

#[test]
fn merge_aliases_override_by_name() {
    let mut base = vec![
        config::ShellAlias {
            name: "vim".into(),
            command: "vi".into(),
        },
        config::ShellAlias {
            name: "ll".into(),
            command: "ls -l".into(),
        },
    ];
    let updates = vec![config::ShellAlias {
        name: "vim".into(),
        command: "nvim".into(),
    }];
    merge_aliases(&mut base, &updates);
    assert_eq!(base.len(), 2);
    assert_eq!(base[0].command, "nvim");
    assert_eq!(base[1].command, "ls -l");
}

#[test]
fn merge_aliases_appends_new() {
    let mut base = vec![config::ShellAlias {
        name: "vim".into(),
        command: "nvim".into(),
    }];
    let updates = vec![config::ShellAlias {
        name: "ll".into(),
        command: "ls -la".into(),
    }];
    merge_aliases(&mut base, &updates);
    assert_eq!(base.len(), 2);
}

#[test]
fn split_add_remove_basic() {
    let vals: Vec<String> = vec!["foo".into(), "-bar".into(), "baz".into(), "-qux".into()];
    let (adds, removes) = split_add_remove(&vals);
    assert_eq!(adds, vec!["foo", "baz"]);
    assert_eq!(removes, vec!["bar", "qux"]);
}

#[test]
fn split_add_remove_empty() {
    let (adds, removes) = split_add_remove(&[]);
    assert!(adds.is_empty());
    assert!(removes.is_empty());
}

#[test]
fn split_add_remove_all_adds() {
    let vals: Vec<String> = vec!["a".into(), "b".into()];
    let (adds, removes) = split_add_remove(&vals);
    assert_eq!(adds, vec!["a", "b"]);
    assert!(removes.is_empty());
}

#[test]
fn split_add_remove_all_removes() {
    let vals: Vec<String> = vec!["-x".into(), "-y".into()];
    let (adds, removes) = split_add_remove(&vals);
    assert!(adds.is_empty());
    assert_eq!(removes, vec!["x", "y"]);
}

#[test]
fn parse_alias_valid() {
    let alias = parse_alias("vim=nvim").unwrap();
    assert_eq!(alias.name, "vim");
    assert_eq!(alias.command, "nvim");
}

#[test]
fn parse_alias_with_args() {
    let alias = parse_alias("ll=ls -la --color").unwrap();
    assert_eq!(alias.name, "ll");
    assert_eq!(alias.command, "ls -la --color");
}

#[test]
fn parse_alias_invalid() {
    assert!(parse_alias("no-equals-sign").is_err());
}

#[test]
fn deep_merge_yaml_maps() {
    let mut base = serde_yaml::from_str::<serde_yaml::Value>("a: 1\nb: 2").unwrap();
    let overlay = serde_yaml::from_str::<serde_yaml::Value>("b: 3\nc: 4").unwrap();
    deep_merge_yaml(&mut base, &overlay);
    assert_eq!(base["a"], serde_yaml::Value::from(1));
    assert_eq!(base["b"], serde_yaml::Value::from(3));
    assert_eq!(base["c"], serde_yaml::Value::from(4));
}

#[test]
fn deep_merge_yaml_nested() {
    let mut base = serde_yaml::from_str::<serde_yaml::Value>("top:\n  a: 1\n  b: 2").unwrap();
    let overlay = serde_yaml::from_str::<serde_yaml::Value>("top:\n  b: 9\n  c: 3").unwrap();
    deep_merge_yaml(&mut base, &overlay);
    assert_eq!(base["top"]["a"], serde_yaml::Value::from(1));
    assert_eq!(base["top"]["b"], serde_yaml::Value::from(9));
    assert_eq!(base["top"]["c"], serde_yaml::Value::from(3));
}

#[test]
fn deep_merge_yaml_overlay_replaces_scalar() {
    let mut base = serde_yaml::from_str::<serde_yaml::Value>("x: old").unwrap();
    let overlay = serde_yaml::from_str::<serde_yaml::Value>("x: new").unwrap();
    deep_merge_yaml(&mut base, &overlay);
    assert_eq!(base["x"], serde_yaml::Value::from("new"));
}

#[test]
fn union_extend_deduplicates() {
    let mut target = vec!["a".to_string(), "b".to_string()];
    union_extend(&mut target, &["b".to_string(), "c".to_string()]);
    assert_eq!(target, vec!["a", "b", "c"]);
}

#[test]
fn union_extend_empty_source() {
    let mut target = vec!["a".to_string()];
    union_extend(&mut target, &[]);
    assert_eq!(target, vec!["a"]);
}

#[test]
fn merge_env_overrides_by_name() {
    let mut base = vec![
        config::EnvVar {
            name: "FOO".into(),
            value: "old".into(),
        },
        config::EnvVar {
            name: "BAR".into(),
            value: "keep".into(),
        },
    ];
    let updates = vec![config::EnvVar {
        name: "FOO".into(),
        value: "new".into(),
    }];
    merge_env(&mut base, &updates);
    assert_eq!(base.len(), 2);
    assert_eq!(base.iter().find(|e| e.name == "FOO").unwrap().value, "new");
    assert_eq!(base.iter().find(|e| e.name == "BAR").unwrap().value, "keep");
}

#[test]
fn merge_env_adds_new() {
    let mut base = vec![];
    let updates = vec![config::EnvVar {
        name: "NEW".into(),
        value: "val".into(),
    }];
    merge_env(&mut base, &updates);
    assert_eq!(base.len(), 1);
    assert_eq!(base[0].name, "NEW");
}

#[test]
fn shell_escape_value_metacharacters() {
    // Contains both single-quote AND $, so must use break-out escaping
    assert_eq!(shell_escape_value("it's a $test"), "'it'\\''s a $test'");
}

#[test]
fn shell_escape_value_backtick() {
    assert_eq!(shell_escape_value("`cmd`"), "'`cmd`'");
}

#[test]
fn shell_escape_value_backslash() {
    assert_eq!(shell_escape_value("a\\b"), "'a\\b'");
}

#[test]
fn shell_escape_value_empty() {
    assert_eq!(shell_escape_value(""), "\"\"");
}

#[test]
fn xml_escape_all_entities() {
    assert_eq!(
        xml_escape("a&b<c>d\"e'f"),
        "a&amp;b&lt;c&gt;d&quot;e&apos;f"
    );
}

#[test]
fn unix_secs_to_iso8601_epoch() {
    let result = unix_secs_to_iso8601(0);
    assert_eq!(result, "1970-01-01T00:00:00Z");
}

#[test]
fn unix_secs_to_iso8601_known_date() {
    let result = unix_secs_to_iso8601(1700000000);
    assert!(result.starts_with("2023-11-14"));
}

#[test]
fn copy_dir_recursive_copies_tree() {
    let src = tempfile::tempdir().unwrap();
    let dst = tempfile::tempdir().unwrap();
    let dst_path = dst.path().join("copy");
    std::fs::create_dir_all(src.path().join("sub")).unwrap();
    std::fs::write(src.path().join("a.txt"), "hello").unwrap();
    std::fs::write(src.path().join("sub/b.txt"), "world").unwrap();
    copy_dir_recursive(src.path(), &dst_path).unwrap();
    assert_eq!(
        std::fs::read_to_string(dst_path.join("a.txt")).unwrap(),
        "hello"
    );
    assert_eq!(
        std::fs::read_to_string(dst_path.join("sub/b.txt")).unwrap(),
        "world"
    );
}

#[test]
fn expand_tilde_with_home() {
    let result = expand_tilde(std::path::Path::new("~/test"));
    let home = home_dir_var().expect("home directory must be available in test");
    let expected = std::path::PathBuf::from(home).join("test");
    assert_eq!(result, expected);
}

#[test]
fn expand_tilde_absolute_unchanged() {
    let result = expand_tilde(std::path::Path::new("/absolute/path"));
    assert_eq!(result, std::path::PathBuf::from("/absolute/path"));
}

#[test]
fn with_test_home_scopes_override_and_restores() {
    // Sanity: no override active at start (other tests' guards must have
    // been released before this one ran on the same thread).
    assert!(test_home_override().is_none());

    let tmp = tempfile::tempdir().unwrap();
    let fake_home = tmp.path().to_path_buf();

    let expanded = with_test_home(&fake_home, || {
        // While scoped, `~` resolves to the tempdir.
        expand_tilde(std::path::Path::new("~/sub/file"))
    });
    assert_eq!(expanded, fake_home.join("sub").join("file"));

    // Override cleared on closure return.
    assert!(test_home_override().is_none());
}

#[test]
fn with_test_home_restores_on_panic() {
    let tmp = tempfile::tempdir().unwrap();
    let fake_home = tmp.path().to_path_buf();

    // catch_unwind to observe the panic without aborting the test.
    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        with_test_home(&fake_home, || {
            assert_eq!(test_home_override().as_deref(), Some(fake_home.as_path()));
            panic!("simulated failure");
        });
    }))
    .is_err();
    assert!(panicked, "closure should have panicked");

    // Guard must have restored on unwind.
    assert!(test_home_override().is_none());
}

#[test]
fn with_test_home_nests_correctly() {
    let outer_tmp = tempfile::tempdir().unwrap();
    let inner_tmp = tempfile::tempdir().unwrap();
    let outer = outer_tmp.path().to_path_buf();
    let inner = inner_tmp.path().to_path_buf();

    with_test_home(&outer, || {
        assert_eq!(test_home_override().as_deref(), Some(outer.as_path()));
        with_test_home(&inner, || {
            assert_eq!(test_home_override().as_deref(), Some(inner.as_path()));
        });
        // Inner guard dropped — outer restored.
        assert_eq!(test_home_override().as_deref(), Some(outer.as_path()));
    });
    assert!(test_home_override().is_none());
}

#[test]
fn default_config_dir_follows_override() {
    let tmp = tempfile::tempdir().unwrap();
    let fake_home = tmp.path().to_path_buf();
    let observed = with_test_home(&fake_home, default_config_dir);
    assert_eq!(observed, fake_home.join(".config").join("cfgd"));
}

#[test]
#[serial_test::serial]
fn default_config_dir_honors_absolute_xdg_config_home() {
    let tmp = tempfile::tempdir().unwrap();
    let xdg = tmp.path().join("xdg-config");
    crate::test_helpers::with_test_env_var("XDG_CONFIG_HOME", Some(&xdg.to_string_lossy()), || {
        assert_eq!(default_config_dir(), xdg.join("cfgd"));
    });
}

#[test]
#[serial_test::serial]
fn default_config_dir_ignores_empty_xdg_config_home() {
    // XDG spec: an empty XDG_CONFIG_HOME is treated as unset, never as the
    // current directory — a relative `cfgd` path would corrupt discovery.
    crate::test_helpers::with_test_env_var("XDG_CONFIG_HOME", Some(""), || {
        let dir = default_config_dir();
        assert_ne!(dir, std::path::PathBuf::from("cfgd"));
        assert!(
            dir.ends_with("cfgd"),
            "fell back to a platform default ending in cfgd, got {}",
            dir.display()
        );
    });
}

#[test]
#[serial_test::serial]
fn default_config_dir_ignores_relative_xdg_config_home() {
    // A relative XDG_CONFIG_HOME is non-conforming and must be ignored, not
    // joined (which would yield a relative, CWD-dependent config path).
    crate::test_helpers::with_test_env_var("XDG_CONFIG_HOME", Some("relative/cfg"), || {
        let dir = default_config_dir();
        assert_ne!(dir, std::path::PathBuf::from("relative/cfg").join("cfgd"));
        assert!(dir.ends_with("cfgd"));
    });
}

#[test]
fn macos_config_dir_prefers_existing_dotconfig() {
    // Non-breaking: an existing ~/.config/cfgd always wins, even when an
    // Application Support config dir is also present — an upgrade must not
    // strand the config a current macOS user already has at ~/.config.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let dotconfig = home.join(".config").join("cfgd");
    let app_support = home
        .join("Library")
        .join("Application Support")
        .join("cfgd");
    std::fs::create_dir_all(&dotconfig).unwrap();
    std::fs::create_dir_all(&app_support).unwrap();
    assert_eq!(resolve_macos_config_dir(home, |p| p.is_dir()), dotconfig);
}

#[test]
fn macos_config_dir_uses_dotconfig_when_only_it_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let dotconfig = home.join(".config").join("cfgd");
    std::fs::create_dir_all(&dotconfig).unwrap();
    assert_eq!(resolve_macos_config_dir(home, |p| p.is_dir()), dotconfig);
}

#[test]
fn macos_config_dir_fresh_install_defaults_to_application_support() {
    // Neither location exists: a fresh install lands at the native macOS
    // location, sharing one root with state and runtime.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    assert_eq!(
        resolve_macos_config_dir(home, |p| p.is_dir()),
        home.join("Library")
            .join("Application Support")
            .join("cfgd")
    );
}

#[test]
fn macos_legacy_migration_detected_when_only_legacy_exists() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    let legacy = home.join(".config").join("cfgd");
    std::fs::create_dir_all(&legacy).unwrap();
    let (got_legacy, got_native) = macos_legacy_config_migration(home).unwrap();
    assert_eq!(got_legacy, legacy);
    assert_eq!(
        got_native,
        home.join("Library")
            .join("Application Support")
            .join("cfgd")
    );
}

#[test]
fn macos_legacy_migration_none_when_native_exists() {
    // Native location present: nothing to migrate, even if legacy also exists.
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path();
    std::fs::create_dir_all(home.join(".config").join("cfgd")).unwrap();
    std::fs::create_dir_all(
        home.join("Library")
            .join("Application Support")
            .join("cfgd"),
    )
    .unwrap();
    assert!(macos_legacy_config_migration(home).is_none());
}

#[test]
fn macos_legacy_migration_none_when_no_legacy() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(macos_legacy_config_migration(tmp.path()).is_none());
}

#[test]
fn move_dir_relocates_tree_and_removes_source() {
    let tmp = tempfile::tempdir().unwrap();
    let src = tmp.path().join("src");
    let dst = tmp.path().join("nested").join("dst");
    std::fs::create_dir_all(src.join("sub")).unwrap();
    std::fs::write(src.join("a.txt"), b"top").unwrap();
    std::fs::write(src.join("sub").join("b.txt"), b"deep").unwrap();

    move_dir(&src, &dst).unwrap();

    assert!(!src.exists(), "source dir should be gone after move");
    assert_eq!(std::fs::read_to_string(dst.join("a.txt")).unwrap(), "top");
    assert_eq!(
        std::fs::read_to_string(dst.join("sub").join("b.txt")).unwrap(),
        "deep"
    );
}

#[test]
fn acquire_apply_lock_and_release() {
    let dir = tempfile::tempdir().unwrap();
    let guard = acquire_apply_lock(dir.path()).unwrap();
    assert!(dir.path().join("apply.lock").exists());
    drop(guard);
}

// --- Reconcile patch resolution tests ---

fn test_reconcile_config(patches: Vec<config::ReconcilePatch>) -> config::ReconcileConfig {
    config::ReconcileConfig {
        interval: "5m".into(),
        on_change: false,
        auto_apply: false,
        policy: None,
        drift_policy: config::DriftPolicy::NotifyOnly,
        patches,
    }
}

#[test]
fn resolve_reconcile_global_only() {
    let cfg = test_reconcile_config(vec![]);
    let eff = resolve_effective_reconcile("some-module", &["default"], &cfg);
    assert_eq!(eff.interval, "5m");
    assert!(!eff.auto_apply);
    assert_eq!(eff.drift_policy, config::DriftPolicy::NotifyOnly);
}

#[test]
fn resolve_reconcile_module_patch() {
    let cfg = test_reconcile_config(vec![config::ReconcilePatch {
        kind: config::ReconcilePatchKind::Module,
        name: Some("certs".into()),
        interval: Some("1m".into()),
        auto_apply: None,
        drift_policy: Some(config::DriftPolicy::Auto),
    }]);
    let eff = resolve_effective_reconcile("certs", &["default"], &cfg);
    assert_eq!(eff.interval, "1m");
    assert!(!eff.auto_apply); // inherited from global
    assert_eq!(eff.drift_policy, config::DriftPolicy::Auto);
}

#[test]
fn resolve_reconcile_profile_patch() {
    let cfg = test_reconcile_config(vec![config::ReconcilePatch {
        kind: config::ReconcilePatchKind::Profile,
        name: Some("work".into()),
        interval: None,
        auto_apply: Some(true),
        drift_policy: None,
    }]);
    let eff = resolve_effective_reconcile("any-mod", &["base", "work"], &cfg);
    assert_eq!(eff.interval, "5m"); // global
    assert!(eff.auto_apply); // from profile patch
}

#[test]
fn resolve_reconcile_module_beats_profile() {
    let cfg = test_reconcile_config(vec![
        config::ReconcilePatch {
            kind: config::ReconcilePatchKind::Profile,
            name: Some("work".into()),
            interval: None,
            auto_apply: Some(false),
            drift_policy: None,
        },
        config::ReconcilePatch {
            kind: config::ReconcilePatchKind::Module,
            name: Some("certs".into()),
            interval: None,
            auto_apply: Some(true),
            drift_policy: None,
        },
    ]);
    let eff = resolve_effective_reconcile("certs", &["work"], &cfg);
    assert!(eff.auto_apply); // module wins over profile
}

#[test]
fn resolve_reconcile_leaf_profile_wins() {
    let cfg = test_reconcile_config(vec![
        config::ReconcilePatch {
            kind: config::ReconcilePatchKind::Profile,
            name: Some("base".into()),
            interval: Some("10m".into()),
            auto_apply: None,
            drift_policy: None,
        },
        config::ReconcilePatch {
            kind: config::ReconcilePatchKind::Profile,
            name: Some("work".into()),
            interval: Some("2m".into()),
            auto_apply: None,
            drift_policy: None,
        },
    ]);
    // work is the leaf (last in chain) → wins
    let eff = resolve_effective_reconcile("any", &["base", "work"], &cfg);
    assert_eq!(eff.interval, "2m");
}

#[test]
fn resolve_reconcile_fields_merge_independently() {
    let cfg = test_reconcile_config(vec![
        config::ReconcilePatch {
            kind: config::ReconcilePatchKind::Profile,
            name: Some("work".into()),
            interval: Some("10m".into()),
            auto_apply: None,
            drift_policy: None,
        },
        config::ReconcilePatch {
            kind: config::ReconcilePatchKind::Module,
            name: Some("certs".into()),
            interval: None,
            auto_apply: None,
            drift_policy: Some(config::DriftPolicy::Auto),
        },
    ]);
    let eff = resolve_effective_reconcile("certs", &["work"], &cfg);
    assert_eq!(eff.interval, "10m"); // from profile patch
    assert_eq!(eff.drift_policy, config::DriftPolicy::Auto); // from module patch
    assert!(!eff.auto_apply); // from global
}

#[test]
fn resolve_reconcile_missing_module_ignored() {
    let cfg = test_reconcile_config(vec![config::ReconcilePatch {
        kind: config::ReconcilePatchKind::Module,
        name: Some("nonexistent".into()),
        interval: Some("1s".into()),
        auto_apply: None,
        drift_policy: None,
    }]);
    // Asking for a different module — patch doesn't apply
    let eff = resolve_effective_reconcile("other", &["default"], &cfg);
    assert_eq!(eff.interval, "5m");
}

#[test]
fn resolve_reconcile_duplicate_module_last_wins() {
    let cfg = test_reconcile_config(vec![
        config::ReconcilePatch {
            kind: config::ReconcilePatchKind::Module,
            name: Some("certs".into()),
            interval: Some("10m".into()),
            auto_apply: None,
            drift_policy: None,
        },
        config::ReconcilePatch {
            kind: config::ReconcilePatchKind::Module,
            name: Some("certs".into()),
            interval: Some("1m".into()),
            auto_apply: None,
            drift_policy: None,
        },
    ]);
    let eff = resolve_effective_reconcile("certs", &["default"], &cfg);
    assert_eq!(eff.interval, "1m"); // last entry wins
}

#[test]
fn resolve_reconcile_kind_wide_module_patch() {
    let cfg = test_reconcile_config(vec![config::ReconcilePatch {
        kind: config::ReconcilePatchKind::Module,
        name: None, // applies to all modules
        interval: Some("30s".into()),
        auto_apply: None,
        drift_policy: None,
    }]);
    let eff = resolve_effective_reconcile("any-module", &["default"], &cfg);
    assert_eq!(eff.interval, "30s");
}

#[test]
fn resolve_reconcile_named_beats_kind_wide() {
    let cfg = test_reconcile_config(vec![
        config::ReconcilePatch {
            kind: config::ReconcilePatchKind::Module,
            name: None, // all modules
            interval: Some("30s".into()),
            auto_apply: None,
            drift_policy: None,
        },
        config::ReconcilePatch {
            kind: config::ReconcilePatchKind::Module,
            name: Some("certs".into()), // specific
            interval: Some("5s".into()),
            auto_apply: None,
            drift_policy: None,
        },
    ]);
    // Named patch wins over kind-wide
    let eff = resolve_effective_reconcile("certs", &["default"], &cfg);
    assert_eq!(eff.interval, "5s");
    // Other modules get kind-wide
    let eff2 = resolve_effective_reconcile("other", &["default"], &cfg);
    assert_eq!(eff2.interval, "30s");
}

#[test]
fn validate_env_var_name_accepts_valid() {
    assert!(validate_env_var_name("PATH").is_ok());
    assert!(validate_env_var_name("_PRIVATE").is_ok());
    assert!(validate_env_var_name("MY_VAR_123").is_ok());
    assert!(validate_env_var_name("a").is_ok());
}

#[test]
fn validate_env_var_name_rejects_invalid() {
    let err = validate_env_var_name("").unwrap_err();
    assert!(err.contains("empty"), "empty should say empty: {err}");

    let err = validate_env_var_name("1STARTS_WITH_DIGIT").unwrap_err();
    assert!(
        err.contains("must start with"),
        "digit-prefix should explain: {err}"
    );

    // All of these should fail with the "invalid characters" message
    for bad in ["HAS SPACE", "HAS;SEMI", "HAS$DOLLAR", "HAS-DASH", "a=b"] {
        assert!(
            validate_env_var_name(bad).is_err(),
            "{bad:?} should be rejected"
        );
    }
}

#[test]
fn validate_env_var_user_name_accepts_normal() {
    assert!(validate_env_var_user_name("PATH").is_ok());
    assert!(validate_env_var_user_name("MY_APP_KEY").is_ok());
    assert!(validate_env_var_user_name("_PRIVATE").is_ok());
    assert!(validate_env_var_user_name("APP_FOO").is_ok());
}

#[test]
fn validate_env_var_user_name_rejects_cfgd_prefix() {
    let err = validate_env_var_user_name("CFGD_FOO").unwrap_err();
    assert!(err.contains("reserved"), "should mention reserved: {err}");
    assert!(err.contains("CFGD_"), "should mention the prefix: {err}");
    assert!(err.contains("APP_FOO"), "should suggest a rename: {err}");

    let err = validate_env_var_user_name("CFGD_").unwrap_err();
    assert!(
        err.contains("reserved"),
        "bare CFGD_ should be rejected: {err}"
    );
}

#[test]
fn validate_env_var_user_name_delegates_shell_safety() {
    let err = validate_env_var_user_name("").unwrap_err();
    assert!(
        err.contains("empty"),
        "empty should fail shell check: {err}"
    );

    let err = validate_env_var_user_name("1BAD").unwrap_err();
    assert!(
        err.contains("must start with"),
        "digit-leading should fail shell check: {err}"
    );
}

#[test]
fn validate_env_var_user_name_rejects_bash_env() {
    let err = validate_env_var_user_name("BASH_ENV").unwrap_err();
    assert!(
        err.contains("reserved"),
        "BASH_ENV should be rejected as reserved: {err}"
    );
    assert!(
        err.contains("alias delivery"),
        "error should explain the reservation reason: {err}"
    );
}

#[test]
fn validate_env_var_user_name_rejects_zdotdir() {
    let err = validate_env_var_user_name("ZDOTDIR").unwrap_err();
    assert!(
        err.contains("reserved"),
        "ZDOTDIR should be rejected as reserved: {err}"
    );
    assert!(
        err.contains("alias delivery"),
        "error should explain the reservation reason: {err}"
    );
}

#[test]
fn parse_env_var_rejects_cfgd_prefix() {
    let err = parse_env_var("CFGD_SECRET=value").unwrap_err();
    assert!(
        err.contains("reserved"),
        "parse_env_var should reject CFGD_*: {err}"
    );
}

#[test]
fn validate_alias_name_accepts_valid() {
    assert!(validate_alias_name("ls").is_ok());
    assert!(validate_alias_name("my-alias").is_ok());
    assert!(validate_alias_name("my.alias").is_ok());
    assert!(validate_alias_name("my_alias_123").is_ok());
}

#[test]
fn validate_alias_name_rejects_invalid() {
    let err = validate_alias_name("").unwrap_err();
    assert!(err.contains("empty"), "empty should say empty: {err}");

    for bad in ["has space", "has;semi", "has$dollar", "a=b", "has/slash"] {
        let err = validate_alias_name(bad).unwrap_err();
        assert!(
            err.contains("must contain only"),
            "{bad:?} rejection should explain allowed chars: {err}"
        );
    }
}

#[test]
fn parse_env_var_validates_name() {
    let ev = parse_env_var("VALID=value").unwrap();
    assert_eq!(ev.name, "VALID");
    assert_eq!(ev.value, "value");

    assert!(
        parse_env_var("1BAD=value")
            .unwrap_err()
            .contains("must start with"),
        "digit-leading name should fail"
    );
    assert!(parse_env_var("BAD;NAME=value").is_err());
}

#[test]
fn parse_env_var_value_with_equals() {
    // Values can contain '=' — only the first '=' splits key from value
    let ev = parse_env_var("PATH=/usr/bin:/bin").unwrap();
    assert_eq!(ev.name, "PATH");
    assert_eq!(ev.value, "/usr/bin:/bin");

    let ev2 = parse_env_var("FOO=a=b=c").unwrap();
    assert_eq!(ev2.name, "FOO");
    assert_eq!(ev2.value, "a=b=c");
}

#[test]
fn parse_env_var_empty_value() {
    let ev = parse_env_var("EMPTY=").unwrap();
    assert_eq!(ev.name, "EMPTY");
    assert_eq!(ev.value, "");
}

#[test]
fn parse_env_var_no_equals() {
    let err = parse_env_var("NOEQUALS").unwrap_err();
    assert!(
        err.contains("KEY=VALUE"),
        "should tell user the expected format, got: {err}"
    );
}

#[test]
fn parse_alias_validates_name() {
    let a = parse_alias("valid=ls -la").unwrap();
    assert_eq!(a.name, "valid");
    assert_eq!(a.command, "ls -la");

    let a2 = parse_alias("my-alias=git status").unwrap();
    assert_eq!(a2.name, "my-alias");
    assert_eq!(a2.command, "git status");

    assert!(parse_alias("bad;name=cmd").is_err());
}

#[test]
fn parse_alias_command_with_equals() {
    // Command can contain '=' — only the first splits name from command
    let a = parse_alias("env=FOO=bar baz").unwrap();
    assert_eq!(a.name, "env");
    assert_eq!(a.command, "FOO=bar baz");
}

#[test]
fn to_posix_string_folds_backslashes() {
    assert_eq!(to_posix_string("C:\\Users\\foo"), "C:/Users/foo");
    assert_eq!(to_posix_string("/home/foo"), "/home/foo");
    assert_eq!(to_posix_string("relative/path"), "relative/path");
}

#[test]
fn posixify_text_borrows_when_no_backslash() {
    let s = "no backslashes here";
    let cow = posixify_text(s);
    assert!(matches!(cow, std::borrow::Cow::Borrowed(_)));
    assert_eq!(cow, "no backslashes here");
}

#[test]
fn posixify_text_owns_when_backslash() {
    let s = "C:\\Users\\foo";
    let cow = posixify_text(s);
    assert!(matches!(cow, std::borrow::Cow::Owned(_)));
    assert_eq!(cow, "C:/Users/foo");
}

#[test]
fn to_file_url_unix_form() {
    assert_eq!(to_file_url("/home/foo"), "file:///home/foo");
}

#[test]
fn to_file_url_windows_form_folds_separators() {
    assert_eq!(to_file_url("C:\\Users\\foo"), "file:///C:/Users/foo");
    // Forward-slash already gets the same result — no special-case needed.
    assert_eq!(to_file_url("C:/Users/foo"), "file:///C:/Users/foo");
}

#[test]
fn normalize_line_endings_borrows_when_lf_only() {
    let s = "line1\nline2\n";
    let cow = normalize_line_endings(s);
    assert!(matches!(cow, std::borrow::Cow::Borrowed(_)));
    assert_eq!(cow, "line1\nline2\n");
}

#[test]
fn normalize_line_endings_owns_when_crlf() {
    let cow = normalize_line_endings("line1\r\nline2\r\n");
    assert!(matches!(cow, std::borrow::Cow::Owned(_)));
    assert_eq!(cow, "line1\nline2\n");
}

#[test]
fn normalize_cfgd_version_folds_current_version_only() {
    let line = format!("Client        {CURRENT_CFGD_VERSION}\n");
    assert_eq!(normalize_cfgd_version(&line), "Client        <VERSION>\n");
}

#[test]
fn normalize_cfgd_version_leaves_other_versions_intact() {
    // A genuinely wrong version must NOT fold to `<VERSION>`, so a real
    // version regression still trips the snapshot diff. `9999.9999.9999` can
    // never equal the running crate version nor contain it as a substring.
    let line = "✓ cfgd 9999.9999.9999 is up to date\n";
    assert!(matches!(
        normalize_cfgd_version(line),
        std::borrow::Cow::Borrowed(_)
    ));
    assert!(!normalize_cfgd_version(line).contains("<VERSION>"));
}

#[test]
fn normalize_for_snapshot_handles_crlf_backslash_and_nested_paths() {
    let captured = "  From file:///C:\\Users\\foo\\bare\\nested\\repo\r\n  hi\r\n";
    let bare = std::path::PathBuf::from("C:\\Users\\foo\\bare");
    let nested = std::path::PathBuf::from("C:\\Users\\foo\\bare\\nested");
    let out = normalize_for_snapshot(captured, &[(&bare, "<BARE>"), (&nested, "<NESTED>")]);
    // Longest path wins — nested substitutes first, so the bare/nested/repo
    // segment becomes <NESTED>/repo, NOT <BARE>/nested/repo.
    assert_eq!(out, "  From file:///<NESTED>/repo\n  hi\n");
}

#[test]
fn normalize_for_snapshot_skips_empty_path_keys() {
    let captured = "no tempdir touched here\n";
    let empty = std::path::PathBuf::new();
    // Empty path keys silently no-op rather than substituting at every char.
    let out = normalize_for_snapshot(captured, &[(&empty, "<NEVER>")]);
    assert_eq!(out, "no tempdir touched here\n");
}

#[test]
fn posixify_os_error_text_collapses_linux_form() {
    let s = "file error: io error on /tmp/foo: File exists (os error 17)";
    assert_eq!(
        posixify_os_error_text(s),
        "file error: io error on /tmp/foo: <os error>"
    );
}

#[test]
fn posixify_os_error_text_collapses_windows_form() {
    let s = "file error: io error on /tmp/foo: Cannot create a file when that file already exists. (os error 183)";
    assert_eq!(
        posixify_os_error_text(s),
        "file error: io error on /tmp/foo: <os error>"
    );
}

#[test]
fn posixify_os_error_text_handles_multiple_occurrences() {
    let s = "first: foo (os error 1) and second: bar (os error 2)";
    assert_eq!(
        posixify_os_error_text(s),
        "first: <os error> and second: <os error>"
    );
}

#[test]
fn posixify_os_error_text_borrows_when_no_marker() {
    let s = "no marker here";
    let cow = posixify_os_error_text(s);
    assert!(matches!(cow, std::borrow::Cow::Borrowed(_)));
}

#[test]
fn posixify_os_error_text_ignores_malformed_marker() {
    // "(os error )" with no digits — should pass through untouched.
    let s = "weird (os error ) thing";
    assert_eq!(posixify_os_error_text(s), s);
}

#[test]
fn posixify_os_error_text_collapses_libgit2_linux_form() {
    let s = "failed to resolve path '<BARE>': No such file or directory; class=Os (2)";
    assert_eq!(
        posixify_os_error_text(s),
        "failed to resolve path '<BARE>': <os error>; class=Os (2)"
    );
}

#[test]
fn posixify_os_error_text_collapses_libgit2_windows_form() {
    let s = "failed to resolve path '<BARE>': The system cannot find the file specified. — ; class=Os (2)";
    assert_eq!(
        posixify_os_error_text(s),
        "failed to resolve path '<BARE>': <os error>; class=Os (2)"
    );
}

#[test]
fn posixify_os_error_text_handles_mixed_libgit2_and_std_markers() {
    let s = "first: foo; class=Os (2) — also: bar (os error 17)";
    assert_eq!(
        posixify_os_error_text(s),
        "first: <os error>; class=Os (2) — also: <os error>"
    );
}

#[test]
fn from_user_input_folds_backslashes() {
    assert_eq!(
        from_user_input("C:\\Users\\foo"),
        std::path::PathBuf::from("C:/Users/foo")
    );
}

#[test]
fn from_user_input_passthrough_for_posix_input() {
    assert_eq!(
        from_user_input("/etc/cfgd.yaml"),
        std::path::PathBuf::from("/etc/cfgd.yaml")
    );
}

#[test]
fn path_display_ext_display_posix_unix_passthrough() {
    let p = std::path::Path::new("/home/foo/bar");
    // On Unix this is identical to .display().to_string(). On Windows the
    // forward slashes are already POSIX-form, so the result is the same.
    assert_eq!(p.display_posix(), "/home/foo/bar");
}

#[test]
fn path_display_ext_posix_composes_in_format() {
    let p = std::path::Path::new("/home/foo");
    let formatted = format!("Path is {}", p.posix());
    assert_eq!(formatted, "Path is /home/foo");
}

#[cfg(windows)]
#[test]
fn path_display_ext_folds_backslashes_on_windows() {
    let p = std::path::Path::new(r"C:\Users\foo");
    assert_eq!(p.display_posix(), "C:/Users/foo");
    assert_eq!(format!("{}", p.posix()), "C:/Users/foo");
}
