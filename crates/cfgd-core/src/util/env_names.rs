//! The `CFGD_*` environment variables the `cfgd` command line binds to a flag,
//! each spelled once for the whole workspace.
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
