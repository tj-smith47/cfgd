use std::collections::HashMap;
use std::path::PathBuf;

use cfgd_core::errors::Result;
use cfgd_core::output::Role;

use cfgd_core::providers::{SystemConfigurator, SystemContext, SystemDrift};

use super::read_command_output;

use super::{diff_yaml_mapping, yaml_value_to_string};

/// Test seams for the two KDE binaries. Named separately because a host carries
/// one generation of each and a test drives one at a time.
const KREADCONFIG_BIN_ENV: &str = "CFGD_KREADCONFIG_BIN";
const KWRITECONFIG_BIN_ENV: &str = "CFGD_KWRITECONFIG_BIN";

/// KdeConfigConfigurator — reads/writes KDE Plasma settings via `kwriteconfig5`/`kwriteconfig6`.
///
/// Config format (three-level: file → group → key → value):
/// ```yaml
/// system:
///   kdeConfig:
///     kdeglobals:
///       General:
///         ColorScheme: BreezeDark
///       KDE:
///         LookAndFeelPackage: org.kde.breezedark.desktop
///     kwinrc:
///       Compositing:
///         Backend: OpenGL
/// ```
pub struct KdeConfigConfigurator;

/// A set seam names the binary outright, so the generation question is the
/// seam's to answer: `tool_cmd` discards the default, and probing the host for
/// a generation the spawn will not use describes the wrong machine.
fn seam_is_set(env_var: &str) -> bool {
    std::env::var_os(env_var).is_some()
}

/// Return the kwriteconfig command name (prefer v6, fallback to v5).
fn kde_write_cmd() -> &'static str {
    if seam_is_set(KWRITECONFIG_BIN_ENV) || cfgd_core::command_available("kwriteconfig6") {
        "kwriteconfig6"
    } else {
        "kwriteconfig5"
    }
}

/// Return the kreadconfig command name (prefer v6, fallback to v5).
fn kde_read_cmd() -> &'static str {
    if seam_is_set(KREADCONFIG_BIN_ENV) || cfgd_core::command_available("kreadconfig6") {
        "kreadconfig6"
    } else {
        "kreadconfig5"
    }
}

fn read_kde_value(file: &str, group: &str, key: &str) -> String {
    let mut cmd = cfgd_core::tool_cmd(KREADCONFIG_BIN_ENV, kde_read_cmd());
    cmd.args(["--file", file, "--group", group, "--key", key]);
    read_command_output(&mut cmd)
}

/// Resolve a `--file` value the way KConfig does: an absolute path stands, a
/// bare name lives under `$XDG_CONFIG_HOME` (`~/.config` when unset).
fn user_config_path(file: &str) -> Option<PathBuf> {
    let path = cfgd_core::expand_tilde(std::path::Path::new(file));
    if path.is_absolute() {
        return Some(path);
    }
    let base = match std::env::var("XDG_CONFIG_HOME") {
        Ok(dir) if !dir.trim().is_empty() => PathBuf::from(dir),
        _ => cfgd_core::expand_tilde(std::path::Path::new("~/.config")),
    };
    Some(base.join(path))
}

/// The system-wide half of the KConfig cascade (`$XDG_CONFIG_DIRS`, `/etc/xdg`
/// when unset) for one file name.
fn system_config_paths(file: &str) -> Vec<PathBuf> {
    if std::path::Path::new(file).is_absolute() {
        return Vec::new();
    }
    let dirs = match std::env::var("XDG_CONFIG_DIRS") {
        Ok(v) if !v.trim().is_empty() => v,
        _ => "/etc/xdg".to_string(),
    };
    dirs.split(':')
        .filter(|d| !d.trim().is_empty())
        .map(|d| PathBuf::from(d).join(file))
        .collect()
}

/// One rc file's plainly-readable entries, keyed `group\u{1}key`.
///
/// `kreadconfig` has no bulk mode, so the file it reads is read directly — and
/// only the entries whose raw text IS what `kreadconfig` would print are kept.
/// Everything else is left absent, and an absent key falls back to the per-key
/// spawn, so the fast path is a converged machine (`kwriteconfig` wrote the
/// value into this very file) and an unconverged key costs exactly what it cost
/// before.
///
/// Not kept, because KConfig would answer differently than the raw text:
/// * a key carrying any `[…]` suffix — `[$e]` shell-expands, `[$i]` marks
///   immutability, `[de]` is a locale variant answered only under that locale
/// * a value containing a backslash — KConfig unescapes `\n`, `\t`, `\s`, `\\`
/// * every entry in the file, when a system cascade file marks anything
///   immutable: an immutable system entry overrides the user file, and only
///   `kreadconfig` knows which
/// * a group header naming more than one group (`[General][Sub]`) — cfgd
///   declares a single group level, and a nested group is a different group
struct RcSnapshot {
    entries: HashMap<String, String>,
    trusted: bool,
}

impl RcSnapshot {
    fn get(&self, group: &str, key: &str) -> Option<&String> {
        if !self.trusted {
            return None;
        }
        self.entries.get(&entry_key(group, key))
    }
}

fn entry_key(group: &str, key: &str) -> String {
    format!("{group}\u{1}{key}")
}

fn snapshot_rc_file(file: &str) -> RcSnapshot {
    let immutable_anywhere = system_config_paths(file).iter().any(|path| {
        std::fs::read_to_string(path)
            .map(|body| body.contains("[$i]"))
            .unwrap_or(false)
    });
    let body = user_config_path(file)
        .and_then(|path| std::fs::read_to_string(path).ok())
        .unwrap_or_default();
    RcSnapshot {
        entries: parse_rc(&body),
        trusted: !immutable_anywhere,
    }
}

fn parse_rc(body: &str) -> HashMap<String, String> {
    let mut entries = HashMap::new();
    // A suffixed spelling of a key (`Path[$e]`, `Name[de]`) is that key's entry
    // too, and which one KConfig answers with depends on the flag and the
    // locale — so seeing one takes the plain spelling out of the snapshot
    // rather than letting it answer for both.
    let mut suffixed: Vec<String> = Vec::new();
    let mut group: Option<String> = None;
    for line in body.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(header) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            // `[General][Sub]` and `[General][$i]` both carry an inner bracket;
            // neither names the plain group cfgd declares.
            group = (!header.contains('[') && !header.contains(']')).then(|| header.to_string());
            continue;
        }
        let Some(ref current) = group else {
            continue;
        };
        let Some((raw_key, raw_value)) = line.split_once('=') else {
            continue;
        };
        let key = raw_key.trim();
        if key.is_empty() {
            continue;
        }
        if let Some((plain, _)) = key.split_once('[') {
            suffixed.push(entry_key(current, plain.trim()));
            continue;
        }
        let value = raw_value.trim();
        if value.contains('\\') {
            continue;
        }
        entries.insert(entry_key(current, key), value.to_string());
    }
    for key in suffixed {
        entries.remove(&key);
    }
    entries
}

impl SystemConfigurator for KdeConfigConfigurator {
    fn name(&self) -> &str {
        "kdeConfig"
    }

    fn is_available(&self) -> bool {
        if seam_is_set(KWRITECONFIG_BIN_ENV) {
            // The v5 fallback is a question about the HOST, and a set seam means
            // the host is not what runs: falling through to it answers
            // "available" for a shim path that does not exist.
            return cfgd_core::command_available_with_seam(KWRITECONFIG_BIN_ENV, "kwriteconfig6");
        }
        cfgd_core::command_available("kwriteconfig6")
            || cfgd_core::command_available("kwriteconfig5")
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let files = match desired.as_mapping() {
            Some(m) => m,
            None => return Ok(Vec::new()),
        };

        let mut drifts = Vec::new();
        for (file_key, groups) in files {
            let file = match file_key.as_str() {
                Some(f) => f,
                None => continue,
            };
            let groups_map = match groups.as_mapping() {
                // A file whose groups declare no keys has nothing to compare,
                // so reading its rc file (and its whole XDG cascade) answers a
                // question nobody asked. Unlike the sibling configurators' own
                // guards this one saves file I/O rather than a spawn, so no
                // shim can observe it.
                Some(m)
                    if m.values()
                        .any(|keys| keys.as_mapping().is_some_and(|k| !k.is_empty())) =>
                {
                    m
                }
                _ => continue,
            };
            // One read of the rc file for every group and key declared under
            // it, instead of one `kreadconfig` spawn per key.
            let snapshot = snapshot_rc_file(file);
            for (group_key, keys) in groups_map {
                let group = match group_key.as_str() {
                    Some(g) => g,
                    None => continue,
                };
                let keys_map = match keys.as_mapping() {
                    Some(m) => m,
                    None => continue,
                };
                let prefix = format!("{}.{}", file, group);
                drifts.extend(diff_yaml_mapping(
                    keys_map,
                    &prefix,
                    yaml_value_to_string,
                    |key_str| match snapshot.get(group, key_str) {
                        Some(value) => value.clone(),
                        None => read_kde_value(file, group, key_str),
                    },
                ));
            }
        }

        Ok(drifts)
    }

    fn apply(&self, desired: &serde_yaml::Value, cx: &SystemContext<'_>) -> Result<()> {
        let files = match desired.as_mapping() {
            Some(m) => m,
            None => return Ok(()),
        };

        let write_cmd = kde_write_cmd();

        for (file_key, groups) in files {
            let file = match file_key.as_str() {
                Some(f) => f,
                None => continue,
            };
            let groups_map = match groups.as_mapping() {
                Some(m) => m,
                None => continue,
            };
            for (group_key, keys) in groups_map {
                let group = match group_key.as_str() {
                    Some(g) => g,
                    None => continue,
                };
                let keys_map = match keys.as_mapping() {
                    Some(m) => m,
                    None => continue,
                };
                for (key, value) in keys_map {
                    let key_str = match key.as_str() {
                        Some(k) => k,
                        None => continue,
                    };
                    let val_str = yaml_value_to_string(value);

                    cx.report(
                        Role::Info,
                        format!(
                            "{} --file {} --group {} --key {} {}",
                            write_cmd, file, group, key_str, val_str
                        ),
                    );

                    let type_flag = match value {
                        serde_yaml::Value::Bool(_) => Some("bool"),
                        serde_yaml::Value::Number(_) => Some("int"),
                        _ => None,
                    };
                    let mut args = vec!["--file", file, "--group", group, "--key", key_str];
                    if let Some(t) = type_flag {
                        args.extend_from_slice(&["--type", t]);
                    }
                    args.push(&val_str);
                    let output = cfgd_core::tool_cmd(KWRITECONFIG_BIN_ENV, write_cmd)
                        .args(&args)
                        .output()
                        .map_err(cfgd_core::errors::CfgdError::Io)?;

                    if !output.status.success() {
                        cx.report(
                            Role::Warn,
                            format!(
                                "{} failed for {}.{}.{}: {}",
                                write_cmd,
                                file,
                                group,
                                key_str,
                                cfgd_core::stderr_lossy_trimmed(&output)
                            ),
                        );
                    }
                }
            }
        }

        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `kdeglobals` carrying one entry of every shape KConfig treats
    /// specially. Every expectation below was captured by running
    /// `kreadconfig6 --file kdeglobals --group <g> --key <k>` against this
    /// exact file (KConfig 6, `XDG_CONFIG_HOME` pointed at its directory).
    const REAL_RC: &str = "[General]\n\
        ColorScheme=BreezeDark\n\
        Name=Jane Doe\n\
        WithEquals=a=b\n\
        Trailing=  padded  \n\
        Empty=\n\
        WithEscape=line1\\nline2\n\
        WithBackslash=a\\\\b\n\
        Path[$e]=$HOME/docs\n\
        Localized[de]=Hallo\n\
        Immutable[$i]=locked\n\
        \n\
        [KDE]\n\
        LookAndFeelPackage=org.kde.breezedark.desktop\n\
        SingleClick=false\n\
        \n\
        [General][Sub]\n\
        Nested=deep\n";

    #[test]
    fn parsed_entries_answer_exactly_what_kreadconfig_answers() {
        let entries = parse_rc(REAL_RC);
        for (group, key, expected) in [
            ("General", "ColorScheme", "BreezeDark"),
            ("General", "Name", "Jane Doe"),
            // The first `=` separates; the rest is value.
            ("General", "WithEquals", "a=b"),
            // kreadconfig trims surrounding whitespace, and so does this.
            ("General", "Trailing", "padded"),
            ("General", "Empty", ""),
            ("KDE", "SingleClick", "false"),
            ("KDE", "LookAndFeelPackage", "org.kde.breezedark.desktop"),
        ] {
            assert_eq!(
                entries.get(&entry_key(group, key)).map(String::as_str),
                Some(expected),
                "raw text and kreadconfig disagree about {group}.{key}"
            );
        }
    }

    #[test]
    fn every_entry_kconfig_would_transform_is_left_to_kreadconfig() {
        let entries = parse_rc(REAL_RC);
        for (group, key, why) in [
            // kreadconfig prints a real newline for `\n`.
            ("General", "WithEscape", "escape sequence"),
            // `a\\b` reads back as `a\b`.
            ("General", "WithBackslash", "escaped backslash"),
            // `[$e]` shell-expands: kreadconfig answers `/root/docs`.
            ("General", "Path", "shell-expanded entry"),
            // `[de]` answers only under that locale; unlocalized reads empty.
            ("General", "Localized", "locale variant"),
            // `[$i]` is an immutability marker, not part of the key.
            ("General", "Immutable", "immutability marker"),
            // `[General][Sub]` is a different group than `General`.
            ("General", "Nested", "nested group"),
        ] {
            assert!(
                !entries.contains_key(&entry_key(group, key)),
                "{group}.{key} carries a {why} and must be re-read"
            );
        }
    }

    /// An rc file under a temp `XDG_CONFIG_HOME`, with the system cascade
    /// pointed at an empty directory so the host's own `/etc/xdg` cannot
    /// decide whether the snapshot is trusted.
    #[cfg(unix)]
    struct RcFixture {
        _home: tempfile::TempDir,
        _dirs: tempfile::TempDir,
        _home_env: cfgd_core::test_helpers::EnvVarGuard,
        _dirs_env: cfgd_core::test_helpers::EnvVarGuard,
    }

    #[cfg(unix)]
    impl RcFixture {
        fn with_system_file(body: &str, system_body: Option<&str>) -> Self {
            let home = tempfile::tempdir().expect("tempdir");
            let dirs = tempfile::tempdir().expect("tempdir");
            std::fs::write(home.path().join("kdeglobals"), body).expect("write rc");
            if let Some(system) = system_body {
                std::fs::write(dirs.path().join("kdeglobals"), system).expect("write system rc");
            }
            let home_env = cfgd_core::test_helpers::EnvVarGuard::set(
                "XDG_CONFIG_HOME",
                home.path().to_str().expect("utf-8 tempdir"),
            );
            let dirs_env = cfgd_core::test_helpers::EnvVarGuard::set(
                "XDG_CONFIG_DIRS",
                dirs.path().to_str().expect("utf-8 tempdir"),
            );
            Self {
                _home: home,
                _dirs: dirs,
                _home_env: home_env,
                _dirs_env: dirs_env,
            }
        }

        fn new(body: &str) -> Self {
            Self::with_system_file(body, None)
        }
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn kde_diff_spawns_nothing_for_keys_the_rc_file_answers() {
        let _fixture = RcFixture::new(REAL_RC);
        let shim = cfgd_core::test_helpers::ToolShim::install(KREADCONFIG_BIN_ENV, 0, "", "");
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            "kdeglobals:\n  \
             General:\n    \
             ColorScheme: BreezeDark\n    \
             Name: Jane Doe\n    \
             WithEquals: other\n  \
             KDE:\n    \
             SingleClick: false\n",
        )
        .unwrap();

        let drifts = KdeConfigConfigurator.diff(&yaml).unwrap();

        assert_eq!(
            shim.argv_lines_naming("kdeglobals").len(),
            0,
            "the rc file answered every key; kreadconfig ran anyway: {}",
            shim.argv_log()
        );
        assert_eq!(drifts.len(), 1, "unexpected drifts: {drifts:?}");
        assert_eq!(drifts[0].key, "kdeglobals.General.WithEquals");
        assert_eq!(drifts[0].actual, "a=b");
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn kde_diff_re_reads_only_the_keys_the_rc_file_cannot_answer() {
        let _fixture = RcFixture::new(REAL_RC);
        let shim =
            cfgd_core::test_helpers::ToolShim::install(KREADCONFIG_BIN_ENV, 0, "/root/docs", "");
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            "kdeglobals:\n  \
             General:\n    \
             ColorScheme: BreezeDark\n    \
             Path: /root/docs\n",
        )
        .unwrap();

        let drifts = KdeConfigConfigurator.diff(&yaml).unwrap();

        assert_eq!(
            shim.argv_lines_naming("kdeglobals"),
            vec!["--file kdeglobals --group General --key Path"],
            "only the shell-expanded entry is asked about"
        );
        assert!(drifts.is_empty(), "unexpected drifts: {drifts:?}");
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn kde_diff_reads_every_key_when_the_system_cascade_marks_anything_immutable() {
        let _fixture =
            RcFixture::with_system_file(REAL_RC, Some("[General]\nColorScheme[$i]=BreezeLight\n"));
        let shim =
            cfgd_core::test_helpers::ToolShim::install(KREADCONFIG_BIN_ENV, 0, "BreezeLight", "");
        let yaml: serde_yaml::Value = serde_yaml::from_str(
            "kdeglobals:\n  General:\n    ColorScheme: BreezeDark\n    Name: Jane Doe\n",
        )
        .unwrap();

        let drifts = KdeConfigConfigurator.diff(&yaml).unwrap();

        assert_eq!(
            shim.argv_lines_naming("kdeglobals").len(),
            2,
            "an immutable system entry overrides the user file, so nothing in it is trusted"
        );
        assert_eq!(drifts.len(), 2, "unexpected drifts: {drifts:?}");
        assert_eq!(drifts[0].actual, "BreezeLight");
    }

    #[test]
    fn kde_find_kwrite_cmd() {
        let cmd = kde_write_cmd();
        assert!(cmd == "kwriteconfig6" || cmd == "kwriteconfig5");
    }

    #[test]
    fn kde_find_kread_cmd() {
        let cmd = kde_read_cmd();
        assert!(cmd == "kreadconfig6" || cmd == "kreadconfig5");
    }

    #[test]
    #[serial_test::serial]
    fn kde_is_available_for_either_kwriteconfig_generation() {
        let _path_lock = cfgd_core::test_helpers::path_env_mutation_guard();
        let _dirs = cfgd_core::test_helpers::BootstrappedPathDirsGuard::capture_and_clear();
        let kc = KdeConfigConfigurator;

        {
            let _empty = cfgd_core::test_helpers::EnvVarGuard::set("PATH", "");
            assert!(
                !kc.is_available(),
                "a host resolving no binaries is not a KDE host"
            );
        }

        // Plasma 5 and 6 ship differently-suffixed binaries and a host carries
        // one or the other, so each generation is asserted on its own — an
        // either-or checked only against a host that has both would not notice
        // one arm being dropped.
        #[cfg(unix)]
        for generation in ["kwriteconfig6", "kwriteconfig5"] {
            let _probe = cfgd_core::test_helpers::ProbePath::containing(&[generation]);
            assert!(
                kc.is_available(),
                "{generation} alone must make this configurator available"
            );
        }
    }

    #[cfg(unix)]
    #[test]
    #[serial_test::serial]
    fn a_set_kwriteconfig_seam_answers_availability_by_itself() {
        // The v5 arm is a question about the HOST. With the seam set, the host
        // is not what runs, so falling through to it reports a KDE host for a
        // shim path that does not exist — and a test that installed a shim to
        // drive the unavailable branch would never reach it.
        let _path_lock = cfgd_core::test_helpers::path_env_mutation_guard();
        let _dirs = cfgd_core::test_helpers::BootstrappedPathDirsGuard::capture_and_clear();
        let _probe = cfgd_core::test_helpers::ProbePath::containing(&["kwriteconfig5"]);
        let _seam = cfgd_core::test_helpers::EnvVarGuard::set(
            KWRITECONFIG_BIN_ENV,
            "/nonexistent/cfgd-qp5/kwriteconfig",
        );

        assert!(
            !KdeConfigConfigurator.is_available(),
            "a seam naming a missing binary is the answer, host binaries or not"
        );
    }

    #[test]
    fn kde_apply_empty_mapping_is_noop() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let kc = KdeConfigConfigurator;
        let yaml = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
        kc.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn kde_apply_non_mapping_is_noop() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let kc = KdeConfigConfigurator;
        let yaml = serde_yaml::Value::String("not a mapping".into());
        kc.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn kde_apply_skips_non_string_file_key() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let kc = KdeConfigConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::Number(42.into()),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        kc.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn kde_apply_skips_groups_non_mapping() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let kc = KdeConfigConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("kdeglobals".into()),
            serde_yaml::Value::String("not a mapping".into()),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        kc.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn kde_apply_skips_non_string_group_key() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let kc = KdeConfigConfigurator;
        let mut groups = serde_yaml::Mapping::new();
        groups.insert(
            serde_yaml::Value::Number(1.into()),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("kdeglobals".into()),
            serde_yaml::Value::Mapping(groups),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        kc.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn kde_apply_skips_keys_non_mapping() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let kc = KdeConfigConfigurator;
        let mut groups = serde_yaml::Mapping::new();
        groups.insert(
            serde_yaml::Value::String("General".into()),
            serde_yaml::Value::String("not a mapping".into()),
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("kdeglobals".into()),
            serde_yaml::Value::Mapping(groups),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        kc.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn kde_apply_skips_non_string_key_in_group() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let kc = KdeConfigConfigurator;
        let mut keys = serde_yaml::Mapping::new();
        keys.insert(
            serde_yaml::Value::Number(99.into()),
            serde_yaml::Value::String("value".into()),
        );
        let mut groups = serde_yaml::Mapping::new();
        groups.insert(
            serde_yaml::Value::String("General".into()),
            serde_yaml::Value::Mapping(keys),
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("kdeglobals".into()),
            serde_yaml::Value::Mapping(groups),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        kc.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
            .unwrap();
    }

    #[test]
    fn kde_diff_non_string_file_key_skipped() {
        let kc = KdeConfigConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::Number(42.into()),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        let drifts = kc.diff(&yaml).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn kde_diff_groups_not_mapping_skipped() {
        let kc = KdeConfigConfigurator;
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("kdeglobals".into()),
            serde_yaml::Value::String("not a mapping".into()),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        let drifts = kc.diff(&yaml).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn kde_diff_non_string_group_key_skipped() {
        let kc = KdeConfigConfigurator;
        let mut groups = serde_yaml::Mapping::new();
        groups.insert(
            serde_yaml::Value::Number(1.into()),
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("kdeglobals".into()),
            serde_yaml::Value::Mapping(groups),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        let drifts = kc.diff(&yaml).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn kde_diff_keys_not_mapping_skipped() {
        let kc = KdeConfigConfigurator;
        let mut groups = serde_yaml::Mapping::new();
        groups.insert(
            serde_yaml::Value::String("General".into()),
            serde_yaml::Value::String("not a mapping".into()),
        );
        let mut outer = serde_yaml::Mapping::new();
        outer.insert(
            serde_yaml::Value::String("kdeglobals".into()),
            serde_yaml::Value::Mapping(groups),
        );
        let yaml = serde_yaml::Value::Mapping(outer);
        let drifts = kc.diff(&yaml).unwrap();
        assert!(drifts.is_empty());
    }

    #[test]
    fn kde_current_state_returns_empty_mapping() {
        let kc = KdeConfigConfigurator;
        let state = kc.current_state().unwrap();
        assert!(state.as_mapping().unwrap().is_empty());
    }
}
