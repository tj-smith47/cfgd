/// Every environment variable `cfgd_bin()` points into the calling
/// test's own directory, with the path under that directory it names.
///
/// Together they cover the home, config, data, state, cache and runtime roots
/// `cfgd` resolves, and the pre-split legacy data directory its startup
/// migration reads. On Linux and macOS the binary reaches each through the
/// home and XDG variables. Windows resolves its cache and runtime roots
/// through known-folder lookups that ignore the environment, so there the
/// `CFGD_*` overrides are what steer them; its config root honors
/// `XDG_CONFIG_HOME` and its legacy data directory follows `USERPROFILE`.
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
    ("CFGD_STATE_DIR", "state"),
    ("CFGD_CACHE_DIR", "home/.cache/cfgd"),
    ("CFGD_RUNTIME_DIR", "home/runtime"),
];

/// The working directory `cfgd_bin()` starts `cfgd` in, under the
/// calling test's own directory.
pub const WORKING_DIR: &str = "cwd";
