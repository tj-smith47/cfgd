/// Every environment variable `cfgd_bin()` points into the calling
/// test's own directory, with the path under that directory it names.
///
/// Together they cover the home, config, data, state, cache and runtime roots
/// `cfgd` resolves, and the pre-split legacy data directory its startup
/// migration reads. On Linux and macOS the binary reaches each through the
/// home and XDG variables. Windows resolves its cache and runtime roots
/// through known-folder lookups that ignore the environment, so there the
/// `CFGD_*` overrides are what steer them; its config root honors
/// `XDG_CONFIG_HOME` and its legacy data directory follows `LOCALAPPDATA`.
///
/// `CFGD_CONFIG_DIR` is left out: the CLI reads it as an explicit
/// `--config-dir`, which turns off what a run does only at the default config
/// directory, such as the `--from` refusal.
pub const ISOLATED_ENV: &[(&str, &str)] = &[
    ("HOME", "home"),
    ("USERPROFILE", "home"),
    ("XDG_CONFIG_HOME", "home/.config"),
    ("XDG_DATA_HOME", "home/.local/share"),
    ("XDG_STATE_HOME", "home/.local/state"),
    ("XDG_CACHE_HOME", "home/.cache"),
    ("XDG_RUNTIME_DIR", "home/.cache/cfgd-runtime-base"),
    ("LOCALAPPDATA", "home/AppData/Local"),
    (cfgd_core::CFGD_STATE_DIR_ENV, "state"),
    (cfgd_core::CFGD_CACHE_DIR_ENV, "home/.cache/cfgd"),
    (cfgd_core::CFGD_RUNTIME_DIR_ENV, "home/runtime"),
];

/// Every environment variable `cfgd_bin()` removes outright: the directories
/// systemd hands a unit (`ConfigurationDirectory=` and its siblings). `cfgd`
/// ranks `CONFIGURATION_DIRECTORY` above `XDG_CONFIG_HOME`, so a test run
/// from inside such a unit would otherwise reach that unit's real config.
pub const REMOVED_ENV: &[&str] = &[
    "CONFIGURATION_DIRECTORY",
    "STATE_DIRECTORY",
    "CACHE_DIRECTORY",
    "RUNTIME_DIRECTORY",
];

/// The working directory `cfgd_bin()` starts `cfgd` in, under the
/// calling test's own directory.
pub const WORKING_DIR: &str = "cwd";
