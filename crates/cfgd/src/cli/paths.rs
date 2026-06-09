use serde::Serialize;

use cfgd_core::PathDisplayExt;
use cfgd_core::output::{Doc, Printer};

use super::Cli;

/// Where an effective directory value came from: a `--*-dir` flag, a
/// `CFGD_*_DIR` env var, or the built-in platform default.
///
/// clap distinguishes these three only through [`clap::parser::ValueSource`],
/// which is reachable from `ArgMatches` in `main.rs` but not from the parsed
/// [`Cli`]. The mapping is performed once up-front (see [`DirSources`]) and
/// threaded into [`cmd_paths`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirSource {
    Flag,
    Env,
    Default,
}

impl DirSource {
    fn label(self) -> &'static str {
        match self {
            DirSource::Flag => "flag",
            DirSource::Env => "env",
            DirSource::Default => "default",
        }
    }
}

// Serialize as the lowercase scripting token (`flag`/`env`/`default`) by hand;
// the derive-attribute spelling for this is banned by the naming-convention audit.
impl Serialize for DirSource {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(self.label())
    }
}

/// Effective source of each of the four resolved directory roots.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirSources {
    pub config: DirSource,
    pub state: DirSource,
    pub cache: DirSource,
    pub runtime: DirSource,
}

impl DirSources {
    /// All roots resolved from their platform default — the correct value when
    /// no `ArgMatches` is available (callers and tests that bypass `main.rs`).
    pub fn all_default() -> Self {
        Self {
            config: DirSource::Default,
            state: DirSource::Default,
            cache: DirSource::Default,
            runtime: DirSource::Default,
        }
    }

    /// Whether the legacy combined-data-dir migration may run: only when BOTH the
    /// state and cache roots are at their platform default. An explicit
    /// `--state-dir`/`--cache-dir` (or `CFGD_STATE_DIR`/`CFGD_CACHE_DIR`) means the
    /// user is driving — possibly at a throwaway location — and real user data must
    /// never be dragged into an overridden/ephemeral root. The config and runtime
    /// roots are irrelevant: the migration only touches state and sources.
    pub fn legacy_migration_eligible(&self) -> bool {
        self.state == DirSource::Default && self.cache == DirSource::Default
    }
}

/// Map a clap [`ValueSource`](clap::parser::ValueSource) for one directory arg
/// to a [`DirSource`]. A command-line value is `Flag`, an env value is `Env`,
/// and anything else (a default, or an unset optional arg) is `Default`.
pub fn dir_source_from_value_source(src: Option<clap::parser::ValueSource>) -> DirSource {
    match src {
        Some(clap::parser::ValueSource::CommandLine) => DirSource::Flag,
        Some(clap::parser::ValueSource::EnvVariable) => DirSource::Env,
        _ => DirSource::Default,
    }
}

/// Collapse the config root's source across BOTH `--config` (file-or-dir, which
/// wins) and `--config-dir`: the user overrode the config root if EITHER arg was
/// supplied. Flag beats Env beats Default.
pub fn config_dir_source(
    config: Option<clap::parser::ValueSource>,
    config_dir: Option<clap::parser::ValueSource>,
) -> DirSource {
    match (
        dir_source_from_value_source(config),
        dir_source_from_value_source(config_dir),
    ) {
        (DirSource::Flag, _) | (_, DirSource::Flag) => DirSource::Flag,
        (DirSource::Env, _) | (_, DirSource::Env) => DirSource::Env,
        _ => DirSource::Default,
    }
}

/// Structured payload for `cfgd paths -o json|yaml`. Each root reports its
/// resolved directory, the effective source of that value, and the key files
/// cfgd owns inside it. `runtime.dir` is `null` when no home directory is
/// resolvable; `runtime.socket` is always present (it falls back to a
/// platform-specific path even with no home).
#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct PathsOutput {
    pub config: ConfigPaths,
    pub state: StatePaths,
    pub cache: CachePaths,
    pub runtime: RuntimePaths,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConfigPaths {
    pub dir: String,
    pub source: DirSource,
    /// Resolved config file inside the config dir (e.g. `cfgd.yaml`).
    pub file: String,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StatePaths {
    /// `null` when no home directory is resolvable (and no override is set).
    pub dir: Option<String>,
    pub source: DirSource,
    /// SQLite state database (`-wal`/`-shm` siblings live alongside it).
    pub db: Option<String>,
    /// Apply mutex serializing live-state mutations.
    pub apply_lock: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CachePaths {
    /// `null` when no home directory is resolvable (and no override is set).
    pub dir: Option<String>,
    pub source: DirSource,
    /// Per-source clones: `<cache>/sources/<name>/`.
    pub sources: Option<String>,
    /// Per-module artifacts: `<cache>/modules/<hash>/`.
    pub modules: Option<String>,
}

#[derive(Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct RuntimePaths {
    /// `null` when no home directory is resolvable.
    pub dir: Option<String>,
    pub source: DirSource,
    /// Effective daemon IPC socket, resolved by the daemon's own
    /// [`cfgd_core::resolve_default_ipc_path`] so it always matches what the
    /// daemon binds: a `CFGD_DAEMON_IPC_PATH` override, else `<runtime>/cfgd.sock`,
    /// else (Unix, no home) the `/tmp/cfgd.sock` last-ditch fallback, else
    /// (Windows) the `\\.\pipe\cfgd` named pipe.
    pub socket: String,
}

/// Resolve the four directory roots and the key files within each into the
/// stable structured payload.
///
/// A discoverability command degrades gracefully: each root resolves
/// independently, so an unresolvable state/cache root (no `$HOME` and no
/// override) reports `null` for that root rather than failing the whole command,
/// while the home-independent socket fallback (`/tmp/cfgd.sock` / named pipe) is
/// still reported.
fn collect_paths_output(cli: &Cli, sources: &DirSources) -> anyhow::Result<PathsOutput> {
    // `cli.config` is the already-resolved config FILE (main.rs folds --config /
    // --config-dir / resolve_config_path / macOS migration into it); the config
    // dir is the directory that actually contains it, never a re-resolved root.
    let config_dir = cli
        .config
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_else(|| cli.config.clone());

    let config = ConfigPaths {
        dir: config_dir.posix().to_string(),
        source: sources.config,
        file: cli.config.posix().to_string(),
    };

    let state_dir = cfgd_core::resolve_state_dir(cli.state_dir.as_deref()).ok();
    let state = StatePaths {
        dir: state_dir.as_ref().map(|d| d.posix().to_string()),
        source: sources.state,
        db: state_dir.as_ref().map(|d| {
            d.join(cfgd_core::state::STATE_DB_FILENAME)
                .posix()
                .to_string()
        }),
        apply_lock: state_dir
            .as_ref()
            .map(|d| d.join(cfgd_core::APPLY_LOCK_FILENAME).posix().to_string()),
    };

    // Build a `ResolvedDirs` solely to spell the cache sub-dirs through the
    // canonical `.sources_dir()` / `.module_cache_dir()` accessors (DRY) rather
    // than open-coding `.join("sources")` / `.join("modules")`. Only the `cache`
    // field is read by those accessors, so the others take empty placeholders to
    // avoid cloning paths that go unused.
    let cache = match cfgd_core::resolve_cache_dir(cli.cache_dir.as_deref()) {
        Ok(cache_dir) => {
            let dirs = cfgd_core::ResolvedDirs {
                config: std::path::PathBuf::new(),
                state: std::path::PathBuf::new(),
                cache: cache_dir.clone(),
                runtime: None,
            };
            CachePaths {
                dir: Some(cache_dir.posix().to_string()),
                source: sources.cache,
                sources: Some(dirs.sources_dir().posix().to_string()),
                modules: Some(dirs.module_cache_dir().posix().to_string()),
            }
        }
        Err(_) => CachePaths {
            dir: None,
            source: sources.cache,
            sources: None,
            modules: None,
        },
    };

    // Resolve the socket through the daemon's own single source of truth so the
    // reported path is exactly what `cfgd daemon` binds and `cfgd daemon status`
    // connects to (honors CFGD_DAEMON_IPC_PATH, --runtime-dir, and the /tmp and
    // named-pipe fallbacks).
    let socket = cfgd_core::resolve_default_ipc_path(cli.runtime_dir.as_deref())
        .posix()
        .to_string();

    let runtime = RuntimePaths {
        dir: cfgd_core::resolve_runtime_dir(cli.runtime_dir.as_deref())
            .as_ref()
            .map(|d| d.posix().to_string()),
        source: sources.runtime,
        socket,
    };

    Ok(PathsOutput {
        config,
        state,
        cache,
        runtime,
    })
}

/// Render an `Option<String>` path for the human Doc: the path, or a clear
/// unavailable marker when the root could not be resolved.
fn or_unavailable(value: &Option<String>) -> String {
    value
        .clone()
        .unwrap_or_else(|| "(no home — unavailable)".to_string())
}

/// Build the `paths` human + structured `Doc` from a collected payload.
pub fn build_paths_doc(output: &PathsOutput) -> Doc {
    let mut doc = Doc::new().heading("cfgd directories");

    let config = &output.config;
    doc = doc.section("Config", |s| {
        s.kv_block([
            ("dir", config.dir.clone()),
            ("source", config.source.label().to_string()),
            ("file", config.file.clone()),
        ])
    });

    let state = &output.state;
    doc = doc.section("State", |s| {
        s.kv_block([
            ("dir", or_unavailable(&state.dir)),
            ("source", state.source.label().to_string()),
            ("db", or_unavailable(&state.db)),
            ("applyLock", or_unavailable(&state.apply_lock)),
        ])
    });

    let cache = &output.cache;
    doc = doc.section("Cache", |s| {
        s.kv_block([
            ("dir", or_unavailable(&cache.dir)),
            ("source", cache.source.label().to_string()),
            ("sources", or_unavailable(&cache.sources)),
            ("modules", or_unavailable(&cache.modules)),
        ])
    });

    let runtime = &output.runtime;
    doc = doc.section("Runtime", |s| {
        s.kv_block([
            ("dir", or_unavailable(&runtime.dir)),
            ("source", runtime.source.label().to_string()),
            ("socket", runtime.socket.clone()),
        ])
    });

    doc.with_data(output)
}

pub fn cmd_paths(cli: &Cli, printer: &Printer, sources: &DirSources) -> anyhow::Result<()> {
    let output = collect_paths_output(cli, sources)?;
    printer.emit(build_paths_doc(&output));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use cfgd_core::output::{OutputFormat, Printer, Verbosity};
    use cfgd_core::test_helpers::EnvVarGuard;
    use clap::parser::ValueSource;
    use serial_test::serial;

    fn test_cli(state_dir: Option<PathBuf>, cache_dir: Option<PathBuf>) -> Cli {
        use clap::Parser;
        let mut cli = Cli::parse_from(["cfgd"]);
        cli.state_dir = state_dir;
        cli.cache_dir = cache_dir;
        cli
    }

    #[test]
    fn dir_source_maps_command_line_to_flag() {
        assert_eq!(
            dir_source_from_value_source(Some(ValueSource::CommandLine)),
            DirSource::Flag
        );
    }

    #[test]
    fn dir_source_maps_env_to_env() {
        assert_eq!(
            dir_source_from_value_source(Some(ValueSource::EnvVariable)),
            DirSource::Env
        );
    }

    #[test]
    fn dir_source_maps_default_and_none_to_default() {
        assert_eq!(
            dir_source_from_value_source(Some(ValueSource::DefaultValue)),
            DirSource::Default
        );
        assert_eq!(dir_source_from_value_source(None), DirSource::Default);
    }

    #[test]
    fn config_dir_source_folds_both_args() {
        use ValueSource::{CommandLine, DefaultValue, EnvVariable};
        // --config flag wins regardless of --config-dir.
        assert_eq!(config_dir_source(Some(CommandLine), None), DirSource::Flag);
        // --config-dir flag promotes even when --config is default.
        assert_eq!(
            config_dir_source(Some(DefaultValue), Some(CommandLine)),
            DirSource::Flag
        );
        // Env on either arg yields Env when no flag present.
        assert_eq!(config_dir_source(Some(EnvVariable), None), DirSource::Env);
        assert_eq!(config_dir_source(None, Some(EnvVariable)), DirSource::Env);
        // Flag beats Env.
        assert_eq!(
            config_dir_source(Some(EnvVariable), Some(CommandLine)),
            DirSource::Flag
        );
        // Neither overridden → Default.
        assert_eq!(
            config_dir_source(Some(DefaultValue), None),
            DirSource::Default
        );
    }

    #[test]
    fn dir_source_serializes_lowercase() {
        assert_eq!(
            serde_json::to_value(DirSource::Flag).unwrap(),
            serde_json::json!("flag")
        );
        assert_eq!(
            serde_json::to_value(DirSource::Env).unwrap(),
            serde_json::json!("env")
        );
        assert_eq!(
            serde_json::to_value(DirSource::Default).unwrap(),
            serde_json::json!("default")
        );
    }

    #[test]
    #[serial]
    fn output_has_all_four_roots_with_camelcase_keys() {
        let _ipc = EnvVarGuard::unset("CFGD_DAEMON_IPC_PATH");
        let state = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let cli = test_cli(
            Some(state.path().to_path_buf()),
            Some(cache.path().to_path_buf()),
        );
        let output =
            collect_paths_output(&cli, &DirSources::all_default()).expect("collect must succeed");
        let v = serde_json::to_value(&output).unwrap();
        let obj = v.as_object().expect("payload is an object");

        for root in ["config", "state", "cache", "runtime"] {
            assert!(obj.contains_key(root), "missing root {root}: {v}");
        }
        // camelCase nested keys.
        assert!(v["state"]["applyLock"].is_string(), "state.applyLock: {v}");
        assert!(v["config"]["file"].is_string(), "config.file: {v}");
        assert!(v["cache"]["sources"].is_string(), "cache.sources: {v}");
        assert!(v["cache"]["modules"].is_string(), "cache.modules: {v}");
    }

    #[test]
    #[serial]
    fn overrides_set_dir_and_source_to_flag() {
        let _ipc = EnvVarGuard::unset("CFGD_DAEMON_IPC_PATH");
        let state = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let cli = test_cli(
            Some(state.path().to_path_buf()),
            Some(cache.path().to_path_buf()),
        );
        let sources = DirSources {
            config: DirSource::Default,
            state: DirSource::Flag,
            cache: DirSource::Flag,
            runtime: DirSource::Default,
        };
        let output = collect_paths_output(&cli, &sources).expect("collect must succeed");

        assert_eq!(
            output.state.dir.as_deref(),
            Some(state.path().posix().to_string()).as_deref()
        );
        assert_eq!(
            output.cache.dir.as_deref(),
            Some(cache.path().posix().to_string()).as_deref()
        );
        assert_eq!(output.state.source, DirSource::Flag);
        assert_eq!(output.cache.source, DirSource::Flag);
        // Reported files nest under the overridden roots.
        assert_eq!(
            output.state.db.as_deref(),
            Some(state.path().join("state.db").posix().to_string()).as_deref()
        );
        assert_eq!(
            output.state.apply_lock.as_deref(),
            Some(state.path().join("apply.lock").posix().to_string()).as_deref()
        );
        assert_eq!(
            output.cache.sources.as_deref(),
            Some(cache.path().join("sources").posix().to_string()).as_deref()
        );
        assert_eq!(
            output.cache.modules.as_deref(),
            Some(cache.path().join("modules").posix().to_string()).as_deref()
        );
    }

    #[test]
    #[serial]
    fn ipc_override_replaces_socket_path() {
        let _ipc = EnvVarGuard::set("CFGD_DAEMON_IPC_PATH", "/custom/cfgd.sock");
        let cli = test_cli(None, None);
        let output =
            collect_paths_output(&cli, &DirSources::all_default()).expect("collect must succeed");
        assert_eq!(output.runtime.socket, "/custom/cfgd.sock");
    }

    // B1 regression: under `--config <dir>/cfgd.yaml` the reported config.dir
    // must be the directory that actually holds the file (its parent), and the
    // config source reflects the override (flag), not a re-resolved root.
    #[test]
    #[serial]
    fn config_dir_is_parent_of_resolved_file_and_source_flag() {
        let _ipc = EnvVarGuard::unset("CFGD_DAEMON_IPC_PATH");
        let cfg_dir = tempfile::tempdir().unwrap();
        let cfg_file = cfg_dir.path().join("cfgd.yaml");
        let mut cli = test_cli(None, None);
        cli.config = cfg_file.clone();
        let sources = DirSources {
            config: DirSource::Flag,
            ..DirSources::all_default()
        };
        let output = collect_paths_output(&cli, &sources).expect("collect must succeed");

        assert_eq!(output.config.file, cfg_file.posix().to_string());
        assert_eq!(output.config.dir, cfg_dir.path().posix().to_string());
        assert_eq!(
            output.config.dir,
            cfg_file.parent().unwrap().posix().to_string()
        );
        assert_eq!(output.config.source, DirSource::Flag);
    }

    // B2/S2 regression: with no home and no override, the socket must be the
    // daemon's `/tmp/cfgd.sock` last-ditch fallback — matching what the daemon
    // actually binds — never null or a phantom `<runtime>/cfgd.sock`.
    #[cfg(unix)]
    #[test]
    #[serial]
    fn socket_falls_back_to_tmp_when_no_home() {
        let _ipc = EnvVarGuard::unset("CFGD_DAEMON_IPC_PATH");
        let _xdg = EnvVarGuard::unset("XDG_RUNTIME_DIR");
        let _home = EnvVarGuard::unset("HOME");
        let cli = test_cli(None, None);
        let output =
            collect_paths_output(&cli, &DirSources::all_default()).expect("collect must succeed");
        assert_eq!(output.runtime.socket, "/tmp/cfgd.sock");
        assert!(output.runtime.dir.is_none(), "runtime.dir must be null");
    }

    // `--runtime-dir` must place the reported socket under that dir, agreeing
    // with the daemon bind which resolves via the same function.
    #[test]
    #[serial]
    fn runtime_dir_override_places_socket_under_it() {
        let _ipc = EnvVarGuard::unset("CFGD_DAEMON_IPC_PATH");
        let rt = tempfile::tempdir().unwrap();
        let mut cli = test_cli(None, None);
        cli.runtime_dir = Some(rt.path().to_path_buf());
        let sources = DirSources {
            runtime: DirSource::Flag,
            ..DirSources::all_default()
        };
        let output = collect_paths_output(&cli, &sources).expect("collect must succeed");

        assert_eq!(
            output.runtime.dir.as_deref(),
            Some(rt.path().posix().to_string()).as_deref()
        );
        assert_eq!(
            output.runtime.socket,
            rt.path().join("cfgd.sock").posix().to_string()
        );
        assert_eq!(output.runtime.source, DirSource::Flag);
    }

    #[test]
    #[serial]
    fn cmd_paths_renders_all_roots_human() {
        let _ipc = EnvVarGuard::unset("CFGD_DAEMON_IPC_PATH");
        let state = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let cli = test_cli(
            Some(state.path().to_path_buf()),
            Some(cache.path().to_path_buf()),
        );
        let (printer, buf) = Printer::for_test_at(Verbosity::Normal);
        cmd_paths(&cli, &printer, &DirSources::all_default()).expect("cmd_paths must succeed");
        let out = buf.lock().unwrap().clone();
        assert!(out.contains("cfgd directories"), "heading missing: {out}");
        for label in ["Config", "State", "Cache", "Runtime"] {
            assert!(out.contains(label), "section {label} missing: {out}");
        }
        assert!(out.contains("applyLock"), "applyLock kv missing: {out}");
    }

    #[test]
    #[serial]
    fn cmd_paths_json_shape() {
        let _ipc = EnvVarGuard::unset("CFGD_DAEMON_IPC_PATH");
        let state = tempfile::tempdir().unwrap();
        let cache = tempfile::tempdir().unwrap();
        let cli = test_cli(
            Some(state.path().to_path_buf()),
            Some(cache.path().to_path_buf()),
        );
        let (printer, buf) = Printer::for_test_with_format(OutputFormat::Json);
        cmd_paths(&cli, &printer, &DirSources::all_default()).expect("cmd_paths must succeed");
        let out = buf.lock().unwrap().clone();
        let v: serde_json::Value =
            serde_json::from_str(out.trim()).unwrap_or_else(|e| panic!("invalid JSON {e}: {out}"));
        assert!(v["config"]["dir"].is_string(), "config.dir: {v}");
        assert!(v["state"]["applyLock"].is_string(), "state.applyLock: {v}");
        assert_eq!(v["cache"]["source"], serde_json::json!("default"));
    }

    // --- legacy_migration_eligible ---

    #[test]
    fn legacy_migration_eligible_when_both_roots_default() {
        assert!(DirSources::all_default().legacy_migration_eligible());
    }

    #[test]
    fn legacy_migration_blocked_when_state_overridden() {
        let sources = DirSources {
            state: DirSource::Flag,
            ..DirSources::all_default()
        };
        assert!(!sources.legacy_migration_eligible());
    }

    #[test]
    fn legacy_migration_blocked_when_cache_overridden() {
        let sources = DirSources {
            cache: DirSource::Env,
            ..DirSources::all_default()
        };
        assert!(!sources.legacy_migration_eligible());
    }

    #[test]
    fn legacy_migration_blocked_when_both_roots_overridden() {
        let sources = DirSources {
            state: DirSource::Flag,
            cache: DirSource::Env,
            ..DirSources::all_default()
        };
        assert!(!sources.legacy_migration_eligible());
    }
}
