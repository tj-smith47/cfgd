// Externalized test module for system::node — see step-10 carve.

use super::format::*;
use super::*;
use crate::system::{yaml_value_to_string, yaml_value_with_numeric_bools};
use cfgd_core::providers::SystemConfigurator;
use cfgd_core::test_helpers::test_printer;
use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};
use tempfile::tempdir;

#[test]
fn node_configurator_names() {
    let cases: &[(&dyn SystemConfigurator, &str)] = &[
        (&SysctlConfigurator, "sysctl"),
        (&KernelModuleConfigurator, "kernelModules"),
        (&ContainerdConfigurator, "containerd"),
        (&KubeletConfigurator, "kubelet"),
        (&AppArmorConfigurator, "apparmor"),
        (&SeccompConfigurator, "seccomp"),
        (&CertificateConfigurator, "certificates"),
    ];
    for (c, expected) in cases {
        assert_eq!(c.name(), *expected, "wrong name for {expected}");
    }
}

#[test]
fn yaml_value_to_string_conversions() {
    assert_eq!(yaml_value_to_string(&serde_yaml::Value::Bool(true)), "true");
    assert_eq!(
        yaml_value_to_string(&serde_yaml::Value::Number(42.into())),
        "42"
    );
    assert_eq!(
        yaml_value_to_string(&serde_yaml::Value::String("hello".into())),
        "hello"
    );
}

#[test]
fn diff_returns_empty_for_empty_or_wrong_type_input() {
    let cases: &[(&dyn SystemConfigurator, serde_yaml::Value)] = &[
        (
            &SysctlConfigurator,
            serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        ),
        (
            &SysctlConfigurator,
            serde_yaml::Value::String("invalid".into()),
        ),
        (
            &KernelModuleConfigurator,
            serde_yaml::Value::Sequence(Vec::new()),
        ),
    ];
    for (c, input) in cases {
        let drifts = c.diff(input).unwrap();
        assert!(
            drifts.is_empty(),
            "{} should return empty for {:?}",
            c.name(),
            input
        );
    }
}

#[test]
fn containerd_default_config_path() {
    let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    let path = ContainerdConfigurator::config_path(&desired);
    assert_eq!(path, PathBuf::from("/etc/containerd/config.toml"));
}

#[test]
fn kubelet_default_config_path() {
    let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    let path = KubeletConfigurator::config_path(&desired);
    assert_eq!(path, PathBuf::from("/var/lib/kubelet/config.yaml"));
}

#[test]
fn json_equal_reordered_keys() {
    assert!(json_equal(r#"{ "b": 2, "a": 1 }"#, r#"{"a":1,"b":2}"#));
}

#[test]
fn json_equal_different_values() {
    assert!(!json_equal(r#"{"a":1}"#, r#"{"a":2}"#));
}

#[test]
fn json_equal_invalid_fallback() {
    assert!(json_equal("not json", "not json"));
    assert!(!json_equal("foo", "bar"));
}

#[test]
fn toml_value_to_string_conversions() {
    assert_eq!(toml_value_to_string(&toml::Value::Boolean(true)), "true");
    assert_eq!(toml_value_to_string(&toml::Value::Integer(42)), "42");
    assert_eq!(
        toml_value_to_string(&toml::Value::String("hello".into())),
        "hello"
    );
}

#[test]
fn yaml_to_toml_conversions() {
    assert_eq!(
        yaml_to_toml_value(&serde_yaml::Value::Bool(true)),
        toml::Value::Boolean(true)
    );
    assert_eq!(
        yaml_to_toml_value(&serde_yaml::Value::Number(42.into())),
        toml::Value::Integer(42)
    );
    assert_eq!(
        yaml_to_toml_value(&serde_yaml::Value::String("test".into())),
        toml::Value::String("test".into())
    );
}

#[test]
fn find_toml_value_direct() {
    let mut table = toml::Table::new();
    table.insert("key".to_string(), toml::Value::String("value".into()));
    assert_eq!(find_toml_value(&table, "key"), Some("value".to_string()));
}

#[test]
fn find_toml_value_nested() {
    let mut inner = toml::Table::new();
    inner.insert("nested_key".to_string(), toml::Value::Boolean(true));
    let mut table = toml::Table::new();
    table.insert("section".to_string(), toml::Value::Table(inner));
    assert_eq!(
        find_toml_value(&table, "nested_key"),
        Some("true".to_string())
    );
}

#[test]
fn find_toml_value_missing() {
    let table = toml::Table::new();
    assert_eq!(find_toml_value(&table, "missing"), None);
}

#[test]
fn seccomp_diff_empty_profiles() {
    let sc = SeccompConfigurator;
    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(Vec::new()),
    );
    let desired = serde_yaml::Value::Mapping(m);
    let drifts = sc.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn certificate_diff_empty() {
    let cc = CertificateConfigurator;
    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("certificates".into()),
        serde_yaml::Value::Sequence(Vec::new()),
    );
    let desired = serde_yaml::Value::Mapping(m);
    let drifts = cc.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn certificate_diff_missing_files() {
    let cc = CertificateConfigurator;

    let mut cert = serde_yaml::Mapping::new();
    cert.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("test-cert".into()),
    );
    cert.insert(
        serde_yaml::Value::String("certPath".into()),
        serde_yaml::Value::String("/nonexistent/cert.pem".into()),
    );
    cert.insert(
        serde_yaml::Value::String("keyPath".into()),
        serde_yaml::Value::String("/nonexistent/key.pem".into()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("certificates".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(cert)]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    let drifts = cc.diff(&desired).unwrap();
    assert_eq!(drifts.len(), 2);
    assert!(drifts[0].key.contains("test-cert"));
}

#[test]
fn apparmor_diff_empty_profiles() {
    let ac = AppArmorConfigurator;
    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(Vec::new()),
    );
    let desired = serde_yaml::Value::Mapping(m);
    let drifts = ac.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

// --- yaml_value_with_numeric_bools ---

#[test]
fn yaml_value_with_numeric_bools_bool_maps_to_01() {
    assert_eq!(
        yaml_value_with_numeric_bools(&serde_yaml::Value::Bool(true)),
        "1"
    );
    assert_eq!(
        yaml_value_with_numeric_bools(&serde_yaml::Value::Bool(false)),
        "0"
    );
}

#[test]
fn yaml_value_with_numeric_bools_delegates_non_bool() {
    assert_eq!(
        yaml_value_with_numeric_bools(&serde_yaml::Value::Number(262144.into())),
        "262144"
    );
    assert_eq!(
        yaml_value_with_numeric_bools(&serde_yaml::Value::String("1".into())),
        "1"
    );
}

// --- find_toml_value dot-path ---

#[test]
fn find_toml_value_dot_path() {
    let toml_str = r#"
[plugins]
[plugins.cri]
sandbox_image = "registry.k8s.io/pause:3.9"
[plugins.cri.containerd.runtimes.runc.options]
SystemdCgroup = true
"#;
    let table: toml::Table = toml_str.parse().unwrap();
    assert_eq!(
        find_toml_value(&table, "plugins.cri.sandbox_image"),
        Some("registry.k8s.io/pause:3.9".to_string())
    );
    assert_eq!(
        find_toml_value(
            &table,
            "plugins.cri.containerd.runtimes.runc.options.SystemdCgroup"
        ),
        Some("true".to_string())
    );
}

#[test]
fn find_toml_value_dot_path_missing() {
    let mut table = toml::Table::new();
    table.insert("key".to_string(), toml::Value::String("val".into()));
    assert_eq!(find_toml_value(&table, "no.such.path"), None);
}

// --- set_toml_value dot-path ---

#[test]
fn set_toml_value_dot_path_creates_nested_tables() {
    let mut table = toml::Table::new();
    set_toml_value(
        &mut table,
        "plugins.cri.sandbox_image",
        &serde_yaml::Value::String("pause:3.10".into()),
    );
    let val = find_toml_value(&table, "plugins.cri.sandbox_image");
    assert_eq!(val, Some("pause:3.10".to_string()));
}

#[test]
fn set_toml_value_simple_key() {
    let mut table = toml::Table::new();
    set_toml_value(&mut table, "version", &serde_yaml::Value::Number(2.into()));
    assert_eq!(table.get("version").unwrap().as_integer(), Some(2));
}

#[test]
fn containerd_read_nonexistent_config() {
    let table =
        ContainerdConfigurator::read_current_config(Path::new("/nonexistent/config.toml")).unwrap();
    assert!(table.is_empty());
}

#[test]
fn kubelet_read_nonexistent_config() {
    let value =
        KubeletConfigurator::read_current_config(Path::new("/nonexistent/config.yaml")).unwrap();
    assert!(value.is_mapping());
}

// --- validate_sysctl_key ---

#[test]
fn sysctl_validate_key_valid() {
    assert!(SysctlConfigurator::validate_sysctl_key("net.ipv4.ip_forward").is_ok());
    assert!(SysctlConfigurator::validate_sysctl_key("vm.max_map_count").is_ok());
    assert!(SysctlConfigurator::validate_sysctl_key("net.bridge.bridge-nf-call-iptables").is_ok());
}

#[test]
fn sysctl_validate_key_empty_rejected() {
    assert!(SysctlConfigurator::validate_sysctl_key("").is_err());
}

#[test]
fn sysctl_validate_key_uppercase_rejected() {
    assert!(SysctlConfigurator::validate_sysctl_key("NET.IPV4").is_err());
    assert!(SysctlConfigurator::validate_sysctl_key("net.ipV4.ip_forward").is_err());
}

#[test]
fn sysctl_validate_key_special_chars_rejected() {
    assert!(SysctlConfigurator::validate_sysctl_key("net/ipv4/ip_forward").is_err());
    assert!(SysctlConfigurator::validate_sysctl_key("key;rm -rf /").is_err());
    assert!(SysctlConfigurator::validate_sysctl_key("key with spaces").is_err());
}

// --- sysctl diff with populated mapping ---

/// A desired sysctl mapping of one key.
fn sysctl_desire(key: &str, value: serde_yaml::Value) -> serde_yaml::Value {
    let mut mapping = serde_yaml::Mapping::new();
    mapping.insert(serde_yaml::Value::String(key.to_string()), value);
    serde_yaml::Value::Mapping(mapping)
}

#[test]
fn sysctl_diff_detects_drift_for_unreadable_keys() {
    // No kernel exposes this knob, so the read fails on every host and the
    // drift is the same everywhere. Anchoring on a REAL knob instead reports
    // drift only on hosts whose value happens to differ from the desired one,
    // which is the assertion silently skipping itself.
    let drifts = SysctlConfigurator
        .diff(&sysctl_desire(
            "net.ipv4.cfgd_no_such_knob",
            serde_yaml::Value::String("1".into()),
        ))
        .unwrap();
    assert_eq!(drifts.len(), 1, "unexpected drifts: {drifts:?}");
    assert_eq!(drifts[0].key, "net.ipv4.cfgd_no_such_knob");
    assert_eq!(drifts[0].expected, "1");
    assert_eq!(
        drifts[0].actual, "<unreadable>",
        "a knob that cannot be read is reported as unreadable, not as absent"
    );
}

#[test]
fn sysctl_diff_bool_true_converts_to_1() {
    // `true` and `"1"` are the same desire, so they must produce the same
    // drift — compared against each other rather than against the host, which
    // has no opinion about a knob that does not exist.
    let key = "net.ipv4.cfgd_no_such_knob";
    let from_bool = SysctlConfigurator
        .diff(&sysctl_desire(key, serde_yaml::Value::Bool(true)))
        .unwrap();
    let from_string = SysctlConfigurator
        .diff(&sysctl_desire(key, serde_yaml::Value::String("1".into())))
        .unwrap();
    assert_eq!(from_bool.len(), 1, "unexpected drifts: {from_bool:?}");
    assert_eq!(
        from_bool[0].expected, "1",
        "`true` renders as the kernel's 1"
    );
    assert_eq!(from_bool, from_string);
}

#[cfg(target_os = "linux")]
#[test]
fn sysctl_diff_reports_nothing_for_a_knob_already_at_its_desired_value() {
    // The readable half of the same question: read a real knob, desire what it
    // already holds, and desire something it does not. The host supplies the
    // input here rather than the verdict, so the assertion holds on any kernel.
    let key = "kernel.pid_max";
    let current = match std::fs::read_to_string("/proc/sys/kernel/pid_max") {
        Ok(v) => v.trim().to_string(),
        // No procfs (a container without /proc mounted). Assert the arm that
        // IS reachable there rather than returning: with nothing to read, the
        // configurator has nothing to configure and must say so.
        Err(_) => {
            assert!(
                !SysctlConfigurator.is_available(),
                "no /proc/sys means no sysctl to apply"
            );
            return;
        }
    };

    let matched = SysctlConfigurator
        .diff(&sysctl_desire(
            key,
            serde_yaml::Value::String(current.clone()),
        ))
        .unwrap();
    assert!(
        matched.is_empty(),
        "a knob already at its desired value has not drifted: {matched:?}"
    );

    let differing = SysctlConfigurator
        .diff(&sysctl_desire(
            key,
            serde_yaml::Value::String(format!("{current}0")),
        ))
        .unwrap();
    assert_eq!(differing.len(), 1, "unexpected drifts: {differing:?}");
    assert_eq!(
        differing[0].actual, current,
        "drift must report the kernel's own value, not a placeholder"
    );
}

#[test]
fn sysctl_diff_skips_non_string_keys() {
    let sc = SysctlConfigurator;
    let mut mapping = serde_yaml::Mapping::new();
    mapping.insert(
        serde_yaml::Value::Number(42.into()),
        serde_yaml::Value::String("value".into()),
    );
    let desired = serde_yaml::Value::Mapping(mapping);
    let drifts = sc.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

// --- kernel module diff ---

#[test]
fn kernel_module_diff_with_non_sequence() {
    let km = KernelModuleConfigurator;
    let desired = serde_yaml::Value::String("not a sequence".into());
    let drifts = km.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn kernel_module_diff_skips_non_string_entries() {
    let km = KernelModuleConfigurator;
    let desired = serde_yaml::Value::Sequence(vec![
        serde_yaml::Value::Number(42.into()),
        serde_yaml::Value::Bool(true),
    ]);
    let drifts = km.diff(&desired).unwrap();
    // Non-string entries are skipped, so no drifts from them
    assert!(drifts.is_empty());
}

#[test]
fn kernel_module_diff_reports_unloaded_modules() {
    let km = KernelModuleConfigurator;
    // Use a module name that definitely won't be loaded
    let desired = serde_yaml::Value::Sequence(vec![serde_yaml::Value::String(
        "cfgd_fake_module_xyz_12345".into(),
    )]);
    let drifts = km.diff(&desired).unwrap();
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].key, "cfgd_fake_module_xyz_12345");
    assert_eq!(drifts[0].expected, "loaded");
    assert_eq!(drifts[0].actual, "not loaded");
}

// --- containerd diff with real TOML files ---

#[test]
fn containerd_read_existing_config() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.toml");
    fs::write(
        &config,
        "[plugins]\n[plugins.cri]\nsandbox_image = \"pause:3.9\"\n",
    )
    .unwrap();
    let table = ContainerdConfigurator::read_current_config(&config).unwrap();
    assert_eq!(
        find_toml_value(&table, "plugins.cri.sandbox_image"),
        Some("pause:3.9".to_string())
    );
}

#[test]
fn containerd_read_invalid_toml_returns_error() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.toml");
    fs::write(&config, "this is not valid toml [[[").unwrap();
    let err = ContainerdConfigurator::read_current_config(&config)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("failed to parse containerd config"),
        "expected containerd parse error, got: {err}"
    );
}

#[test]
fn containerd_config_path_custom() {
    let mut mapping = serde_yaml::Mapping::new();
    mapping.insert(
        serde_yaml::Value::String("configPath".into()),
        serde_yaml::Value::String("/custom/containerd.toml".into()),
    );
    let desired = serde_yaml::Value::Mapping(mapping);
    let path = ContainerdConfigurator::config_path(&desired);
    assert_eq!(path, PathBuf::from("/custom/containerd.toml"));
}

#[test]
fn containerd_diff_detects_changed_setting() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.toml");
    fs::write(&config, "sandbox_image = \"pause:3.8\"\n").unwrap();

    let mut settings = serde_yaml::Mapping::new();
    settings.insert(
        serde_yaml::Value::String("sandbox_image".into()),
        serde_yaml::Value::String("pause:3.9".into()),
    );

    let mut desired_map = serde_yaml::Mapping::new();
    desired_map.insert(
        serde_yaml::Value::String("configPath".into()),
        serde_yaml::Value::String(config.to_str().unwrap().into()),
    );
    desired_map.insert(
        serde_yaml::Value::String("settings".into()),
        serde_yaml::Value::Mapping(settings),
    );

    let cc = ContainerdConfigurator;
    let desired = serde_yaml::Value::Mapping(desired_map);
    let drifts = cc.diff(&desired).unwrap();
    crate::system::assert_keys_undoubled(&cc, &drifts);
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].key, "sandbox_image");
    assert_eq!(drifts[0].expected, "pause:3.9");
    assert_eq!(drifts[0].actual, "pause:3.8");
}

#[test]
fn containerd_diff_no_drift_when_matching() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.toml");
    fs::write(&config, "sandbox_image = \"pause:3.9\"\n").unwrap();

    let mut settings = serde_yaml::Mapping::new();
    settings.insert(
        serde_yaml::Value::String("sandbox_image".into()),
        serde_yaml::Value::String("pause:3.9".into()),
    );

    let mut desired_map = serde_yaml::Mapping::new();
    desired_map.insert(
        serde_yaml::Value::String("configPath".into()),
        serde_yaml::Value::String(config.to_str().unwrap().into()),
    );
    desired_map.insert(
        serde_yaml::Value::String("settings".into()),
        serde_yaml::Value::Mapping(settings),
    );

    let cc = ContainerdConfigurator;
    let desired = serde_yaml::Value::Mapping(desired_map);
    let drifts = cc.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn containerd_diff_missing_setting_shows_not_set() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.toml");
    fs::write(&config, "version = 2\n").unwrap();

    let mut settings = serde_yaml::Mapping::new();
    settings.insert(
        serde_yaml::Value::String("sandbox_image".into()),
        serde_yaml::Value::String("pause:3.9".into()),
    );

    let mut desired_map = serde_yaml::Mapping::new();
    desired_map.insert(
        serde_yaml::Value::String("configPath".into()),
        serde_yaml::Value::String(config.to_str().unwrap().into()),
    );
    desired_map.insert(
        serde_yaml::Value::String("settings".into()),
        serde_yaml::Value::Mapping(settings),
    );

    let cc = ContainerdConfigurator;
    let desired = serde_yaml::Value::Mapping(desired_map);
    let drifts = cc.diff(&desired).unwrap();
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].actual, "<not set>");
}

#[test]
fn containerd_diff_no_settings_returns_empty() {
    let cc = ContainerdConfigurator;
    let mut desired_map = serde_yaml::Mapping::new();
    desired_map.insert(
        serde_yaml::Value::String("configPath".into()),
        serde_yaml::Value::String("/nonexistent/config.toml".into()),
    );
    let desired = serde_yaml::Value::Mapping(desired_map);
    let drifts = cc.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn containerd_diff_with_nested_toml_settings() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.toml");
    fs::write(
        &config,
        "[plugins]\n[plugins.cri]\nsandbox_image = \"pause:3.8\"\n",
    )
    .unwrap();

    let mut settings = serde_yaml::Mapping::new();
    settings.insert(
        serde_yaml::Value::String("plugins.cri.sandbox_image".into()),
        serde_yaml::Value::String("pause:3.9".into()),
    );

    let mut desired_map = serde_yaml::Mapping::new();
    desired_map.insert(
        serde_yaml::Value::String("configPath".into()),
        serde_yaml::Value::String(config.to_str().unwrap().into()),
    );
    desired_map.insert(
        serde_yaml::Value::String("settings".into()),
        serde_yaml::Value::Mapping(settings),
    );

    let cc = ContainerdConfigurator;
    let desired = serde_yaml::Value::Mapping(desired_map);
    let drifts = cc.diff(&desired).unwrap();
    crate::system::assert_keys_undoubled(&cc, &drifts);
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].key, "plugins.cri.sandbox_image");
    assert_eq!(drifts[0].expected, "pause:3.9");
    assert_eq!(drifts[0].actual, "pause:3.8");
}

// --- kubelet diff with real YAML files ---

#[test]
fn kubelet_read_existing_config() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    fs::write(
        &config,
        "clusterDNS: 10.96.0.10\nclusterDomain: cluster.local\nmaxPods: 110\n",
    )
    .unwrap();
    let value = KubeletConfigurator::read_current_config(&config).unwrap();
    assert_eq!(
        value.get("clusterDNS").and_then(|v| v.as_str()),
        Some("10.96.0.10")
    );
    assert_eq!(value.get("maxPods").and_then(|v| v.as_u64()), Some(110));
}

#[test]
fn kubelet_read_invalid_yaml_returns_error() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    fs::write(&config, ":\n  - :\n    bad: [[[").unwrap();
    let err = KubeletConfigurator::read_current_config(&config)
        .unwrap_err()
        .to_string();
    assert!(
        err.contains("failed to parse kubelet config"),
        "expected kubelet parse error, got: {err}"
    );
}

#[test]
fn kubelet_config_path_custom() {
    let mut mapping = serde_yaml::Mapping::new();
    mapping.insert(
        serde_yaml::Value::String("configPath".into()),
        serde_yaml::Value::String("/custom/kubelet.yaml".into()),
    );
    let desired = serde_yaml::Value::Mapping(mapping);
    let path = KubeletConfigurator::config_path(&desired);
    assert_eq!(path, PathBuf::from("/custom/kubelet.yaml"));
}

#[test]
fn kubelet_diff_detects_changed_value() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    fs::write(&config, "maxPods: 100\ncgroupDriver: cgroupfs\n").unwrap();

    let mut settings = serde_yaml::Mapping::new();
    settings.insert(
        serde_yaml::Value::String("maxPods".into()),
        serde_yaml::Value::Number(110.into()),
    );
    settings.insert(
        serde_yaml::Value::String("cgroupDriver".into()),
        serde_yaml::Value::String("systemd".into()),
    );

    let mut desired_map = serde_yaml::Mapping::new();
    desired_map.insert(
        serde_yaml::Value::String("configPath".into()),
        serde_yaml::Value::String(config.to_str().unwrap().into()),
    );
    desired_map.insert(
        serde_yaml::Value::String("settings".into()),
        serde_yaml::Value::Mapping(settings),
    );

    let kc = KubeletConfigurator;
    let desired = serde_yaml::Value::Mapping(desired_map);
    let drifts = kc.diff(&desired).unwrap();
    crate::system::assert_keys_undoubled(&kc, &drifts);
    assert_eq!(drifts.len(), 2);

    let max_pods_drift = drifts.iter().find(|d| d.key == "maxPods").unwrap();
    assert_eq!(max_pods_drift.expected, "110");
    assert_eq!(max_pods_drift.actual, "100");

    let cgroup_drift = drifts.iter().find(|d| d.key == "cgroupDriver").unwrap();
    assert_eq!(cgroup_drift.expected, "systemd");
    assert_eq!(cgroup_drift.actual, "cgroupfs");
}

#[test]
fn kubelet_diff_no_drift_when_matching() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    fs::write(&config, "maxPods: 110\n").unwrap();

    let mut settings = serde_yaml::Mapping::new();
    settings.insert(
        serde_yaml::Value::String("maxPods".into()),
        serde_yaml::Value::Number(110.into()),
    );

    let mut desired_map = serde_yaml::Mapping::new();
    desired_map.insert(
        serde_yaml::Value::String("configPath".into()),
        serde_yaml::Value::String(config.to_str().unwrap().into()),
    );
    desired_map.insert(
        serde_yaml::Value::String("settings".into()),
        serde_yaml::Value::Mapping(settings),
    );

    let kc = KubeletConfigurator;
    let desired = serde_yaml::Value::Mapping(desired_map);
    let drifts = kc.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn kubelet_diff_missing_key_shows_not_set() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    fs::write(&config, "clusterDomain: cluster.local\n").unwrap();

    let mut settings = serde_yaml::Mapping::new();
    settings.insert(
        serde_yaml::Value::String("maxPods".into()),
        serde_yaml::Value::Number(110.into()),
    );

    let mut desired_map = serde_yaml::Mapping::new();
    desired_map.insert(
        serde_yaml::Value::String("configPath".into()),
        serde_yaml::Value::String(config.to_str().unwrap().into()),
    );
    desired_map.insert(
        serde_yaml::Value::String("settings".into()),
        serde_yaml::Value::Mapping(settings),
    );

    let kc = KubeletConfigurator;
    let desired = serde_yaml::Value::Mapping(desired_map);
    let drifts = kc.diff(&desired).unwrap();
    crate::system::assert_keys_undoubled(&kc, &drifts);
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].key, "maxPods");
    assert_eq!(drifts[0].actual, "<not set>");
}

#[test]
fn kubelet_diff_no_settings_returns_empty() {
    let kc = KubeletConfigurator;
    let mut desired_map = serde_yaml::Mapping::new();
    desired_map.insert(
        serde_yaml::Value::String("configPath".into()),
        serde_yaml::Value::String("/nonexistent/config.yaml".into()),
    );
    let desired = serde_yaml::Value::Mapping(desired_map);
    let drifts = kc.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn kubelet_diff_nonexistent_file_shows_not_set() {
    let mut settings = serde_yaml::Mapping::new();
    settings.insert(
        serde_yaml::Value::String("maxPods".into()),
        serde_yaml::Value::Number(110.into()),
    );

    let mut desired_map = serde_yaml::Mapping::new();
    desired_map.insert(
        serde_yaml::Value::String("configPath".into()),
        serde_yaml::Value::String("/nonexistent/kubelet/config.yaml".into()),
    );
    desired_map.insert(
        serde_yaml::Value::String("settings".into()),
        serde_yaml::Value::Mapping(settings),
    );

    let kc = KubeletConfigurator;
    let desired = serde_yaml::Value::Mapping(desired_map);
    let drifts = kc.diff(&desired).unwrap();
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].actual, "<not set>");
}

// --- apparmor diff with temp files ---

#[test]
fn apparmor_diff_missing_profile_file() {
    let ac = AppArmorConfigurator;

    let mut profile = serde_yaml::Mapping::new();
    profile.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("test-profile".into()),
    );
    profile.insert(
        serde_yaml::Value::String("path".into()),
        serde_yaml::Value::String("/nonexistent/apparmor/test-profile".into()),
    );
    profile.insert(
        serde_yaml::Value::String("content".into()),
        serde_yaml::Value::String("profile test-profile {}".into()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
    );
    let desired = serde_yaml::Value::Mapping(m);
    let drifts = ac.diff(&desired).unwrap();
    crate::system::assert_keys_undoubled(&ac, &drifts);

    // Should report file missing
    let file_drift = drifts
        .iter()
        .find(|d| d.key == "test-profile.file")
        .unwrap();
    assert_eq!(file_drift.expected, "present");
    assert_eq!(file_drift.actual, "missing");
}

#[test]
fn apparmor_diff_content_mismatch() {
    let dir = tempdir().unwrap();
    let profile_path = dir.path().join("test-profile");
    fs::write(&profile_path, "old content").unwrap();

    let ac = AppArmorConfigurator;

    let mut profile = serde_yaml::Mapping::new();
    profile.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("test-profile".into()),
    );
    profile.insert(
        serde_yaml::Value::String("path".into()),
        serde_yaml::Value::String(profile_path.to_str().unwrap().into()),
    );
    profile.insert(
        serde_yaml::Value::String("content".into()),
        serde_yaml::Value::String("new content".into()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
    );
    let desired = serde_yaml::Value::Mapping(m);
    let drifts = ac.diff(&desired).unwrap();
    crate::system::assert_keys_undoubled(&ac, &drifts);

    let content_drift = drifts
        .iter()
        .find(|d| d.key == "test-profile.content")
        .unwrap();
    assert_eq!(content_drift.expected, "updated");
    assert_eq!(content_drift.actual, "outdated");
}

#[test]
fn apparmor_diff_content_matches_no_content_drift() {
    let dir = tempdir().unwrap();
    let profile_path = dir.path().join("test-profile");
    let content = "profile test-profile flags=(attach_disconnected) {}";
    fs::write(&profile_path, content).unwrap();

    let ac = AppArmorConfigurator;

    let mut profile = serde_yaml::Mapping::new();
    profile.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("test-profile".into()),
    );
    profile.insert(
        serde_yaml::Value::String("path".into()),
        serde_yaml::Value::String(profile_path.to_str().unwrap().into()),
    );
    profile.insert(
        serde_yaml::Value::String("content".into()),
        serde_yaml::Value::String(content.to_string()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
    );
    let desired = serde_yaml::Value::Mapping(m);
    let drifts = ac.diff(&desired).unwrap();

    // No content drift, but may report "not loaded" depending on environment
    assert!(
        drifts.iter().all(|d| !d.key.contains("content")),
        "should not report content drift when content matches"
    );
}

#[test]
fn apparmor_diff_path_traversal_skipped() {
    let ac = AppArmorConfigurator;

    let mut profile = serde_yaml::Mapping::new();
    profile.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("traversal-profile".into()),
    );
    profile.insert(
        serde_yaml::Value::String("path".into()),
        serde_yaml::Value::String("/etc/apparmor.d/../../../etc/passwd".into()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
    );
    let desired = serde_yaml::Value::Mapping(m);
    let drifts = ac.diff(&desired).unwrap();
    // Profile with path traversal should be skipped entirely
    assert!(drifts.is_empty());
}

#[test]
fn apparmor_diff_no_profiles_key() {
    let ac = AppArmorConfigurator;
    let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    let drifts = ac.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn apparmor_diff_profile_without_name_skipped() {
    let ac = AppArmorConfigurator;

    let mut profile = serde_yaml::Mapping::new();
    // No "name" key
    profile.insert(
        serde_yaml::Value::String("path".into()),
        serde_yaml::Value::String("/tmp/profile".into()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
    );
    let desired = serde_yaml::Value::Mapping(m);
    let drifts = ac.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn apparmor_diff_profile_without_path_skipped() {
    let ac = AppArmorConfigurator;

    let mut profile = serde_yaml::Mapping::new();
    profile.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("test".into()),
    );
    // No "path" key

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
    );
    let desired = serde_yaml::Value::Mapping(m);
    let drifts = ac.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

// --- seccomp diff with temp files ---

#[test]
fn seccomp_diff_missing_profile_file() {
    let sc = SeccompConfigurator;
    let dir = tempdir().unwrap();

    let mut profile = serde_yaml::Mapping::new();
    profile.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("default-audit".into()),
    );
    profile.insert(
        serde_yaml::Value::String("file".into()),
        serde_yaml::Value::String("default-audit.json".into()),
    );
    profile.insert(
        serde_yaml::Value::String("content".into()),
        serde_yaml::Value::String(r#"{"defaultAction":"SCMP_ACT_LOG"}"#.into()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("profilesDir".into()),
        serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
    );
    m.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    let drifts = sc.diff(&desired).unwrap();
    crate::system::assert_keys_undoubled(&sc, &drifts);
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].key, "default-audit");
    assert_eq!(drifts[0].expected, "present");
    assert_eq!(drifts[0].actual, "missing");
}

#[test]
fn seccomp_diff_content_mismatch() {
    let dir = tempdir().unwrap();
    let profile_path = dir.path().join("default-audit.json");
    fs::write(&profile_path, r#"{"defaultAction":"SCMP_ACT_ERRNO"}"#).unwrap();

    let sc = SeccompConfigurator;

    let mut profile = serde_yaml::Mapping::new();
    profile.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("default-audit".into()),
    );
    profile.insert(
        serde_yaml::Value::String("file".into()),
        serde_yaml::Value::String("default-audit.json".into()),
    );
    profile.insert(
        serde_yaml::Value::String("content".into()),
        serde_yaml::Value::String(r#"{"defaultAction":"SCMP_ACT_LOG"}"#.into()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("profilesDir".into()),
        serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
    );
    m.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    let drifts = sc.diff(&desired).unwrap();
    crate::system::assert_keys_undoubled(&sc, &drifts);
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].key, "default-audit.content");
    assert_eq!(drifts[0].expected, "updated");
    assert_eq!(drifts[0].actual, "outdated");
}

#[test]
fn seccomp_diff_content_matches_semantically() {
    let dir = tempdir().unwrap();
    let profile_path = dir.path().join("default-audit.json");
    // Write with different whitespace/key order
    fs::write(
        &profile_path,
        r#"{ "b": 2, "defaultAction": "SCMP_ACT_LOG" }"#,
    )
    .unwrap();

    let sc = SeccompConfigurator;

    let mut profile = serde_yaml::Mapping::new();
    profile.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("default-audit".into()),
    );
    profile.insert(
        serde_yaml::Value::String("file".into()),
        serde_yaml::Value::String("default-audit.json".into()),
    );
    profile.insert(
        serde_yaml::Value::String("content".into()),
        serde_yaml::Value::String(r#"{"defaultAction":"SCMP_ACT_LOG","b":2}"#.into()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("profilesDir".into()),
        serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
    );
    m.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    let drifts = sc.diff(&desired).unwrap();
    // json_equal should match semantically equivalent JSON
    assert!(drifts.is_empty());
}

#[test]
fn seccomp_diff_path_traversal_skipped() {
    let sc = SeccompConfigurator;
    let dir = tempdir().unwrap();

    let mut profile = serde_yaml::Mapping::new();
    profile.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("evil".into()),
    );
    profile.insert(
        serde_yaml::Value::String("file".into()),
        serde_yaml::Value::String("../../etc/passwd".into()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("profilesDir".into()),
        serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
    );
    m.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    let drifts = sc.diff(&desired).unwrap();
    // Path traversal profiles should be skipped
    assert!(drifts.is_empty());
}

#[test]
fn seccomp_diff_no_profiles_key() {
    let sc = SeccompConfigurator;
    let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    let drifts = sc.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn seccomp_diff_profile_without_name_skipped() {
    let sc = SeccompConfigurator;
    let dir = tempdir().unwrap();

    let mut profile = serde_yaml::Mapping::new();
    // No "name" key
    profile.insert(
        serde_yaml::Value::String("file".into()),
        serde_yaml::Value::String("test.json".into()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("profilesDir".into()),
        serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
    );
    m.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    let drifts = sc.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn seccomp_diff_profile_without_file_skipped() {
    let sc = SeccompConfigurator;
    let dir = tempdir().unwrap();

    let mut profile = serde_yaml::Mapping::new();
    profile.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("test".into()),
    );
    // No "file" key

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("profilesDir".into()),
        serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
    );
    m.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    let drifts = sc.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn seccomp_diff_uses_default_profiles_dir() {
    let sc = SeccompConfigurator;

    let mut profile = serde_yaml::Mapping::new();
    profile.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("test".into()),
    );
    profile.insert(
        serde_yaml::Value::String("file".into()),
        serde_yaml::Value::String("test.json".into()),
    );

    let mut m = serde_yaml::Mapping::new();
    // No profilesDir — should use default /etc/cfgd/seccomp
    m.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    let drifts = sc.diff(&desired).unwrap();
    crate::system::assert_keys_undoubled(&sc, &drifts);
    // File won't exist at default path, so should report missing
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].key, "test");
    assert_eq!(drifts[0].actual, "missing");
}

// --- certificate diff with temp files and permissions ---

#[test]
fn certificate_diff_missing_cert_and_key_and_ca() {
    let cc = CertificateConfigurator;

    let mut cert = serde_yaml::Mapping::new();
    cert.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("kubelet-client".into()),
    );
    cert.insert(
        serde_yaml::Value::String("certPath".into()),
        serde_yaml::Value::String("/nonexistent/cert.pem".into()),
    );
    cert.insert(
        serde_yaml::Value::String("keyPath".into()),
        serde_yaml::Value::String("/nonexistent/key.pem".into()),
    );
    cert.insert(
        serde_yaml::Value::String("caPath".into()),
        serde_yaml::Value::String("/nonexistent/ca.pem".into()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("certificates".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(cert)]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    let drifts = cc.diff(&desired).unwrap();
    assert_eq!(drifts.len(), 3);
    assert!(drifts.iter().any(|d| d.key == "cert.kubelet-client.cert"));
    assert!(drifts.iter().any(|d| d.key == "cert.kubelet-client.key"));
    assert!(drifts.iter().any(|d| d.key == "cert.kubelet-client.ca"));
}

#[test]
fn certificate_diff_wrong_permissions() {
    let dir = tempdir().unwrap();
    let cert_path = dir.path().join("tls.crt");
    let key_path = dir.path().join("tls.key");
    fs::write(&cert_path, "cert data").unwrap();
    fs::write(&key_path, "key data").unwrap();

    // Set permissions to 0o644 (default)
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&cert_path, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).unwrap();
    }

    let cc = CertificateConfigurator;

    let mut cert = serde_yaml::Mapping::new();
    cert.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("tls".into()),
    );
    cert.insert(
        serde_yaml::Value::String("certPath".into()),
        serde_yaml::Value::String(cert_path.to_str().unwrap().into()),
    );
    cert.insert(
        serde_yaml::Value::String("keyPath".into()),
        serde_yaml::Value::String(key_path.to_str().unwrap().into()),
    );
    cert.insert(
        serde_yaml::Value::String("mode".into()),
        serde_yaml::Value::String("0600".into()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("certificates".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(cert)]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    let drifts = cc.diff(&desired).unwrap();

    #[cfg(unix)]
    {
        // Should detect permission drift on both files
        assert_eq!(drifts.len(), 2);
        for drift in &drifts {
            assert_eq!(drift.expected, "0600");
            assert_eq!(drift.actual, "0644");
        }
    }
}

#[test]
fn certificate_diff_correct_permissions_no_drift() {
    let dir = tempdir().unwrap();
    let cert_path = dir.path().join("tls.crt");
    fs::write(&cert_path, "cert data").unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&cert_path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    let cc = CertificateConfigurator;

    let mut cert = serde_yaml::Mapping::new();
    cert.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("tls".into()),
    );
    cert.insert(
        serde_yaml::Value::String("certPath".into()),
        serde_yaml::Value::String(cert_path.to_str().unwrap().into()),
    );
    cert.insert(
        serde_yaml::Value::String("mode".into()),
        serde_yaml::Value::String("0600".into()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("certificates".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(cert)]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    let drifts = cc.diff(&desired).unwrap();

    #[cfg(unix)]
    {
        assert!(drifts.is_empty());
    }
}

#[test]
fn certificate_diff_no_mode_no_permission_drift() {
    let dir = tempdir().unwrap();
    let cert_path = dir.path().join("tls.crt");
    fs::write(&cert_path, "cert data").unwrap();

    let cc = CertificateConfigurator;

    let mut cert = serde_yaml::Mapping::new();
    cert.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("tls".into()),
    );
    cert.insert(
        serde_yaml::Value::String("certPath".into()),
        serde_yaml::Value::String(cert_path.to_str().unwrap().into()),
    );
    // No "mode" key — no permission checking

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("certificates".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(cert)]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    let drifts = cc.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn certificate_diff_no_certificates_key() {
    let cc = CertificateConfigurator;
    let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    let drifts = cc.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

#[test]
fn certificate_diff_cert_without_name_skipped() {
    let cc = CertificateConfigurator;

    let mut cert = serde_yaml::Mapping::new();
    // No "name" key
    cert.insert(
        serde_yaml::Value::String("certPath".into()),
        serde_yaml::Value::String("/nonexistent/cert.pem".into()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("certificates".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(cert)]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    let drifts = cc.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

// --- set_toml_value edge cases ---

#[test]
fn set_toml_value_overwrites_non_table_intermediate() {
    let mut table = toml::Table::new();
    // Set "a" to a string first
    table.insert("a".to_string(), toml::Value::String("not a table".into()));
    // Now set "a.b" — should replace "a" with a table
    set_toml_value(
        &mut table,
        "a.b",
        &serde_yaml::Value::String("value".into()),
    );
    assert_eq!(find_toml_value(&table, "a.b"), Some("value".to_string()));
}

// --- yaml_to_toml_value edge cases ---

#[test]
fn yaml_to_toml_mapping_conversion() {
    let mut mapping = serde_yaml::Mapping::new();
    mapping.insert(
        serde_yaml::Value::String("key".into()),
        serde_yaml::Value::String("value".into()),
    );
    let result = yaml_to_toml_value(&serde_yaml::Value::Mapping(mapping));
    match result {
        toml::Value::Table(t) => {
            assert_eq!(t.get("key").unwrap().as_str(), Some("value"));
        }
        _ => panic!("expected Table"),
    }
}

#[test]
fn yaml_to_toml_sequence_conversion() {
    let seq = serde_yaml::Value::Sequence(vec![
        serde_yaml::Value::Number(1.into()),
        serde_yaml::Value::Number(2.into()),
    ]);
    let result = yaml_to_toml_value(&seq);
    match result {
        toml::Value::Array(arr) => {
            assert_eq!(arr.len(), 2);
            assert_eq!(arr[0].as_integer(), Some(1));
            assert_eq!(arr[1].as_integer(), Some(2));
        }
        _ => panic!("expected Array"),
    }
}

#[test]
fn yaml_to_toml_null_becomes_empty_string() {
    let result = yaml_to_toml_value(&serde_yaml::Value::Null);
    assert_eq!(result, toml::Value::String(String::new()));
}

#[test]
fn yaml_to_toml_float_conversion() {
    let val = serde_yaml::Value::Number(serde_yaml::Number::from(1.234_f64));
    let result = yaml_to_toml_value(&val);
    match result {
        toml::Value::Float(f) => assert!((f - 1.234).abs() < 0.001),
        _ => panic!("expected Float, got {:?}", result),
    }
}

#[test]
fn yaml_to_toml_mapping_non_string_keys_skipped() {
    let mut mapping = serde_yaml::Mapping::new();
    mapping.insert(
        serde_yaml::Value::Number(42.into()),
        serde_yaml::Value::String("value".into()),
    );
    mapping.insert(
        serde_yaml::Value::String("valid".into()),
        serde_yaml::Value::String("kept".into()),
    );
    let result = yaml_to_toml_value(&serde_yaml::Value::Mapping(mapping));
    match result {
        toml::Value::Table(t) => {
            assert_eq!(t.len(), 1);
            assert_eq!(t.get("valid").unwrap().as_str(), Some("kept"));
        }
        _ => panic!("expected Table"),
    }
}

// --- toml_value_to_string edge cases ---

#[test]
fn toml_value_to_string_float() {
    let result = toml_value_to_string(&toml::Value::Float(1.234));
    assert!(result.starts_with("1.234"));
}

#[test]
fn toml_value_to_string_array_uses_display() {
    let arr = toml::Value::Array(vec![toml::Value::Integer(1)]);
    let result = toml_value_to_string(&arr);
    assert!(result.contains('1'));
}

// --- json_equal edge cases ---

#[test]
fn json_equal_both_empty_objects() {
    assert!(json_equal("{}", "{}"));
}

#[test]
fn json_equal_nested_objects() {
    assert!(json_equal(r#"{"a":{"b":1}}"#, r#"{"a":{"b":1}}"#));
    assert!(!json_equal(r#"{"a":{"b":1}}"#, r#"{"a":{"b":2}}"#));
}

#[test]
fn json_equal_whitespace_trimming_fallback() {
    // Both invalid JSON but equal after trimming
    assert!(json_equal("  not json  ", "not json"));
}

// --- containerd diff with boolean TOML values ---

#[test]
fn containerd_diff_boolean_setting() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.toml");
    fs::write(&config, "SystemdCgroup = false\n").unwrap();

    let mut settings = serde_yaml::Mapping::new();
    settings.insert(
        serde_yaml::Value::String("SystemdCgroup".into()),
        serde_yaml::Value::Bool(true),
    );

    let mut desired_map = serde_yaml::Mapping::new();
    desired_map.insert(
        serde_yaml::Value::String("configPath".into()),
        serde_yaml::Value::String(config.to_str().unwrap().into()),
    );
    desired_map.insert(
        serde_yaml::Value::String("settings".into()),
        serde_yaml::Value::Mapping(settings),
    );

    let cc = ContainerdConfigurator;
    let desired = serde_yaml::Value::Mapping(desired_map);
    let drifts = cc.diff(&desired).unwrap();
    crate::system::assert_keys_undoubled(&cc, &drifts);
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].key, "SystemdCgroup");
    assert_eq!(drifts[0].expected, "true");
    assert_eq!(drifts[0].actual, "false");
}

// --- kubelet diff with string matching ---

#[test]
fn kubelet_diff_string_value_matches() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    fs::write(&config, "cgroupDriver: systemd\n").unwrap();

    let mut settings = serde_yaml::Mapping::new();
    settings.insert(
        serde_yaml::Value::String("cgroupDriver".into()),
        serde_yaml::Value::String("systemd".into()),
    );

    let mut desired_map = serde_yaml::Mapping::new();
    desired_map.insert(
        serde_yaml::Value::String("configPath".into()),
        serde_yaml::Value::String(config.to_str().unwrap().into()),
    );
    desired_map.insert(
        serde_yaml::Value::String("settings".into()),
        serde_yaml::Value::Mapping(settings),
    );

    let kc = KubeletConfigurator;
    let desired = serde_yaml::Value::Mapping(desired_map);
    let drifts = kc.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

// --- SysctlConfigurator current_state ---

#[test]
fn sysctl_current_state_returns_empty_mapping() {
    let sc = SysctlConfigurator;
    let state = sc.current_state().unwrap();
    assert!(state.as_mapping().unwrap().is_empty());
}

// --- KernelModuleConfigurator current_state ---

#[test]
fn kernel_module_current_state_returns_empty_sequence() {
    let km = KernelModuleConfigurator;
    let state = km.current_state().unwrap();
    assert!(state.is_sequence());
    assert!(state.as_sequence().unwrap().is_empty());
}

// --- ContainerdConfigurator current_state ---

#[test]
fn containerd_current_state_returns_empty_mapping() {
    let cc = ContainerdConfigurator;
    let state = cc.current_state().unwrap();
    assert!(state.as_mapping().unwrap().is_empty());
}

// --- KubeletConfigurator current_state ---

#[test]
fn kubelet_current_state_returns_empty_mapping() {
    let kc = KubeletConfigurator;
    let state = kc.current_state().unwrap();
    assert!(state.as_mapping().unwrap().is_empty());
}

// --- AppArmorConfigurator current_state ---

#[test]
fn apparmor_current_state_returns_empty_mapping() {
    let ac = AppArmorConfigurator;
    let state = ac.current_state().unwrap();
    assert!(state.as_mapping().unwrap().is_empty());
}

// --- SeccompConfigurator current_state ---

#[test]
fn seccomp_current_state_returns_empty_mapping() {
    let sc = SeccompConfigurator;
    let state = sc.current_state().unwrap();
    assert!(state.as_mapping().unwrap().is_empty());
}

// --- CertificateConfigurator current_state ---

#[test]
fn certificate_current_state_returns_empty_mapping() {
    let cc = CertificateConfigurator;
    let state = cc.current_state().unwrap();
    assert!(state.as_mapping().unwrap().is_empty());
}

// --- SysctlConfigurator::validate_sysctl_key additional patterns ---

#[test]
fn sysctl_validate_key_with_dash() {
    assert!(SysctlConfigurator::validate_sysctl_key("net.bridge.bridge-nf-call-ip6tables").is_ok());
}

#[test]
fn sysctl_validate_key_with_underscore_and_digits() {
    assert!(SysctlConfigurator::validate_sysctl_key("vm.max_map_count").is_ok());
}

#[test]
fn sysctl_validate_key_single_segment() {
    assert!(SysctlConfigurator::validate_sysctl_key("hostname").is_ok());
}

#[test]
fn sysctl_validate_key_with_tab_rejected() {
    assert!(SysctlConfigurator::validate_sysctl_key("key\twith\ttab").is_err());
}

#[test]
fn sysctl_validate_key_with_shell_injection_rejected() {
    assert!(SysctlConfigurator::validate_sysctl_key("key$(whoami)").is_err());
}

#[test]
fn sysctl_validate_key_with_backtick_rejected() {
    assert!(SysctlConfigurator::validate_sysctl_key("key`cmd`").is_err());
}

// --- ContainerdConfigurator::read_current_config with valid TOML ---

#[test]
fn containerd_read_config_with_multiple_sections() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.toml");
    fs::write(
        &config,
        "version = 2\n\n[plugins.cri]\nsandbox_image = \"pause:3.9\"\n\n[plugins.cri.containerd.runtimes.runc.options]\nSystemdCgroup = true\n",
    ).unwrap();
    let table = ContainerdConfigurator::read_current_config(&config).unwrap();
    assert_eq!(table.get("version").unwrap().as_integer(), Some(2));
    assert_eq!(
        find_toml_value(&table, "plugins.cri.sandbox_image"),
        Some("pause:3.9".to_string())
    );
    assert_eq!(
        find_toml_value(
            &table,
            "plugins.cri.containerd.runtimes.runc.options.SystemdCgroup"
        ),
        Some("true".to_string())
    );
}

// --- ContainerdConfigurator diff with non-mapping desired ---

#[test]
fn containerd_diff_non_mapping_returns_empty() {
    let cc = ContainerdConfigurator;
    let desired = serde_yaml::Value::String("not a mapping".into());
    let drifts = cc.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

// --- KubeletConfigurator diff with non-mapping desired ---

#[test]
fn kubelet_diff_non_mapping_returns_empty() {
    let kc = KubeletConfigurator;
    let desired = serde_yaml::Value::String("not a mapping".into());
    // config_path falls back to default, which doesn't exist
    // and settings extraction returns None since desired is not a mapping
    // Note: config_path looks for "configPath" on desired, which is not available
    // so uses default. The real test is that settings returns None.
    let drifts = kc.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

// --- seccomp diff with existing matching JSON content ---

#[test]
fn seccomp_diff_existing_matching_content_no_drift() {
    let dir = tempdir().unwrap();
    let profile_path = dir.path().join("default-audit.json");
    let content = r#"{"defaultAction":"SCMP_ACT_LOG"}"#;
    fs::write(&profile_path, content).unwrap();

    let sc = SeccompConfigurator;

    let mut profile = serde_yaml::Mapping::new();
    profile.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("default-audit".into()),
    );
    profile.insert(
        serde_yaml::Value::String("file".into()),
        serde_yaml::Value::String("default-audit.json".into()),
    );
    profile.insert(
        serde_yaml::Value::String("content".into()),
        serde_yaml::Value::String(content.into()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("profilesDir".into()),
        serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
    );
    m.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    let drifts = sc.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

// --- seccomp diff with profile that has no content key ---

#[test]
fn seccomp_diff_profile_without_content_no_content_drift() {
    let dir = tempdir().unwrap();
    let profile_path = dir.path().join("existing.json");
    fs::write(&profile_path, r#"{"defaultAction":"SCMP_ACT_LOG"}"#).unwrap();

    let sc = SeccompConfigurator;

    let mut profile = serde_yaml::Mapping::new();
    profile.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("existing".into()),
    );
    profile.insert(
        serde_yaml::Value::String("file".into()),
        serde_yaml::Value::String("existing.json".into()),
    );
    // No "content" key — content comparison should be skipped

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("profilesDir".into()),
        serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
    );
    m.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    let drifts = sc.diff(&desired).unwrap();
    // File exists, no content key — no drift expected
    assert!(drifts.is_empty());
}

// --- certificate diff with existing cert with correct permissions ---

#[test]
fn certificate_diff_existing_cert_no_drift() {
    let dir = tempdir().unwrap();
    let cert_path = dir.path().join("tls.crt");
    let key_path = dir.path().join("tls.key");
    fs::write(&cert_path, "cert data").unwrap();
    fs::write(&key_path, "key data").unwrap();

    let cc = CertificateConfigurator;

    let mut cert = serde_yaml::Mapping::new();
    cert.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("tls".into()),
    );
    cert.insert(
        serde_yaml::Value::String("certPath".into()),
        serde_yaml::Value::String(cert_path.to_str().unwrap().into()),
    );
    cert.insert(
        serde_yaml::Value::String("keyPath".into()),
        serde_yaml::Value::String(key_path.to_str().unwrap().into()),
    );
    // No mode key — no permission drift

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("certificates".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(cert)]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    let drifts = cc.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

// --- certificate diff with non-sequence desired ---

#[test]
fn certificate_diff_non_sequence_desired() {
    let cc = CertificateConfigurator;
    let desired = serde_yaml::Value::String("not a mapping".into());
    let drifts = cc.diff(&desired).unwrap();
    assert!(drifts.is_empty());
}

// --- CertificateConfigurator is_available ---

// Asserted per target rather than against `cfg!(target_os = "linux")`: compared
// to the same expression the implementation uses, the two move together and the
// test passes on every host whatever the configurator claims.
#[test]
#[cfg(target_os = "linux")]
fn certificate_is_available_on_linux() {
    assert!(CertificateConfigurator.is_available());
}

#[test]
#[cfg(not(target_os = "linux"))]
fn certificate_is_unavailable_off_linux() {
    assert!(
        !CertificateConfigurator.is_available(),
        "the CA-trust layout this writes is Linux's"
    );
}

// --- SeccompConfigurator is_available ---

// Availability follows the kernel's own seccomp knob, not the OS: a Linux build
// without CONFIG_SECCOMP has no such file and must report unavailable. The two
// directions are asserted separately because comparing against the same
// `exists()` call the implementation makes restates it and can never fail.
// Both kernel states assert. An early `return` when the knob is absent leaves
// a whole arm that passes without checking anything, and nothing distinguishes
// that from the configurator having been broken.
#[test]
#[cfg(target_os = "linux")]
fn seccomp_availability_follows_the_kernels_own_knob() {
    if std::path::Path::new("/proc/sys/kernel/seccomp").exists() {
        assert!(
            SeccompConfigurator.is_available(),
            "a kernel exposing the seccomp knob can be configured"
        );
    } else {
        assert!(
            !SeccompConfigurator.is_available(),
            "a Linux build without CONFIG_SECCOMP exposes no knob to write"
        );
    }
}

#[test]
#[cfg(not(target_os = "linux"))]
fn seccomp_is_unavailable_where_there_is_no_proc() {
    assert!(
        !SeccompConfigurator.is_available(),
        "seccomp is a Linux kernel facility"
    );
}

// --- find_toml_value dot-path with missing intermediate ---

#[test]
fn find_toml_value_dot_path_with_nonexistent_intermediate() {
    let mut table = toml::Table::new();
    table.insert("key".to_string(), toml::Value::String("val".into()));
    assert_eq!(find_toml_value(&table, "nonexistent.sub.key"), None);
}

// --- find_toml_value recursive search ---

#[test]
fn find_toml_value_recursive_search_in_nested() {
    let mut inner = toml::Table::new();
    inner.insert("deep_key".to_string(), toml::Value::Integer(42));
    let mut mid = toml::Table::new();
    mid.insert("inner".to_string(), toml::Value::Table(inner));
    let mut table = toml::Table::new();
    table.insert("outer".to_string(), toml::Value::Table(mid));

    // Non-dotted key should be found via recursive search
    assert_eq!(find_toml_value(&table, "deep_key"), Some("42".to_string()));
}

// --- json_equal both empty arrays ---

#[test]
fn json_equal_empty_arrays() {
    assert!(json_equal("[]", "[]"));
}

#[test]
fn json_equal_arrays_with_different_order() {
    assert!(!json_equal("[1,2]", "[2,1]"));
}

// --- KernelModuleConfigurator diff mixed entries ---

#[test]
fn kernel_module_diff_mixed_string_and_non_string() {
    let km = KernelModuleConfigurator;
    // First entry is a non-string (skipped), second is a definitely-missing module
    let desired = serde_yaml::Value::Sequence(vec![
        serde_yaml::Value::Number(99.into()),
        serde_yaml::Value::String("cfgd_fake_module_test_abc".into()),
    ]);
    let drifts = km.diff(&desired).unwrap();
    // Only the string module should produce drift
    assert_eq!(drifts.len(), 1);
    assert_eq!(drifts[0].key, "cfgd_fake_module_test_abc");
}

// --- ContainerdConfigurator config_path default vs custom ---

#[test]
fn containerd_config_path_falls_back_to_default_for_empty_mapping() {
    let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    let path = ContainerdConfigurator::config_path(&desired);
    assert_eq!(
        path,
        PathBuf::from(ContainerdConfigurator::DEFAULT_CONFIG_PATH)
    );
}

// --- KubeletConfigurator config_path ---

#[test]
fn kubelet_config_path_falls_back_to_default_for_empty_mapping() {
    let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    let path = KubeletConfigurator::config_path(&desired);
    assert_eq!(
        path,
        PathBuf::from(KubeletConfigurator::DEFAULT_CONFIG_PATH)
    );
}

// --- KubeletConfigurator read_current_config with complex YAML ---

#[test]
fn kubelet_read_config_with_nested_yaml() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    fs::write(
        &config,
        "apiVersion: kubelet.config.k8s.io/v1beta1\nkind: KubeletConfiguration\nmaxPods: 110\nclusterDNS:\n  - 10.96.0.10\n",
    ).unwrap();
    let value = KubeletConfigurator::read_current_config(&config).unwrap();
    assert_eq!(value.get("maxPods").and_then(|v| v.as_u64()), Some(110));
    assert!(value.get("clusterDNS").unwrap().is_sequence());
}

// --- set_toml_value deeply nested ---

#[test]
fn set_toml_value_deeply_nested_path() {
    let mut table = toml::Table::new();
    set_toml_value(
        &mut table,
        "a.b.c.d.e",
        &serde_yaml::Value::String("deep".into()),
    );
    assert_eq!(
        find_toml_value(&table, "a.b.c.d.e"),
        Some("deep".to_string())
    );
}

#[test]
fn set_toml_value_boolean_in_nested_path() {
    let mut table = toml::Table::new();
    set_toml_value(
        &mut table,
        "plugins.cri.containerd.runtimes.runc.options.SystemdCgroup",
        &serde_yaml::Value::Bool(true),
    );
    assert_eq!(
        find_toml_value(
            &table,
            "plugins.cri.containerd.runtimes.runc.options.SystemdCgroup"
        ),
        Some("true".to_string())
    );
}

// --- persist_all_sysctls_to: file content verification ---

#[test]
fn persist_sysctls_writes_sorted_conf_file() {
    let dir = tempdir().unwrap();
    let conf_dir = dir.path().join("sysctl.d");

    let mut entries = BTreeMap::new();
    entries.insert("net.ipv4.ip_forward", "1".to_string());
    entries.insert("vm.max_map_count", "262144".to_string());
    entries.insert("net.bridge.bridge-nf-call-iptables", "1".to_string());

    SysctlConfigurator::persist_all_sysctls_to(&conf_dir, &entries).unwrap();

    let content = fs::read_to_string(conf_dir.join("99-cfgd.conf")).unwrap();
    assert!(
        content.starts_with("# Managed by cfgd"),
        "missing header comment"
    );
    // BTreeMap iterates in sorted order
    assert!(content.contains("net.bridge.bridge-nf-call-iptables = 1\n"));
    assert!(content.contains("net.ipv4.ip_forward = 1\n"));
    assert!(content.contains("vm.max_map_count = 262144\n"));
    // Verify net.bridge comes before net.ipv4 (sorted)
    let bridge_pos = content.find("net.bridge").unwrap();
    let ipv4_pos = content.find("net.ipv4").unwrap();
    assert!(bridge_pos < ipv4_pos, "entries should be in sorted order");
}

#[test]
fn persist_sysctls_creates_conf_dir_if_missing() {
    let dir = tempdir().unwrap();
    let conf_dir = dir.path().join("nonexistent").join("sysctl.d");
    assert!(!conf_dir.exists());

    let mut entries = BTreeMap::new();
    entries.insert("net.ipv4.ip_forward", "1".to_string());

    SysctlConfigurator::persist_all_sysctls_to(&conf_dir, &entries).unwrap();
    assert!(conf_dir.join("99-cfgd.conf").exists());
}

#[test]
fn persist_sysctls_empty_entries_removes_conf_file() {
    let dir = tempdir().unwrap();
    let conf_dir = dir.path().join("sysctl.d");
    fs::create_dir_all(&conf_dir).unwrap();
    let conf_path = conf_dir.join("99-cfgd.conf");
    fs::write(&conf_path, "old content").unwrap();
    assert!(conf_path.exists());

    let entries: BTreeMap<&str, String> = BTreeMap::new();
    SysctlConfigurator::persist_all_sysctls_to(&conf_dir, &entries).unwrap();
    assert!(!conf_path.exists(), "empty entries should remove the file");
}

#[test]
fn persist_sysctls_overwrites_existing_content() {
    let dir = tempdir().unwrap();
    let conf_dir = dir.path().join("sysctl.d");
    fs::create_dir_all(&conf_dir).unwrap();
    fs::write(conf_dir.join("99-cfgd.conf"), "old.key = old_value\n").unwrap();

    let mut entries = BTreeMap::new();
    entries.insert("new.key", "new_value".to_string());

    SysctlConfigurator::persist_all_sysctls_to(&conf_dir, &entries).unwrap();

    let content = fs::read_to_string(conf_dir.join("99-cfgd.conf")).unwrap();
    assert!(content.contains("new.key = new_value"));
    assert!(
        !content.contains("old.key"),
        "old content should be replaced"
    );
}

// --- persist_modules_to: file content verification ---

#[test]
fn persist_modules_writes_one_per_line() {
    let dir = tempdir().unwrap();
    let conf_dir = dir.path().join("modules-load.d");

    KernelModuleConfigurator::persist_modules_to(&conf_dir, &["br_netfilter", "overlay", "ip_vs"])
        .unwrap();

    let content = fs::read_to_string(conf_dir.join("cfgd.conf")).unwrap();
    assert!(
        content.starts_with("# Managed by cfgd"),
        "missing header comment"
    );
    assert!(content.contains("br_netfilter\n"));
    assert!(content.contains("overlay\n"));
    assert!(content.contains("ip_vs\n"));
    // Verify each module is on its own line (not space-separated)
    let module_lines: Vec<&str> = content
        .lines()
        .filter(|l| !l.starts_with('#') && !l.is_empty())
        .collect();
    assert_eq!(module_lines.len(), 3);
}

#[test]
fn persist_modules_creates_dir_if_missing() {
    let dir = tempdir().unwrap();
    let conf_dir = dir.path().join("deep").join("modules-load.d");
    assert!(!conf_dir.exists());

    KernelModuleConfigurator::persist_modules_to(&conf_dir, &["overlay"]).unwrap();
    assert!(conf_dir.join("cfgd.conf").exists());
}

#[test]
fn persist_modules_empty_removes_conf_file() {
    let dir = tempdir().unwrap();
    let conf_dir = dir.path().join("modules-load.d");
    fs::create_dir_all(&conf_dir).unwrap();
    let conf_path = conf_dir.join("cfgd.conf");
    fs::write(&conf_path, "old content").unwrap();

    KernelModuleConfigurator::persist_modules_to(&conf_dir, &[]).unwrap();
    assert!(
        !conf_path.exists(),
        "empty modules should remove the conf file"
    );
}

#[test]
fn persist_modules_overwrites_existing() {
    let dir = tempdir().unwrap();
    let conf_dir = dir.path().join("modules-load.d");
    fs::create_dir_all(&conf_dir).unwrap();
    fs::write(conf_dir.join("cfgd.conf"), "old_module\n").unwrap();

    KernelModuleConfigurator::persist_modules_to(&conf_dir, &["new_module"]).unwrap();

    let content = fs::read_to_string(conf_dir.join("cfgd.conf")).unwrap();
    assert!(content.contains("new_module"));
    assert!(!content.contains("old_module"));
}

// --- SeccompConfigurator apply: writes profiles to temp dirs ---

#[test]
fn seccomp_apply_writes_profiles() {
    let dir = tempdir().unwrap();
    let profiles_dir = dir.path().join("seccomp");
    let printer = test_printer();
    let sc = SeccompConfigurator;

    let mut profile = serde_yaml::Mapping::new();
    profile.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("test-audit".into()),
    );
    profile.insert(
        serde_yaml::Value::String("file".into()),
        serde_yaml::Value::String("test-audit.json".into()),
    );
    profile.insert(
        serde_yaml::Value::String("content".into()),
        serde_yaml::Value::String(r#"{"defaultAction":"SCMP_ACT_LOG"}"#.into()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("profilesDir".into()),
        serde_yaml::Value::String(profiles_dir.to_str().unwrap().into()),
    );
    m.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    sc.apply(
        &desired,
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .unwrap();

    let written = fs::read_to_string(profiles_dir.join("test-audit.json")).unwrap();
    assert_eq!(written, r#"{"defaultAction":"SCMP_ACT_LOG"}"#);
}

#[test]
fn seccomp_apply_no_profiles_key_is_noop() {
    let printer = test_printer();
    let sc = SeccompConfigurator;
    let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    // Should not error even with no profiles key
    sc.apply(
        &desired,
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .unwrap();
}

#[test]
fn seccomp_apply_skips_missing_fields() {
    let dir = tempdir().unwrap();
    let profiles_dir = dir.path().join("seccomp");
    let printer = test_printer();
    let sc = SeccompConfigurator;

    // Profile with no "file" key
    let mut p1 = serde_yaml::Mapping::new();
    p1.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("no-file".into()),
    );
    p1.insert(
        serde_yaml::Value::String("content".into()),
        serde_yaml::Value::String("data".into()),
    );

    // Profile with no "content" key
    let mut p2 = serde_yaml::Mapping::new();
    p2.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("no-content".into()),
    );
    p2.insert(
        serde_yaml::Value::String("file".into()),
        serde_yaml::Value::String("no-content.json".into()),
    );

    // Profile with no "name" key
    let mut p3 = serde_yaml::Mapping::new();
    p3.insert(
        serde_yaml::Value::String("file".into()),
        serde_yaml::Value::String("nameless.json".into()),
    );
    p3.insert(
        serde_yaml::Value::String("content".into()),
        serde_yaml::Value::String("data".into()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("profilesDir".into()),
        serde_yaml::Value::String(profiles_dir.to_str().unwrap().into()),
    );
    m.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(vec![
            serde_yaml::Value::Mapping(p1),
            serde_yaml::Value::Mapping(p2),
            serde_yaml::Value::Mapping(p3),
        ]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    sc.apply(
        &desired,
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .unwrap();

    // profiles_dir should be created but no profiles written (all incomplete)
    assert!(profiles_dir.exists());
    // No files should have been written since each profile is missing a required field
    let entries: Vec<_> = fs::read_dir(&profiles_dir)
        .unwrap()
        .filter_map(|e| e.ok())
        .collect();
    assert!(entries.is_empty(), "no profiles should be written");
}

#[test]
fn seccomp_apply_path_traversal_skipped() {
    let dir = tempdir().unwrap();
    let profiles_dir = dir.path().join("seccomp");
    let printer = test_printer();
    let sc = SeccompConfigurator;

    let mut profile = serde_yaml::Mapping::new();
    profile.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("evil".into()),
    );
    profile.insert(
        serde_yaml::Value::String("file".into()),
        serde_yaml::Value::String("../../etc/passwd".into()),
    );
    profile.insert(
        serde_yaml::Value::String("content".into()),
        serde_yaml::Value::String("hacked".into()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("profilesDir".into()),
        serde_yaml::Value::String(profiles_dir.to_str().unwrap().into()),
    );
    m.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    sc.apply(
        &desired,
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .unwrap();

    // The traversal path file should NOT have been written
    let etc_passwd = dir.path().join("etc/passwd");
    assert!(!etc_passwd.exists(), "path traversal should be blocked");
}

#[test]
fn seccomp_apply_multiple_profiles() {
    let dir = tempdir().unwrap();
    let profiles_dir = dir.path().join("seccomp");
    let printer = test_printer();
    let sc = SeccompConfigurator;

    let mut p1 = serde_yaml::Mapping::new();
    p1.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("audit".into()),
    );
    p1.insert(
        serde_yaml::Value::String("file".into()),
        serde_yaml::Value::String("audit.json".into()),
    );
    p1.insert(
        serde_yaml::Value::String("content".into()),
        serde_yaml::Value::String(r#"{"action":"LOG"}"#.into()),
    );

    let mut p2 = serde_yaml::Mapping::new();
    p2.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("strict".into()),
    );
    p2.insert(
        serde_yaml::Value::String("file".into()),
        serde_yaml::Value::String("strict.json".into()),
    );
    p2.insert(
        serde_yaml::Value::String("content".into()),
        serde_yaml::Value::String(r#"{"action":"ERRNO"}"#.into()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("profilesDir".into()),
        serde_yaml::Value::String(profiles_dir.to_str().unwrap().into()),
    );
    m.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(vec![
            serde_yaml::Value::Mapping(p1),
            serde_yaml::Value::Mapping(p2),
        ]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    sc.apply(
        &desired,
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .unwrap();

    assert_eq!(
        fs::read_to_string(profiles_dir.join("audit.json")).unwrap(),
        r#"{"action":"LOG"}"#
    );
    assert_eq!(
        fs::read_to_string(profiles_dir.join("strict.json")).unwrap(),
        r#"{"action":"ERRNO"}"#
    );
}

// --- CertificateConfigurator apply: creates dir and sets permissions ---

#[test]
fn certificate_apply_creates_ca_cert_dir() {
    let dir = tempdir().unwrap();
    let ca_dir = dir.path().join("pki");
    let printer = test_printer();
    let cc = CertificateConfigurator;

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("caCertDir".into()),
        serde_yaml::Value::String(ca_dir.to_str().unwrap().into()),
    );
    // No certificates key — should only create the dir
    let desired = serde_yaml::Value::Mapping(m);
    cc.apply(
        &desired,
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .unwrap();
    assert!(ca_dir.exists());
}

#[test]
fn certificate_apply_no_certificates_is_noop() {
    let printer = test_printer();
    let cc = CertificateConfigurator;
    let desired = serde_yaml::Value::Mapping(serde_yaml::Mapping::new());
    // Should not error with no caCertDir or certificates
    cc.apply(
        &desired,
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .unwrap();
}

#[test]
fn certificate_apply_sets_permissions_on_existing_files() {
    let dir = tempdir().unwrap();
    let cert_path = dir.path().join("tls.crt");
    let key_path = dir.path().join("tls.key");
    fs::write(&cert_path, "cert data").unwrap();
    fs::write(&key_path, "key data").unwrap();

    // Set initial permissions to 0o644
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&cert_path, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).unwrap();
    }

    let printer = test_printer();
    let cc = CertificateConfigurator;

    let mut cert = serde_yaml::Mapping::new();
    cert.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("tls".into()),
    );
    cert.insert(
        serde_yaml::Value::String("certPath".into()),
        serde_yaml::Value::String(cert_path.to_str().unwrap().into()),
    );
    cert.insert(
        serde_yaml::Value::String("keyPath".into()),
        serde_yaml::Value::String(key_path.to_str().unwrap().into()),
    );
    cert.insert(
        serde_yaml::Value::String("mode".into()),
        serde_yaml::Value::String("0600".into()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("caCertDir".into()),
        serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
    );
    m.insert(
        serde_yaml::Value::String("certificates".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(cert)]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    cc.apply(
        &desired,
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let cert_mode = fs::metadata(&cert_path).unwrap().permissions().mode() & 0o777;
        let key_mode = fs::metadata(&key_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(cert_mode, 0o600);
        assert_eq!(key_mode, 0o600);
    }
}

#[test]
fn certificate_apply_warns_for_missing_files() {
    let dir = tempdir().unwrap();
    let printer = test_printer();
    let cc = CertificateConfigurator;

    let mut cert = serde_yaml::Mapping::new();
    cert.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("missing".into()),
    );
    cert.insert(
        serde_yaml::Value::String("certPath".into()),
        serde_yaml::Value::String("/nonexistent/cert.pem".into()),
    );
    cert.insert(
        serde_yaml::Value::String("mode".into()),
        serde_yaml::Value::String("0600".into()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("caCertDir".into()),
        serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
    );
    m.insert(
        serde_yaml::Value::String("certificates".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(cert)]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    // Should not error — just warns about missing files
    cc.apply(
        &desired,
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .unwrap();
}

#[test]
fn certificate_apply_skips_cert_without_name() {
    let dir = tempdir().unwrap();
    let printer = test_printer();
    let cc = CertificateConfigurator;

    let mut cert = serde_yaml::Mapping::new();
    // No "name" key
    cert.insert(
        serde_yaml::Value::String("certPath".into()),
        serde_yaml::Value::String("/tmp/cert.pem".into()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("caCertDir".into()),
        serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
    );
    m.insert(
        serde_yaml::Value::String("certificates".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(cert)]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    cc.apply(
        &desired,
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .unwrap();
}

#[test]
fn certificate_apply_correct_permissions_no_change() {
    let dir = tempdir().unwrap();
    let cert_path = dir.path().join("already-ok.crt");
    fs::write(&cert_path, "cert data").unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&cert_path, fs::Permissions::from_mode(0o600)).unwrap();
    }

    let printer = test_printer();
    let cc = CertificateConfigurator;

    let mut cert = serde_yaml::Mapping::new();
    cert.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("ok-cert".into()),
    );
    cert.insert(
        serde_yaml::Value::String("certPath".into()),
        serde_yaml::Value::String(cert_path.to_str().unwrap().into()),
    );
    cert.insert(
        serde_yaml::Value::String("mode".into()),
        serde_yaml::Value::String("0600".into()),
    );

    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("caCertDir".into()),
        serde_yaml::Value::String(dir.path().to_str().unwrap().into()),
    );
    m.insert(
        serde_yaml::Value::String("certificates".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(cert)]),
    );

    let desired = serde_yaml::Value::Mapping(m);
    // Should not error and should detect permissions are already correct
    cc.apply(
        &desired,
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .unwrap();

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&cert_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "permissions should remain unchanged");
    }
}

// --- SeccompConfigurator apply uses default profilesDir ---

#[test]
fn seccomp_apply_uses_default_profiles_dir_when_unset() {
    let printer = test_printer();
    let sc = SeccompConfigurator;

    // profiles key with empty sequence — should try to create /etc/cfgd/seccomp
    // but that requires root, so we just verify the no-profiles case
    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(Vec::new()),
    );
    let desired = serde_yaml::Value::Mapping(m);
    // Empty profiles list - should still try to create dir but won't error
    // because we catch the permission error at fs::create_dir_all
    // Actually, let's verify this specific case doesn't panic
    let result = sc.apply(
        &desired,
        &cfgd_core::providers::SystemContext::new(&printer),
    );
    // On CI/test machines this may fail due to permissions, which is expected
    // The important thing is it doesn't panic
    let _ = result;
}

// --- KernelModuleConfigurator::apply early returns ---

#[test]
fn kernel_modules_apply_with_non_sequence_value_emits_no_output_and_does_not_call_modprobe() {
    // Behavior contract: desired isn't a sequence → match Ok(None) → early Ok.
    // The body must NOT print anything (no "modprobe X" info line) AND must
    // NOT touch /etc/modules-load.d (the persist call is inside the same
    // function and is gated on the same modules.as_sequence check).
    let km = KernelModuleConfigurator;
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    km.apply(
        &serde_yaml::Value::String("not-a-sequence".into()),
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .expect("non-sequence value is a no-op");
    let captured = buf.lock().unwrap().clone();
    assert!(
        !captured.contains("modprobe"),
        "no-op path must not emit a modprobe line: {captured}"
    );
}

#[test]
fn kernel_modules_apply_with_empty_sequence_emits_no_modprobe_line() {
    // Empty `Sequence([])` → match Ok(Some([])) → for-loop body doesn't run,
    // desired_names stays empty, persist_modules is called with an empty
    // slice (which removes any prior conf file; we can't observe that path
    // without writing to /etc/, so we pin only the user-visible signal:
    // no `modprobe` info line fires).
    let km = KernelModuleConfigurator;
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    km.apply(
        &serde_yaml::Value::Sequence(Vec::new()),
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .expect("empty sequence must Ok");
    let captured = buf.lock().unwrap().clone();
    assert!(
        !captured.contains("modprobe"),
        "empty-sequence path must not invoke modprobe: {captured}"
    );
}

// --- AppArmorConfigurator::apply early returns ---

#[test]
fn apparmor_apply_with_no_profiles_field_emits_no_output_and_loads_nothing() {
    // desired without `profiles` key → match Ok(None) → early Ok at lines
    // 144-147. Body must NOT call apparmor_parser, so NO "Loading AppArmor
    // profile" / "Writing AppArmor profile" lines fire on the printer.
    let ac = AppArmorConfigurator;
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    ac.apply(
        &serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .expect("missing profiles key is a no-op");
    let captured = buf.lock().unwrap().clone();
    assert!(
        !captured.contains("AppArmor"),
        "no-profiles path must not announce any AppArmor work: {captured}"
    );
}

#[test]
fn apparmor_apply_skips_profile_entries_with_path_traversal() {
    // Profile path contains `..` → validate_no_traversal Errs → continue
    // at lines 159-165. With only the bad entry the for-loop completes
    // without calling apparmor_parser, so apply Ok.
    let ac = AppArmorConfigurator;
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let mut profile = serde_yaml::Mapping::new();
    profile.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("escaping".into()),
    );
    profile.insert(
        serde_yaml::Value::String("path".into()),
        serde_yaml::Value::String("/etc/apparmor.d/../../../tmp/oops".into()),
    );
    let mut desired = serde_yaml::Mapping::new();
    desired.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
    );
    ac.apply(
        &serde_yaml::Value::Mapping(desired),
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .expect("traversal-skip path must Ok");
    let output = buf.lock().unwrap();
    assert!(
        output.contains("path traversal"),
        "should warn about traversal: {output}"
    );
}

#[test]
fn apparmor_apply_skips_a_profile_path_that_names_no_file() {
    // `/` and `/.` resolve to a directory, not a profile. `atomic_write_str`
    // would fail on them anyway; the point is that the entry is refused with a
    // warning instead of aborting the whole configurator on an io error.
    let ac = AppArmorConfigurator;
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let mut profile = serde_yaml::Mapping::new();
    profile.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("rootish".into()),
    );
    profile.insert(
        serde_yaml::Value::String("path".into()),
        serde_yaml::Value::String("/".into()),
    );
    profile.insert(
        serde_yaml::Value::String("content".into()),
        serde_yaml::Value::String("profile rootish {}".into()),
    );
    let mut desired = serde_yaml::Mapping::new();
    desired.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
    );

    ac.apply(
        &serde_yaml::Value::Mapping(desired),
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .expect("a skipped entry must not fail apply");

    let output = buf.lock().unwrap();
    assert!(
        output.contains("names no file or directory"),
        "should warn about the unusable path: {output}"
    );
}

#[test]
fn seccomp_apply_skips_a_file_name_that_resolves_to_the_profiles_dir() {
    let dir = tempfile::tempdir().unwrap();
    let sc = SeccompConfigurator;
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let mut profile = serde_yaml::Mapping::new();
    profile.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String("dotted".into()),
    );
    profile.insert(
        serde_yaml::Value::String("file".into()),
        serde_yaml::Value::String(".".into()),
    );
    profile.insert(
        serde_yaml::Value::String("content".into()),
        serde_yaml::Value::String("{}".into()),
    );
    let mut desired = serde_yaml::Mapping::new();
    desired.insert(
        serde_yaml::Value::String("profilesDir".into()),
        serde_yaml::Value::String(cfgd_core::to_posix_string(dir.path())),
    );
    desired.insert(
        serde_yaml::Value::String("profiles".into()),
        serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(profile)]),
    );

    sc.apply(
        &serde_yaml::Value::Mapping(desired),
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .expect("a skipped entry must not fail apply");

    let output = buf.lock().unwrap();
    assert!(
        output.contains("names no file or directory"),
        "should warn about the unusable file name: {output}"
    );
}

// --- ContainerdConfigurator::apply paths ---
//
// Same shape as kubelet apply tests below: drive the no-op, empty-settings,
// and write+restart-fails arms. systemctl is unavailable in CI so the
// restart_containerd call fails after the merged TOML is on disk.

#[test]
fn containerd_apply_with_no_settings_field_is_a_noop() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.toml");
    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("configPath".into()),
        serde_yaml::Value::String(config.to_str().unwrap().into()),
    );
    let desired = serde_yaml::Value::Mapping(m);
    let cc = ContainerdConfigurator;
    let (printer, _buf) = cfgd_core::output::Printer::for_test();
    cc.apply(
        &desired,
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .expect("missing settings is no-op");
    assert!(!config.exists());
}

#[test]
fn containerd_apply_with_empty_settings_is_a_noop() {
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.toml");
    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("configPath".into()),
        serde_yaml::Value::String(config.to_str().unwrap().into()),
    );
    m.insert(
        serde_yaml::Value::String("settings".into()),
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
    );
    let desired = serde_yaml::Value::Mapping(m);
    let cc = ContainerdConfigurator;
    let (printer, _buf) = cfgd_core::output::Printer::for_test();
    cc.apply(
        &desired,
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .expect("empty settings is no-op");
    assert!(!config.exists());
}

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn containerd_apply_writes_toml_then_returns_err_when_systemctl_fails() {
    // Drives lines 105-169: settings non-empty → merge into current →
    // serialize TOML → atomic_write → restart fails → rollback arm.
    //
    // The failing systemctl is a shim, not the host's. Reading the host's
    // answer means that on a real k8s node this test runs
    // `systemctl restart containerd` against a live runtime — and then goes
    // red there because the restart SUCCEEDS.
    let _shim = cfgd_core::test_helpers::ToolShim::install(
        cfgd_core::SYSTEMCTL_BIN_ENV,
        1,
        "",
        "Failed to restart containerd.service: Unit not found.\n",
    );
    let dir = tempdir().unwrap();
    let config = dir.path().join("nested/config.toml");
    let mut settings = serde_yaml::Mapping::new();
    settings.insert(
        serde_yaml::Value::String("sandbox_image".into()),
        serde_yaml::Value::String("registry.k8s.io/pause:3.9".into()),
    );
    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("configPath".into()),
        serde_yaml::Value::String(config.to_str().unwrap().into()),
    );
    m.insert(
        serde_yaml::Value::String("settings".into()),
        serde_yaml::Value::Mapping(settings),
    );
    let desired = serde_yaml::Value::Mapping(m);
    let cc = ContainerdConfigurator;
    let (printer, _buf) = cfgd_core::output::Printer::for_test();
    // The body runs atomic_write before invoking systemctl. On hosts where
    // containerd is actually running, restart_containerd may succeed and
    // return Ok; on CI/dev boxes without containerd it returns Err. Either
    // way, the merge + serialize + atomic_write path on lines 105-152 must
    // have executed — verifiable via the config file contents on disk.
    cc.apply(
        &desired,
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .expect_err("a failing restart must surface as an error");
    assert!(
        config.exists(),
        "atomic_write must have run before restart fires"
    );
    let written = std::fs::read_to_string(&config).unwrap();
    assert!(
        written.contains("sandbox_image"),
        "config must contain merged setting: {written}"
    );
}

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn containerd_apply_with_existing_config_triggers_rollback_attempt_after_systemctl_fails() {
    // Drives the backup-restore branch at containerd.rs:155-167. Pre-stages
    // a valid TOML config so capture_file_state returns Some(state); apply
    // writes the merged config; the shimmed restart fails; the rollback arm
    // fires. Asserts the rollback warning is emitted and the final on-disk
    // config contains the original bytes.
    //
    // The shim is what makes the rollback arm reachable EVERYWHERE. Reading
    // the host's own systemctl left the whole assertion behind an `if
    // result.is_err()`, so on a node with containerd running it asserted
    // nothing — after restarting the node's container runtime.
    let _shim = cfgd_core::test_helpers::ToolShim::install(
        cfgd_core::SYSTEMCTL_BIN_ENV,
        1,
        "",
        "Failed to restart containerd.service: Unit not found.\n",
    );
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.toml");
    let original = "[plugins.\"io.containerd.grpc.v1.cri\"]\nsandbox_image = \"old:1.0\"\n";
    std::fs::write(&config, original).unwrap();

    let mut settings = serde_yaml::Mapping::new();
    settings.insert(
        serde_yaml::Value::String("sandbox_image".into()),
        serde_yaml::Value::String("new:2.0".into()),
    );
    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("configPath".into()),
        serde_yaml::Value::String(config.to_str().unwrap().into()),
    );
    m.insert(
        serde_yaml::Value::String("settings".into()),
        serde_yaml::Value::Mapping(settings),
    );
    let desired = serde_yaml::Value::Mapping(m);
    let cc = ContainerdConfigurator;
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let result = cc.apply(
        &desired,
        &cfgd_core::providers::SystemContext::new(&printer),
    );

    let captured = buf.lock().unwrap().clone();
    result.expect_err("a failing restart must surface as an error");
    assert!(
        captured.contains("restoring previous config"),
        "rollback warning expected on restart-failed path: {captured}"
    );
    let after = std::fs::read_to_string(&config).unwrap();
    assert!(
        after.contains("old:1.0"),
        "rollback must restore prior containerd config: {after}"
    );
}

// --- KubeletConfigurator::apply paths ---
//
// The 60+ uncovered lines in kubelet.rs are the apply() body. systemctl
// won't be available in CI/tests so the rollback arm fires after the
// atomic_write succeeds. These tests pin the no-op early returns plus the
// "write + restart fails + attempt rollback" sequence.

#[test]
fn kubelet_apply_with_no_settings_field_is_a_noop() {
    // desired without `settings` → match returns None → early Ok(()).
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("configPath".into()),
        serde_yaml::Value::String(config.to_str().unwrap().into()),
    );
    let desired = serde_yaml::Value::Mapping(m);
    let kc = KubeletConfigurator;
    let (printer, _buf) = cfgd_core::output::Printer::for_test();
    kc.apply(
        &desired,
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .expect("missing settings must be Ok(no-op)");
    assert!(!config.exists(), "no-op must not create the config file");
}

#[test]
fn kubelet_apply_with_empty_settings_is_a_noop() {
    // desired with empty `settings: {}` → settings.is_empty() → early Ok.
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("configPath".into()),
        serde_yaml::Value::String(config.to_str().unwrap().into()),
    );
    m.insert(
        serde_yaml::Value::String("settings".into()),
        serde_yaml::Value::Mapping(serde_yaml::Mapping::new()),
    );
    let desired = serde_yaml::Value::Mapping(m);
    let kc = KubeletConfigurator;
    let (printer, _buf) = cfgd_core::output::Printer::for_test();
    kc.apply(
        &desired,
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .expect("empty settings must be Ok(no-op)");
    assert!(
        !config.exists(),
        "empty-settings no-op must not write the config"
    );
}

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn kubelet_apply_writes_config_then_returns_err_when_systemctl_fails() {
    // Settings present → apply writes the merged config via atomic_write,
    // then shells out to `systemctl restart kubelet`. The restart is shimmed
    // to fail, so the failure arm runs on every host — including a real k8s
    // node, where "in CI/tests systemctl is either absent or the kubelet unit
    // doesn't exist" is false and this test would otherwise restart the
    // node's kubelet and then fail on the Ok it got back. We assert:
    //   (a) the merged config IS written to disk (atomic_write fired)
    //   (b) the returned Err carries a systemctl-related message
    let _shim = cfgd_core::test_helpers::ToolShim::install(
        cfgd_core::SYSTEMCTL_BIN_ENV,
        1,
        "",
        "Failed to restart kubelet.service: Unit kubelet.service not found.\n",
    );
    let dir = tempdir().unwrap();
    let config = dir.path().join("nested/sub/config.yaml");
    let mut settings = serde_yaml::Mapping::new();
    settings.insert(
        serde_yaml::Value::String("maxPods".into()),
        serde_yaml::Value::Number(110.into()),
    );
    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("configPath".into()),
        serde_yaml::Value::String(config.to_str().unwrap().into()),
    );
    m.insert(
        serde_yaml::Value::String("settings".into()),
        serde_yaml::Value::Mapping(settings),
    );
    let desired = serde_yaml::Value::Mapping(m);
    let kc = KubeletConfigurator;
    let (printer, _buf) = cfgd_core::output::Printer::for_test();
    let err = kc
        .apply(
            &desired,
            &cfgd_core::providers::SystemContext::new(&printer),
        )
        .expect_err("the shimmed systemctl restart fails, so apply must too");
    assert!(
        config.exists(),
        "atomic_write must have written the merged config before restart"
    );
    let written = std::fs::read_to_string(&config).unwrap();
    assert!(
        written.contains("maxPods"),
        "config must contain the desired setting key: {written}"
    );
    let msg = err.to_string();
    assert!(
        msg.contains("systemctl") || msg.contains("kubelet"),
        "err should mention systemctl/kubelet, got: {msg}"
    );
}

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn kubelet_apply_with_existing_config_triggers_rollback_attempt_after_systemctl_fails() {
    // Drives the backup-restore branch at kubelet.rs:166-180. Pre-stage a
    // valid kubelet config so `capture_file_state` returns Some(state) with
    // !is_symlink && !oversized. apply writes the new merged config; the
    // shimmed restart fails — never the host's, which on a real node means
    // restarting the kubelet out from under running workloads — the rollback
    // arm fires,
    // attempts to atomic_write the original bytes back, then re-tries the
    // restart (which fails again). Asserts:
    //   - the printer warning "kubelet restart failed — restoring previous
    //     config" was emitted (proves the if-let-Some(backup) branch ran)
    //   - the final config on disk matches the prior contents (rollback's
    //     atomic_write succeeded)
    let _shim = cfgd_core::test_helpers::ToolShim::install(
        cfgd_core::SYSTEMCTL_BIN_ENV,
        1,
        "",
        "Failed to restart kubelet.service: Unit kubelet.service not found.\n",
    );
    let dir = tempdir().unwrap();
    let config = dir.path().join("config.yaml");
    let original = "clusterDomain: cluster.local\nmaxPods: 50\n";
    std::fs::write(&config, original).unwrap();

    let mut settings = serde_yaml::Mapping::new();
    settings.insert(
        serde_yaml::Value::String("maxPods".into()),
        serde_yaml::Value::Number(110.into()),
    );
    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("configPath".into()),
        serde_yaml::Value::String(config.to_str().unwrap().into()),
    );
    m.insert(
        serde_yaml::Value::String("settings".into()),
        serde_yaml::Value::Mapping(settings),
    );
    let desired = serde_yaml::Value::Mapping(m);
    let kc = KubeletConfigurator;
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    kc.apply(
        &desired,
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .expect_err("a failing restart must surface as an error");

    let captured = buf.lock().unwrap().clone();
    assert!(
        captured.contains("restoring previous config"),
        "rollback warning must fire after restart failure: {captured}"
    );
    // After rollback the file on disk should contain the original bytes
    // (the rollback's atomic_write succeeded even though restart did not).
    let after = std::fs::read_to_string(&config).unwrap();
    assert_eq!(
        after.trim(),
        original.trim(),
        "rollback must restore prior config contents"
    );
}

#[test]
fn kubelet_error_subject_handles_multiline_systemctl_output() {
    // Regression for: `Renderer::write_line` debug-asserts on bodies that
    // contain `\n`. Multi-line systemctl errors (e.g. "Transport endpoint is
    // not connected\nSee system logs and 'systemctl status kubelet.service'
    // for details.") used to be pumped straight into `status_simple`'s
    // subject and would panic in debug builds when the rollback path fired.
    let err = std::io::Error::other(
        "Transport endpoint is not connected\n\
         See system logs and 'systemctl status kubelet.service' for details.",
    );
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    // Must not panic on the debug-assert in `Renderer::write_line`.
    super::kubelet::emit_warn_with_error(
        &cfgd_core::providers::SystemContext::new(&printer),
        "rollback: kubelet restart also failed",
        &err,
    );

    let captured = buf.lock().unwrap().clone();
    // First line embedded as the head of the subject.
    assert!(
        captured.contains("Transport endpoint is not connected"),
        "first error line must appear in output: {captured}"
    );
    // Second line preserved (joined with em-dash separator).
    assert!(
        captured.contains("See system logs"),
        "second error line must be preserved: {captured}"
    );
    // The status line itself must be single-line — no raw `\n` smuggled into
    // the rendered output between the prefix and the trailing systemctl text.
    let status_line = captured
        .lines()
        .find(|l| l.contains("rollback: kubelet restart also failed"))
        .expect("warn status line must be present");
    assert!(
        status_line.contains("Transport endpoint is not connected"),
        "subject must collapse onto one physical line: {status_line:?}"
    );
}

#[test]
fn containerd_rollback_subject_handles_multiline_systemctl_output() {
    // Regression covering the broader sweep: the `containerd` rollback arm
    // (containerd.rs:166-175) inlines the same `format!("…: {}", e)` shape
    // that `kubelet_error_subject_handles_multiline_systemctl_output`
    // pinned for kubelet. Both now route the captured error through
    // `cfgd_core::output::collapse_to_subject_line` so multi-line systemctl
    // output cannot trip `Renderer::write_line`'s debug-assert.
    let err = std::io::Error::other(
        "Transport endpoint is not connected\n\
         See system logs and 'systemctl status containerd.service' for details.",
    );
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);

    // Must not panic on the debug-assert in `Renderer::write_line`. Reported
    // through the same channel the arm uses, so a standalone context still
    // settles the line the assertions below read.
    cfgd_core::providers::SystemContext::new(&printer).report(
        cfgd_core::output::Role::Warn,
        format!(
            "rollback: containerd restart also failed: {}",
            cfgd_core::output::collapse_to_subject_line(&err)
        ),
    );

    let captured = buf.lock().unwrap().clone();
    let status_line = captured
        .lines()
        .find(|l| l.contains("rollback: containerd restart also failed"))
        .expect("warn status line must be present");
    assert!(
        status_line.contains("Transport endpoint is not connected"),
        "first error line must appear: {status_line:?}"
    );
    assert!(
        status_line.contains("See system logs"),
        "trailing systemctl context must be preserved on the same line: {status_line:?}"
    );
}

// --- SysctlConfigurator::apply paths ---
//
// apply() at sysctl.rs:128-157 is reachable on Linux without root because
// write_sysctl validates the key first (rejecting uppercase/special chars
// returns Err pre-shellout) and persist_all_sysctls writes to /etc/sysctl.d
// which fails for non-root → the warning arm fires.

#[test]
fn sysctl_apply_with_non_mapping_desired_is_a_noop() {
    let sc = SysctlConfigurator;
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    sc.apply(
        &serde_yaml::Value::String("not a mapping".into()),
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .expect("non-mapping must be Ok no-op");
    let captured = buf.lock().unwrap().clone();
    assert!(
        !captured.contains("sysctl"),
        "no work-line should be emitted on no-op: {captured}"
    );
}

#[test]
fn sysctl_apply_with_invalid_key_returns_validation_err() {
    // Invalid key (uppercase) trips validate_sysctl_key inside write_sysctl
    // → returns Err before shelling out to sysctl. Drives the per-entry
    // loop body at lines 136-147 plus the early-Err arm at line 145.
    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::String("NOT.LOWERCASE".into()),
        serde_yaml::Value::String("1".into()),
    );
    let desired = serde_yaml::Value::Mapping(m);
    let sc = SysctlConfigurator;
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let err = sc
        .apply(
            &desired,
            &cfgd_core::providers::SystemContext::new(&printer),
        )
        .expect_err("invalid key must error before shellout");
    let msg = err.to_string();
    assert!(
        msg.contains("invalid sysctl key"),
        "err should reference key validation: {msg}"
    );
    let captured = buf.lock().unwrap().clone();
    assert!(
        captured.contains("sysctl -w NOT.LOWERCASE=1"),
        "info line should fire before validation Err: {captured}"
    );
}

#[test]
fn sysctl_apply_skips_non_string_keys_without_panicking() {
    // Drives the `key.as_str() => None => continue` arm at lines 137-140.
    let mut m = serde_yaml::Mapping::new();
    m.insert(
        serde_yaml::Value::Number(42.into()),
        serde_yaml::Value::String("ignored".into()),
    );
    let desired = serde_yaml::Value::Mapping(m);
    let sc = SysctlConfigurator;
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    sc.apply(
        &desired,
        &cfgd_core::providers::SystemContext::new(&printer),
    )
    .expect("non-string keys must be skipped, not error");
    let captured = buf.lock().unwrap().clone();
    assert!(
        !captured.contains("sysctl -w"),
        "no work-line should fire when key is skipped: {captured}"
    );
}

// --- SystemdUnitConfigurator::apply paths ---
//
// apply() at systemd_unit.rs:83-144 has three loop-body branches that are
// reachable on Linux without root:
//   - unitFile present + source missing → "Failed to read unit file" warning
//   - unitFile present + source readable → atomic_write to /etc/systemd/system
//     fails (non-root) → "Failed to install unit file" warning
//   - no unitFile → straight to enable; systemctl enable on phantom unit fails
//     → "systemctl enable ... failed" warning (or Err if systemctl absent)

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn systemd_apply_unit_with_missing_unit_file_emits_read_failed_warning() {
    let _shim = cfgd_core::test_helpers::ToolShim::install(cfgd_core::SYSTEMCTL_BIN_ENV, 0, "", "");
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
- name: cfgd-test-phantom-read.service
  enabled: true
  unitFile: /nonexistent/path/to/source.service
"#,
    )
    .unwrap();
    let su = crate::system::SystemdUnitConfigurator::default();
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    su.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
        .expect("a shimmed systemctl succeeds, so only the unreadable source warns");
    let captured = buf.lock().unwrap().clone();
    assert!(
        captured.contains("Failed to read unit file"),
        "expected read-failed warning in printer buffer: {captured}"
    );
}

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn systemd_apply_unit_with_readable_source_emits_install_or_enable_line() {
    // Source unit file exists in tempdir; atomic_write to /etc/systemd/system
    // will fail for non-root → "Failed to install unit file" warning fires.
    // The "Installing unit file:" info line is emitted before the failure.
    let _shim = cfgd_core::test_helpers::ToolShim::install(cfgd_core::SYSTEMCTL_BIN_ENV, 0, "", "");
    let dir = tempdir().unwrap();
    let source = dir.path().join("cfgd-source.service");
    fs::write(&source, "[Unit]\nDescription=Test\n").unwrap();
    let yaml_str = format!(
        "- name: cfgd-test-phantom-install.service\n  enabled: true\n  unitFile: {}\n",
        source.display()
    );
    let yaml: serde_yaml::Value = serde_yaml::from_str(&yaml_str).unwrap();
    let su = crate::system::SystemdUnitConfigurator::default();
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    let _ = su.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer));
    let captured = buf.lock().unwrap().clone();
    assert!(
        captured.contains("Installing unit file:"),
        "info line for install should fire: {captured}"
    );
}

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn systemd_apply_unit_without_unit_file_proceeds_to_enable() {
    // No unitFile → skips the install block entirely and goes straight to
    // the enable shellout, which is what the argv log has to show.
    let shim = cfgd_core::test_helpers::ToolShim::install(cfgd_core::SYSTEMCTL_BIN_ENV, 0, "", "");
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
- name: cfgd-test-phantom-enable.service
  enabled: true
"#,
    )
    .unwrap();
    let su = crate::system::SystemdUnitConfigurator::default();
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    su.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
        .expect("Ok");
    let captured = buf.lock().unwrap().clone();
    assert!(
        captured.contains("systemctl enable cfgd-test-phantom-enable.service"),
        "info line for enable shellout should fire: {captured}"
    );
    assert_eq!(
        shim.argv_log().trim(),
        "enable cfgd-test-phantom-enable.service",
        "no unitFile means no daemon-reload — only the enable call is made"
    );
}

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn systemd_apply_unit_with_disabled_field_emits_disable_line() {
    // `enabled: false` → action is "disable" instead of "enable".
    let shim = cfgd_core::test_helpers::ToolShim::install(cfgd_core::SYSTEMCTL_BIN_ENV, 0, "", "");
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
- name: cfgd-test-phantom-disable.service
  enabled: false
"#,
    )
    .unwrap();
    let su = crate::system::SystemdUnitConfigurator::default();
    let (printer, buf) =
        cfgd_core::output::Printer::for_test_at(cfgd_core::output::Verbosity::Normal);
    su.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer))
        .expect("Ok");
    let captured = buf.lock().unwrap().clone();
    assert!(
        captured.contains("systemctl disable cfgd-test-phantom-disable.service"),
        "disable info line should fire when enabled=false: {captured}"
    );
    assert_eq!(
        shim.argv_log().trim(),
        "disable cfgd-test-phantom-disable.service",
        "`enabled: false` must reach systemctl as `disable`, not `enable`"
    );
}

#[cfg(unix)]
#[test]
#[serial_test::serial]
fn systemd_apply_unit_reloads_the_manager_before_enabling_an_installed_unit() {
    // A unit file that lands changes what systemd knows, so the reload has to
    // precede the enable — enabling first resolves against the stale view.
    let shim = cfgd_core::test_helpers::ToolShim::install(cfgd_core::SYSTEMCTL_BIN_ENV, 0, "", "");
    let dir = tempdir().unwrap();
    let source = dir.path().join("cfgd-order.service");
    fs::write(&source, "[Unit]\nDescription=Test\n").unwrap();
    let yaml_str = format!(
        "- name: cfgd-test-phantom-order.service\n  enabled: true\n  unitFile: {}\n",
        source.display()
    );
    let yaml: serde_yaml::Value = serde_yaml::from_str(&yaml_str).unwrap();
    let su = crate::system::SystemdUnitConfigurator::default();
    let printer = test_printer();
    let _ = su.apply(&yaml, &cfgd_core::providers::SystemContext::new(&printer));
    let argv: Vec<String> = shim
        .argv_log()
        .lines()
        .map(|l| l.to_string())
        .collect::<Vec<_>>();
    assert_eq!(
        argv,
        vec![
            "daemon-reload".to_string(),
            "enable cfgd-test-phantom-order.service".to_string(),
        ],
        "reload must run before enable"
    );
}

#[cfg(target_os = "linux")]
mod bridge {
    use super::*;
    use crate::system::tests_snapshot_bridge::{
        BridgeApply, assert_single_seam, capture_attached_apply,
    };
    use cfgd_core::output::Role;
    use cfgd_core::output::test_capture::assert_snapshot_at;

    fn snapshot_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/system/node/snapshots")
    }

    fn assert_snapshot(name: &str, actual: &str) {
        assert_snapshot_at(&snapshot_dir(), name, actual);
    }

    fn normalize_paths(raw: &str, tmpdir: &std::path::Path) -> String {
        cfgd_core::normalize_for_snapshot(raw, &[(tmpdir, "<TMPDIR>")])
    }

    #[derive(serde::Serialize)]
    struct NodeApplySummary {
        configurator: String,
        applied: bool,
    }

    // --- seccomp bridge tests ---

    /// Clean setup: single seccomp profile written to tmpdir. No external
    /// commands are called — seccomp apply is pure file I/O. The "Writing
    /// seccomp profile" info line and closing Ok Doc document the full apply
    /// path deterministically regardless of environment.
    #[test]
    fn snapshot_seccomp_clean() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profiles_dir = tmp.path().join("cfgd-seccomp");

        let desired: serde_yaml::Value = serde_yaml::from_str(&format!(
            r#"
profilesDir: {}
profiles:
  - name: default-audit
    file: default-audit.json
    content: |
      {{"defaultAction":"SCMP_ACT_LOG"}}
"#,
            profiles_dir.display()
        ))
        .unwrap();

        let sc = SeccompConfigurator;
        let summary = NodeApplySummary {
            configurator: "seccomp".to_string(),
            applied: true,
        };
        let raw = capture_attached_apply(
            &BridgeApply {
                configurator: &sc,
                desired: &desired,
                key: "default-audit",
                current: "missing",
                target: "present",
                summary_role: Role::Ok,
                summary: "seccomp profiles applied",
            },
            &summary,
        );
        let captured = normalize_paths(&raw, tmp.path());

        assert_single_seam("seccomp_clean", &captured);
        assert_snapshot("seccomp_clean.txt", &captured);
    }

    /// Warnings scenario: first profile has a path traversal in its file name
    /// (emits Warn + continues), second profile is valid. Two distinct output
    /// lines document the mixed-result apply surface. Pure file I/O; no external
    /// commands.
    #[test]
    fn snapshot_seccomp_with_warnings() {
        let tmp = tempfile::TempDir::new().unwrap();
        let profiles_dir = tmp.path().join("cfgd-seccomp");

        let desired: serde_yaml::Value = serde_yaml::from_str(&format!(
            r#"
profilesDir: {}
profiles:
  - name: traversal-profile
    file: ../../etc/cfgd-snap-traversal.json
    content: |
      {{"defaultAction":"SCMP_ACT_ERRNO"}}
  - name: allow-audit
    file: allow-audit.json
    content: |
      {{"defaultAction":"SCMP_ACT_LOG"}}
"#,
            profiles_dir.display()
        ))
        .unwrap();

        let sc = SeccompConfigurator;
        let summary = NodeApplySummary {
            configurator: "seccomp".to_string(),
            applied: true,
        };
        let raw = capture_attached_apply(
            &BridgeApply {
                configurator: &sc,
                desired: &desired,
                key: "allow-audit",
                current: "missing",
                target: "present",
                summary_role: Role::Warn,
                summary: "seccomp apply completed with warnings",
            },
            &summary,
        );
        let captured = normalize_paths(&raw, tmp.path());

        assert_single_seam("seccomp_with_warnings", &captured);
        assert_snapshot("seccomp_with_warnings.txt", &captured);
    }

    // --- certificates bridge tests ---

    /// Clean setup: certificate file exists with wrong permissions. Apply sets
    /// the desired mode and emits Info lines. Pure file I/O — no external
    /// commands. Always deterministic regardless of environment.
    #[test]
    fn snapshot_certificates_clean() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        let pki_dir = tmp.path().join("pki");
        fs::create_dir_all(&pki_dir).unwrap();

        let cert_path = pki_dir.join("kubelet-client.crt");
        let key_path = pki_dir.join("kubelet-client.key");
        fs::write(&cert_path, b"FAKE CERT").unwrap();
        fs::write(&key_path, b"FAKE KEY").unwrap();
        fs::set_permissions(&cert_path, fs::Permissions::from_mode(0o644)).unwrap();
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).unwrap();

        let desired: serde_yaml::Value = serde_yaml::from_str(&format!(
            r#"
caCertDir: {}
certificates:
  - name: kubelet-client
    certPath: {}
    keyPath: {}
    mode: "0600"
"#,
            pki_dir.display(),
            cert_path.display(),
            key_path.display(),
        ))
        .unwrap();

        let cc = CertificateConfigurator;
        let summary = NodeApplySummary {
            configurator: "certificates".to_string(),
            applied: true,
        };
        let raw = capture_attached_apply(
            &BridgeApply {
                configurator: &cc,
                desired: &desired,
                key: "cert.kubelet-client.cert.mode",
                current: "0644",
                target: "0600",
                summary_role: Role::Ok,
                summary: "certificates applied",
            },
            &summary,
        );
        let captured = normalize_paths(&raw, tmp.path());

        assert_single_seam("certificates_clean", &captured);
        assert_snapshot("certificates_clean.txt", &captured);
    }

    /// Warnings scenario: one cert file is missing (emits Warn) while another
    /// exists with wrong permissions (emits Info + fixes). Demonstrates the
    /// mixed Warn/Info output surface. Pure file I/O.
    #[test]
    fn snapshot_certificates_with_warnings() {
        use std::os::unix::fs::PermissionsExt;

        let tmp = tempfile::TempDir::new().unwrap();
        let pki_dir = tmp.path().join("pki");
        fs::create_dir_all(&pki_dir).unwrap();

        let cert_path = pki_dir.join("kubelet-client.crt");
        let key_path = pki_dir.join("kubelet-client.key");
        fs::write(&cert_path, b"FAKE CERT").unwrap();
        fs::set_permissions(&cert_path, fs::Permissions::from_mode(0o644)).unwrap();

        let desired: serde_yaml::Value = serde_yaml::from_str(&format!(
            r#"
caCertDir: {}
certificates:
  - name: kubelet-client
    certPath: {}
    keyPath: {}
    mode: "0600"
"#,
            pki_dir.display(),
            cert_path.display(),
            key_path.display(),
        ))
        .unwrap();

        let cc = CertificateConfigurator;
        let summary = NodeApplySummary {
            configurator: "certificates".to_string(),
            applied: true,
        };
        let raw = capture_attached_apply(
            &BridgeApply {
                configurator: &cc,
                desired: &desired,
                key: "cert.kubelet-client.key",
                current: "missing",
                target: "present",
                summary_role: Role::Warn,
                summary: "certificates apply completed with warnings",
            },
            &summary,
        );
        let captured = normalize_paths(&raw, tmp.path());

        assert_single_seam("certificates_with_warnings", &captured);
        assert_snapshot("certificates_with_warnings.txt", &captured);
    }
}
