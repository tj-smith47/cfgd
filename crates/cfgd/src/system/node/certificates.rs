use std::fs;
use std::path::Path;

use cfgd_core::errors::Result;
use cfgd_core::output::{Printer, Role};
use cfgd_core::providers::{SystemConfigurator, SystemDrift};

// ---------------------------------------------------------------------------
// CertificateConfigurator
// ---------------------------------------------------------------------------

/// Manages TLS certificates for node services.
///
/// Config format:
/// ```yaml
/// certificates:
///   caCertDir: /etc/kubernetes/pki
///   certificates:
///     - name: kubelet-client
///       certPath: /etc/kubernetes/pki/kubelet-client.crt
///       keyPath: /etc/kubernetes/pki/kubelet-client.key
///       mode: "0600"
/// ```
pub struct CertificateConfigurator;

impl SystemConfigurator for CertificateConfigurator {
    fn name(&self) -> &str {
        "certificates"
    }

    fn is_available(&self) -> bool {
        cfg!(target_os = "linux")
    }

    fn current_state(&self) -> Result<serde_yaml::Value> {
        Ok(serde_yaml::Value::Mapping(serde_yaml::Mapping::new()))
    }

    fn diff(&self, desired: &serde_yaml::Value) -> Result<Vec<SystemDrift>> {
        let mut drifts = Vec::new();

        let certs = match desired.get("certificates").and_then(|v| v.as_sequence()) {
            Some(s) => s,
            None => return Ok(drifts),
        };

        for cert in certs {
            let name = match cert.get("name").and_then(|v| v.as_str()) {
                Some(n) => n,
                None => continue,
            };

            if let Some(cert_path) = cert.get("certPath").and_then(|v| v.as_str())
                && !Path::new(cert_path).exists()
            {
                drifts.push(SystemDrift {
                    key: format!("cert.{}.cert", name),
                    expected: "present".to_string(),
                    actual: "missing".to_string(),
                });
            }

            if let Some(key_path) = cert.get("keyPath").and_then(|v| v.as_str())
                && !Path::new(key_path).exists()
            {
                drifts.push(SystemDrift {
                    key: format!("cert.{}.key", name),
                    expected: "present".to_string(),
                    actual: "missing".to_string(),
                });
            }

            if let Some(ca_path) = cert.get("caPath").and_then(|v| v.as_str())
                && !Path::new(ca_path).exists()
            {
                drifts.push(SystemDrift {
                    key: format!("cert.{}.ca", name),
                    expected: "present".to_string(),
                    actual: "missing".to_string(),
                });
            }

            if let Some(mode_str) = cert.get("mode").and_then(|v| v.as_str()) {
                let desired_mode = cfgd_core::parse_octal_mode(mode_str)?;
                for path_key in &["certPath", "keyPath", "caPath"] {
                    if let Some(path) = cert.get(*path_key).and_then(|v| v.as_str())
                        && let Ok(meta) = fs::metadata(path)
                        && let Some(current_mode) = cfgd_core::file_permissions_mode(&meta)
                        && current_mode != desired_mode
                    {
                        drifts.push(SystemDrift {
                            key: format!("cert.{}.{}.mode", name, path_key),
                            expected: format!("{:04o}", desired_mode),
                            actual: format!("{:04o}", current_mode),
                        });
                    }
                }
            }
        }

        Ok(drifts)
    }

    fn apply(&self, desired: &serde_yaml::Value, printer: &Printer) -> Result<()> {
        let ca_cert_dir = desired
            .get("caCertDir")
            .and_then(|v| v.as_str())
            .unwrap_or("/etc/kubernetes/pki");

        let certs = match desired.get("certificates").and_then(|v| v.as_sequence()) {
            Some(s) => s,
            None => {
                // Create the directory when caCertDir is explicitly configured
                // (signals intent to manage the PKI directory) even if no certs are listed yet.
                if desired.get("caCertDir").is_some() {
                    fs::create_dir_all(ca_cert_dir)?;
                }
                return Ok(());
            }
        };

        fs::create_dir_all(ca_cert_dir)?;

        for cert in certs {
            let name = match cert.get("name").and_then(|v| v.as_str()) {
                Some(n) => n,
                None => continue,
            };

            let mode_str = cert.get("mode").and_then(|v| v.as_str()).unwrap_or("0644");
            let desired_mode = cfgd_core::parse_octal_mode(mode_str)?;

            for path_key in &["certPath", "keyPath", "caPath"] {
                if let Some(path_str) = cert.get(*path_key).and_then(|v| v.as_str()) {
                    let path = Path::new(path_str);
                    if path.exists() {
                        let meta = fs::metadata(path)?;
                        let current_mode = cfgd_core::file_permissions_mode(&meta);
                        if current_mode != Some(desired_mode) {
                            printer.status_simple(
                                Role::Info,
                                format!(
                                    "Setting permissions {:04o} on {} ({})",
                                    desired_mode, path_str, name
                                ),
                            );
                            cfgd_core::set_file_permissions(path, desired_mode)?;
                        }
                    } else {
                        printer.status_simple(
                            Role::Warn,
                            format!("Certificate file missing: {} ({})", path_str, name),
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

    #[test]
    fn diff_rejects_invalid_mode_instead_of_silently_defaulting() {
        let desired: serde_yaml::Value = serde_yaml::from_str(
            r#"
certificates:
  - name: apiserver
    certPath: /nonexistent/apiserver.crt
    mode: "9zz"
"#,
        )
        .unwrap();
        let result = CertificateConfigurator.diff(&desired);
        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("invalid octal mode must surface as an error, not default to 0644"),
        };
        assert!(
            err.to_string().contains("9zz"),
            "error should name the offending mode: {err}"
        );
    }

    #[test]
    fn apply_rejects_invalid_mode_instead_of_silently_defaulting() {
        let (printer, _doc) = cfgd_core::output::Printer::for_test_doc();
        let dir = tempfile::tempdir().unwrap();
        let ca_dir = dir.path().join("pki");
        let cert_path = dir.path().join("apiserver.crt");
        std::fs::write(&cert_path, "cert").unwrap();
        let desired: serde_yaml::Value = serde_yaml::from_str(&format!(
            r#"
caCertDir: {}
certificates:
  - name: apiserver
    certPath: {}
    mode: "9zz"
"#,
            ca_dir.display(),
            cert_path.display(),
        ))
        .unwrap();
        let err = CertificateConfigurator
            .apply(&desired, &printer)
            .expect_err("invalid octal mode must surface as an error, not default to 0644");
        assert!(
            err.to_string().contains("9zz"),
            "error should name the offending mode: {err}"
        );
    }
}
