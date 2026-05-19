use super::*;
use std::os::unix::fs::PermissionsExt;
use tempfile::TempDir;

mod bridge {
    use super::*;
    use cfgd_core::output_v2::test_capture::{assert_snapshot_at, strip_ansi};
    use cfgd_core::output_v2::{Doc, Printer as PrinterV2, Role};

    fn snapshot_dir() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/system/ssh_keys/snapshots")
    }

    fn assert_snapshot(name: &str, actual: &str) {
        assert_snapshot_at(&snapshot_dir(), name, actual);
    }

    fn normalize_paths(raw: &str, tmpdir: &std::path::Path) -> String {
        raw.replace(&tmpdir.to_string_lossy().to_string(), "<TMPDIR>")
    }

    #[derive(serde::Serialize)]
    struct KeyApplySummary {
        name: String,
        applied: bool,
    }

    #[test]
    fn snapshot_ssh_keys_clean() {
        let tmp = TempDir::new().unwrap();
        let ssh_dir = tmp.path().join(".ssh");
        let key_path = ssh_dir.join("id_ed25519");

        let desired =
            make_desired(&[("default", "ed25519", Some(&key_path.display().to_string()))]);

        let (printer, cap) = PrinterV2::for_test_doc();
        let c = SshKeysConfigurator;
        c.apply(&desired, &printer).unwrap();

        let summary = KeyApplySummary {
            name: "default".to_string(),
            applied: true,
        };
        let doc = Doc::new()
            .status(Role::Ok, "SSH keys applied")
            .with_data(&summary);
        printer.emit(doc);
        drop(printer);

        let raw = strip_ansi(&cap.human());
        let captured = normalize_paths(&raw, tmp.path());

        assert!(
            captured.contains("\n\n"),
            "ssh_keys_clean missing blank line at seam:\n{captured}"
        );
        assert!(
            !captured.contains("\n\n\n"),
            "ssh_keys_clean has duplicate blank line:\n{captured}"
        );

        assert_snapshot("ssh_keys_clean.txt", &captured);
    }

    #[test]
    fn snapshot_ssh_keys_with_warnings() {
        let tmp = TempDir::new().unwrap();
        let ssh_dir = tmp.path().join(".ssh");
        fs::create_dir_all(&ssh_dir).unwrap();
        let key_path = ssh_dir.join("id_ed25519");

        fs::write(&key_path, b"FAKE PRIVATE KEY").unwrap();
        let pub_path = ssh_dir.join("id_ed25519.pub");
        fs::write(&pub_path, b"ssh-rsa AAAA fake-key comment\n").unwrap();
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600)).unwrap();

        let desired =
            make_desired_with_perms("default", "ed25519", &key_path.display().to_string(), "600");

        let (printer, cap) = PrinterV2::for_test_doc();
        let c = SshKeysConfigurator;
        c.apply(&desired, &printer).unwrap();

        let summary = KeyApplySummary {
            name: "default".to_string(),
            applied: true,
        };
        let doc = Doc::new()
            .status(Role::Warn, "SSH keys applied with warnings")
            .with_data(&summary);
        printer.emit(doc);
        drop(printer);

        let raw = strip_ansi(&cap.human());
        let captured = normalize_paths(&raw, tmp.path());

        assert!(
            captured.contains("\n\n"),
            "ssh_keys_with_warnings missing blank line at seam:\n{captured}"
        );
        assert!(
            !captured.contains("\n\n\n"),
            "ssh_keys_with_warnings has duplicate blank line:\n{captured}"
        );

        assert_snapshot("ssh_keys_with_warnings.txt", &captured);
    }
}

fn make_desired(entries: &[(&str, &str, Option<&str>)]) -> serde_yaml::Value {
    // entries: (name, type, optional path override)
    let mut seq = Vec::new();
    for (name, key_type, path) in entries {
        let mut map = serde_yaml::Mapping::new();
        map.insert(
            serde_yaml::Value::String("name".into()),
            serde_yaml::Value::String(name.to_string()),
        );
        map.insert(
            serde_yaml::Value::String("type".into()),
            serde_yaml::Value::String(key_type.to_string()),
        );
        if let Some(p) = path {
            map.insert(
                serde_yaml::Value::String("path".into()),
                serde_yaml::Value::String(p.to_string()),
            );
        }
        seq.push(serde_yaml::Value::Mapping(map));
    }
    serde_yaml::Value::Sequence(seq)
}

fn make_desired_with_perms(
    name: &str,
    key_type: &str,
    path: &str,
    permissions: &str,
) -> serde_yaml::Value {
    let mut map = serde_yaml::Mapping::new();
    map.insert(
        serde_yaml::Value::String("name".into()),
        serde_yaml::Value::String(name.into()),
    );
    map.insert(
        serde_yaml::Value::String("type".into()),
        serde_yaml::Value::String(key_type.into()),
    );
    map.insert(
        serde_yaml::Value::String("path".into()),
        serde_yaml::Value::String(path.into()),
    );
    map.insert(
        serde_yaml::Value::String("permissions".into()),
        serde_yaml::Value::String(permissions.into()),
    );
    serde_yaml::Value::Sequence(vec![serde_yaml::Value::Mapping(map)])
}

#[test]
fn diff_detects_missing_key() {
    let tmp = TempDir::new().unwrap();
    let key_path = tmp.path().join("id_ed25519");
    let desired = make_desired(&[("default", "ed25519", Some(&key_path.display().to_string()))]);

    let c = SshKeysConfigurator;
    let drifts = c.diff(&desired).unwrap();

    assert!(
        drifts.iter().any(|d| d.key.ends_with(".exists")),
        "expected a missing-key drift, got {} drifts",
        drifts.len()
    );
}

#[test]
fn diff_detects_wrong_permissions() {
    let tmp = TempDir::new().unwrap();
    let ssh_dir = tmp.path().join(".ssh");
    fs::create_dir_all(&ssh_dir).unwrap();
    let key_path = ssh_dir.join("id_ed25519");

    // Write a fake private key and public key
    fs::write(&key_path, b"FAKE PRIVATE KEY").unwrap();
    let pub_path = ssh_dir.join("id_ed25519.pub");
    fs::write(&pub_path, b"ssh-ed25519 AAAA fake-key comment\n").unwrap();

    // Set permissions to 644 (wrong, desired is 600)
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).unwrap();

    let desired =
        make_desired_with_perms("default", "ed25519", &key_path.display().to_string(), "600");

    let c = SshKeysConfigurator;
    let drifts = c.diff(&desired).unwrap();

    let perms_drift = drifts.iter().find(|d| d.key.ends_with(".permissions"));
    assert!(
        perms_drift.is_some(),
        "expected permissions drift, got {} drifts",
        drifts.len()
    );
    let pd = perms_drift.unwrap();
    assert_eq!(pd.expected, "600");
    assert_eq!(pd.actual, "644");
}

#[test]
fn diff_returns_empty_when_key_is_correct() {
    let tmp = TempDir::new().unwrap();
    let ssh_dir = tmp.path().join(".ssh");
    fs::create_dir_all(&ssh_dir).unwrap();
    let key_path = ssh_dir.join("id_ed25519");

    fs::write(&key_path, b"FAKE PRIVATE KEY").unwrap();
    let pub_path = ssh_dir.join("id_ed25519.pub");
    fs::write(&pub_path, b"ssh-ed25519 AAAA fake-key comment\n").unwrap();

    // Set correct permissions 600
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600)).unwrap();

    let desired =
        make_desired_with_perms("default", "ed25519", &key_path.display().to_string(), "600");

    let c = SshKeysConfigurator;
    let drifts = c.diff(&desired).unwrap();

    assert!(
        drifts.is_empty(),
        "expected no drift, got {} drift entries",
        drifts.len()
    );
}

#[test]
fn diff_detects_type_mismatch() {
    let tmp = TempDir::new().unwrap();
    let ssh_dir = tmp.path().join(".ssh");
    fs::create_dir_all(&ssh_dir).unwrap();
    let key_path = ssh_dir.join("id_ed25519");

    // Write a fake private key and an RSA public key header
    fs::write(&key_path, b"FAKE PRIVATE KEY").unwrap();
    let pub_path = ssh_dir.join("id_ed25519.pub");
    fs::write(&pub_path, b"ssh-rsa AAAA fakekey comment\n").unwrap();
    fs::set_permissions(&key_path, fs::Permissions::from_mode(0o600)).unwrap();

    // Declare as ed25519 but the public key header is ssh-rsa
    let desired =
        make_desired_with_perms("default", "ed25519", &key_path.display().to_string(), "600");

    let c = SshKeysConfigurator;
    let drifts = c.diff(&desired).unwrap();

    let type_drift = drifts.iter().find(|d| d.key.ends_with(".type"));
    assert!(
        type_drift.is_some(),
        "expected a type drift entry, got {} drift entries",
        drifts.len()
    );
    let td = type_drift.unwrap();
    assert_eq!(td.expected, "ed25519");
    assert_eq!(td.actual, "rsa");
}

#[test]
fn apply_generates_key() {
    let tmp = TempDir::new().unwrap();
    let ssh_dir = tmp.path().join(".ssh");
    let key_path = ssh_dir.join("id_ed25519");

    let desired = make_desired(&[("default", "ed25519", Some(&key_path.display().to_string()))]);

    let (printer, _doc) = cfgd_core::output_v2::Printer::for_test_doc();
    let c = SshKeysConfigurator;
    c.apply(&desired, &printer).unwrap();

    assert!(key_path.exists(), "private key file was not generated");
    assert!(
        ssh_dir.join("id_ed25519.pub").exists(),
        "public key file was not generated"
    );
}

#[test]
fn apply_creates_parent_dir_with_700() {
    let tmp = TempDir::new().unwrap();
    let ssh_dir = tmp.path().join(".ssh");
    let key_path = ssh_dir.join("id_ed25519");

    // ssh_dir must not exist before apply
    assert!(!ssh_dir.exists());

    let desired = make_desired(&[("default", "ed25519", Some(&key_path.display().to_string()))]);

    let (printer, _doc) = cfgd_core::output_v2::Printer::for_test_doc();
    let c = SshKeysConfigurator;
    c.apply(&desired, &printer).unwrap();

    let meta = fs::metadata(&ssh_dir).unwrap();
    let mode = meta.permissions().mode() & 0o777;
    assert_eq!(
        mode, 0o700,
        "SSH directory should have mode 700, got {:o}",
        mode
    );
}

// --- SshKeySpec::resolved_path ---

#[test]
fn resolved_path_uses_provided_path() {
    let spec = SshKeySpec {
        name: "test".to_string(),
        key_type: "ed25519".to_string(),
        bits: None,
        path: Some("/custom/path/id_ed25519".to_string()),
        comment: None,
        passphrase: None,
        permissions: "600".to_string(),
    };
    assert_eq!(
        spec.resolved_path(),
        PathBuf::from("/custom/path/id_ed25519")
    );
}

#[test]
fn resolved_path_default_ed25519() {
    let spec = SshKeySpec {
        name: "default".to_string(),
        key_type: "ed25519".to_string(),
        bits: None,
        path: None,
        comment: None,
        passphrase: None,
        permissions: "600".to_string(),
    };
    let path = spec.resolved_path();
    assert!(
        path.to_str().unwrap().ends_with(".ssh/id_ed25519"),
        "expected path ending in .ssh/id_ed25519, got {}",
        path.display()
    );
}

#[test]
fn resolved_path_default_rsa() {
    let spec = SshKeySpec {
        name: "rsa-default".to_string(),
        key_type: "rsa".to_string(),
        bits: None,
        path: None,
        comment: None,
        passphrase: None,
        permissions: "600".to_string(),
    };
    let path = spec.resolved_path();
    assert!(
        path.to_str().unwrap().ends_with(".ssh/id_rsa"),
        "expected path ending in .ssh/id_rsa, got {}",
        path.display()
    );
}

// --- SshKeySpec::permissions_mode ---

#[test]
fn permissions_mode_valid_600() {
    let spec = SshKeySpec {
        name: "test".to_string(),
        key_type: "ed25519".to_string(),
        bits: None,
        path: None,
        comment: None,
        passphrase: None,
        permissions: "600".to_string(),
    };
    assert_eq!(spec.permissions_mode().unwrap(), 0o600);
}

#[test]
fn permissions_mode_valid_644() {
    let spec = SshKeySpec {
        name: "test".to_string(),
        key_type: "ed25519".to_string(),
        bits: None,
        path: None,
        comment: None,
        passphrase: None,
        permissions: "644".to_string(),
    };
    assert_eq!(spec.permissions_mode().unwrap(), 0o644);
}

#[test]
fn permissions_mode_invalid_returns_error() {
    let spec = SshKeySpec {
        name: "test".to_string(),
        key_type: "ed25519".to_string(),
        bits: None,
        path: None,
        comment: None,
        passphrase: None,
        permissions: "xyz".to_string(),
    };
    assert!(spec.permissions_mode().is_err());
}

// --- detect_key_type ---

#[test]
fn detect_key_type_ed25519() {
    let tmp = TempDir::new().unwrap();
    let key_path = tmp.path().join("id_test");
    let pub_path = tmp.path().join("id_test.pub");
    fs::write(&key_path, b"FAKE").unwrap();
    fs::write(&pub_path, b"ssh-ed25519 AAAA comment\n").unwrap();

    assert_eq!(
        SshKeysConfigurator::detect_key_type(&key_path),
        Some("ed25519".to_string())
    );
}

#[test]
fn detect_key_type_rsa() {
    let tmp = TempDir::new().unwrap();
    let key_path = tmp.path().join("id_test");
    let pub_path = tmp.path().join("id_test.pub");
    fs::write(&key_path, b"FAKE").unwrap();
    fs::write(&pub_path, b"ssh-rsa AAAA comment\n").unwrap();

    assert_eq!(
        SshKeysConfigurator::detect_key_type(&key_path),
        Some("rsa".to_string())
    );
}

#[test]
fn detect_key_type_ecdsa() {
    let tmp = TempDir::new().unwrap();
    let key_path = tmp.path().join("id_test");
    let pub_path = tmp.path().join("id_test.pub");
    fs::write(&key_path, b"FAKE").unwrap();
    fs::write(&pub_path, b"ecdsa-sha2-nistp256 AAAA comment\n").unwrap();

    assert_eq!(
        SshKeysConfigurator::detect_key_type(&key_path),
        Some("ecdsa".to_string())
    );
}

#[test]
fn detect_key_type_no_pub_file() {
    let tmp = TempDir::new().unwrap();
    let key_path = tmp.path().join("id_test");
    fs::write(&key_path, b"FAKE").unwrap();
    // No .pub file

    assert_eq!(SshKeysConfigurator::detect_key_type(&key_path), None);
}

// --- parse_desired ---

#[test]
fn parse_desired_empty_sequence() {
    let desired = serde_yaml::Value::Sequence(Vec::new());
    let specs = SshKeysConfigurator::parse_desired(&desired).unwrap();
    assert!(specs.is_empty());
}

#[test]
fn parse_desired_non_sequence_returns_empty() {
    let desired = serde_yaml::Value::String("not a sequence".into());
    let specs = SshKeysConfigurator::parse_desired(&desired).unwrap();
    assert!(specs.is_empty());
}

#[test]
fn parse_desired_unsupported_key_type_errors() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
- name: bad
  type: dsa
"#,
    )
    .unwrap();
    let result = SshKeysConfigurator::parse_desired(&yaml);
    assert!(result.is_err());
    let err = format!("{}", result.unwrap_err());
    assert!(
        err.contains("unsupported key type"),
        "error should mention unsupported type: {err}"
    );
}

#[test]
fn parse_desired_valid_entry_with_defaults() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
- name: mykey
"#,
    )
    .unwrap();
    let specs = SshKeysConfigurator::parse_desired(&yaml).unwrap();
    assert_eq!(specs.len(), 1);
    assert_eq!(specs[0].name, "mykey");
    assert_eq!(specs[0].key_type, "ed25519");
    assert_eq!(specs[0].permissions, "600");
}

#[test]
fn parse_desired_valid_rsa_with_bits() {
    let yaml: serde_yaml::Value = serde_yaml::from_str(
        r#"
- name: rsa-key
  type: rsa
  bits: 8192
  comment: "test key"
"#,
    )
    .unwrap();
    let specs = SshKeysConfigurator::parse_desired(&yaml).unwrap();
    assert_eq!(specs[0].key_type, "rsa");
    assert_eq!(specs[0].bits, Some(8192));
    assert_eq!(specs[0].comment, Some("test key".to_string()));
}

// --- current_perms_str ---

#[test]
fn current_perms_str_returns_octal() {
    let tmp = TempDir::new().unwrap();
    let file = tmp.path().join("test_file");
    fs::write(&file, "data").unwrap();
    fs::set_permissions(&file, fs::Permissions::from_mode(0o644)).unwrap();

    let perms = SshKeysConfigurator::current_perms_str(&file);
    assert_eq!(perms, Some("644".to_string()));
}

#[test]
fn current_perms_str_nonexistent_returns_none() {
    let perms = SshKeysConfigurator::current_perms_str(Path::new("/nonexistent/file"));
    assert_eq!(perms, None);
}

// --- current_state ---

#[test]
fn current_state_is_empty_sequence() {
    let c = SshKeysConfigurator;
    let state = c.current_state().unwrap();
    assert!(state.is_sequence());
    assert!(state.as_sequence().unwrap().is_empty());
}

// --- diff with multiple keys ---

#[test]
fn diff_multiple_keys_reports_all_missing() {
    let tmp = TempDir::new().unwrap();
    let key1 = tmp.path().join("id_ed25519");
    let key2 = tmp.path().join("id_rsa");

    let desired = make_desired(&[
        ("key1", "ed25519", Some(&key1.display().to_string())),
        ("key2", "rsa", Some(&key2.display().to_string())),
    ]);

    let c = SshKeysConfigurator;
    let drifts = c.diff(&desired).unwrap();

    let exists_drifts: Vec<_> = drifts
        .iter()
        .filter(|d| d.key.ends_with(".exists"))
        .collect();
    assert_eq!(
        exists_drifts.len(),
        2,
        "both missing keys should be reported"
    );
}
