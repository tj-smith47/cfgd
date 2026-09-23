/// Every environment variable `cfgd_bin()` points into the calling
/// test's own directory, with the path under that directory it names.
///
/// These are the variables `cfgd` and the crates it builds on resolve a home,
/// config, data, state or cache directory from, on every OS it runs on.
pub const ISOLATED_ENV: &[(&str, &str)] = &[
    ("HOME", "home"),
    ("USERPROFILE", "home"),
    ("XDG_CONFIG_HOME", "home/.config"),
    ("XDG_DATA_HOME", "home/.local/share"),
    ("XDG_STATE_HOME", "home/.local/state"),
    ("XDG_CACHE_HOME", "home/.cache"),
    ("CFGD_STATE_DIR", "state"),
];

/// The working directory `cfgd_bin()` starts `cfgd` in, under the
/// calling test's own directory.
pub const WORKING_DIR: &str = "cwd";
