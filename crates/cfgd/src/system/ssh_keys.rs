use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde::{Deserialize, Serialize};

use cfgd_core::errors::{CfgdError, Result};
use cfgd_core::output::Printer;
use cfgd_core::providers::{SystemConfigurator, SystemDrift};

// ---------------------------------------------------------------------------
// Config types
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SshKeySpec {
    pub name: String,
    #[serde(rename = "type", default = "default_key_type")]
    pub key_type: String,
    pub bits: Option<u32>,
    pub path: Option<String>,
    pub comment: Option<String>,
    pub passphrase: Option<String>,
    #[serde(default = "default_permissions")]
    pub permissions: String,
}

fn default_key_type() -> String {
    "ed25519".to_string()
}

fn default_permissions() -> String {
    "600".to_string()
}

impl SshKeySpec {
    /// Resolve the effective private key path (expands tilde, applies type default).
    fn resolved_path(&self) -> PathBuf {
        if let Some(ref p) = self.path {
            cfgd_core::expand_tilde(Path::new(p))
        } else {
            match self.key_type.as_str() {
                "rsa" => cfgd_core::expand_tilde(Path::new("~/.ssh/id_rsa")),
                _ => cfgd_core::expand_tilde(Path::new("~/.ssh/id_ed25519")),
            }
        }
    }

    /// Parse the `permissions` string (octal like "600") into a u32 mode.
    fn permissions_mode(&self) -> Result<u32> {
        u32::from_str_radix(&self.permissions, 8).map_err(|_| {
            CfgdError::Config(cfgd_core::errors::ConfigError::Invalid {
                message: format!(
                    "invalid permissions '{}' for SSH key '{}': must be an octal string like '600'",
                    self.permissions, self.name
                ),
            })
        })
    }
}

// ---------------------------------------------------------------------------
// SshKeysConfigurator
// ---------------------------------------------------------------------------

pub struct SshKeysConfigurator;

impl SshKeysConfigurator {
    /// Deserialize the desired value into a list of `SshKeySpec`.
    fn parse_desired(desired: &serde_yaml::Value) -> Result<Vec<SshKeySpec>> {
        let seq = match desired.as_sequence() {
            Some(s) => s,
            None => return Ok(Vec::new()),
        };
        let mut specs = Vec::with_capacity(seq.len());
        for item in seq {
            let spec: SshKeySpec = serde_yaml::from_value(item.clone()).map_err(|e| {
                CfgdError::Config(cfgd_core::errors::ConfigError::Invalid {
                    message: format!("invalid sshKeys entry: {}", e),
                })
            })?;
            // Validate key type — only ed25519 and rsa are supported
            match spec.key_type.as_str() {
                "ed25519" | "rsa" => {}
                other => {
                    return Err(CfgdError::Config(cfgd_core::errors::ConfigError::Invalid {
                        message: format!(
                            "unsupported key type '{}' for SSH key '{}': only 'ed25519' and 'rsa' are supported",
                            other, spec.name
                        ),
                    }));
                }
            }
            specs.push(spec);
        }
        Ok(specs)
    }

    /// Detect the key type of an existing key by reading the public key header.
    ///
    /// We read `<path>.pub` rather than the private key to avoid any passphrase prompt.
    fn detect_key_type(private_path: &Path) -> Option<String> {
        let pub_path = {
            let mut p = private_path.to_path_buf();
            let ext = p
                .extension()
                .map(|e| format!("{}.pub", e.to_string_lossy()))
                .unwrap_or_else(|| "pub".to_string());
            p.set_extension(ext);
            p
        };
        let content = fs::read_to_string(&pub_path).ok()?;
        let first_word = content.split_whitespace().next()?;
        // Map OpenSSH public key type header to our config key type name
        let key_type = match first_word {
            "ssh-rsa" => "rsa",
            "ssh-ed25519" => "ed25519",
            "ecdsa-sha2-nistp256" | "ecdsa-sha2-nistp384" | "ecdsa-sha2-nistp521" => "ecdsa",
            other => other,
        };
        Some(key_type.to_string())
    }

    /// Read current Unix permissions of a file as an octal string (e.g. "600").
    fn current_perms_str(path: &Path) -> Option<String> {
        let meta = fs::metadata(path).ok()?;
        let mode = cfgd_core::file_permissions_mode(&meta)?;
        Some(format!("{:o}", mode))
    }

    /// Ensure the SSH directory (parent of `path`) exists with mode 700.
    fn ensure_ssh_dir(path: &Path, printer: &Printer) -> Result<()> {
        let dir = match path.parent() {
            Some(d) => d,
            None => return Ok(()),
        };
        if !dir.exists() {
            printer.info(&format!("Creating SSH directory: {}", dir.display()));
            fs::create_dir_all(dir)?;
            cfgd_core::set_file_permissions(dir, 0o700)?;
        }
        Ok(())
    }

    /// Generate an SSH key pair using `ssh-keygen`.
    fn generate_key(spec: &SshKeySpec, path: &Path, printer: &Printer) -> Result<()> {
        printer.info(&format!(
            "Generating {} SSH key '{}' at {}",
            spec.key_type,
            spec.name,
            path.display()
        ));

        let mut cmd = Command::new("ssh-keygen");
        cmd.arg("-t").arg(&spec.key_type);
        cmd.arg("-f").arg(path);

        // RSA bits
        if spec.key_type == "rsa" {
            if let Some(bits) = spec.bits {
                cmd.arg("-b").arg(bits.to_string());
            } else {
                cmd.arg("-b").arg("4096");
            }
        }

        // Comment
        if let Some(ref comment) = spec.comment {
            cmd.arg("-C").arg(comment);
        }

        // Passphrase — for now only empty passphrase is supported.
        // The `passphrase` field may hold a secret provider URI for future resolution.
        cmd.arg("-N").arg("");

        let output = cmd.output().map_err(CfgdError::Io)?;

        if !output.status.success() {
            return Err(CfgdError::Io(std::io::Error::other(format!(
                "ssh-keygen failed for key '{}': {}",
                spec.name,
                cfgd_core::stderr_lossy_trimmed(&output)
            ))));
        }

        Ok(())
    }

    /// Apply correct permissions to a private key file.
    fn apply_permissions(spec: &SshKeySpec, path: &Path, printer: &Printer) -> Result<()> {
        let mode = spec.permissions_mode()?;
        printer.info(&format!(
            "Setting permissions {} on {}",
            spec.permissions,
            path.display()
        ));
        cfgd_core::set_file_permissions(path, mode)?;
        Ok(())
    }
}

impl SystemConfigurator for SshKeysConfigurator {
    fn name(&self) -> &str {
        "sshKeys"
    }

    fn is_available(&self) -> bool {
        cfgd_core::command_available("ssh-keygen")
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        // Return empty sequence — state is computed on demand during diff
        Ok(serde_yaml::Value::Sequence(Vec::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let specs = Self::parse_desired(desired)?;
        let mut drifts = Vec::new();

        for spec in &specs {
            let path = spec.resolved_path();
            let key_id = format!("sshKeys.{}", spec.name);

            if !path.exists() {
                drifts.push(SystemDrift {
                    key: format!("{}.exists", key_id),
                    expected: "present".to_string(),
                    actual: "missing".to_string(),
                });
                // No further checks possible without the key
                continue;
            }

            // Check key type via public key file
            if let Some(actual_type) = Self::detect_key_type(&path)
                && actual_type != spec.key_type
            {
                drifts.push(SystemDrift {
                    key: format!("{}.type", key_id),
                    expected: spec.key_type.clone(),
                    actual: actual_type,
                });
            }

            // Check permissions (Unix only; Windows always matches)
            if let Some(actual_perms) = Self::current_perms_str(&path)
                && actual_perms != spec.permissions
            {
                drifts.push(SystemDrift {
                    key: format!("{}.permissions", key_id),
                    expected: spec.permissions.clone(),
                    actual: actual_perms,
                });
            }
        }

        Ok(drifts)
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let specs = Self::parse_desired(desired)?;

        for spec in &specs {
            let path = spec.resolved_path();

            // Warn about unsupported passphrase field
            if spec.passphrase.is_some() {
                printer.warning(&format!(
                    "SSH key '{}': passphrase field is not yet supported; key will be generated without passphrase",
                    spec.name
                ));
            }

            // Ensure parent directory exists with mode 700
            Self::ensure_ssh_dir(&path, printer)?;

            if !path.exists() {
                // Generate new key
                Self::generate_key(spec, &path, printer)?;
                // ssh-keygen already sets 600 on the private key, but enforce our config
                Self::apply_permissions(spec, &path, printer)?;
            } else {
                // Key exists — only fix permissions if wrong
                let needs_perms_fix = Self::current_perms_str(&path)
                    .map(|p| p != spec.permissions)
                    .unwrap_or(false);

                if needs_perms_fix {
                    Self::apply_permissions(spec, &path, printer)?;
                }

                // Type mismatch is drift but we do NOT regenerate an existing key
                // to avoid destroying data. Warn the user instead.
                if let Some(actual_type) = Self::detect_key_type(&path)
                    && actual_type != spec.key_type
                {
                    printer.warning(&format!(
                        "SSH key '{}' at {} is type '{}' but '{}' is desired; \
                         cfgd will not overwrite an existing key — remove it manually to regenerate",
                        spec.name,
                        path.display(),
                        actual_type,
                        spec.key_type
                    ));
                }
            }
        }

        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
#[cfg(unix)]
mod tests {
    use super::*;
    use cfgd_core::output::Verbosity;
    use std::os::unix::fs::PermissionsExt;
    use tempfile::TempDir;

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
        let desired =
            make_desired(&[("default", "ed25519", Some(&key_path.display().to_string()))]);

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

        let desired =
            make_desired(&[("default", "ed25519", Some(&key_path.display().to_string()))]);

        let printer = Printer::new(Verbosity::Quiet);
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

        let desired =
            make_desired(&[("default", "ed25519", Some(&key_path.display().to_string()))]);

        let printer = Printer::new(Verbosity::Quiet);
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
}
