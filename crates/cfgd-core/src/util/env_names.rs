//! The `CFGD_*` environment names production binds or reads, one const each and
//! spelled once for the whole workspace: every name clap binds to a flag, and
//! every name a production read (`std::env::var`, `env_or`, a `*_BIN` seam such
//! as `tool_cmd`) would otherwise spell as a literal. A `*_BIN` seam a module
//! already names with its own const beside its command factory keeps that const.
//! Files built only for tests (`test_helpers::is_test_only_file`: the
//! `fake-cosign` fixture binary and every module gated to tests) keep their own
//! names, since no shipped binary reads them.
//!
//! A clap `env =` binding and every other reader or writer of the same variable
//! name it through these, so a rename moves every reader together.

/// Config file or directory to read (`--config`).
pub const CFGD_CONFIG_ENV: &str = "CFGD_CONFIG";
/// Profile to act on (`--profile`); also exported to every hook script.
pub const CFGD_PROFILE_ENV: &str = "CFGD_PROFILE";
/// Verbosity level (`-v`); boolish spellings fold to `0`/`1`.
pub const CFGD_VERBOSE_ENV: &str = "CFGD_VERBOSE";
/// Suppress non-essential output (`--quiet`).
pub const CFGD_QUIET_ENV: &str = "CFGD_QUIET";
/// Answer every confirmation prompt yes (`--yes`).
pub const CFGD_YES_ENV: &str = "CFGD_YES";
/// When to colorize output (`--color`).
pub const CFGD_COLOR_ENV: &str = "CFGD_COLOR";
/// Theme preset for this invocation (`--theme`).
pub const CFGD_THEME_ENV: &str = "CFGD_THEME";
/// Which declared env values render masked (`--mask-env-values`).
pub const CFGD_MASK_ENV_VALUES_ENV: &str = "CFGD_MASK_ENV_VALUES";
/// What to do when the config is behind the schema (`--migration-policy`).
pub const CFGD_MIGRATION_POLICY_ENV: &str = "CFGD_MIGRATION_POLICY";
/// Update posture for this invocation (`--update-policy`).
pub const CFGD_UPDATE_POLICY_ENV: &str = "CFGD_UPDATE_POLICY";
/// Wrap top-level array payloads in a KRM List envelope (`--list-envelope`).
pub const CFGD_LIST_ENVELOPE_ENV: &str = "CFGD_LIST_ENVELOPE";
/// State directory override (`--state-dir`).
pub const CFGD_STATE_DIR_ENV: &str = "CFGD_STATE_DIR";
/// Config directory override (`--config-dir`); also exported to every hook script.
pub const CFGD_CONFIG_DIR_ENV: &str = "CFGD_CONFIG_DIR";
/// Cache directory override (`--cache-dir`).
pub const CFGD_CACHE_DIR_ENV: &str = "CFGD_CACHE_DIR";
/// Runtime directory override (`--runtime-dir`).
pub const CFGD_RUNTIME_DIR_ENV: &str = "CFGD_RUNTIME_DIR";
/// Installation scope, `user` or `system` (`--scope`).
pub const CFGD_SCOPE_ENV: &str = "CFGD_SCOPE";
/// Refuse a self-upgrade whose cosign signature does not verify (`cfgd upgrade --require-cosign`).
pub const CFGD_REQUIRE_COSIGN_ENV: &str = "CFGD_REQUIRE_COSIGN";
/// Device gateway URL (`cfgd checkin` / `cfgd enroll --server-url`).
pub const CFGD_SERVER_URL_ENV: &str = "CFGD_SERVER_URL";
/// Device gateway API key: `cfgd checkin --api-key` sends it, and the gateway checks
/// admin requests against it.
pub const CFGD_API_KEY_ENV: &str = "CFGD_API_KEY";
/// Device identifier a check-in reports (`cfgd checkin --device-id`).
pub const CFGD_DEVICE_ID_ENV: &str = "CFGD_DEVICE_ID";
/// Bootstrap token for token-based enrollment (`cfgd enroll --token`).
pub const CFGD_ENROLL_TOKEN_ENV: &str = "CFGD_ENROLL_TOKEN";
/// Username to enroll as (`cfgd enroll --username`).
pub const CFGD_ENROLL_USERNAME_ENV: &str = "CFGD_ENROLL_USERNAME";
/// Whether tutorial usage hints render; no flag binds it, since `--hints` and
/// `--no-hints` have opposite polarities.
pub const CFGD_USAGE_HINTS_ENV: &str = "CFGD_USAGE_HINTS";
/// Set to anything, lets a source or module be fetched from a local path or
/// `file://` URL; for development and test hosts.
pub const CFGD_ALLOW_LOCAL_SOURCES_ENV: &str = "CFGD_ALLOW_LOCAL_SOURCES";
/// Base URL of the Anthropic API the `cfgd` AI client talks to.
pub const CFGD_ANTHROPIC_URL_ENV: &str = "CFGD_ANTHROPIC_URL";
/// `1` or `true` adds the Windows Event Log sink to a Windows daemon.
pub const CFGD_WINDOWS_EVENT_LOG_ENV: &str = "CFGD_WINDOWS_EVENT_LOG";
/// Path of the daemon's IPC socket or pipe, in place of the derived default.
pub const CFGD_DAEMON_IPC_PATH_ENV: &str = "CFGD_DAEMON_IPC_PATH";
/// Size of the device gateway's SQLite reader pool.
pub const CFGD_GATEWAY_DB_READ_POOL_SIZE_ENV: &str = "CFGD_GATEWAY_DB_READ_POOL_SIZE";
/// Enrollment method the device gateway offers: `key`, or a token otherwise.
pub const CFGD_ENROLLMENT_METHOD_ENV: &str = "CFGD_ENROLLMENT_METHOD";
/// Path of the device gateway's SQLite database.
pub const CFGD_SERVER_DB_PATH_ENV: &str = "CFGD_SERVER_DB_PATH";
/// Days the device gateway keeps check-in history.
pub const CFGD_RETENTION_DAYS_ENV: &str = "CFGD_RETENTION_DAYS";
/// Test seam: the `ssh-keygen` binary enrollment signs a challenge with.
pub const CFGD_SSH_KEYGEN_BIN_ENV: &str = "CFGD_SSH_KEYGEN_BIN";
/// Base URL of the GitHub Releases API that the update check and `cfgd upgrade`
/// query, in place of `https://api.github.com`.
pub const CFGD_GITHUB_API_BASE_ENV: &str = "CFGD_GITHUB_API_BASE";
/// Set to anything but empty, `0` or `false`, silences the automatic update check.
pub const CFGD_NO_UPDATE_CHECK_ENV: &str = "CFGD_NO_UPDATE_CHECK";
/// Registries the CSI node plugin may pull modules from: comma-separated
/// `host[:port]` entries, `*` for any. Unset accepts any registry and warns at
/// startup.
pub const CFGD_CSI_ALLOWED_REGISTRIES_ENV: &str = "CFGD_CSI_ALLOWED_REGISTRIES";
/// Browser origins the device gateway accepts cross-origin requests from:
/// comma-separated scheme+host(+port) URLs such as `https://fleet.internal`,
/// `*` for any (development only). Unset or empty refuses every cross-origin
/// request; same-origin requests from the dashboard still work.
pub const CFGD_GATEWAY_ALLOWED_ORIGINS_ENV: &str = "CFGD_GATEWAY_ALLOWED_ORIGINS";
