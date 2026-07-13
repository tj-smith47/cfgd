use super::*;
use crate::test_helpers::EnvVarGuard;
use std::path::{Path, PathBuf};

// --- spawn_blocking_with_test_home: override round-trips onto the worker ---

#[tokio::test(flavor = "current_thread")]
async fn spawn_blocking_with_test_home_round_trips_override_into_worker() {
    let dir = tempfile::TempDir::new().unwrap();
    let expected = dir.path().to_path_buf();
    let _home = with_test_home_guard(dir.path());
    let seen = spawn_blocking_with_test_home(test_home_override)
        .await
        .unwrap();
    assert_eq!(seen, Some(expected));
    // The worker's guard is scoped to the closure; the caller's override
    // is untouched.
    assert_eq!(test_home_override().as_deref(), Some(dir.path()));
}

#[tokio::test(flavor = "current_thread")]
async fn spawn_blocking_with_test_home_is_transparent_without_override() {
    assert_eq!(test_home_override(), None);
    let seen = spawn_blocking_with_test_home(test_home_override)
        .await
        .unwrap();
    assert_eq!(seen, None);
}

// --- resolve_* override precedence (flag/env path wins) ---

#[test]
fn resolve_config_dir_returns_override_when_some() {
    let over = PathBuf::from("/explicit/config");
    assert_eq!(resolve_config_dir(Some(&over), Scope::User), over);
}

#[test]
#[serial_test::serial]
fn resolve_config_dir_falls_through_to_default_when_none() {
    let dir = tempfile::tempdir().unwrap();
    let _sd = EnvVarGuard::unset("CONFIGURATION_DIRECTORY");
    let _home = with_test_home_guard(dir.path());
    assert_eq!(resolve_config_dir(None, Scope::User), default_config_dir());
}

#[test]
fn resolve_state_dir_returns_override_when_some() {
    let over = PathBuf::from("/explicit/state");
    assert_eq!(resolve_state_dir(Some(&over), Scope::User).unwrap(), over);
}

#[test]
#[serial_test::serial]
fn resolve_state_dir_falls_through_to_default_when_none() {
    let _cfgd = EnvVarGuard::set("CFGD_STATE_DIR", "/from/env/state");
    assert_eq!(
        resolve_state_dir(None, Scope::User).unwrap(),
        crate::state::default_state_dir().unwrap()
    );
}

#[test]
fn resolve_cache_dir_returns_override_when_some() {
    let over = PathBuf::from("/explicit/cache");
    assert_eq!(resolve_cache_dir(Some(&over), Scope::User).unwrap(), over);
}

#[test]
#[serial_test::serial]
fn resolve_cache_dir_falls_through_to_default_when_none() {
    let dir = tempfile::tempdir().unwrap();
    let _sd = EnvVarGuard::unset("CACHE_DIRECTORY");
    let _home = with_test_home_guard(dir.path());
    assert_eq!(
        resolve_cache_dir(None, Scope::User).unwrap(),
        default_cache_dir().unwrap()
    );
}

#[test]
fn resolve_runtime_dir_returns_override_when_some() {
    let over = PathBuf::from("/explicit/runtime");
    assert_eq!(resolve_runtime_dir(Some(&over), Scope::User), Some(over));
}

#[test]
#[serial_test::serial]
fn resolve_runtime_dir_falls_through_to_default_when_none() {
    let dir = tempfile::tempdir().unwrap();
    let _sd = EnvVarGuard::unset("RUNTIME_DIRECTORY");
    let _home = with_test_home_guard(dir.path());
    let _xdg = EnvVarGuard::set("XDG_RUNTIME_DIR", "/run/user/test");
    assert_eq!(
        resolve_runtime_dir(None, Scope::User),
        default_runtime_dir()
    );
}

// --- default_state_dir: CFGD_STATE_DIR short-circuit + XDG_STATE_HOME ---

#[test]
#[serial_test::serial]
fn default_state_dir_honors_cfgd_state_dir_env() {
    let _cfgd = EnvVarGuard::set("CFGD_STATE_DIR", "/verbatim/state/dir");
    assert_eq!(
        crate::state::default_state_dir().unwrap(),
        PathBuf::from("/verbatim/state/dir")
    );
}

#[cfg(target_os = "linux")]
#[test]
#[serial_test::serial]
fn default_state_dir_honors_xdg_state_home_on_linux() {
    let dir = tempfile::tempdir().unwrap();
    let _cfgd = EnvVarGuard::unset("CFGD_STATE_DIR");
    let _sd = EnvVarGuard::unset("STATE_DIRECTORY");
    let _xdg = EnvVarGuard::set("XDG_STATE_HOME", dir.path().to_str().unwrap());
    let _home = EnvVarGuard::set("HOME", dir.path().to_str().unwrap());
    let state = crate::state::default_state_dir().unwrap();
    assert!(
        state.ends_with("cfgd"),
        "tail must be cfgd, got: {}",
        state.display()
    );
    assert!(
        state.starts_with(dir.path()),
        "must be under XDG_STATE_HOME, got: {}",
        state.display()
    );
}

// --- default_cache_dir: single root, tail = cfgd ---

#[test]
#[serial_test::serial]
fn default_cache_dir_tail_is_cfgd() {
    let dir = tempfile::tempdir().unwrap();
    let _cfgd = EnvVarGuard::unset("CFGD_CACHE_DIR");
    let _sd = EnvVarGuard::unset("CACHE_DIRECTORY");
    let _home = with_test_home_guard(dir.path());
    let cache = default_cache_dir().unwrap();
    assert!(
        cache.ends_with("cfgd"),
        "tail must be cfgd, got: {}",
        cache.display()
    );
    assert!(
        cache.starts_with(dir.path()),
        "must be under test home, got: {}",
        cache.display()
    );
}

#[test]
#[serial_test::serial]
fn default_cache_dir_honors_cfgd_cache_dir_env() {
    let _cfgd = EnvVarGuard::set("CFGD_CACHE_DIR", "/verbatim/cache/dir");
    assert_eq!(
        default_cache_dir().unwrap(),
        PathBuf::from("/verbatim/cache/dir")
    );
}

// --- ResolvedDirs: unified cache root + sub-paths ---

#[test]
#[serial_test::serial]
fn resolved_dirs_sources_and_modules_share_one_cache_root() {
    let dir = tempfile::tempdir().unwrap();
    let _home = with_test_home_guard(dir.path());
    let _cfgd = EnvVarGuard::unset("CFGD_STATE_DIR");
    let _cache = EnvVarGuard::unset("CFGD_CACHE_DIR");
    let _config_sd = EnvVarGuard::unset("CONFIGURATION_DIRECTORY");
    let _state_sd = EnvVarGuard::unset("STATE_DIRECTORY");
    let _cache_sd = EnvVarGuard::unset("CACHE_DIRECTORY");
    let _runtime_sd = EnvVarGuard::unset("RUNTIME_DIRECTORY");
    let dirs = ResolvedDirs::resolve(None, None, None, None, Scope::User).unwrap();
    assert!(dirs.sources_dir().ends_with("sources"));
    assert!(dirs.module_cache_dir().ends_with("modules"));
    assert_eq!(dirs.sources_dir().parent(), Some(dirs.cache.as_path()));
    assert_eq!(dirs.module_cache_dir().parent(), Some(dirs.cache.as_path()));
    assert!(dirs.cache.ends_with("cfgd"));
}

#[test]
fn resolved_dirs_resolve_threads_overrides() {
    let cfg = Path::new("/o/config");
    let state = Path::new("/o/state");
    let cache = Path::new("/o/cache");
    let runtime = Path::new("/o/runtime");
    let dirs = ResolvedDirs::resolve(
        Some(cfg),
        Some(state),
        Some(cache),
        Some(runtime),
        Scope::User,
    )
    .unwrap();
    assert_eq!(dirs.config, cfg);
    assert_eq!(dirs.state, state);
    assert_eq!(dirs.cache, cache);
    assert_eq!(dirs.runtime.as_deref(), Some(runtime));
    assert_eq!(dirs.sources_dir(), cache.join("sources"));
    assert_eq!(dirs.module_cache_dir(), cache.join("modules"));
}

// --- default_runtime_dir: CFGD_RUNTIME_DIR short-circuit ---

#[test]
#[serial_test::serial]
fn default_runtime_dir_honors_cfgd_runtime_dir_env() {
    let _cfgd = EnvVarGuard::set("CFGD_RUNTIME_DIR", "/verbatim/runtime/dir");
    assert_eq!(
        default_runtime_dir(),
        Some(PathBuf::from("/verbatim/runtime/dir"))
    );
}

// --- default_runtime_dir: XDG_RUNTIME_DIR vs cache/runtime fallback ---

#[cfg(target_os = "linux")]
#[test]
#[serial_test::serial]
fn default_runtime_dir_uses_xdg_runtime_dir_when_set() {
    let _cfgd = EnvVarGuard::unset("CFGD_RUNTIME_DIR");
    let _sd = EnvVarGuard::unset("RUNTIME_DIRECTORY");
    let _xdg = EnvVarGuard::set("XDG_RUNTIME_DIR", "/run/user/4242");
    let runtime = default_runtime_dir().expect("runtime dir resolves with XDG set");
    assert_eq!(runtime, PathBuf::from("/run/user/4242").join("cfgd"));
}

#[cfg(target_os = "linux")]
#[test]
#[serial_test::serial]
fn default_runtime_dir_falls_back_to_cache_runtime_subdir_on_linux() {
    let dir = tempfile::tempdir().unwrap();
    let _cfgd = EnvVarGuard::unset("CFGD_RUNTIME_DIR");
    let _sd = EnvVarGuard::unset("RUNTIME_DIRECTORY");
    let _xdg = EnvVarGuard::unset("XDG_RUNTIME_DIR");
    let _home = with_test_home_guard(dir.path());
    let runtime = default_runtime_dir().expect("runtime dir resolves without XDG");
    assert!(
        runtime.ends_with("cfgd/runtime"),
        "fallback must nest under cfgd/runtime, got: {}",
        runtime.display()
    );
}

// --- move_file ---

#[test]
fn move_file_relocates_and_removes_source() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a.txt");
    let dst = dir.path().join("sub").join("b.txt");
    std::fs::write(&src, b"payload").unwrap();
    move_file(&src, &dst).unwrap();
    assert!(!src.exists(), "source must be gone after move");
    assert_eq!(std::fs::read(&dst).unwrap(), b"payload");
}

#[test]
fn move_file_refuses_when_destination_exists() {
    let dir = tempfile::tempdir().unwrap();
    let src = dir.path().join("a.txt");
    let dst = dir.path().join("b.txt");
    std::fs::write(&src, b"new").unwrap();
    std::fs::write(&dst, b"old").unwrap();
    let err = move_file(&src, &dst).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::AlreadyExists);
    // Neither file disturbed.
    assert_eq!(std::fs::read(&src).unwrap(), b"new");
    assert_eq!(std::fs::read(&dst).unwrap(), b"old");
}

// --- legacy_data_dir ---

#[test]
#[serial_test::serial]
fn legacy_data_dir_resolves_under_test_home() {
    let dir = tempfile::tempdir().unwrap();
    let _home = with_test_home_guard(dir.path());
    let legacy = legacy_data_dir().expect("legacy data dir resolves under test home");
    assert!(
        legacy.ends_with(".local/share/cfgd"),
        "legacy data dir must end with .local/share/cfgd, got: {}",
        legacy.display()
    );
}

// --- Scope mapping ---

#[test]
fn scope_from_system_flag_maps_both_directions() {
    assert_eq!(Scope::from_system_flag(true), Scope::System);
    assert_eq!(Scope::from_system_flag(false), Scope::User);
    assert!(Scope::System.is_system());
    assert!(!Scope::User.is_system());
}

// --- systemd_dir helper ---

#[test]
#[serial_test::serial]
fn systemd_dir_takes_first_colon_entry() {
    let _v = EnvVarGuard::set("STATE_DIRECTORY", "/var/lib/cfgd:/extra/two");
    assert_eq!(
        systemd_dir("STATE_DIRECTORY"),
        Some(PathBuf::from("/var/lib/cfgd"))
    );
}

#[test]
#[serial_test::serial]
fn systemd_dir_skips_leading_empty_entry() {
    let _v = EnvVarGuard::set("STATE_DIRECTORY", ":/second/entry");
    assert_eq!(
        systemd_dir("STATE_DIRECTORY"),
        Some(PathBuf::from("/second/entry"))
    );
}

#[test]
#[serial_test::serial]
fn systemd_dir_none_when_unset_or_empty() {
    let _unset = EnvVarGuard::unset("STATE_DIRECTORY");
    assert_eq!(systemd_dir("STATE_DIRECTORY"), None);
    let _empty = EnvVarGuard::set("STATE_DIRECTORY", "");
    assert_eq!(systemd_dir("STATE_DIRECTORY"), None);
}

// --- CONFIG: precedence matrix ---

#[test]
#[serial_test::serial]
fn config_systemd_dir_wins_over_system_scope() {
    let _sd = EnvVarGuard::set("CONFIGURATION_DIRECTORY", "/run/systemd/cfgd-config");
    assert_eq!(
        default_config_dir_for(Scope::System),
        PathBuf::from("/run/systemd/cfgd-config")
    );
}

#[test]
#[serial_test::serial]
#[cfg(target_os = "linux")]
fn config_system_scope_is_etc_on_linux() {
    let _sd = EnvVarGuard::unset("CONFIGURATION_DIRECTORY");
    assert_eq!(
        default_config_dir_for(Scope::System),
        PathBuf::from("/etc/cfgd")
    );
}

#[test]
#[serial_test::serial]
#[cfg(target_os = "macos")]
fn config_system_scope_is_library_on_macos() {
    let _sd = EnvVarGuard::unset("CONFIGURATION_DIRECTORY");
    assert_eq!(
        default_config_dir_for(Scope::System),
        PathBuf::from("/Library/Application Support/cfgd")
    );
}

#[test]
#[serial_test::serial]
#[cfg(windows)]
fn config_system_scope_under_program_data_on_windows() {
    let _sd = EnvVarGuard::unset("CONFIGURATION_DIRECTORY");
    let resolved = default_config_dir_for(Scope::System);
    assert!(
        resolved.ends_with("cfgd"),
        "tail must be cfgd, got: {}",
        resolved.display()
    );
    assert_eq!(resolved, program_data_dir().join("cfgd"));
}

#[test]
#[serial_test::serial]
fn config_user_scope_matches_zero_arg_default() {
    let dir = tempfile::tempdir().unwrap();
    let _sd = EnvVarGuard::unset("CONFIGURATION_DIRECTORY");
    let _home = with_test_home_guard(dir.path());
    assert_eq!(default_config_dir_for(Scope::User), default_config_dir());
    assert!(default_config_dir_for(Scope::User).starts_with(dir.path()));
}

// --- STATE: precedence matrix ---

#[test]
#[serial_test::serial]
fn state_cfgd_env_wins_over_systemd_and_system_scope() {
    let _cfgd = EnvVarGuard::set("CFGD_STATE_DIR", "/from/cfgd/env");
    let _sd = EnvVarGuard::set("STATE_DIRECTORY", "/from/systemd");
    assert_eq!(
        crate::state::default_state_dir_for(Scope::System).unwrap(),
        PathBuf::from("/from/cfgd/env")
    );
}

#[test]
#[serial_test::serial]
fn state_systemd_dir_wins_over_system_scope() {
    let _cfgd = EnvVarGuard::unset("CFGD_STATE_DIR");
    let _sd = EnvVarGuard::set("STATE_DIRECTORY", "/from/systemd/state");
    assert_eq!(
        crate::state::default_state_dir_for(Scope::System).unwrap(),
        PathBuf::from("/from/systemd/state")
    );
}

#[test]
#[serial_test::serial]
#[cfg(target_os = "linux")]
fn state_system_scope_is_var_lib_on_linux() {
    let _cfgd = EnvVarGuard::unset("CFGD_STATE_DIR");
    let _sd = EnvVarGuard::unset("STATE_DIRECTORY");
    assert_eq!(
        crate::state::default_state_dir_for(Scope::System).unwrap(),
        PathBuf::from("/var/lib/cfgd")
    );
}

#[test]
#[serial_test::serial]
#[cfg(target_os = "macos")]
fn state_system_scope_is_library_on_macos() {
    let _cfgd = EnvVarGuard::unset("CFGD_STATE_DIR");
    let _sd = EnvVarGuard::unset("STATE_DIRECTORY");
    assert_eq!(
        crate::state::default_state_dir_for(Scope::System).unwrap(),
        PathBuf::from("/Library/Application Support/cfgd/state")
    );
}

// --- CACHE: precedence matrix ---

#[test]
#[serial_test::serial]
fn cache_cfgd_env_wins_over_systemd_and_system_scope() {
    let _cfgd = EnvVarGuard::set("CFGD_CACHE_DIR", "/from/cfgd/cache");
    let _sd = EnvVarGuard::set("CACHE_DIRECTORY", "/from/systemd/cache");
    assert_eq!(
        default_cache_dir_for(Scope::System).unwrap(),
        PathBuf::from("/from/cfgd/cache")
    );
}

#[test]
#[serial_test::serial]
fn cache_systemd_dir_wins_over_system_scope() {
    let _cfgd = EnvVarGuard::unset("CFGD_CACHE_DIR");
    let _sd = EnvVarGuard::set("CACHE_DIRECTORY", "/from/systemd/cache");
    assert_eq!(
        default_cache_dir_for(Scope::System).unwrap(),
        PathBuf::from("/from/systemd/cache")
    );
}

#[test]
#[serial_test::serial]
#[cfg(target_os = "linux")]
fn cache_system_scope_is_var_cache_on_linux() {
    let _cfgd = EnvVarGuard::unset("CFGD_CACHE_DIR");
    let _sd = EnvVarGuard::unset("CACHE_DIRECTORY");
    assert_eq!(
        default_cache_dir_for(Scope::System).unwrap(),
        PathBuf::from("/var/cache/cfgd")
    );
}

#[test]
#[serial_test::serial]
#[cfg(target_os = "macos")]
fn cache_system_scope_is_library_caches_on_macos() {
    let _cfgd = EnvVarGuard::unset("CFGD_CACHE_DIR");
    let _sd = EnvVarGuard::unset("CACHE_DIRECTORY");
    assert_eq!(
        default_cache_dir_for(Scope::System).unwrap(),
        PathBuf::from("/Library/Caches/cfgd")
    );
}

// --- RUNTIME: precedence matrix ---

#[test]
#[serial_test::serial]
fn runtime_cfgd_env_wins_over_systemd_and_system_scope() {
    let _cfgd = EnvVarGuard::set("CFGD_RUNTIME_DIR", "/from/cfgd/runtime");
    let _sd = EnvVarGuard::set("RUNTIME_DIRECTORY", "/from/systemd/runtime");
    assert_eq!(
        default_runtime_dir_for(Scope::System),
        Some(PathBuf::from("/from/cfgd/runtime"))
    );
}

#[test]
#[serial_test::serial]
fn runtime_systemd_dir_wins_over_system_scope() {
    let _cfgd = EnvVarGuard::unset("CFGD_RUNTIME_DIR");
    let _sd = EnvVarGuard::set("RUNTIME_DIRECTORY", "/from/systemd/runtime");
    assert_eq!(
        default_runtime_dir_for(Scope::System),
        Some(PathBuf::from("/from/systemd/runtime"))
    );
}

#[test]
#[serial_test::serial]
#[cfg(target_os = "linux")]
fn runtime_system_scope_is_run_cfgd_on_linux() {
    let _cfgd = EnvVarGuard::unset("CFGD_RUNTIME_DIR");
    let _sd = EnvVarGuard::unset("RUNTIME_DIRECTORY");
    assert_eq!(
        default_runtime_dir_for(Scope::System),
        Some(PathBuf::from("/run/cfgd"))
    );
}

#[test]
#[serial_test::serial]
#[cfg(target_os = "macos")]
fn runtime_system_scope_is_library_on_macos() {
    let _cfgd = EnvVarGuard::unset("CFGD_RUNTIME_DIR");
    let _sd = EnvVarGuard::unset("RUNTIME_DIRECTORY");
    assert_eq!(
        default_runtime_dir_for(Scope::System),
        Some(PathBuf::from("/Library/Application Support/cfgd/runtime"))
    );
}

// --- Scope-aware resolvers honor the override before scope ---

#[test]
fn resolvers_override_wins_over_system_scope() {
    let over = PathBuf::from("/explicit/override");
    assert_eq!(resolve_config_dir(Some(&over), Scope::System), over);
    assert_eq!(resolve_state_dir(Some(&over), Scope::System).unwrap(), over);
    assert_eq!(resolve_cache_dir(Some(&over), Scope::System).unwrap(), over);
    assert_eq!(resolve_runtime_dir(Some(&over), Scope::System), Some(over));
}

#[test]
#[serial_test::serial]
#[cfg(target_os = "linux")]
fn resolved_dirs_system_scope_resolves_fhs_roots() {
    let _config_sd = EnvVarGuard::unset("CONFIGURATION_DIRECTORY");
    let _state_sd = EnvVarGuard::unset("STATE_DIRECTORY");
    let _cache_sd = EnvVarGuard::unset("CACHE_DIRECTORY");
    let _runtime_sd = EnvVarGuard::unset("RUNTIME_DIRECTORY");
    let _cfgd_state = EnvVarGuard::unset("CFGD_STATE_DIR");
    let _cfgd_cache = EnvVarGuard::unset("CFGD_CACHE_DIR");
    let _cfgd_runtime = EnvVarGuard::unset("CFGD_RUNTIME_DIR");
    let dirs = ResolvedDirs::resolve(None, None, None, None, Scope::System).unwrap();
    assert_eq!(dirs.config, PathBuf::from("/etc/cfgd"));
    assert_eq!(dirs.state, PathBuf::from("/var/lib/cfgd"));
    assert_eq!(dirs.cache, PathBuf::from("/var/cache/cfgd"));
    assert_eq!(dirs.runtime, Some(PathBuf::from("/run/cfgd")));
}
